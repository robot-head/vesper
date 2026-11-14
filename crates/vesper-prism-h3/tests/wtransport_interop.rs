//! Interop with wtransport-proto 0.7.2, the protocol crate of the
//! `wtransport` WebTransport implementation (an independent codebase).
//!
//! The test plays a WebTransport client the way `wtransport` does, with
//! every byte produced and parsed by wtransport-proto: its control stream
//! header and SETTINGS frame, its session request (its QPACK encoder), its
//! WEBTRANSPORT_STREAM and unidirectional stream headers, and its HTTP/3
//! datagram codec. Our server, over the in-memory transport, must accept
//! all of it, and wtransport-proto must parse everything our server sends
//! back: SETTINGS, the 200 response (its QPACK decoder), server-opened
//! stream headers, datagrams and the CLOSE_WEBTRANSPORT_SESSION capsule.

mod support;

use std::borrow::Cow;

use support::{Mem, pair};
use vesper_prism_h3::connection::{Event, H3Config, ServerConnection};
use vesper_prism_h3::session::{DatagramPath, SessionEvent, SessionKind};
use vesper_prism_h3::transport::Transport;
use wtransport_proto::bytes::BufferReader;
use wtransport_proto::capsule::{Capsule, CapsuleKind, capsules::CloseWebTransportSession};
use wtransport_proto::datagram::Datagram;
use wtransport_proto::frame::{Frame, FrameKind};
use wtransport_proto::headers::Headers;
use wtransport_proto::ids::{QStreamId, SessionId, StreamId};
use wtransport_proto::session::{SessionRequest, SessionResponse};
use wtransport_proto::settings::{SettingId, Settings};
use wtransport_proto::stream_header::{StreamHeader, StreamKind};
use wtransport_proto::varint::VarInt;

fn session_id(id: u64) -> SessionId {
    SessionId::try_from_session_stream(StreamId::new(VarInt::try_from(id).unwrap())).unwrap()
}

/// Reads everything available on a stream of ours.
fn drain(c: &mut Mem, id: u64) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let r = c.read(id, &mut buf).unwrap();
        out.extend_from_slice(&buf[..r.len]);
        if r.fin || r.len == 0 {
            return (out, r.fin);
        }
    }
}

fn frames(bytes: &[u8]) -> Vec<Frame<'static>> {
    let mut r = BufferReader::new(bytes);
    let mut out = Vec::new();
    while let Some(f) = Frame::read_from_buffer(&mut r).unwrap() {
        let owned = match f.kind() {
            FrameKind::Headers => Frame::new_headers(Cow::Owned(f.payload().to_vec())),
            FrameKind::Data => Frame::new_data(Cow::Owned(f.payload().to_vec())),
            FrameKind::Settings => Frame::new_settings(Cow::Owned(f.payload().to_vec())),
            k => panic!("unexpected frame {k:?}"),
        };
        out.push(owned);
    }
    assert_eq!(r.capacity(), 0, "trailing bytes");
    out
}

struct Peer {
    server: ServerConnection,
    c: Mem,
    s: Mem,
}

impl Peer {
    fn new() -> Self {
        let (mut c, mut s) = pair();
        c.max_datagram = Some(1200);
        s.max_datagram = Some(1200);
        Self {
            server: ServerConnection::new(H3Config::default().with_webtransport(1)),
            c,
            s,
        }
    }
    fn poll(&mut self) -> Vec<Event> {
        for _ in 0..3 {
            self.server.poll(&mut self.s);
        }
        std::iter::from_fn(|| self.server.poll_event()).collect()
    }
}

#[test]
fn a_wtransport_proto_client_session() {
    let mut p = Peer::new();

    // The client's control stream and SETTINGS, as wtransport builds them
    // (QPACK without a dynamic table).
    let ctl = p.c.raw_open(true);
    let mut w = Vec::new();
    StreamHeader::new_control().write(&mut w).unwrap();
    Settings::builder()
        .qpack_max_table_capacity(VarInt::from_u32(0))
        .qpack_blocked_streams(VarInt::from_u32(0))
        .enable_webtransport()
        .enable_h3_datagrams()
        .enable_connect_protocol()
        .webtransport_max_sessions(VarInt::from_u32(1))
        .build()
        .generate_frame()
        .write(&mut w)
        .unwrap();
    p.c.write(ctl, &w).unwrap();
    assert!(p.poll().is_empty());
    let peer = p.server.peer_settings().unwrap();
    assert!(peer.enable_connect_protocol && peer.h3_datagram);
    assert_eq!(peer.webtransport_max_sessions, Some(1));

    // Our SETTINGS, parsed by wtransport-proto.
    let mut control = None;
    while let Some(id) = p.c.accept() {
        let (bytes, _) = drain(&mut p.c, id);
        let mut r = BufferReader::new(&bytes);
        let h = StreamHeader::read_from_buffer(&mut r).unwrap().unwrap();
        if matches!(h.kind(), StreamKind::Control) {
            let rest = r.buffer_remaining().to_vec();
            control = Some(rest);
        }
    }
    let control = control.expect("server control stream");
    let f = frames(&control);
    let settings = Settings::with_frame(&f[0]).unwrap();
    let one = Some(VarInt::from_u32(1));
    assert_eq!(settings.get(SettingId::EnableWebTransport), one);
    assert_eq!(settings.get(SettingId::H3Datagram), one);
    assert_eq!(settings.get(SettingId::EnableConnectProtocol), one);
    assert_eq!(
        settings.get(SettingId::WebTransportMaxSessions),
        Some(VarInt::from_u32(1))
    );

    // The session request: wtransport-proto's headers and QPACK.
    let sid = p.c.raw_open(false);
    let mut req = SessionRequest::new("https://prism.test:4433/chat?room=1").unwrap();
    req.insert("origin", "https://app.test").unwrap();
    let mut w = Vec::new();
    req.headers().generate_frame().write(&mut w).unwrap();
    p.c.write(sid, &w).unwrap();
    let mut ev = p.poll();
    let Some(Event::Connect(c)) = ev.pop() else {
        panic!("{ev:?}")
    };
    assert_eq!(c.kind, SessionKind::WebTransport);
    assert_eq!(c.head.method, "CONNECT");
    assert_eq!(c.head.protocol.as_deref(), Some("webtransport"));
    assert_eq!(c.head.authority.as_deref(), Some("prism.test:4433"));
    assert_eq!(c.head.path, "/chat?room=1");
    assert!(
        c.head
            .fields
            .iter()
            .any(|f| f.name == b"origin" && f.value == b"https://app.test")
    );
    let session = c
        .responder
        .accept_session(&mut p.server, &mut p.s, Vec::new())
        .unwrap();
    let (bytes, fin) = drain(&mut p.c, sid);
    assert!(!fin);
    let f = frames(&bytes);
    let resp = SessionResponse::try_from(Headers::with_frame(&f[0]).unwrap()).unwrap();
    assert_eq!(resp.code().into_inner(), 200);

    // A bidirectional WebTransport stream: WEBTRANSPORT_STREAM + session.
    let ws = session_id(sid);
    let bidi = p.c.raw_open(false);
    let mut w = Vec::new();
    Frame::new_webtransport(ws).write(&mut w).unwrap();
    w.extend_from_slice(b"hello from wtransport");
    p.c.write(bidi, &w).unwrap();
    p.c.finish(bidi).unwrap();
    // A unidirectional one.
    let uni = p.c.raw_open(true);
    let mut w = Vec::new();
    StreamHeader::new_webtransport(ws).write(&mut w).unwrap();
    w.extend_from_slice(b"uni payload");
    p.c.write(uni, &w).unwrap();
    p.c.finish(uni).unwrap();
    // A datagram.
    let dg = Datagram::new(QStreamId::from_session_id(ws), b"wt datagram");
    let mut buf = vec![0u8; dg.write_size()];
    dg.write(&mut buf).unwrap();
    p.c.raw_datagram(&buf);

    let ev = p.poll();
    assert!(ev.iter().any(|e| matches!(e,
        Event::Session(SessionEvent::Stream { stream_id, bidi: true, session }) if *stream_id == bidi && *session == sid)));
    assert!(ev.iter().any(|e| matches!(e,
        Event::Session(SessionEvent::Stream { stream_id, bidi: false, .. }) if *stream_id == uni)));
    assert!(ev.iter().any(|e| matches!(e,
        Event::Session(SessionEvent::Datagram { payload, .. }) if payload == b"wt datagram")));
    let mut got = Vec::new();
    let mut b = [0u8; 8];
    loop {
        let r = p.server.read_stream(&mut p.s, bidi, &mut b).unwrap();
        got.extend_from_slice(&b[..r.len]);
        if r.fin {
            break;
        }
    }
    assert_eq!(got, b"hello from wtransport");
    let mut got = Vec::new();
    loop {
        let r = p.server.read_stream(&mut p.s, uni, &mut b).unwrap();
        got.extend_from_slice(&b[..r.len]);
        if r.fin {
            break;
        }
    }
    assert_eq!(got, b"uni payload");

    // Our side, parsed by wtransport-proto.
    p.server.write_stream(&mut p.s, bidi, b"echo").unwrap();
    p.server.finish_stream(&mut p.s, bidi).unwrap();
    let (bytes, fin) = drain(&mut p.c, bidi);
    assert_eq!((bytes.as_slice(), fin), (&b"echo"[..], true));
    let sb = p.server.open_stream(&mut p.s, &session, true).unwrap();
    p.server
        .write_stream(&mut p.s, sb, b"opened by us")
        .unwrap();
    let su = p.server.open_stream(&mut p.s, &session, false).unwrap();
    p.server.write_stream(&mut p.s, su, b"uni by us").unwrap();
    assert_eq!(
        p.server
            .send_datagram(&mut p.s, &session, b"datagram by us")
            .unwrap(),
        DatagramPath::Quic
    );
    let mut opened = Vec::new();
    while let Some(id) = p.c.accept() {
        opened.push(id);
    }
    assert!(opened.contains(&sb) && opened.contains(&su));
    let (bytes, _) = drain(&mut p.c, sb);
    let mut r = BufferReader::new(&bytes);
    let f = Frame::read_from_buffer(&mut r).unwrap().unwrap();
    assert!(matches!(f.kind(), FrameKind::WebTransport));
    assert_eq!(f.session_id().unwrap().into_u64(), sid);
    assert_eq!(r.buffer_remaining(), b"opened by us");
    let (bytes, _) = drain(&mut p.c, su);
    let mut r = BufferReader::new(&bytes);
    let h = StreamHeader::read_from_buffer(&mut r).unwrap().unwrap();
    assert!(matches!(h.kind(), StreamKind::WebTransport));
    assert_eq!(h.session_id().unwrap().into_u64(), sid);
    assert_eq!(r.buffer_remaining(), b"uni by us");
    let d = p.c.recv_datagram().unwrap();
    let dg = Datagram::read(&d).unwrap();
    assert_eq!(dg.qstream_id().into_session_id().into_u64(), sid);
    assert_eq!(dg.payload(), b"datagram by us");

    // CLOSE_WEBTRANSPORT_SESSION, parsed by wtransport-proto (it expects
    // one capsule per DATA frame, which is what we send).
    let _ = session
        .close(&mut p.server, &mut p.s, 0xbeef, "goodbye".as_bytes())
        .unwrap();
    p.poll();
    let (bytes, fin) = drain(&mut p.c, sid);
    assert!(fin);
    let f = frames(&bytes);
    assert_eq!(f.len(), 1);
    let capsule = Capsule::with_frame(&f[0]).unwrap();
    assert!(matches!(
        capsule.kind(),
        CapsuleKind::CloseWebTransportSession
    ));
    let close = CloseWebTransportSession::with_capsule(&capsule).unwrap();
    assert_eq!(close.error_code(), VarInt::from_u32(0xbeef));
    assert_eq!(close.reason(), "goodbye");
    // The still-open server bidi stream was reset with WT_SESSION_GONE.
    assert_eq!(
        p.c.peer_reset(sb),
        Some(vesper_prism_h3::error::Code::WT_SESSION_GONE.0)
    );
}

#[test]
fn a_wtransport_proto_session_ended_by_fin() {
    // wtransport ends a session by finishing the CONNECT stream; that is a
    // close with code 0 and no reason.
    let mut p = Peer::new();
    let ctl = p.c.raw_open(true);
    let mut w = Vec::new();
    StreamHeader::new_control().write(&mut w).unwrap();
    Settings::builder()
        .enable_webtransport()
        .enable_h3_datagrams()
        .enable_connect_protocol()
        .webtransport_max_sessions(VarInt::from_u32(1))
        .build()
        .generate_frame()
        .write(&mut w)
        .unwrap();
    p.c.write(ctl, &w).unwrap();
    p.poll();
    let sid = p.c.raw_open(false);
    let mut w = Vec::new();
    SessionRequest::new("https://prism.test/")
        .unwrap()
        .headers()
        .generate_frame()
        .write(&mut w)
        .unwrap();
    p.c.write(sid, &w).unwrap();
    let Some(Event::Connect(c)) = p.poll().pop() else {
        panic!()
    };
    let session = c
        .responder
        .accept_session(&mut p.server, &mut p.s, Vec::new())
        .unwrap();
    p.c.finish(sid).unwrap();
    let ev = p.poll();
    assert!(
        matches!(&ev[..], [Event::Session(SessionEvent::Closed { session, code: 0, reason, reset: None })]
            if *session == sid && reason.is_empty()),
        "{ev:?}"
    );
    let _ = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
    p.poll();
    assert_eq!(p.server.open_requests(), 0);
}
