//! Extended CONNECT (RFC 9220), HTTP Datagrams and capsules (RFC 9297) and
//! WebTransport over HTTP/3 (draft-ietf-webtrans-http3) between this
//! crate's client and server over the in-memory stream pair, with and
//! without QUIC DATAGRAM frames.

mod support;

use support::{Mem, pair};
use vesper_prism_h3::capsule;
use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{
    ConnectRequest, ConnectionError, Event, H3Config, Response, SendError, ServerConnection,
    Session,
};
use vesper_prism_h3::error::Code;
use vesper_prism_h3::frame::{self, Settings};
use vesper_prism_h3::qpack::{Encoder, EncoderConfig, Field};
use vesper_prism_h3::session::{DatagramPath, SessionError, SessionEvent, SessionKind};
use vesper_prism_h3::transport::Transport;

struct Pair {
    client: ClientConnection,
    server: ServerConnection,
    c: Mem,
    s: Mem,
}

impl Pair {
    fn new(datagrams: bool) -> Self {
        Self::with(
            H3Config::default().with_webtransport(4),
            H3Config::default().with_webtransport(4),
            datagrams,
        )
    }
    fn with(ccfg: H3Config, scfg: H3Config, datagrams: bool) -> Self {
        let (mut c, mut s) = pair();
        if datagrams {
            c.max_datagram = Some(1200);
            s.max_datagram = Some(1200);
        }
        let mut p = Self {
            client: ClientConnection::new(ccfg),
            server: ServerConnection::new(scfg),
            c,
            s,
        };
        p.step();
        p
    }
    fn step(&mut self) {
        for _ in 0..4 {
            self.client.poll(&mut self.c);
            self.server.poll(&mut self.s);
        }
    }
    fn server_events(&mut self) -> Vec<Event> {
        self.step();
        std::iter::from_fn(|| self.server.poll_event()).collect()
    }
    fn client_events(&mut self) -> Vec<ClientEvent> {
        self.step();
        std::iter::from_fn(|| self.client.poll_event()).collect()
    }
    fn connect(&mut self, protocol: &str) -> (u64, ConnectRequest) {
        let id = self
            .client
            .open_session(
                &mut self.c,
                &ClientRequest::connect("prism.test", "/wt", protocol),
            )
            .unwrap();
        let mut ev = self.server_events();
        assert_eq!(ev.len(), 1, "{ev:?}");
        let Some(Event::Connect(c)) = ev.pop() else {
            panic!("{ev:?}")
        };
        assert_eq!(c.stream_id, id);
        (id, *c)
    }
    /// Opens a WebTransport session and returns (session id, obligation).
    fn session(&mut self) -> (u64, Session) {
        let (id, c) = self.connect("webtransport");
        assert_eq!(c.kind, SessionKind::WebTransport);
        assert_eq!(c.head.protocol.as_deref(), Some("webtransport"));
        let session = c
            .responder
            .accept_session(&mut self.server, &mut self.s, Vec::new())
            .unwrap();
        let ev = self.client_events();
        assert!(
            matches!(&ev[..], [ClientEvent::SessionEstablished { session_id, head }]
                if *session_id == id && head.status == 200),
            "{ev:?}"
        );
        (id, session)
    }
}

fn read_all(
    f: &mut dyn FnMut(&mut [u8]) -> Result<vesper_prism_h3::transport::ReadOutcome, SessionError>,
) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 7];
    loop {
        let r = f(&mut buf).unwrap();
        out.extend_from_slice(&buf[..r.len]);
        if r.fin {
            return (out, true);
        }
        if r.len == 0 {
            return (out, false);
        }
    }
}

#[test]
fn settings_offer_extended_connect_datagrams_and_webtransport() {
    let p = Pair::new(true);
    let peer = p.client.peer_settings().unwrap();
    assert!(peer.enable_connect_protocol);
    assert!(peer.h3_datagram);
    assert_eq!(peer.webtransport_max_sessions, Some(4));
    assert!(peer.no_rfc7540_priorities);
    // Without QUIC DATAGRAM frames, SETTINGS_H3_DATAGRAM is not sent.
    let p = Pair::new(false);
    assert!(!p.client.peer_settings().unwrap().h3_datagram);
    assert!(!p.server.sent_settings().h3_datagram);
    assert!(p.server.sent_settings().enable_connect_protocol);
}

#[test]
fn boolean_settings_other_than_zero_or_one_are_errors() {
    for id in [
        frame::SETTINGS_ENABLE_CONNECT_PROTOCOL,
        frame::SETTINGS_H3_DATAGRAM,
        0x09,
        frame::SETTINGS_ENABLE_WEBTRANSPORT,
    ] {
        let mut payload = Vec::new();
        frame::put_varint(&mut payload, id);
        frame::put_varint(&mut payload, 2);
        assert_eq!(
            Settings::parse(&payload),
            Err(frame::FrameError::BadSetting(id))
        );
    }
    // Drafts 07 and 09+ max-sessions identifiers are both understood.
    for id in [
        frame::SETTINGS_WEBTRANSPORT_MAX_SESSIONS_DRAFT07,
        frame::SETTINGS_WT_MAX_SESSIONS,
    ] {
        let mut payload = Vec::new();
        frame::put_varint(&mut payload, id);
        frame::put_varint(&mut payload, 9);
        assert_eq!(
            Settings::parse(&payload).unwrap().webtransport_max_sessions,
            Some(9)
        );
    }
}

#[test]
fn webtransport_streams_and_quic_datagrams_round_trip() {
    let mut p = Pair::new(true);
    let (sid, session) = p.session();
    assert_eq!(session.id(), sid);

    // Client bidi stream: echo.
    let bidi = p.client.open_stream(&mut p.c, sid, true).unwrap();
    assert_eq!(
        p.client
            .write_stream(&mut p.c, bidi, b"ping over wt")
            .unwrap(),
        12
    );
    p.client.finish_stream(&mut p.c, bidi).unwrap();
    // Client uni stream.
    let uni = p.client.open_stream(&mut p.c, sid, false).unwrap();
    p.client.write_stream(&mut p.c, uni, b"one way").unwrap();
    p.client.finish_stream(&mut p.c, uni).unwrap();
    // Datagram.
    assert_eq!(
        p.client.send_datagram(&mut p.c, sid, b"dgram-c").unwrap(),
        DatagramPath::Quic
    );

    let ev = p.server_events();
    let mut seen_bidi = false;
    let mut seen_uni = false;
    let mut seen_dgram = false;
    for e in ev {
        match e {
            Event::Session(SessionEvent::Stream {
                session,
                stream_id,
                bidi: true,
            }) if session == sid && stream_id == bidi => seen_bidi = true,
            Event::Session(SessionEvent::Stream {
                session,
                stream_id,
                bidi: false,
            }) if session == sid && stream_id == uni => seen_uni = true,
            Event::Session(SessionEvent::Datagram { session, payload })
                if session == sid && payload == b"dgram-c" =>
            {
                seen_dgram = true
            }
            e => panic!("unexpected {e:?}"),
        }
    }
    assert!(seen_bidi && seen_uni && seen_dgram);
    assert_eq!(p.server.stream_session(bidi), Some(sid));
    let (data, fin) = read_all(&mut |b| p.server.read_stream(&mut p.s, bidi, b));
    assert_eq!((data.as_slice(), fin), (&b"ping over wt"[..], true));
    let (data, fin) = read_all(&mut |b| p.server.read_stream(&mut p.s, uni, b));
    assert_eq!((data.as_slice(), fin), (&b"one way"[..], true));
    p.server.write_stream(&mut p.s, bidi, b"pong").unwrap();
    p.server.finish_stream(&mut p.s, bidi).unwrap();
    // Server-initiated streams and datagrams.
    let sbidi = p.server.open_stream(&mut p.s, &session, true).unwrap();
    p.server
        .write_stream(&mut p.s, sbidi, b"from server")
        .unwrap();
    let suni = p.server.open_stream(&mut p.s, &session, false).unwrap();
    p.server.write_stream(&mut p.s, suni, b"push-ish").unwrap();
    p.server.finish_stream(&mut p.s, suni).unwrap();
    assert_eq!(
        p.server
            .send_datagram(&mut p.s, &session, b"dgram-s")
            .unwrap(),
        DatagramPath::Quic
    );
    let ev = p.client_events();
    assert!(ev.contains(&ClientEvent::Session(SessionEvent::Stream {
        session: sid,
        stream_id: sbidi,
        bidi: true
    })));
    assert!(ev.contains(&ClientEvent::Session(SessionEvent::Stream {
        session: sid,
        stream_id: suni,
        bidi: false
    })));
    assert!(ev.contains(&ClientEvent::Session(SessionEvent::Datagram {
        session: sid,
        payload: b"dgram-s".to_vec()
    })));
    let (data, fin) = read_all(&mut |b| p.client.read_stream(&mut p.c, bidi, b));
    assert_eq!((data.as_slice(), fin), (&b"pong"[..], true));
    let (data, fin) = read_all(&mut |b| p.client.read_stream(&mut p.c, sbidi, b));
    assert_eq!((data.as_slice(), fin), (&b"from server"[..], false));
    let (data, fin) = read_all(&mut |b| p.client.read_stream(&mut p.c, suni, b));
    assert_eq!((data.as_slice(), fin), (&b"push-ish"[..], true));
    p.client.write_stream(&mut p.c, sbidi, b"reply").unwrap();

    // The server closes the session with a code and reason: the client
    // sees it, and the open stream is reset with WT_SESSION_GONE.
    let sent = session
        .close(&mut p.server, &mut p.s, 0x1234, "done here".as_bytes())
        .unwrap();
    assert_eq!(sent.stream_id, sid);
    assert_eq!(sent.reset, None);
    let ev = p.client_events();
    assert!(
        ev.contains(&ClientEvent::Session(SessionEvent::Closed {
            session: sid,
            code: 0x1234,
            reason: b"done here".to_vec(),
            reset: None
        })),
        "{ev:?}"
    );
    assert_eq!(p.c.peer_reset(sbidi), Some(Code::WT_SESSION_GONE.0));
    assert_eq!(
        p.client.write_stream(&mut p.c, sbidi, b"x"),
        Err(SessionError::UnknownStream)
    );
    p.step();
    assert_eq!(p.server.open_requests(), 0);
    assert_eq!(p.server.webtransport_streams(), 0);
    assert!(p.server.error().is_none() && p.client.error().is_none());
    let st = p.server.session_stats();
    assert_eq!(
        (st.sessions, st.datagrams_received, st.datagrams_sent),
        (1, 1, 1)
    );
}

#[test]
fn without_quic_datagrams_http_datagrams_use_capsules() {
    let mut p = Pair::new(false);
    let (sid, session) = p.session();
    assert_eq!(
        p.client
            .send_datagram(&mut p.c, sid, b"capsule me")
            .unwrap(),
        DatagramPath::Capsule
    );
    let ev = p.server_events();
    assert!(matches!(&ev[..],
        [Event::Session(SessionEvent::Datagram { session, payload })]
            if *session == sid && payload == b"capsule me"));
    assert_eq!(
        p.server.send_datagram(&mut p.s, &session, b"back").unwrap(),
        DatagramPath::Capsule
    );
    let ev = p.client_events();
    assert_eq!(
        ev,
        [ClientEvent::Session(SessionEvent::Datagram {
            session: sid,
            payload: b"back".to_vec()
        })]
    );
    // The client closes; the server is told and discharges with close().
    p.client
        .close_session(&mut p.c, sid, 7, b"client bye")
        .unwrap();
    let ev = p.server_events();
    assert!(
        matches!(&ev[..], [Event::Session(SessionEvent::Closed { session, code: 7, reason, reset: None })]
            if *session == sid && reason == b"client bye"),
        "{ev:?}"
    );
    let _ = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
    p.step();
    assert_eq!(p.server.open_requests(), 0);
    assert_eq!(p.client.in_flight(), 0);
}

#[test]
fn a_tunnel_protocol_carries_http_datagrams_only() {
    let mut p = Pair::new(true);
    let (sid, c) = p.connect("connect-udp");
    assert_eq!(c.kind, SessionKind::Tunnel);
    let session = c
        .responder
        .accept_session(
            &mut p.server,
            &mut p.s,
            vec![Field::new("capsule-protocol", "?1")],
        )
        .unwrap();
    let ev = p.client_events();
    assert!(
        matches!(&ev[..], [ClientEvent::SessionEstablished { head, .. }]
        if head.fields.iter().any(|f| f.name == b"capsule-protocol"))
    );
    // Context ID 0 then a UDP payload: opaque to HTTP Datagrams.
    p.client.send_datagram(&mut p.c, sid, b"\x00udp").unwrap();
    let ev = p.server_events();
    assert!(matches!(&ev[..],
        [Event::Session(SessionEvent::Datagram { payload, .. })] if payload == b"\x00udp"));
    // No WebTransport streams in a tunnel.
    assert_eq!(
        p.server.open_stream(&mut p.s, &session, true),
        Err(SessionError::NotWebTransport)
    );
    let _ = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
    let ev = p.client_events();
    assert!(matches!(
        &ev[..],
        [ClientEvent::Session(SessionEvent::Closed {
            code: 0,
            reset: None,
            ..
        })]
    ));
}

#[test]
fn streams_before_acceptance_are_held_then_released() {
    let mut p = Pair::new(true);
    let (sid, c) = p.connect("webtransport");
    // The client knows nothing yet, so it cannot open streams through the
    // API; a raw peer does it the way a browser racing the response would.
    let early = p.c.raw_open(true);
    let mut w = Vec::new();
    frame::put_varint(&mut w, frame::STREAM_WEBTRANSPORT);
    frame::put_varint(&mut w, sid);
    w.extend_from_slice(b"early bytes");
    p.c.write(early, &w).unwrap();
    p.c.raw_datagram(&[(sid / 4) as u8, b'e']);
    assert!(p.server_events().is_empty());
    let session = c
        .responder
        .accept_session(&mut p.server, &mut p.s, Vec::new())
        .unwrap();
    let ev = p.server_events();
    assert!(ev.iter().any(|e| matches!(e,
        Event::Session(SessionEvent::Stream { stream_id, bidi: false, .. }) if *stream_id == early)));
    assert!(ev.iter().any(|e| matches!(e,
        Event::Session(SessionEvent::Datagram { payload, .. }) if payload == b"e")));
    let (data, _) = read_all(&mut |b| p.server.read_stream(&mut p.s, early, b));
    assert_eq!(data, b"early bytes");
    let _ = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
}

#[test]
fn held_stream_bound_and_gone_sessions_are_refused() {
    let mut p = Pair::new(true);
    // Streams naming a session that does not exist yet (stream 400): held
    // up to the bound, then WT_BUFFERED_STREAM_REJECTED.
    let mut ids = Vec::new();
    for _ in 0..17 {
        let id = p.c.raw_open(false);
        let mut w = Vec::new();
        frame::put_varint(&mut w, frame::WEBTRANSPORT_STREAM);
        frame::put_varint(&mut w, 400);
        p.c.write(id, &w).unwrap();
        ids.push(id);
    }
    p.step();
    assert_eq!(
        p.c.stopped(ids[16]),
        Some(Code::WT_BUFFERED_STREAM_REJECTED.0)
    );
    assert_eq!(p.c.stopped(ids[0]), None);
    assert_eq!(p.server.session_stats().streams_rejected, 1);
    // A normal request means its stream is not a session: a stream that
    // names it is refused with WT_SESSION_GONE.
    let req = p
        .client
        .send_request(&mut p.c, &ClientRequest::get("prism.test", "/"))
        .unwrap();
    let ev = p.server_events();
    let Some(Event::Request(r)) = ev.into_iter().next() else {
        panic!()
    };
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 204,
                fields: Vec::new(),
                body: Vec::new(),
            },
        )
        .unwrap();
    let late = p.c.raw_open(false);
    let mut w = Vec::new();
    frame::put_varint(&mut w, frame::WEBTRANSPORT_STREAM);
    frame::put_varint(&mut w, req);
    p.c.write(late, &w).unwrap();
    p.step();
    assert_eq!(p.c.stopped(late), Some(Code::WT_SESSION_GONE.0));
    // A session ID that is not a client bidi stream ID: H3_ID_ERROR.
    let bad = p.c.raw_open(false);
    let mut w = Vec::new();
    frame::put_varint(&mut w, frame::WEBTRANSPORT_STREAM);
    frame::put_varint(&mut w, 2);
    p.c.write(bad, &w).unwrap();
    p.step();
    assert_eq!(p.server.error(), Some(ConnectionError::BadStreamId(2)));
    assert_eq!(p.c.peer_closed(), Some(Code::ID_ERROR));
}

#[test]
fn a_dropped_session_is_detected_and_ends_the_session() {
    let mut p = Pair::new(true);
    let (sid, session) = p.session();
    let s = p.client.open_stream(&mut p.c, sid, true).unwrap();
    p.client.write_stream(&mut p.c, s, b"x").unwrap();
    p.step();
    drop(session);
    let ev = p.client_events();
    assert_eq!(p.server.stats().leaked_responders, 1);
    assert_eq!(p.c.peer_reset(sid), Some(Code::INTERNAL_ERROR.0));
    assert!(
        ev.contains(&ClientEvent::Session(SessionEvent::Closed {
            session: sid,
            code: 0,
            reason: Vec::new(),
            reset: Some(Code::INTERNAL_ERROR.0)
        })),
        "{ev:?}"
    );
    // The session's stream was reset too.
    assert_eq!(p.c.peer_reset(s), Some(Code::WT_SESSION_GONE.0));
}

#[test]
fn refusals_and_limits() {
    let mut p = Pair::new(true);
    // Refusing a session with a status: an ordinary response.
    let (sid, c) = p.connect("webtransport");
    let _ = c
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 403,
                fields: Vec::new(),
                body: b"no".to_vec(),
            },
        )
        .unwrap();
    let ev = p.client_events();
    assert!(
        matches!(&ev[..], [ClientEvent::Response(r)] if r.stream_id == sid && r.head.status == 403 && r.body == b"no")
    );
    assert_eq!(
        p.client.open_stream(&mut p.c, sid, true),
        Err(SessionError::NotEstablished)
    );
    p.step();
    assert_eq!(p.server.open_requests(), 0);

    // accept_session on an ordinary request is refused.
    let mut p = Pair::new(true);
    p.client
        .send_request(&mut p.c, &ClientRequest::get("prism.test", "/"))
        .unwrap();
    let Some(Event::Request(r)) = p.server_events().into_iter().next() else {
        panic!()
    };
    let (responder, e) = r
        .responder
        .accept_session(&mut p.server, &mut p.s, Vec::new())
        .unwrap_err();
    assert_eq!(e, SendError::NotExtendedConnect);
    let _ = responder.reset(&mut p.server, &mut p.s, Code::NO_ERROR);

    // Max sessions (1): a second WebTransport CONNECT is rejected.
    let mut p = Pair::with(
        H3Config::default().with_webtransport(1),
        H3Config::default().with_webtransport(1),
        true,
    );
    let (_, s1) = p.session();
    let second = p
        .client
        .open_session(
            &mut p.c,
            &ClientRequest::connect("prism.test", "/2", "webtransport"),
        )
        .unwrap();
    let ev = p.server_events();
    assert!(matches!(&ev[..], [Event::RequestFailed { stream_id, code }]
        if *stream_id == second && *code == Code::REQUEST_REJECTED));
    let _ = s1.close(&mut p.server, &mut p.s, 0, b"").unwrap();

    // A bad close reason comes back with the obligation.
    let mut p = Pair::new(true);
    let (_, s) = p.session();
    let (s, e) = s.close(&mut p.server, &mut p.s, 0, &[0xff; 3]).unwrap_err();
    assert_eq!(e, SendError::BadCloseReason);
    let (s, e) = s
        .close(&mut p.server, &mut p.s, 0, &[b'a'; 1025])
        .unwrap_err();
    assert_eq!(e, SendError::BadCloseReason);
    let _ = s.close(&mut p.server, &mut p.s, 0, b"ok").unwrap();

    // The client refuses to open sessions the server did not offer.
    let mut p = Pair::with(
        H3Config::default().with_webtransport(1),
        H3Config::default(),
        true,
    );
    assert_eq!(
        p.client.open_session(
            &mut p.c,
            &ClientRequest::connect("prism.test", "/", "webtransport")
        ),
        Err(vesper_prism_h3::client::ClientError::Unsupported)
    );
    // ... and before the server's SETTINGS arrive.
    let (mut c, _s) = pair();
    let mut client = ClientConnection::new(H3Config::default().with_webtransport(1));
    assert_eq!(
        client.open_session(
            &mut c,
            &ClientRequest::connect("prism.test", "/", "webtransport")
        ),
        Err(vesper_prism_h3::client::ClientError::NotReady)
    );
}

#[test]
fn webtransport_application_error_codes_cross_the_wire() {
    let mut p = Pair::new(true);
    let (sid, session) = p.session();
    let s = p.client.open_stream(&mut p.c, sid, true).unwrap();
    p.client.write_stream(&mut p.c, s, b"abc").unwrap();
    p.step();
    p.client.reset_stream(&mut p.c, s, 7).unwrap();
    p.server_events();
    // Bytes the server already held are still delivered; then the reset.
    let r = p.server.read_stream(&mut p.s, s, &mut [0; 8]).unwrap();
    assert_eq!(r.len, 3);
    assert_eq!(
        p.server.read_stream(&mut p.s, s, &mut [0; 8]),
        Err(SessionError::StreamReset(Code::webtransport(7).0))
    );
    assert_eq!(Code(Code::webtransport(7).0).webtransport_code(), Some(7));
    // STOP_SENDING from the server: the client's next write fails.
    let s2 = p.client.open_stream(&mut p.c, sid, true).unwrap();
    p.client.write_stream(&mut p.c, s2, b"abc").unwrap();
    p.server_events();
    p.server.stop_stream(&mut p.s, s2, 9).unwrap();
    assert_eq!(
        p.client.write_stream(&mut p.c, s2, b"more"),
        Err(SessionError::StreamReset(Code::webtransport(9).0))
    );
    let _ = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
}

/// A raw client: its control stream and a request with arbitrary fields.
fn raw_connect(p: &mut Pair, fields: &[(&str, &str)]) -> u64 {
    let mut enc = Encoder::new(EncoderConfig::default());
    let list: Vec<Field> = fields.iter().map(|(n, v)| Field::new(*n, *v)).collect();
    let block = enc.encode(0, &list);
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &block);
    let id = p.c.raw_open(false);
    p.c.write(id, &w).unwrap();
    id
}

#[test]
fn malformed_datagrams_and_capsules() {
    // A quarter stream ID past the stream ID space: H3_DATAGRAM_ERROR.
    let mut p = Pair::new(true);
    let mut d = Vec::new();
    frame::put_varint(&mut d, 1 << 60);
    p.c.raw_datagram(&d);
    p.step();
    assert_eq!(p.server.error(), Some(ConnectionError::Datagram));
    assert_eq!(p.c.peer_closed(), Some(Code::DATAGRAM_ERROR));

    // A datagram when we never offered HTTP Datagrams.
    let mut p = Pair::with(H3Config::default(), H3Config::default(), true);
    p.c.raw_datagram(&[0, 1, 2]);
    p.step();
    assert_eq!(p.server.error(), Some(ConnectionError::Datagram));

    // A datagram for no session is dropped and counted.
    let mut p = Pair::new(true);
    p.c.raw_datagram(&[9, 1]);
    p.step();
    assert!(p.server.error().is_none());
    assert_eq!(p.server.session_stats().datagrams_dropped, 1);

    // An oversized DATAGRAM capsule on an established session: the stream
    // is reset with H3_DATAGRAM_ERROR and the session ends.
    let mut p = Pair::new(true);
    let (sid, session) = p.session();
    let mut c = Vec::new();
    frame::put_varint(&mut c, capsule::DATAGRAM);
    frame::put_varint(&mut c, 1 << 20);
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::DATA, &c);
    p.c.write(sid, &w).unwrap();
    let ev = p.server_events();
    assert!(
        ev.iter().any(
            |e| matches!(e, Event::RequestFailed { code, .. } if *code == Code::DATAGRAM_ERROR)
        ),
        "{ev:?}"
    );
    assert!(ev.iter().any(|e| matches!(e, Event::Session(SessionEvent::Closed { reset: Some(c), .. }) if *c == Code::DATAGRAM_ERROR.0)));
    assert_eq!(p.c.peer_reset(sid), Some(Code::DATAGRAM_ERROR.0));
    // The obligation is still discharged by close().
    let sent = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
    assert!(sent.reset.is_some());

    // :protocol without our SETTINGS_ENABLE_CONNECT_PROTOCOL: malformed.
    let mut p = Pair::with(H3Config::default(), H3Config::default(), false);
    let id = raw_connect(
        &mut p,
        &[
            (":method", "CONNECT"),
            (":protocol", "webtransport"),
            (":scheme", "https"),
            (":authority", "a"),
            (":path", "/"),
        ],
    );
    p.step();
    assert_eq!(p.c.peer_reset(id), Some(Code::MESSAGE_ERROR.0));
    // :protocol on GET is malformed even when enabled.
    let mut p = Pair::new(false);
    let id = raw_connect(
        &mut p,
        &[
            (":method", "GET"),
            (":protocol", "webtransport"),
            (":scheme", "https"),
            (":authority", "a"),
            (":path", "/"),
        ],
    );
    p.step();
    assert_eq!(p.c.peer_reset(id), Some(Code::MESSAGE_ERROR.0));
}

#[test]
fn drain_and_unknown_capsules() {
    let mut p = Pair::new(true);
    let (sid, session) = p.session();
    let mut c = Vec::new();
    capsule::write_capsule(&mut c, 0x4242, b"skip me");
    capsule::write_capsule(&mut c, capsule::DRAIN_WEBTRANSPORT_SESSION, b"");
    // Split one capsule stream over two DATA frames.
    let (a, b) = c.split_at(5);
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::DATA, a);
    frame::write_frame(&mut w, frame::DATA, b);
    p.c.write(sid, &w).unwrap();
    let ev = p.server_events();
    assert!(matches!(&ev[..],
        [Event::Session(SessionEvent::Draining { session })] if *session == sid));
    // A FIN on the CONNECT stream ends the session (code 0).
    p.c.finish(sid).unwrap();
    let ev = p.server_events();
    assert!(
        matches!(
            &ev[..],
            [Event::Session(SessionEvent::Closed {
                code: 0,
                reset: None,
                ..
            })]
        ),
        "{ev:?}"
    );
    let _ = session.close(&mut p.server, &mut p.s, 0, b"").unwrap();
    p.step();
    assert_eq!(p.server.open_requests(), 0);
}
