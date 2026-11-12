//! Wave five lane 13 (issue #133): the RFC 9000 §2-4 stream layer, sans-IO.
//! Two `StreamSet`s exchange packets built by `fill`; acknowledgements and
//! losses are injected directly, as RFC 9002 recovery would report them.

mod common;

use common::Rng;
use vesper_warp::frame::{Frame, StreamFrame, frames, parse_frame};
use vesper_warp::stream::{
    PacketFrames, RecvState, Role, SendState, SentFrame, StreamConfig, StreamError, StreamEvent,
    StreamSet, TransportError,
};
use vesper_warp::{PacketType, PnSpace, TransportErrorCode};

fn set(role: Role, cfg: StreamConfig) -> StreamSet {
    StreamSet::new(role, cfg, cfg.local_params()).unwrap()
}

fn pair(cfg: StreamConfig) -> (StreamSet, StreamSet) {
    (set(Role::Client, cfg), set(Role::Server, cfg))
}

/// One side's outgoing packets: (pn, bytes, frames).
struct Sender {
    next_pn: u64,
    buf: Vec<u8>,
}

impl Sender {
    fn new() -> Self {
        Self {
            next_pn: 0,
            buf: vec![0; 1200],
        }
    }
    /// Builds one packet; `None` when there is nothing to send.
    fn build(&mut self, s: &mut StreamSet) -> Option<(u64, Vec<u8>)> {
        let mut rec = PacketFrames::default();
        let n = s.fill(&mut self.buf, &mut rec);
        if n == 0 {
            return None;
        }
        let pn = self.next_pn;
        self.next_pn += 1;
        s.commit(pn, &rec);
        Some((pn, self.buf[..n].to_vec()))
    }
}

fn deliver(to: &mut StreamSet, bytes: &[u8]) -> Result<(), TransportError> {
    for f in frames(bytes, PacketType::OneRtt) {
        to.on_frame(&f.unwrap())?;
    }
    Ok(())
}

/// Sends everything pending from `a` to `b` without loss, acknowledging
/// each packet. Returns packets sent.
fn exchange(a: &mut StreamSet, sa: &mut Sender, b: &mut StreamSet) -> usize {
    let mut n = 0;
    while let Some((pn, bytes)) = sa.build(a) {
        deliver(b, &bytes).unwrap();
        a.on_packet_acked(pn);
        n += 1;
        assert!(n < 100_000);
    }
    n
}

fn read_all(s: &mut StreamSet, id: u64) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 1000];
    loop {
        let r = s.read(id, &mut buf).unwrap();
        out.extend_from_slice(&buf[..r.len]);
        if r.fin || r.len == 0 {
            return (out, r.fin);
        }
    }
}

fn stream_frame(id: u64, offset: u64, data: &[u8], fin: bool) -> Frame<'_> {
    Frame::Stream(StreamFrame {
        stream_id: id,
        offset,
        data,
        fin,
        has_length: true,
    })
}

fn code(r: Result<(), TransportError>) -> TransportErrorCode {
    r.expect_err("a transport error").code
}

#[test]
fn new_frames_round_trip() {
    let cases = [
        Frame::ResetStream {
            stream_id: 4,
            error_code: 9,
            final_size: 1000,
        },
        Frame::StopSending {
            stream_id: 2,
            error_code: 3,
        },
        Frame::MaxStreams {
            uni: false,
            max: 100,
        },
        Frame::MaxStreams { uni: true, max: 7 },
        Frame::DataBlocked(5000),
        Frame::StreamDataBlocked {
            stream_id: 8,
            limit: 64,
        },
        Frame::StreamsBlocked {
            uni: true,
            limit: 3,
        },
        Frame::StreamsBlocked {
            uni: false,
            limit: 1 << 60,
        },
    ];
    for f in cases {
        let mut buf = [0u8; 32];
        let n = f.encode(&mut buf).unwrap();
        assert_eq!(parse_frame(&buf[..n], PacketType::OneRtt), Ok((f, n)));
        // Not allowed in Initial or Handshake packets (Table 3).
        assert!(parse_frame(&buf[..n], PacketType::Initial).is_err());
    }
    // A stream count above 2^60 is a FRAME_ENCODING_ERROR (§19.11).
    let mut buf = [0u8; 16];
    buf[0] = 0x12;
    let n = vesper_warp::varint::encode((1 << 60) + 1, &mut buf[1..]).unwrap();
    let e = parse_frame(&buf[..1 + n], PacketType::OneRtt).unwrap_err();
    assert_eq!(e.transport_code(), TransportErrorCode::FrameEncodingError);
    assert!(
        Frame::MaxStreams {
            uni: false,
            max: (1 << 60) + 1
        }
        .encode(&mut buf)
        .is_err()
    );
}

#[test]
fn send_and_receive_state_machines() {
    let (mut c, mut s) = pair(StreamConfig::default());
    let (mut sc, mut ss) = (Sender::new(), Sender::new());
    let id = c.open(false).unwrap();
    assert_eq!(id, 0);
    assert_eq!(c.send_state(id), Some(SendState::Ready));
    assert_eq!(c.recv_state(id), Some(RecvState::Recv));
    c.write(id, b"hello").unwrap();
    let (pn, bytes) = sc.build(&mut c).unwrap();
    assert_eq!(c.send_state(id), Some(SendState::Send));
    deliver(&mut s, &bytes).unwrap();
    assert_eq!(s.poll_event(), Some(StreamEvent::Opened(0)));
    assert_eq!(s.poll_event(), Some(StreamEvent::Readable(0)));
    assert_eq!(s.recv_state(id), Some(RecvState::Recv));
    c.on_packet_acked(pn);
    c.finish(id).unwrap();
    assert_eq!(c.write(id, b"x"), Err(StreamError::Finished(id)));
    // FIN alone in its own frame.
    let (pn, bytes) = sc.build(&mut c).unwrap();
    assert_eq!(c.send_state(id), Some(SendState::DataSent));
    deliver(&mut s, &bytes).unwrap();
    assert_eq!(s.recv_state(id), Some(RecvState::DataRecvd));
    c.on_packet_acked(pn);
    assert_eq!(c.send_state(id), Some(SendState::DataRecvd));
    assert_eq!(c.poll_event(), Some(StreamEvent::Finished(id)));
    let (got, fin) = read_all(&mut s, id);
    assert_eq!((got.as_slice(), fin), (&b"hello"[..], true));
    assert_eq!(s.recv_state(id), Some(RecvState::DataRead));
    // The server finishes its half; once acknowledged both sides forget it.
    s.finish(id).unwrap();
    exchange(&mut s, &mut ss, &mut c);
    assert_eq!(read_all(&mut c, id), (Vec::new(), true));
    assert!(c.is_empty(), "client stream closed");
    assert!(s.is_empty(), "server stream closed");
    // The peer-initiated stream's close raised the server's MAX_STREAMS,
    // and the MAX_STREAMS frame reaches the client.
    exchange(&mut s, &mut ss, &mut c);
    assert_eq!(c.stream_limits().0[0], 9);
    assert_eq!(s.stats().streams_closed, 1);
}

#[test]
fn reset_and_stop_sending_state_machines() {
    let (mut c, mut s) = pair(StreamConfig::default());
    let (mut sc, mut ss) = (Sender::new(), Sender::new());
    let id = c.open(true).unwrap();
    assert_eq!(id, 2, "client uni stream 0");
    assert_eq!(c.recv_state(id), None, "send-only");
    c.write(id, &[7; 3000]).unwrap();
    let (_, bytes) = sc.build(&mut c).unwrap();
    deliver(&mut s, &bytes).unwrap();
    assert!(s.send_state(id).is_none(), "receive-only at the server");
    assert_eq!(s.write(id, b"x"), Err(StreamError::NotWritable(id)));
    // The packet carrying the reset is lost, so RESET_STREAM is resent.
    c.reset(id, 42).unwrap();
    assert_eq!(c.send_state(id), Some(SendState::ResetSent));
    let (pn, _lost) = sc.build(&mut c).unwrap();
    c.on_packet_lost(pn);
    let (pn, bytes) = sc.build(&mut c).unwrap();
    assert!(
        frames(&bytes, PacketType::OneRtt)
            .any(|f| matches!(f, Ok(Frame::ResetStream { error_code: 42, .. }))),
        "RESET_STREAM retransmitted"
    );
    deliver(&mut s, &bytes).unwrap();
    c.on_packet_acked(pn);
    assert_eq!(
        c.send_state(id),
        None,
        "reset acknowledged: stream forgotten"
    );
    assert_eq!(s.recv_state(id), Some(RecvState::ResetRecvd));
    let mut buf = [0u8; 8];
    assert_eq!(
        s.read(id, &mut buf),
        Err(StreamError::Reset {
            stream_id: id,
            error_code: 42
        })
    );
    assert!(s.is_empty(), "ResetRead: forgotten");
    // Unread reset bytes are released to connection credit.
    let (max, highest) = s.recv_credit();
    assert!(highest <= max);

    // STOP_SENDING: the server asks; the client resets with that code.
    let id = c.open(false).unwrap();
    c.write(id, &[1; 100]).unwrap();
    exchange(&mut c, &mut sc, &mut s);
    s.stop_sending(id, 5).unwrap();
    exchange(&mut s, &mut ss, &mut c);
    assert!(std::iter::from_fn(|| c.poll_event()).any(|e| e
        == StreamEvent::StopSending {
            stream_id: id,
            error_code: 5
        }));
    assert_eq!(c.send_state(id), Some(SendState::ResetSent));
    exchange(&mut c, &mut sc, &mut s);
    assert_eq!(c.send_state(id), Some(SendState::ResetRecvd));
    assert_eq!(s.recv_state(id), Some(RecvState::ResetRecvd));
}

#[test]
fn lossy_reordered_transfer_is_exact() {
    for (seed, loss) in [(1u64, 10u64), (2, 50), (3, 200), (4, 400)] {
        let cfg = StreamConfig {
            stream_window: 8192,
            conn_window: 12_000,
            send_buffer: 4096,
            ..StreamConfig::default()
        };
        let (mut c, mut s) = pair(cfg);
        let (mut sc, mut ss) = (Sender::new(), Sender::new());
        let mut rng = Rng(seed);
        let data = rng.bytes(100_000);
        let id = c.open(false).unwrap();
        let mut sent = 0;
        let mut got = Vec::new();
        let mut in_flight: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut back: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut finished = false;
        for _ in 0..200_000 {
            if sent < data.len() {
                sent += c.write(id, &data[sent..]).unwrap();
            } else if !finished {
                c.finish(id).unwrap();
                finished = true;
            }
            while in_flight.len() < 16 {
                let Some(p) = sc.build(&mut c) else { break };
                in_flight.push(p);
            }
            while let Some(p) = ss.build(&mut s) {
                back.push(p);
            }
            // Deliver in random order with loss (per mille).
            while !in_flight.is_empty() {
                let i = rng.below(in_flight.len() as u64) as usize;
                let (pn, bytes) = in_flight.swap_remove(i);
                if rng.below(1000) < loss {
                    c.on_packet_lost(pn);
                } else {
                    deliver(&mut s, &bytes).unwrap();
                    c.on_packet_acked(pn);
                }
            }
            for (pn, bytes) in back.drain(..) {
                if rng.below(1000) < loss {
                    s.on_packet_lost(pn);
                } else {
                    deliver(&mut c, &bytes).unwrap();
                    s.on_packet_acked(pn);
                }
            }
            if s.recv_state(id).is_none() {
                continue;
            }
            let (chunk, fin) = read_all(&mut s, id);
            got.extend_from_slice(&chunk);
            // Flow control never exceeded.
            let (max, used) = c.send_credit();
            assert!(used <= max);
            if fin {
                break;
            }
        }
        assert!(
            got == data,
            "seed {seed}: every byte exactly once, in order"
        );
        assert!(loss == 0 || c.stats().stream_bytes_retransmitted > 0);
        assert_eq!(c.tracked_frames(), 0);
    }
}

#[test]
fn flow_control_and_blocked_frames() {
    let cfg = StreamConfig {
        stream_window: 1000,
        conn_window: 1500,
        ..StreamConfig::default()
    };
    let (mut c, mut s) = pair(cfg);
    let (mut sc, mut ss) = (Sender::new(), Sender::new());
    let a = c.open(false).unwrap();
    let b = c.open(false).unwrap();
    c.write(a, &[1; 5000]).unwrap();
    c.write(b, &[2; 5000]).unwrap();
    let mut blocked = Vec::new();
    while let Some((pn, bytes)) = sc.build(&mut c) {
        for f in frames(&bytes, PacketType::OneRtt) {
            match f.unwrap() {
                Frame::DataBlocked(l) => blocked.push(("conn", l)),
                Frame::StreamDataBlocked { limit, .. } => blocked.push(("stream", limit)),
                _ => {}
            }
        }
        deliver(&mut s, &bytes).unwrap();
        c.on_packet_acked(pn);
    }
    assert_eq!(
        c.send_credit(),
        (1500, 1500),
        "connection credit used, not exceeded"
    );
    assert!(blocked.contains(&("conn", 1500)), "{blocked:?}");
    // Reading half the window re-advertises; the MAX_DATA packet is lost
    // and the retransmission carries the current value.
    let mut buf = [0u8; 1000];
    s.read(a, &mut buf).unwrap();
    let (pn, _) = ss.build(&mut s).unwrap();
    s.read(b, &mut buf[..500]).unwrap();
    s.on_packet_lost(pn);
    let (pn, bytes) = ss.build(&mut s).unwrap();
    let max_data: Vec<u64> = frames(&bytes, PacketType::OneRtt)
        .filter_map(|f| match f.unwrap() {
            Frame::MaxData(m) => Some(m),
            _ => None,
        })
        .collect();
    let current = s.recv_credit().0;
    assert_eq!(current, 1000 + 1500);
    assert_eq!(
        max_data,
        vec![current],
        "MAX_DATA resent with its current value"
    );
    let msd = frames(&bytes, PacketType::OneRtt)
        .filter(|f| matches!(f, Ok(Frame::MaxStreamData { .. })))
        .count();
    assert_eq!(
        msd, 2,
        "both streams' MAX_STREAM_DATA (one resent, one new)"
    );
    deliver(&mut c, &bytes).unwrap();
    s.on_packet_acked(pn);
    assert_eq!(c.send_credit().0, current);
    assert!(c.stats().blocked_frames_sent >= 2);

    // STREAMS_BLOCKED once the peer's MAX_STREAMS is used up.
    let mut c = set(
        Role::Client,
        StreamConfig {
            max_streams_uni: 2,
            ..cfg
        },
    );
    c.open(true).unwrap();
    c.open(true).unwrap();
    assert_eq!(c.open(true), Err(StreamError::StreamLimit { uni: true }));
    let mut sc = Sender::new();
    let (_, bytes) = sc.build(&mut c).unwrap();
    assert!(frames(&bytes, PacketType::OneRtt).any(|f| f
        == Ok(Frame::StreamsBlocked {
            uni: true,
            limit: 2
        })));
}

#[test]
fn peer_violations_are_typed_transport_errors() {
    let cfg = StreamConfig {
        stream_window: 1000,
        conn_window: 1500,
        crypto_buffer: 4096,
        ..StreamConfig::default()
    };
    let mut s = set(Role::Server, cfg);
    // Stream credit.
    assert_eq!(
        code(s.on_frame(&stream_frame(0, 900, &[0; 101], false))),
        TransportErrorCode::FlowControlError
    );
    // Connection credit: two streams of 800 > 1500.
    let mut s = set(Role::Server, cfg);
    s.on_frame(&stream_frame(0, 0, &[0; 800], false)).unwrap();
    assert_eq!(
        code(s.on_frame(&stream_frame(4, 0, &[0; 800], false))),
        TransportErrorCode::FlowControlError
    );
    // Stream limit: bidi index 8 with MAX_STREAMS 8.
    let mut s = set(Role::Server, cfg);
    assert_eq!(
        code(s.on_frame(&stream_frame(8 << 2, 0, b"x", false))),
        TransportErrorCode::StreamLimitError
    );
    // A server-initiated stream the server never opened.
    assert_eq!(
        code(s.on_frame(&stream_frame(1, 0, b"x", false))),
        TransportErrorCode::StreamStateError
    );
    // STREAM on our own unidirectional stream.
    let sid = s.open(true).unwrap();
    assert_eq!(sid, 3);
    assert_eq!(
        code(s.on_frame(&stream_frame(sid, 0, b"x", false))),
        TransportErrorCode::StreamStateError
    );
    // MAX_STREAM_DATA and STOP_SENDING on the peer's unidirectional stream.
    assert_eq!(
        code(s.on_frame(&Frame::MaxStreamData {
            stream_id: 2,
            max: 10
        })),
        TransportErrorCode::StreamStateError
    );
    assert_eq!(
        code(s.on_frame(&Frame::StopSending {
            stream_id: 2,
            error_code: 0
        })),
        TransportErrorCode::StreamStateError
    );
    // Final size: changed, below received data, data past it.
    let mut s = set(Role::Server, cfg);
    s.on_frame(&stream_frame(0, 0, &[0; 100], true)).unwrap();
    assert_eq!(
        code(s.on_frame(&stream_frame(0, 0, &[0; 50], true))),
        TransportErrorCode::FinalSizeError
    );
    assert_eq!(
        code(s.on_frame(&stream_frame(0, 100, &[0; 1], false))),
        TransportErrorCode::FinalSizeError
    );
    assert_eq!(
        code(s.on_frame(&Frame::ResetStream {
            stream_id: 0,
            error_code: 1,
            final_size: 99
        })),
        TransportErrorCode::FinalSizeError
    );
    s.on_frame(&stream_frame(4, 0, &[0; 200], false)).unwrap();
    assert_eq!(
        code(s.on_frame(&Frame::ResetStream {
            stream_id: 4,
            error_code: 1,
            final_size: 150
        })),
        TransportErrorCode::FinalSizeError
    );
    // A reset's final size counts against connection credit.
    s.on_frame(&stream_frame(12, 0, &[0; 800], false)).unwrap();
    assert_eq!(
        code(s.on_frame(&Frame::ResetStream {
            stream_id: 8,
            error_code: 1,
            final_size: 1000
        })),
        TransportErrorCode::FlowControlError
    );
    // CRYPTO past the buffer.
    let mut s = set(Role::Server, cfg);
    assert_eq!(
        code(s.on_frame(&Frame::Crypto {
            offset: 4000,
            data: &[0; 200]
        })),
        TransportErrorCode::CryptoBufferExceeded
    );
    // Duplicates and late frames for closed streams are fine.
    let mut s = set(Role::Server, cfg);
    s.on_frame(&stream_frame(0, 0, b"abc", false)).unwrap();
    s.on_frame(&stream_frame(0, 0, b"abc", false)).unwrap();
    s.on_frame(&stream_frame(0, 1, b"bcde", true)).unwrap();
    assert_eq!(read_all(&mut s, 0), (b"abcde".to_vec(), true));
    s.finish(0).unwrap();
}

#[test]
fn crypto_streams_reassemble_per_space() {
    let (mut c, mut s) = pair(StreamConfig::default());
    let msg: Vec<u8> = (0..6000u32).map(|i| i as u8).collect();
    for space in PnSpace::ALL {
        let cs = c.crypto_mut(space);
        assert_eq!(cs.write(&msg), 4096, "bounded by crypto_buffer");
    }
    // Initial and Handshake data is built by the handshake layer; 1-RTT
    // CRYPTO goes through `fill`.
    let mut sc = Sender::new();
    let mut frames_seen = Vec::new();
    while let Some((pn, bytes)) = sc.build(&mut c) {
        for f in frames(&bytes, PacketType::OneRtt) {
            if let Frame::Crypto { offset, data } = f.unwrap() {
                frames_seen.push((offset, data.to_vec()));
            }
        }
        if pn == 0 {
            c.on_packet_lost(pn);
        } else {
            c.on_packet_acked(pn);
        }
    }
    // Deliver out of order: the retransmission of the lost first packet
    // arrived last.
    frames_seen.reverse();
    for (off, data) in &frames_seen {
        s.on_crypto_frame(PnSpace::ApplicationData, *off, data)
            .unwrap();
    }
    let mut out = vec![0u8; 5000];
    let n = s.crypto_mut(PnSpace::ApplicationData).read(&mut out);
    assert_eq!(&out[..n], &msg[..4096]);
    assert!(
        c.crypto(PnSpace::ApplicationData)
            .send_buffer()
            .acked_prefix()
            == 4096,
        "all acknowledged after the retransmission"
    );
    // The handshake spaces' buffers are independent.
    let hs = c.crypto_mut(PnSpace::Handshake);
    let mut buf = [0u8; 1200];
    let (n, off, len) = hs.write_frame(&mut buf).unwrap();
    assert_eq!(off, 0);
    s.on_crypto_frame(PnSpace::Handshake, off, &buf[n - len..n])
        .unwrap();
    assert_eq!(s.crypto(PnSpace::Handshake).readable(), len as u64);
    assert_eq!(s.crypto(PnSpace::Initial).readable(), 0);
}

#[test]
fn record_eviction_only_causes_spurious_retransmission() {
    let cfg = StreamConfig {
        tracked_frames: 16,
        ..StreamConfig::default()
    };
    let (mut c, mut s) = pair(cfg);
    let id = c.open(false).unwrap();
    let data = Rng(3).bytes(30_000);
    c.write(id, &data).unwrap();
    c.finish(id).unwrap();
    let mut sc = Sender::new();
    let mut held = Vec::new();
    // Build many packets without acknowledging any: old records are evicted.
    for _ in 0..40 {
        let Some(p) = sc.build(&mut c) else { break };
        held.push(p);
    }
    assert!(c.stats().records_evicted > 0);
    assert!(c.tracked_frames() <= 16);
    for (pn, bytes) in held {
        deliver(&mut s, &bytes).unwrap();
        c.on_packet_acked(pn);
    }
    exchange(&mut c, &mut sc, &mut s);
    assert_eq!(read_all(&mut s, id), (data, true));
}

#[test]
fn sent_frame_records_are_exposed() {
    let (mut c, _) = pair(StreamConfig::default());
    let id = c.open(false).unwrap();
    c.write(id, b"abc").unwrap();
    let mut rec = PacketFrames::default();
    let mut buf = [0u8; 100];
    let n = c.fill(&mut buf, &mut rec);
    assert!(n > 0);
    assert_eq!(
        rec.frames(),
        &[SentFrame::Stream {
            stream_id: 0,
            offset: 0,
            len: 3,
            fin: false
        }]
    );
    // Not sent after all: requeued and built again.
    c.requeue(&rec);
    let n2 = c.fill(&mut buf, &mut rec);
    assert_eq!(n, n2);
}
