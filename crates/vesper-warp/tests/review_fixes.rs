//! Regressions for the PR #101 review: key updates, launch backpressure,
//! oversize replies and the RFC 9002 loss timer after an ACK.

mod support;

use support::*;
use vesper_warp::endpoint::{Discard, Echo, EndpointConfig, Responder, WarpError};

fn config() -> EndpointConfig {
    EndpointConfig {
        initial_rtt_ns: 20 * MS,
        ..EndpointConfig::default()
    }
}

fn assert_clean<R: Responder>(e: &mut Endpoint<R>) {
    let census = e.xsk_mut().census();
    assert_eq!(census.app_rx, 0, "{census:?}");
    assert_eq!(census.app_tx, 0, "{census:?}");
    assert_eq!(census.leaked, 0, "{census:?}");
    assert_eq!(e.stats().leases_orphaned, 0);
    assert_eq!(e.stats().tx_abandoned, 0);
}

/// Sends `count` chunks, stepping the network whenever the sender is
/// window-limited or backpressured. Returns how often each error was seen.
fn bulk<C: Responder, S: Responder>(p: &mut Pair<C, S>, count: u64, len: usize) -> (u64, u64) {
    let chunk = vec![9u8; len];
    let (mut sent, mut offset) = (0u64, 0u64);
    let (mut cwnd, mut backpressure) = (0, 0);
    for _ in 0..20_000 {
        if sent == count {
            break;
        }
        while sent < count {
            match p
                .client
                .send_stream(p.c_conn, 0, offset, &chunk, false, p.now)
            {
                Ok(_) => {
                    sent += 1;
                    offset += len as u64;
                }
                Err(WarpError::CwndLimited) => {
                    cwnd += 1;
                    break;
                }
                Err(WarpError::LaunchBackpressure) => {
                    backpressure += 1;
                    break;
                }
                Err(e) => panic!("{e:?}"),
            }
        }
        p.step();
    }
    assert_eq!(sent, count);
    (cwnd, backpressure)
}

#[test]
fn keys_update_on_the_packet_interval_and_traffic_continues() {
    let cfg = EndpointConfig {
        key_update_interval: 16,
        ..config()
    };
    let mut p = Pair::new(cfg, Discard::default(), Echo::default());
    bulk(&mut p, 200, 500);
    assert!(p.settle(2_000), "did not settle");
    assert_eq!(p.client.app().received_bytes, 200 * 500);
    let (c, s) = (p.client.stats(), p.server.stats());
    // One side initiates each update; the other rotates on seeing the new
    // key phase.
    assert!(c.key_updates + s.key_updates > 1, "c={c:?} s={s:?}");
    assert_eq!(c.rx_dropped + s.rx_dropped, 0, "c={c:?} s={s:?}");
    assert_eq!(c.packets_lost + s.packets_lost, 0);
    let ck = p.client.connection(p.c_conn).unwrap().keys;
    let sk = p.server.connection(p.s_conn).unwrap().keys;
    assert!(ck.send_generation() > 1 && ck.recv_generation() > 1);
    assert!(sk.send_generation() > 1 && sk.recv_generation() > 1);
    assert_clean(&mut p.client);
    assert_clean(&mut p.server);
}

#[test]
fn explicit_key_update_is_accepted_by_the_peer() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    p.client
        .send_stream(p.c_conn, 0, 0, b"a", false, p.now)
        .unwrap();
    assert!(p.settle(50));
    p.client.initiate_key_update(p.c_conn).unwrap();
    p.client
        .send_stream(p.c_conn, 0, 1, b"b", false, p.now)
        .unwrap();
    assert!(p.settle(50));
    assert_eq!(p.client.app().received_bytes, 2);
    assert_eq!(
        p.server
            .connection(p.s_conn)
            .unwrap()
            .keys
            .recv_generation(),
        1
    );
    assert_eq!(p.server.stats().rx_dropped, 0);
}

#[test]
fn a_full_launch_queue_backpressures_instead_of_sending_early() {
    let cfg = EndpointConfig {
        launch_capacity: 2,
        ..config()
    };
    let mut p = Pair::new(cfg, Discard::default(), Echo::default());
    let (_, backpressure) = bulk(&mut p, 100, 1000);
    assert!(p.settle(2_000), "did not settle");
    assert!(backpressure > 0, "the queue filled");
    assert_eq!(p.client.app().received_bytes, 100 * 1000);
    let c = p.client.stats();
    assert_eq!(c.launch_refused, 0, "{c:?}");
    assert!(c.deferred_acks > 0, "ACKs waited for room: {c:?}");
    assert_eq!(c.launch_queued, c.launch_released);
    assert_eq!(c.packets_lost, 0);
    // Nothing left before its departure time.
    let mut last = 0;
    for &(_, edt) in &p.client.tap().log {
        assert!(edt >= last || edt == 0, "EDT went backwards");
        last = last.max(edt);
    }
    assert_clean(&mut p.client);
    assert_clean(&mut p.server);
}

#[test]
fn an_echo_too_large_for_the_reply_falls_back_to_its_ack() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    // Owe the server an ACK so the client's next packet carries one and
    // the server's reply needs room for its own ACK and the echo.
    p.client
        .send_stream(p.c_conn, 0, 0, b"x", false, p.now)
        .unwrap();
    assert!(p.settle(50));
    let mut len = 1200;
    let big = vec![3u8; 1200];
    loop {
        match p
            .client
            .send_stream(p.c_conn, 0, 1, &big[..len], false, p.now)
        {
            Ok(_) => break,
            Err(WarpError::PacketTooLarge { .. }) => len -= 1,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(p.settle(400), "did not settle");
    let s = p.server.stats();
    assert_eq!(s.rx_dropped, 0);
    // The echo plus the server's ACK does not fit: the reply carries the
    // ACK alone, so the client's packet is still acknowledged.
    assert_eq!(s.oversize_replies, 1, "{s:?}");
    assert_eq!(p.client.app().received_bytes, 1, "{s:?}");
    assert_eq!(p.client.stats().packets_lost, 0);
    let cc = p.client.connection(p.c_conn).unwrap();
    assert_eq!(cc.recovery.bytes_in_flight(), 0);
    assert_clean(&mut p.client);
    assert_clean(&mut p.server);
}

#[test]
fn time_threshold_loss_fires_from_the_recovery_timer_before_pto() {
    let mut p = Pair::new(config(), Discard::default(), Discard::default());
    // Warm up the RTT estimate.
    p.client
        .send_stream(p.c_conn, 0, 0, b"w", false, p.now)
        .unwrap();
    assert!(p.settle(50));
    let first = p.client.connection(p.c_conn).unwrap().tile.next_tx_pn;
    p.client.tap_mut().drop_pn = Some(first);
    p.client
        .send_stream(p.c_conn, 0, 1, b"a", false, p.now)
        .unwrap();
    p.client
        .send_stream(p.c_conn, 0, 2, b"b", false, p.now)
        .unwrap();
    assert!(p.settle(400), "did not settle");
    let c = p.client.stats();
    assert_eq!(p.client.tap().dropped, 1);
    assert_eq!(c.packets_lost, 1, "{c:?}");
    assert!(c.recovery_timers_fired > 0, "{c:?}");
    assert_eq!(c.pto_fired, 0, "lost by time threshold, not PTO: {c:?}");
    assert_clean(&mut p.client);
    assert_clean(&mut p.server);
}

#[test]
fn a_probe_that_cannot_be_sent_keeps_its_lease() {
    // One tracked packet per connection: the PTO probe for a lost packet
    // cannot be tracked and fails.
    let cfg = EndpointConfig {
        max_tracked_per_conn: 1,
        ..config()
    };
    let mut p = Pair::new(cfg, Discard::default(), Discard::default());
    p.client.tap_mut().drop_pn = Some(0);
    p.client
        .send_stream(p.c_conn, 0, 0, b"a", false, p.now)
        .unwrap();
    for _ in 0..400 {
        p.step();
    }
    let c = p.client.stats();
    assert!(c.pto_fired > 0, "{c:?}");
    assert!(c.probes_failed > 0, "{c:?}");
    // The lost packet's lease survives every failed probe.
    assert_eq!(p.client.parked_leases(), 1, "{c:?}");
    assert_eq!(c.leases_orphaned, 0);
    let census = p.client.xsk_mut().census();
    assert_eq!(census.leaked, 0, "{census:?}");
    assert_eq!(p.client.xsk().leaked_owners(), 0);
}
