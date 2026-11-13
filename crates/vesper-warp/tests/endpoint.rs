//! End-to-end §35.3 path: two Warp endpoints over lane 3's software NIC.

mod support;

use support::*;
use vesper_warp::endpoint::{
    CidLookup, Discard, DropReason, Echo, EndpointConfig, RxOutcome, StreamMode,
};
use vesper_warp_crypto::CryptoError;
use vesper_warp_umem::XdpNic;

fn config() -> EndpointConfig {
    EndpointConfig {
        initial_rtt_ns: 20 * MS,
        // The wave-four raw path: the Responder sees STREAM frames.
        stream_mode: StreamMode::Raw,
        ..EndpointConfig::default()
    }
}

/// Every chunk is back on the fill ring or free list, or on its way: no
/// frame is held by the application and none leaked.
fn assert_umem_clean<R: vesper_warp::endpoint::Responder>(e: &mut Endpoint<R>) {
    let census = e.xsk_mut().census();
    assert_eq!(census.app_rx, 0, "{census:?}");
    assert_eq!(census.app_tx, 0, "{census:?}");
    assert_eq!(census.leaked, 0, "{census:?}");
    assert_eq!(e.xsk().leaked_owners(), 0);
    assert_eq!(e.stats().leases_orphaned, 0);
    assert_eq!(e.stats().tx_abandoned, 0);
}

#[test]
fn stream_is_echoed_and_acknowledged_end_to_end() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    let msg = b"hello, warp";
    let pn = p
        .client
        .send_stream(p.c_conn, 0, 0, msg, false, p.now)
        .unwrap();
    assert_eq!(pn, 0);
    assert!(p.settle(50), "did not settle");

    // The server authenticated the packet, echoed it in the same frame and
    // the client got the echo.
    assert_eq!(p.server.app().echoed_bytes, msg.len() as u64);
    assert_eq!(p.client.app().received_bytes, msg.len() as u64);
    let s = p.server.stats();
    let c = p.client.stats();
    assert_eq!(s.rx_processed, 2, "data packet, then the client's ACK");
    assert_eq!(s.replies, 1);
    assert_eq!(c.rx_processed, 1, "the echo");
    assert_eq!(c.ack_only_sent, 1, "the client's reply is a pure ACK");
    assert_eq!(s.rx_dropped + c.rx_dropped, 0);

    // Leases: both ack-eliciting packets were parked and retired on ACK.
    assert_eq!(c.leases_parked, 1);
    assert_eq!(c.leases_retired_acked, 1);
    assert_eq!(s.leases_parked, 1);
    assert_eq!(s.leases_retired_acked, 1);
    assert_eq!(s.packets_lost + c.packets_lost, 0);

    // The tile, recovery and BBR agree after the exchange.
    for (e_tile, rtt) in [
        (p.client.connection(p.c_conn).unwrap(), true),
        (p.server.connection(p.s_conn).unwrap(), true),
    ] {
        assert_eq!(e_tile.tile.bytes_in_flight, 0);
        assert_eq!(e_tile.recovery.bytes_in_flight(), 0);
        assert_eq!(e_tile.cc.controller().bytes_in_flight(), 0);
        assert_eq!(e_tile.cc.snapshots_held(), 0);
        assert_eq!(e_tile.recovery.rtt().has_sample(), rtt);
        assert!(e_tile.cc.controller().delivered_bytes() > 0);
        assert_eq!(e_tile.tile.cwnd_bytes, e_tile.cc.cwnd_bytes());
        assert_ne!(e_tile.tile.bbr_min_rtt_ns, u64::MAX);
    }
    let ct = p.client.connection(p.c_conn).unwrap().tile;
    let st = p.server.connection(p.s_conn).unwrap().tile;
    assert_eq!(ct.next_tx_pn, 2, "data, then the ACK of the echo");
    assert_eq!(ct.largest_rx_pn, Some(0));
    assert_eq!(st.next_tx_pn, 1);
    assert_eq!(st.largest_rx_pn, Some(1));
    // The client sent just before the first step delivered it; the echo
    // arrived one step (one wire delay) later.
    let rtt = p
        .client
        .connection(p.c_conn)
        .unwrap()
        .recovery
        .rtt()
        .latest_ns();
    assert_eq!(rtt, p.delay_ns);

    assert_umem_clean(&mut p.client);
    assert_umem_clean(&mut p.server);
}

#[test]
fn bulk_stream_is_paced_through_the_launch_queue() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    let chunk = [7u8; 1000];
    let mut offset = 0u64;
    let mut sent = 0u64;
    let mut rounds = 0;
    while sent < 300 && rounds < 10_000 {
        rounds += 1;
        // Send while BBR's window allows, then let the network move.
        while sent < 300 {
            match p
                .client
                .send_stream(p.c_conn, 0, offset, &chunk, false, p.now)
            {
                Ok(_) => {
                    offset += chunk.len() as u64;
                    sent += 1;
                }
                Err(vesper_warp::endpoint::WarpError::CwndLimited) => break,
                Err(e) => panic!("{e:?}"),
            }
        }
        p.step();
    }
    assert_eq!(sent, 300);
    let settled = p.settle(2_000);
    assert!(
        settled,
        "did not settle: c={:?} s={:?} cq={} cl={} sq={} sl={}",
        p.client.stats(),
        p.server.stats(),
        p.client.queued_launches(),
        p.client.parked_leases(),
        p.server.queued_launches(),
        p.server.parked_leases()
    );
    assert_eq!(p.server.app().echoed_bytes, 300 * 1000);
    assert_eq!(p.client.app().received_bytes, 300 * 1000);

    let c = p.client.stats();
    assert!(c.launch_queued > 0, "BBR spaced packets in time: {c:?}");
    assert_eq!(c.launch_queued, c.launch_released);
    assert_eq!(c.packets_lost, 0);
    // Departure times leave the NIC in non-decreasing order for the
    // paced (stream) packets; ACK-only packets are not paced.
    let mut last = 0;
    for &(_, edt) in &p.client.tap().log {
        assert!(edt >= last || edt == 0, "EDT went backwards");
        last = last.max(edt);
    }
    let bbr = p
        .client
        .connection(p.c_conn)
        .unwrap()
        .cc
        .controller()
        .clone();
    assert!(bbr.round_count() > 1);
    assert!(bbr.max_bw() > 0);
    assert_eq!(bbr.bytes_in_flight(), 0);
    let tile = p.client.connection(p.c_conn).unwrap().tile;
    assert_eq!(tile.bbr_bw_bytes_per_sec, bbr.max_bw());
    assert_umem_clean(&mut p.client);
    assert_umem_clean(&mut p.server);
}

#[test]
fn lost_echo_is_recovered_by_pto_and_reported() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    p.server.tap_mut().drop_pn = Some(0);
    p.client
        .send_stream(p.c_conn, 0, 0, b"first", false, p.now)
        .unwrap();
    // The echo (server pn 0) vanishes; the client's packet is acknowledged
    // by nothing either, so both sides hit PTO.
    assert!(p.settle(400), "did not settle");
    let s = p.server.stats();
    assert_eq!(p.server.tap().dropped, 1);
    assert!(s.pto_fired > 0, "{s:?}");
    assert!(s.probes_sent > 0, "{s:?}");
    assert_eq!(
        s.packets_lost, 1,
        "the dropped echo is declared lost: {s:?}"
    );
    assert_eq!(s.leases_retired_lost, 1);
    assert_eq!(p.server.app().lost_packets, 1);
    // Data loss is reported, not retransmitted (no stream layer).
    assert_eq!(p.client.app().received_bytes, 0);
    let sc = p.server.connection(p.s_conn).unwrap();
    assert_eq!(sc.recovery.bytes_in_flight(), 0);
    assert_eq!(sc.cc.controller().bytes_in_flight(), 0);
    assert!(sc.cc.controller().lost_bytes() > 0);
    assert_umem_clean(&mut p.client);
    assert_umem_clean(&mut p.server);
}

#[test]
fn bad_packets_are_dropped_and_their_frames_recycled() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    p.client
        .send_stream(p.c_conn, 0, 0, b"payload", false, p.now)
        .unwrap();
    p.client.poll(p.now).unwrap();
    let mut buf = [0u8; 2048];
    let n = p.client.tap_mut().pop_into(&mut buf).unwrap();
    let good = buf[..n].to_vec();

    let deliver = |p: &mut Pair<Discard, Echo>, bytes: &[u8]| {
        p.server.xsk_mut().backend_mut().deliver(bytes);
        p.server.poll_and_step_quic_rx_one(p.now).unwrap()
    };
    // A flipped ciphertext byte fails authentication.
    let mut tampered = good.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert_eq!(
        deliver(&mut p, &tampered),
        RxOutcome::Dropped(DropReason::Crypto(CryptoError::AuthenticationFailed))
    );
    // An unknown connection ID.
    let mut stranger = good.clone();
    stranger[1] ^= 0xff;
    assert_eq!(
        deliver(&mut p, &stranger),
        RxOutcome::Dropped(DropReason::UnknownCid)
    );
    // A long header (no handshake support).
    let mut long = good.clone();
    long[0] |= 0x80;
    assert!(matches!(deliver(&mut p, &long), RxOutcome::Dropped(_)));
    // The genuine packet, then a replay of it.
    assert!(matches!(
        deliver(&mut p, &good),
        RxOutcome::Processed {
            pn: 0,
            replied: true,
            ..
        }
    ));
    assert!(matches!(
        deliver(&mut p, &good),
        RxOutcome::Dropped(DropReason::Duplicate(_))
    ));
    let s = p.server.stats();
    assert_eq!(s.rx_dropped, 4);
    assert_eq!(s.rx_dropped_crypto, 1);
    assert_eq!(s.rx_dropped_unknown_cid, 1);
    assert_eq!(s.rx_processed, 1);
    let opener_failures = p
        .server
        .connection(p.s_conn)
        .unwrap()
        .keys
        .authentication_failures();
    assert_eq!(opener_failures, 1);
    assert_umem_clean(&mut p.server);
}

#[test]
fn tile_scan_lookup_routes_like_the_hash_index() {
    let cfg = EndpointConfig {
        cid_lookup: CidLookup::TileScan,
        ..config()
    };
    let mut p = Pair::new(cfg, Discard::default(), Echo::default());
    p.client
        .send_stream(p.c_conn, 4, 0, b"tiles", true, p.now)
        .unwrap();
    assert!(p.settle(50));
    assert_eq!(p.client.app().received_bytes, 5);
}

#[test]
fn many_connections_share_one_endpoint() {
    let mut server = endpoint(config(), Echo::default());
    let mut client = endpoint(config(), Discard::default());
    let mut pairs = Vec::new();
    for i in 0..40u8 {
        let c_cid = [b'c', 0, 0, 0, 0, 0, 0, i];
        let s_cid = [b's', 0, 0, 0, 0, 0, 0, i];
        let c = client
            .open_connection(&c_cid, &s_cid, client_keys(), 0)
            .unwrap();
        let s = server
            .open_connection(&s_cid, &c_cid, server_keys(), 0)
            .unwrap();
        pairs.push((c, s));
    }
    let mut now = 0;
    for (i, &(c, _)) in pairs.iter().enumerate() {
        client
            .send_stream(c, 0, 0, &[i as u8; 32], false, now)
            .unwrap();
    }
    let mut buf = [0u8; 2048];
    for _ in 0..20 {
        client.poll(now).unwrap();
        server.poll(now).unwrap();
        while let Some(n) = client.tap_mut().pop_into(&mut buf) {
            server.xsk_mut().backend_mut().deliver(&buf[..n]);
        }
        while let Some(n) = server.tap_mut().pop_into(&mut buf) {
            client.xsk_mut().backend_mut().deliver(&buf[..n]);
        }
        while server.poll_and_step_quic_rx_one(now).unwrap() != RxOutcome::Idle {}
        while client.poll_and_step_quic_rx_one(now).unwrap() != RxOutcome::Idle {}
        server.xsk_mut().backend_mut().step();
        client.xsk_mut().backend_mut().step();
        now += MS;
    }
    assert_eq!(server.app().echoed_bytes, 40 * 32);
    assert_eq!(client.app().received_bytes, 40 * 32);
    for &(_, s) in &pairs {
        let v = server.connection(s).unwrap();
        assert_eq!(v.tile.largest_rx_pn, Some(1), "data + ACK per connection");
        assert_eq!(v.tile.next_tx_pn, 1);
    }
    // Closing one connection frees its slot; the rest keep working.
    let (c0, s0) = pairs[0];
    server.close_connection(s0, now).unwrap();
    assert!(server.connection(s0).is_none());
    client.send_stream(c0, 0, 32, b"late", false, now).unwrap();
    client.poll(now).unwrap();
    while let Some(n) = client.tap_mut().pop_into(&mut buf) {
        server.xsk_mut().backend_mut().deliver(&buf[..n]);
    }
    assert_eq!(
        server.poll_and_step_quic_rx_one(now).unwrap(),
        RxOutcome::Dropped(DropReason::UnknownCid)
    );
}

#[test]
fn dropping_an_endpoint_with_work_outstanding_is_clean() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    for i in 0..20 {
        p.client
            .send_stream(p.c_conn, 0, i * 100, &[1; 100], false, p.now)
            .unwrap();
    }
    assert!(p.client.parked_leases() + p.client.queued_launches() > 0);
    let leaked_before = vesper_warp_wheel::leaked_timer_entries();
    drop(p);
    assert_eq!(vesper_warp_wheel::leaked_timer_entries(), leaked_before);
}

#[test]
fn rx_and_tx_frames_conserve_the_umem() {
    let mut p = Pair::new(config(), Discard::default(), Echo::default());
    for i in 0..50 {
        p.client
            .send_stream(p.c_conn, 0, i * 10, &[3; 10], false, p.now)
            .unwrap();
        p.step();
    }
    assert!(p.settle(200));
    for e in [p.client.xsk_mut(), p.server.xsk_mut()] {
        let census = e.census();
        let frames = e.config().frame_count;
        assert_eq!(
            census.free
                + census.fill_posted
                + census.app_rx
                + census.app_tx
                + census.tx_posted
                + census.in_flight
                + census.leaked,
            frames
        );
        assert_eq!(census.in_flight + census.tx_posted, 0, "{census:?}");
        let _ = e.harvest_completions(0);
    }
}
