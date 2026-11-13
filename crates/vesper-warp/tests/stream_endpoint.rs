//! Wave five lane 13 (issue #133): reliable streams through `WarpEndpoint`
//! over lossy, reordering in-memory links. Every byte must arrive exactly
//! once and in order, flow-control limits must hold, and peer violations
//! must close the connection with the RFC 9000 error code.

mod common;
mod support;

use common::Rng;
use support::*;
use vesper_warp::TransportErrorCode;
use vesper_warp::endpoint::{
    ConnectionError, Discard, EndpointConfig, RxOutcome, StreamMode, WarpError,
};
use vesper_warp::stream::{RecvState, SendState, StreamConfig, StreamEvent};

fn config() -> EndpointConfig {
    EndpointConfig {
        initial_rtt_ns: 20 * MS,
        ..EndpointConfig::default()
    }
}

/// A lossy, reordering link between the pair's endpoints.
struct Link {
    rng: Rng,
    /// Loss probability, per million datagrams.
    loss_ppm: u64,
    /// Probability a delivered datagram is held back one step.
    reorder_ppm: u64,
    held_to_server: Vec<Vec<u8>>,
    held_to_client: Vec<Vec<u8>>,
    dropped: u64,
    reordered: u64,
    buf: Box<[u8; 2048]>,
}

impl Link {
    fn new(seed: u64, loss_ppm: u64, reorder_ppm: u64) -> Self {
        Self {
            rng: Rng(seed),
            loss_ppm,
            reorder_ppm,
            held_to_server: Vec::new(),
            held_to_client: Vec::new(),
            dropped: 0,
            reordered: 0,
            buf: Box::new([0; 2048]),
        }
    }

    /// Moves datagrams with loss and reordering, runs both receive paths,
    /// NICs and timers, then advances time by the wire delay.
    fn step(&mut self, p: &mut Pair<Discard, Discard>) {
        let now = p.now;
        let old_s = std::mem::take(&mut self.held_to_server);
        let old_c = std::mem::take(&mut self.held_to_client);
        let mut to_server = Vec::new();
        let mut to_client = Vec::new();
        let mut fresh_s = Vec::new();
        while let Some(n) = p.client.tap_mut().pop_into(&mut self.buf) {
            fresh_s.push(self.buf[..n].to_vec());
        }
        let mut fresh_c = Vec::new();
        while let Some(n) = p.server.tap_mut().pop_into(&mut self.buf) {
            fresh_c.push(self.buf[..n].to_vec());
        }
        for (fresh, now_out, held) in [
            (fresh_s, &mut to_server, &mut self.held_to_server),
            (fresh_c, &mut to_client, &mut self.held_to_client),
        ] {
            for d in fresh {
                if self.rng.below(1_000_000) < self.loss_ppm {
                    self.dropped += 1;
                } else if self.rng.below(1_000_000) < self.reorder_ppm {
                    self.reordered += 1;
                    held.push(d);
                } else {
                    now_out.push(d);
                }
            }
        }
        // Held-back datagrams from the previous step arrive after newer ones.
        to_server.extend(old_s);
        to_client.extend(old_c);
        for d in &to_server {
            p.server.xsk_mut().backend_mut().deliver(d);
        }
        for d in &to_client {
            p.client.xsk_mut().backend_mut().deliver(d);
        }
        for _ in 0..4096 {
            let a = p.server.poll_and_step_quic_rx_one(now).unwrap();
            let b = p.client.poll_and_step_quic_rx_one(now).unwrap();
            if a == RxOutcome::Idle && b == RxOutcome::Idle {
                break;
            }
        }
        p.client.xsk_mut().backend_mut().step();
        p.server.xsk_mut().backend_mut().step();
        p.client.poll(now).unwrap();
        p.server.poll(now).unwrap();
        p.now += p.delay_ns;
    }
}

/// Writes as much of `data[*sent..]` as the stream takes.
fn pump(
    e: &mut Endpoint<Discard>,
    conn: vesper_warp_shard::SlotHandle,
    id: u64,
    data: &[u8],
    sent: &mut usize,
    fin: &mut bool,
) {
    if *sent < data.len() {
        *sent += e.stream_write(conn, id, &data[*sent..]).unwrap();
    }
    if *sent == data.len() && !*fin {
        e.stream_finish(conn, id).unwrap();
        *fin = true;
    }
}

/// Reads everything available into `got`; returns whether FIN was read.
fn drain(
    e: &mut Endpoint<Discard>,
    conn: vesper_warp_shard::SlotHandle,
    id: u64,
    got: &mut Vec<u8>,
) -> bool {
    let mut buf = [0u8; 4096];
    loop {
        match e.stream_read(conn, id, &mut buf) {
            Ok(r) => {
                got.extend_from_slice(&buf[..r.len]);
                if r.fin {
                    return true;
                }
                if r.len == 0 {
                    return false;
                }
            }
            Err(WarpError::Stream(vesper_warp::stream::StreamError::UnknownStream(_))) => {
                return true;
            }
            Err(e) => panic!("read failed: {e:?}"),
        }
    }
}

/// Client sends `len` bytes on a bidi stream, the server echoes them back,
/// both FIN. Returns the pair and link after both sides read everything.
fn echo_transfer(
    cfg: EndpointConfig,
    len: usize,
    loss_ppm: u64,
    reorder_ppm: u64,
    seed: u64,
) -> (Pair<Discard, Discard>, Link) {
    let mut p = Pair::new(cfg, Discard::default(), Discard::default());
    p.record = false;
    p.client.tap_mut().log_enabled = false;
    p.server.tap_mut().log_enabled = false;
    let mut link = Link::new(seed, loss_ppm, reorder_ppm);
    let data = Rng(seed ^ 0xfeed).bytes(len);
    let id = p.client.open_stream(p.c_conn, false).unwrap();
    assert_eq!(id, 0);
    let (mut c_sent, mut c_fin) = (0usize, false);
    let mut s_got = Vec::new();
    let mut s_echoed = 0usize;
    let mut s_fin_read = false;
    let mut s_fin = false;
    let mut c_got = Vec::new();
    let mut c_done = false;
    let mut steps = 0;
    while !c_done {
        steps += 1;
        assert!(
            steps < 200_000,
            "transfer did not finish (got {} of {len} back)",
            c_got.len()
        );
        pump(&mut p.client, p.c_conn, id, &data, &mut c_sent, &mut c_fin);
        // Server: read, echo what was read, FIN once all is read and echoed.
        while let Some(ev) = p.server.poll_stream_event(p.s_conn) {
            if let StreamEvent::Opened(sid) = ev {
                assert_eq!(sid, id);
            }
        }
        if !s_fin_read
            && p.server
                .connection(p.s_conn)
                .unwrap()
                .streams
                .recv_state(id)
                .is_some()
        {
            s_fin_read = drain(&mut p.server, p.s_conn, id, &mut s_got);
        }
        if s_echoed < s_got.len() {
            s_echoed += p
                .server
                .stream_write(p.s_conn, id, &s_got[s_echoed..])
                .unwrap();
        }
        if s_fin_read && s_echoed == s_got.len() && !s_fin {
            p.server.stream_finish(p.s_conn, id).unwrap();
            s_fin = true;
        }
        if p.client
            .connection(p.c_conn)
            .unwrap()
            .streams
            .recv_state(id)
            .is_some()
            || !c_got.is_empty()
        {
            c_done = drain(&mut p.client, p.c_conn, id, &mut c_got);
        }
        // Flow control holds at every step, in both directions.
        for (e, conn) in [(&p.client, p.c_conn), (&p.server, p.s_conn)] {
            let v = e.connection(conn).unwrap();
            let (max, sent) = v.streams.send_credit();
            assert!(sent <= max, "sent {sent} past MAX_DATA {max}");
            assert_eq!(v.error, None);
        }
        link.step(&mut p);
    }
    assert_eq!(s_got.len(), len, "server received every byte");
    assert!(s_got == data, "server received the bytes in order");
    assert!(c_got == data, "client received the echo exactly, in order");
    (p, link)
}

fn settle_streams(p: &mut Pair<Discard, Discard>, link: &mut Link) {
    // Let the last ACKs land so both streams close.
    for _ in 0..2000 {
        link.step(p);
        if p.client.connection(p.c_conn).unwrap().streams.is_empty()
            && p.server.connection(p.s_conn).unwrap().streams.is_empty()
        {
            return;
        }
    }
    panic!("streams did not close");
}

#[test]
fn clean_link_delivers_and_closes_streams() {
    let (mut p, mut link) = echo_transfer(config(), 300_000, 0, 0, 1);
    settle_streams(&mut p, &mut link);
    let c = p.client.connection(p.c_conn).unwrap().streams.stats();
    assert_eq!(c.stream_bytes_retransmitted, 0);
    assert_eq!(c.streams_closed, 1);
    let s = p.server.connection(p.s_conn).unwrap().streams.stats();
    assert_eq!(s.streams_opened_remote, 1);
    assert_eq!(s.streams_closed, 1);
    // The server's MAX_STREAMS went up when the client's stream closed.
    let (_, ours) = p
        .server
        .connection(p.s_conn)
        .unwrap()
        .streams
        .stream_limits();
    assert_eq!(ours[0], StreamConfig::default().max_streams_bidi + 1);
    // Windows were re-advertised: 300 kB is more than one stream window.
    assert!(c.control_frames_sent > 0);
}

fn lossy(loss_ppm: u64, seed: u64) {
    let (mut p, mut link) = echo_transfer(config(), 200_000, loss_ppm, 50_000, seed);
    assert!(link.dropped > 0 && link.reordered > 0);
    let c = p.client.connection(p.c_conn).unwrap().streams.stats();
    let s = p.server.connection(p.s_conn).unwrap().streams.stats();
    assert!(
        c.stream_bytes_retransmitted + s.stream_bytes_retransmitted > 0,
        "lost stream data was retransmitted"
    );
    assert!(c.stream_frames_lost + s.stream_frames_lost > 0);
    settle_streams(&mut p, &mut link);
    for e in [&p.client, &p.server] {
        let st = e.stats();
        assert_eq!(st.transport_errors, 0);
        assert_eq!(st.closes_received, 0);
    }
}

#[test]
fn one_percent_loss_with_reordering() {
    lossy(10_000, 11);
}

#[test]
fn five_percent_loss_with_reordering() {
    lossy(50_000, 12);
}

#[test]
fn twenty_percent_loss_with_reordering() {
    lossy(200_000, 13);
}

#[test]
fn small_windows_are_respected_and_signal_blocked() {
    let cfg = EndpointConfig {
        streams: StreamConfig {
            stream_window: 4096,
            conn_window: 6000,
            send_buffer: 16 * 1024,
            ..StreamConfig::default()
        },
        ..config()
    };
    let (p, _) = echo_transfer(cfg, 60_000, 20_000, 20_000, 5);
    let c = p.client.connection(p.c_conn).unwrap().streams.stats();
    assert!(
        c.blocked_frames_sent > 0,
        "the sender reported being blocked"
    );
    let s = p.server.connection(p.s_conn).unwrap().streams.stats();
    assert!(
        s.control_frames_sent > 10,
        "credit was re-advertised many times"
    );
}

#[test]
fn many_streams_both_directions() {
    let mut p = Pair::new(config(), Discard::default(), Discard::default());
    let mut link = Link::new(99, 30_000, 30_000);
    let mut rng = Rng(7);
    // Client: 4 bidi + 2 uni; server: 2 uni.
    let mut sends = Vec::new();
    for uni in [false, false, false, false, true, true] {
        let id = p.client.open_stream(p.c_conn, uni).unwrap();
        let len = 20_000 + rng.below(20_000) as usize;
        sends.push((true, id, rng.bytes(len), 0usize, false));
    }
    for _ in 0..2 {
        let id = p.server.open_stream(p.s_conn, true).unwrap();
        sends.push((false, id, rng.bytes(30_000), 0usize, false));
    }
    let mut got: std::collections::HashMap<(bool, u64), (Vec<u8>, bool)> = Default::default();
    for step in 0..100_000 {
        for (client, id, data, sent, fin) in &mut sends {
            let (e, c) = if *client {
                (&mut p.client, p.c_conn)
            } else {
                (&mut p.server, p.s_conn)
            };
            pump(e, c, *id, data, sent, fin);
        }
        for (client, id, _, _, _) in &sends {
            // Read at the other side.
            let (e, c) = if *client {
                (&mut p.server, p.s_conn)
            } else {
                (&mut p.client, p.c_conn)
            };
            let entry = got.entry((*client, *id)).or_default();
            if !entry.1 && e.connection(c).unwrap().streams.recv_state(*id).is_some() {
                entry.1 = drain(e, c, *id, &mut entry.0);
            }
        }
        // Bidi streams' other halves: the receiver finishes them empty.
        for (client, id, _, _, _) in &sends {
            if vesper_warp::stream::is_uni(*id) {
                continue;
            }
            let (e, c) = if *client {
                (&mut p.server, p.s_conn)
            } else {
                (&mut p.client, p.c_conn)
            };
            if e.connection(c).unwrap().streams.send_state(*id) == Some(SendState::Ready) {
                e.stream_finish(c, *id).unwrap();
            }
            let (e, c) = if *client {
                (&mut p.client, p.c_conn)
            } else {
                (&mut p.server, p.s_conn)
            };
            if e.connection(c).unwrap().streams.recv_state(*id).is_some() {
                let mut sink = Vec::new();
                drain(e, c, *id, &mut sink);
                assert!(sink.is_empty());
            }
        }
        while p.client.poll_stream_event(p.c_conn).is_some() {}
        while p.server.poll_stream_event(p.s_conn).is_some() {}
        link.step(&mut p);
        let all = sends
            .iter()
            .all(|(client, id, _, _, _)| got.get(&(*client, *id)).is_some_and(|g| g.1));
        if all
            && p.client.connection(p.c_conn).unwrap().streams.is_empty()
            && p.server.connection(p.s_conn).unwrap().streams.is_empty()
        {
            for (client, id, data, _, _) in &sends {
                assert!(got[&(*client, *id)].0 == *data, "stream {id} intact");
            }
            assert!(step > 0);
            return;
        }
    }
    panic!("streams did not finish");
}

#[test]
fn reset_and_stop_sending_cross_the_wire() {
    let mut p = Pair::new(config(), Discard::default(), Discard::default());
    let a = p.client.open_stream(p.c_conn, false).unwrap();
    let b = p.client.open_stream(p.c_conn, true).unwrap();
    p.client.stream_write(p.c_conn, a, &[1; 5000]).unwrap();
    p.client.stream_write(p.c_conn, b, &[2; 5000]).unwrap();
    for _ in 0..4 {
        p.step();
    }
    // The client abandons `b`; the server asks the client to stop `a`.
    p.client.stream_reset(p.c_conn, b, 77).unwrap();
    p.server.stream_stop_sending(p.s_conn, a, 88).unwrap();
    for _ in 0..10 {
        p.step();
    }
    let mut events = Vec::new();
    while let Some(ev) = p.server.poll_stream_event(p.s_conn) {
        events.push(ev);
    }
    assert!(events.contains(&StreamEvent::Reset {
        stream_id: b,
        error_code: 77
    }));
    let mut cev = Vec::new();
    while let Some(ev) = p.client.poll_stream_event(p.c_conn) {
        cev.push(ev);
    }
    assert!(cev.contains(&StreamEvent::StopSending {
        stream_id: a,
        error_code: 88
    }));
    // STOP_SENDING made the client reset `a` with the same code.
    assert_eq!(
        p.client.connection(p.c_conn).unwrap().streams.send_state(a),
        Some(SendState::ResetRecvd)
    );
    assert_eq!(
        p.server.connection(p.s_conn).unwrap().streams.recv_state(a),
        Some(RecvState::ResetRecvd)
    );
    let mut buf = [0u8; 16];
    assert!(matches!(
        p.server.stream_read(p.s_conn, b, &mut buf),
        Err(WarpError::Stream(vesper_warp::stream::StreamError::Reset {
            error_code: 77,
            ..
        }))
    ));
    assert!(matches!(
        p.client.stream_write(p.c_conn, a, b"more"),
        Err(WarpError::Stream(vesper_warp::stream::StreamError::Reset {
            error_code: 88,
            ..
        }))
    ));
    // Unread data of the reset stream was returned to connection credit.
    let (max, highest) = p.server.connection(p.s_conn).unwrap().streams.recv_credit();
    assert!(highest <= max);
    assert_eq!(
        p.client.stats().transport_errors + p.server.stats().transport_errors,
        0
    );
}

#[test]
fn flow_control_violation_closes_the_connection() {
    // A raw-mode client ignores the reliable server's 4 KiB stream window.
    let server_cfg = EndpointConfig {
        streams: StreamConfig {
            stream_window: 4096,
            ..StreamConfig::default()
        },
        ..config()
    };
    let mut p = Pair::new(server_cfg, Discard::default(), Discard::default());
    // Rebuild the client in raw mode.
    p.client = endpoint(
        EndpointConfig {
            stream_mode: StreamMode::Raw,
            ..server_cfg
        },
        Discard::default(),
    );
    p.c_conn = p
        .client
        .open_connection(&CLIENT_CID, &SERVER_CID, client_keys(), 0)
        .unwrap();
    p.client
        .send_stream(p.c_conn, 0, 4000, &[9; 200], false, p.now)
        .unwrap();
    for _ in 0..4 {
        p.step();
    }
    let err = p.server.connection_error(p.s_conn).expect("server closed");
    let ConnectionError::Local(e) = err else {
        panic!("{err:?}")
    };
    assert_eq!(e.code, TransportErrorCode::FlowControlError);
    assert_eq!(p.server.stats().closes_sent, 1);
    // The client saw CONNECTION_CLOSE with FLOW_CONTROL_ERROR (0x03).
    assert_eq!(
        p.client.connection_error(p.c_conn),
        Some(ConnectionError::Peer {
            error_code: 0x03,
            frame_type: Some(0x0e)
        })
    );
    assert!(matches!(
        p.server.stream_write(p.s_conn, 0, b"x"),
        Err(WarpError::ConnectionClosed(_))
    ));
    // Further packets from the client are dropped by the closing server.
    let before = p.server.stats().rx_dropped_closed;
    assert!(matches!(
        p.client.send_stream(p.c_conn, 0, 0, b"late", false, p.now),
        Err(WarpError::ConnectionClosed(_))
    ));
    assert_eq!(p.server.stats().rx_dropped_closed, before);
}

#[test]
fn stream_limit_violation_closes_the_connection() {
    let mut p = Pair::new(config(), Discard::default(), Discard::default());
    p.client = endpoint(
        EndpointConfig {
            stream_mode: StreamMode::Raw,
            ..config()
        },
        Discard::default(),
    );
    p.c_conn = p
        .client
        .open_connection(&CLIENT_CID, &SERVER_CID, client_keys(), 0)
        .unwrap();
    // Bidi stream index 8 with a limit of 8 (indices 0..=7).
    p.client
        .send_stream(p.c_conn, 8 << 2, 0, b"x", false, p.now)
        .unwrap();
    for _ in 0..4 {
        p.step();
    }
    let Some(ConnectionError::Local(e)) = p.server.connection_error(p.s_conn) else {
        panic!("server should have closed")
    };
    assert_eq!(e.code, TransportErrorCode::StreamLimitError);
    assert_eq!(
        p.client.connection_error(p.c_conn),
        Some(ConnectionError::Peer {
            error_code: 0x04,
            frame_type: Some(0x0a)
        })
    );
}

#[test]
fn raw_and_reliable_apis_are_separate() {
    let mut p = Pair::new(config(), Discard::default(), Discard::default());
    assert_eq!(
        p.client.send_stream(p.c_conn, 0, 0, b"x", false, 0),
        Err(WarpError::StreamMode)
    );
    let mut raw = endpoint(
        EndpointConfig {
            stream_mode: StreamMode::Raw,
            ..config()
        },
        Discard::default(),
    );
    let c = raw
        .open_connection(&CLIENT_CID, &SERVER_CID, client_keys(), 0)
        .unwrap();
    assert_eq!(raw.open_stream(c, false), Err(WarpError::StreamMode));
}

#[test]
#[ignore]
fn sweep_seeds() {
    for seed in 100..160 {
        for loss in [10_000, 50_000, 200_000, 300_000] {
            eprintln!("seed {seed} loss {loss}");
            lossy(loss, seed);
        }
    }
}

#[test]
fn closing_suppresses_queued_packets() {
    // A reliable client with paced stream packets queued receives a
    // violation from a raw server and closes: nothing but
    // CONNECTION_CLOSE may leave afterwards (RFC 9000 §10.2).
    let mut p = Pair::new(config(), Discard::default(), Discard::default());
    p.server = endpoint(
        EndpointConfig {
            stream_mode: StreamMode::Raw,
            role: vesper_warp::stream::Role::Server,
            ..config()
        },
        Discard::default(),
    );
    p.s_conn = p
        .server
        .open_connection(&SERVER_CID, &CLIENT_CID, server_keys(), 0)
        .unwrap();
    let id = p.client.open_stream(p.c_conn, false).unwrap();
    let data = vec![3u8; 60_000];
    let mut sent = 0;
    for _ in 0..3 {
        sent += p.client.stream_write(p.c_conn, id, &data[sent..]).unwrap();
        p.step();
    }
    sent += p.client.stream_write(p.c_conn, id, &data[sent..]).unwrap();
    p.client.flush(p.c_conn, p.now).unwrap();
    assert!(p.client.queued_launches() > 0, "paced packets are queued");
    // Stream 2 is a client unidirectional stream the client never opened.
    p.server
        .send_stream(p.s_conn, 2, 0, b"x", false, p.now)
        .unwrap();
    p.step();
    let Some(ConnectionError::Local(e)) = p.client.connection_error(p.c_conn) else {
        panic!("client should have closed")
    };
    assert_eq!(e.code, TransportErrorCode::StreamStateError);
    let before = p.client.stats();
    for _ in 0..50 {
        p.step();
    }
    let after = p.client.stats();
    assert!(
        after.tx_suppressed_closed > 0,
        "queued packets were abandoned"
    );
    assert_eq!(p.client.queued_launches(), 0);
    // Only CONNECTION_CLOSE packets (not ack-eliciting) were sent.
    assert_eq!(
        after.packets_sent - before.packets_sent,
        after.closes_sent - before.closes_sent
    );
    assert_eq!(after.probes_sent, before.probes_sent);
    assert_eq!(
        p.server.connection_error(p.s_conn),
        Some(ConnectionError::Peer {
            error_code: 0x05,
            frame_type: Some(0x0a)
        })
    );
    assert!(sent > 0);
}
