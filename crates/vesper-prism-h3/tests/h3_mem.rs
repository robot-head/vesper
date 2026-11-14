//! HTTP/3 over an in-memory stream pair: request/response, the response
//! obligation, and every connection- and stream-error path RFC 9114 names
//! that this crate implements.

mod support;

use support::{Mem, pair};
use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{
    ConnectionError, Event, H3Config, Request, Response, SendError, ServerConnection,
};
use vesper_prism_h3::error::Code;
use vesper_prism_h3::frame::{self, Settings};
use vesper_prism_h3::qpack::{Encoder, EncoderConfig, Field};
use vesper_prism_h3::transport::Transport;

struct Pair {
    client: ClientConnection,
    server: ServerConnection,
    c: Mem,
    s: Mem,
}

impl Pair {
    fn new() -> Self {
        Self::with(H3Config::default(), H3Config::default())
    }
    fn with(ccfg: H3Config, scfg: H3Config) -> Self {
        let (c, s) = pair();
        Self {
            client: ClientConnection::new(ccfg),
            server: ServerConnection::new(scfg),
            c,
            s,
        }
    }
    fn step(&mut self) {
        for _ in 0..4 {
            self.client.poll(&mut self.c);
            self.server.poll(&mut self.s);
        }
    }
    fn request(&mut self) -> Request {
        self.step();
        match self.server.poll_event() {
            Some(Event::Request(r)) => *r,
            Some(e) => panic!("unexpected {e:?}"),
            None => panic!("no request"),
        }
    }
    fn response(&mut self) -> ClientEvent {
        self.step();
        self.client.poll_event().expect("client event")
    }
}

fn get(path: &str) -> ClientRequest {
    ClientRequest::get("prism.test", path)
}

#[test]
fn request_and_response_round_trip() {
    let mut p = Pair::new();
    let id = p.client.send_request(&mut p.c, &get("/hello?x=1")).unwrap();
    let req = p.request();
    assert_eq!(req.stream_id, id);
    assert_eq!(req.head.method, "GET");
    assert_eq!(req.head.path, "/hello?x=1");
    assert_eq!(req.head.authority.as_deref(), Some("prism.test"));
    let sent = req
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 200,
                fields: vec![Field::new("content-type", "text/plain")],
                body: b"hi there".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(sent.reset, None);
    let ClientEvent::Response(r) = p.response() else {
        panic!()
    };
    assert_eq!(r.stream_id, id);
    assert_eq!(r.head.status, 200);
    assert_eq!(r.body, b"hi there");
    assert!(r.head.fields.contains(&Field::new("content-length", "8")));
    // Both sides exchanged SETTINGS.
    assert_eq!(
        p.client.peer_settings().unwrap().qpack_max_table_capacity,
        4096
    );
    assert!(p.server.peer_settings().is_some());
    // Response complete and flushed: the server forgot the stream.
    p.step();
    assert_eq!(p.server.open_requests(), 0);
    assert_eq!(p.server.stats().responses, 1);
    assert!(p.c.closed().is_none() && p.s.closed().is_none());
}

#[test]
fn many_requests_share_the_dynamic_table() {
    let mut p = Pair::new();
    // Exchange SETTINGS first: before them the client's table capacity is 0.
    p.step();
    for i in 0..20 {
        let mut r = get(&format!("/items/{i}"));
        r.fields.push(Field::new("user-agent", "vesper-test/1.0"));
        r.fields.push(Field::new("x-tenant", "acme"));
        p.client.send_request(&mut p.c, &r).unwrap();
    }
    p.step();
    let mut n = 0;
    while let Some(e) = p.server.poll_event() {
        let Event::Request(r) = e else {
            panic!("{e:?}")
        };
        assert!(r.head.fields.contains(&Field::new("x-tenant", "acme")));
        let body = r.head.path.clone().into_bytes();
        let _ = r
            .responder
            .respond(
                &mut p.server,
                &mut p.s,
                Response {
                    status: 200,
                    fields: vec![Field::new("x-tenant", "acme")],
                    body,
                },
            )
            .unwrap();
        n += 1;
    }
    assert_eq!(n, 20);
    p.step();
    let mut got = 0;
    while let Some(ClientEvent::Response(r)) = p.client.poll_event() {
        assert!(r.body.starts_with(b"/items/"));
        got += 1;
    }
    assert_eq!(got, 20);
    // The server's encoder inserted fields once and referenced them later.
    let es = p.server.encoder().stats();
    assert!(es.inserts > 0 && es.dynamic_refs > es.inserts, "{es:?}");
    assert!(
        p.server.decoder().insert_count() > 0,
        "client used the table too"
    );
    // At most SETTINGS_QPACK_BLOCKED_STREAMS (16) sections could block.
    assert!(es.blocking_sections <= 16 && es.literals > 0, "{es:?}");
}

#[test]
fn streaming_body_with_trailers_and_post_body() {
    let mut p = Pair::new();
    let mut req = get("/upload");
    req.method = "POST".into();
    req.body = vec![7; 5000];
    p.client.send_request(&mut p.c, &req).unwrap();
    let r = p.request();
    assert_eq!(r.body.len(), 5000);
    assert_eq!(r.head.content_length, Some(5000));
    let mut body = r
        .responder
        .send_headers(&mut p.server, &mut p.s, 200, vec![], None)
        .unwrap();
    body.send_data(&mut p.server, &mut p.s, b"part one, ")
        .unwrap();
    body.send_data(&mut p.server, &mut p.s, b"part two")
        .unwrap();
    let _ = body
        .finish(
            &mut p.server,
            &mut p.s,
            vec![Field::new("x-checksum", "abc")],
        )
        .unwrap();
    let ClientEvent::Response(resp) = p.response() else {
        panic!()
    };
    assert_eq!(resp.body, b"part one, part two");
    assert_eq!(resp.trailers, vec![Field::new("x-checksum", "abc")]);
}

#[test]
fn head_and_bodyless_statuses() {
    let mut p = Pair::new();
    let mut req = get("/doc");
    req.method = "HEAD".into();
    p.client.send_request(&mut p.c, &req).unwrap();
    let r = p.request();
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 200,
                fields: vec![],
                body: vec![1; 42],
            },
        )
        .unwrap();
    let ClientEvent::Response(resp) = p.response() else {
        panic!()
    };
    assert!(resp.body.is_empty());
    assert!(
        resp.head
            .fields
            .contains(&Field::new("content-length", "42"))
    );

    // 204 with a body is refused; the responder comes back.
    p.client.send_request(&mut p.c, &get("/empty")).unwrap();
    let r = p.request();
    let (responder, err) = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 204,
                fields: vec![],
                body: vec![1],
            },
        )
        .unwrap_err();
    assert!(matches!(err, SendError::InvalidResponse(_)));
    // Informational and invalid statuses are refused too.
    let (responder, err) = responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 101,
                fields: vec![],
                body: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(err, SendError::InvalidResponse(_)));
    let _ = responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 204,
                fields: vec![],
                body: vec![],
            },
        )
        .unwrap();
    let ClientEvent::Response(resp) = p.response() else {
        panic!()
    };
    assert_eq!(resp.head.status, 204);
    assert!(!resp.head.fields.iter().any(|f| f.name == b"content-length"));
}

#[test]
fn a_dropped_responder_is_detected_and_reset() {
    let mut p = Pair::new();
    let id = p.client.send_request(&mut p.c, &get("/forgot")).unwrap();
    let r = p.request();
    drop(r);
    p.step();
    assert_eq!(p.server.stats().leaked_responders, 1);
    assert_eq!(
        p.response(),
        ClientEvent::Failed {
            stream_id: id,
            code: Code::INTERNAL_ERROR
        }
    );
    // An explicit reset is a discharge, not a leak.
    let id = p.client.send_request(&mut p.c, &get("/no")).unwrap();
    let r = p.request();
    let sent = r
        .responder
        .reset(&mut p.server, &mut p.s, Code::REQUEST_REJECTED)
        .unwrap();
    assert_eq!(sent.reset, Some(Code::REQUEST_REJECTED));
    assert_eq!(
        p.response(),
        ClientEvent::Failed {
            stream_id: id,
            code: Code::REQUEST_REJECTED
        }
    );
    assert_eq!(p.server.stats().leaked_responders, 1);
}

#[test]
fn goaway_refuses_later_requests() {
    let mut p = Pair::new();
    let first = p.client.send_request(&mut p.c, &get("/a")).unwrap();
    let r = p.request();
    p.server.shutdown(&mut p.s);
    // In-flight requests still complete.
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 200,
                fields: vec![],
                body: b"ok".to_vec(),
            },
        )
        .unwrap();
    p.step();
    let mut saw_goaway = false;
    let mut saw_response = false;
    while let Some(e) = p.client.poll_event() {
        match e {
            ClientEvent::GoAway(id) => {
                assert_eq!(id, first + 4);
                saw_goaway = true;
            }
            ClientEvent::Response(r) => saw_response = r.stream_id == first,
            e => panic!("{e:?}"),
        }
    }
    assert!(saw_goaway && saw_response);
    assert!(p.client.send_request(&mut p.c, &get("/b")).is_err());
    // A raw request past the GOAWAY ID is rejected by the server.
    let late = p.c.raw_open(false);
    let mut enc = Encoder::new(EncoderConfig::default());
    let mut wire = Vec::new();
    frame::write_frame(
        &mut wire,
        frame::HEADERS,
        &enc.encode(late, &get("/c").field_list()),
    );
    p.c.write(late, &wire).unwrap();
    p.c.finish(late).unwrap();
    p.step();
    assert_eq!(p.c.peer_reset(late), Some(Code::REQUEST_REJECTED.0));
}

#[test]
fn client_goaway_is_reported_once() {
    let (mut srv, mut c, mut s) = raw();
    let ctl = raw_control(&mut c);
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::GOAWAY, 0);
    c.write(ctl, &w).unwrap();
    for _ in 0..4 {
        srv.poll(&mut s);
    }
    assert!(matches!(srv.poll_event(), Some(Event::GoAway(0))));
    assert!(srv.poll_event().is_none());
    // A larger GOAWAY afterwards is an H3_ID_ERROR.
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::GOAWAY, 4);
    c.write(ctl, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::ID_ERROR);
}

/// Opens a raw client control stream with SETTINGS.
fn raw_control(c: &mut Mem) -> u64 {
    let id = c.raw_open(true);
    let mut w = Vec::new();
    frame::put_varint(&mut w, frame::STREAM_CONTROL);
    Settings::default().encode(&mut w);
    c.write(id, &w).unwrap();
    id
}

fn raw_request(c: &mut Mem, payload: &[u8], fin: bool) -> u64 {
    let id = c.raw_open(false);
    c.write(id, payload).unwrap();
    if fin {
        c.finish(id).unwrap();
    }
    id
}

fn headers(fields: &[(&str, &str)]) -> Vec<u8> {
    let list: Vec<Field> = fields.iter().map(|(n, v)| Field::new(*n, *v)).collect();
    let mut enc = Encoder::new(EncoderConfig::default());
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &enc.encode(0, &list));
    w
}

const GOOD: &[(&str, &str)] = &[
    (":method", "GET"),
    (":scheme", "https"),
    (":authority", "a.test"),
    (":path", "/"),
];

/// A server and a raw client: returns the server, the client transport
/// and the server transport.
fn raw() -> (ServerConnection, Mem, Mem) {
    let (c, s) = pair();
    (ServerConnection::new(H3Config::default()), c, s)
}

fn conn_error(server: &mut ServerConnection, s: &mut Mem) -> (ConnectionError, Code) {
    for _ in 0..4 {
        server.poll(s);
    }
    let e = server.error().expect("connection error");
    assert_eq!(s.closed(), Some(e.code()));
    (e, e.code())
}

#[test]
fn control_stream_errors() {
    // First frame is not SETTINGS.
    let (mut srv, mut c, mut s) = raw();
    let id = c.raw_open(true);
    let mut w = Vec::new();
    frame::put_varint(&mut w, frame::STREAM_CONTROL);
    frame::write_varint_frame(&mut w, frame::GOAWAY, 0);
    c.write(id, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::MISSING_SETTINGS);

    // A second SETTINGS.
    let (mut srv, mut c, mut s) = raw();
    let id = raw_control(&mut c);
    let mut w = Vec::new();
    Settings::default().encode(&mut w);
    c.write(id, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::FRAME_UNEXPECTED);

    // DATA on the control stream.
    let (mut srv, mut c, mut s) = raw();
    let id = raw_control(&mut c);
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::DATA, b"x");
    c.write(id, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::FRAME_UNEXPECTED);

    // A second control stream.
    let (mut srv, mut c, mut s) = raw();
    raw_control(&mut c);
    raw_control(&mut c);
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::STREAM_CREATION_ERROR);

    // Closing the control stream.
    let (mut srv, mut c, mut s) = raw();
    let id = raw_control(&mut c);
    c.finish(id).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::CLOSED_CRITICAL_STREAM);

    // An HTTP/2 setting.
    let (mut srv, mut c, mut s) = raw();
    let id = c.raw_open(true);
    let mut w = vec![0x00];
    frame::write_frame(&mut w, frame::SETTINGS, &[0x02, 0x00]);
    c.write(id, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::SETTINGS_ERROR);

    // A push stream from a client.
    let (mut srv, mut c, mut s) = raw();
    let id = c.raw_open(true);
    c.write(id, &[frame::STREAM_PUSH as u8, 0]).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::STREAM_CREATION_ERROR);

    // CANCEL_PUSH without any MAX_PUSH_ID.
    let (mut srv, mut c, mut s) = raw();
    let id = raw_control(&mut c);
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::CANCEL_PUSH, 3);
    c.write(id, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::ID_ERROR);

    // A decreasing MAX_PUSH_ID.
    let (mut srv, mut c, mut s) = raw();
    let id = raw_control(&mut c);
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::MAX_PUSH_ID, 5);
    frame::write_varint_frame(&mut w, frame::MAX_PUSH_ID, 4);
    c.write(id, &w).unwrap();
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::ID_ERROR);

    // Garbage on the QPACK encoder stream.
    let (mut srv, mut c, mut s) = raw();
    raw_control(&mut c);
    let id = c.raw_open(true);
    // Insert with static name reference index 120 (no such entry).
    c.write(
        id,
        &[frame::STREAM_QPACK_ENCODER as u8, 0x3f, 0xe1, 0xff, 0x01],
    )
    .unwrap();
    assert_eq!(
        conn_error(&mut srv, &mut s).1,
        Code::QPACK_ENCODER_STREAM_ERROR
    );

    // A Section Acknowledgment for nothing on the decoder stream.
    let (mut srv, mut c, mut s) = raw();
    raw_control(&mut c);
    let id = c.raw_open(true);
    c.write(id, &[frame::STREAM_QPACK_DECODER as u8, 0x80])
        .unwrap();
    assert_eq!(
        conn_error(&mut srv, &mut s).1,
        Code::QPACK_DECODER_STREAM_ERROR
    );
}

#[test]
fn unknown_streams_frames_and_settings_are_tolerated() {
    let (mut srv, mut c, mut s) = raw();
    let ctl = raw_control(&mut c);
    // Grease frame and unknown frame on the control stream.
    let mut w = Vec::new();
    frame::write_frame(&mut w, 0x21 + 0x1f * 3, b"grease");
    frame::write_frame(&mut w, 0x7777, b"");
    c.write(ctl, &w).unwrap();
    // An unknown (grease) stream type: STOP_SENDING, no connection error.
    let odd = c.raw_open(true);
    let mut w = Vec::new();
    frame::put_varint(&mut w, 0x21 + 0x1f);
    w.extend_from_slice(b"ignored bytes");
    c.write(odd, &w).unwrap();
    // A request with unknown frames around its HEADERS.
    let mut req = Vec::new();
    frame::write_frame(&mut req, 0x40, b"x");
    req.extend(headers(GOOD));
    frame::write_frame(&mut req, 0x41, b"y");
    let id = raw_request(&mut c, &req, true);
    for _ in 0..4 {
        srv.poll(&mut s);
    }
    assert!(srv.error().is_none(), "{:?}", srv.error());
    assert_eq!(c.stopped(odd), Some(Code::STREAM_CREATION_ERROR.0));
    let Some(Event::Request(r)) = srv.poll_event() else {
        panic!()
    };
    assert_eq!(r.stream_id, id);
    let _ = r.responder.reset(&mut srv, &mut s, Code::NO_ERROR);
    assert!(srv.stats().unknown_frames >= 4);
    assert_eq!(srv.stats().unknown_streams, 1);
}

#[test]
fn request_stream_errors() {
    // DATA before HEADERS: a connection error.
    let (mut srv, mut c, mut s) = raw();
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::DATA, b"x");
    raw_request(&mut c, &w, true);
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::FRAME_UNEXPECTED);

    // A reserved HTTP/2 frame type on a request stream.
    let (mut srv, mut c, mut s) = raw();
    let mut w = headers(GOOD);
    frame::write_frame(&mut w, 0x06, b"12345678");
    raw_request(&mut c, &w, true);
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::FRAME_UNEXPECTED);

    // SETTINGS on a request stream.
    let (mut srv, mut c, mut s) = raw();
    let mut w = Vec::new();
    Settings::default().encode(&mut w);
    raw_request(&mut c, &w, true);
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::FRAME_UNEXPECTED);

    // A frame cut off by FIN.
    let (mut srv, mut c, mut s) = raw();
    let mut w = headers(GOOD);
    w.extend_from_slice(&[frame::DATA as u8, 10, 1, 2]);
    raw_request(&mut c, &w, true);
    assert_eq!(conn_error(&mut srv, &mut s).1, Code::FRAME_ERROR);

    // Invalid QPACK in HEADERS: a connection error.
    let (mut srv, mut c, mut s) = raw();
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &[0x00, 0x00, 0xff, 0x7f]);
    raw_request(&mut c, &w, true);
    assert_eq!(
        conn_error(&mut srv, &mut s).1,
        Code::QPACK_DECOMPRESSION_FAILED
    );

    // Stream errors: the stream is reset, the connection survives.
    for (fields, body, code) in [
        (
            vec![(":method", "GET"), (":path", "/")],
            None,
            Code::MESSAGE_ERROR,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("Upper", "x"),
            ],
            None,
            Code::MESSAGE_ERROR,
        ),
        (
            vec![
                (":method", "POST"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("content-length", "3"),
            ],
            Some(&b"12"[..]),
            Code::MESSAGE_ERROR,
        ),
        (
            vec![
                (":method", "POST"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("content-length", "1"),
            ],
            Some(&b"12"[..]),
            Code::MESSAGE_ERROR,
        ),
        (
            vec![(":method", "CONNECT"), (":authority", "a:443")],
            None,
            Code::REQUEST_REJECTED,
        ),
        (
            vec![
                (":method", "CONNECT"),
                (":protocol", "webtransport"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/wt"),
            ],
            None,
            Code::MESSAGE_ERROR,
        ),
    ] {
        let (mut srv, mut c, mut s) = raw();
        let mut w = headers(&fields);
        if let Some(b) = body {
            frame::write_frame(&mut w, frame::DATA, b);
        }
        let id = raw_request(&mut c, &w, true);
        for _ in 0..3 {
            srv.poll(&mut s);
        }
        assert!(srv.error().is_none(), "{fields:?}: {:?}", srv.error());
        assert_eq!(c.peer_reset(id), Some(code.0), "{fields:?}");
        assert!(matches!(
            srv.poll_event(),
            Some(Event::RequestFailed { stream_id, code: c2 }) if stream_id == id && c2 == code
        ));
    }

    // FIN with no HEADERS at all: H3_REQUEST_INCOMPLETE.
    let (mut srv, mut c, mut s) = raw();
    let id = raw_request(&mut c, &[], true);
    for _ in 0..3 {
        srv.poll(&mut s);
    }
    assert_eq!(c.peer_reset(id), Some(Code::REQUEST_INCOMPLETE.0));

    // A body larger than the limit: H3_EXCESSIVE_LOAD, before buffering it.
    let (c, s) = pair();
    let (mut c, mut s) = (c, s);
    let mut srv = ServerConnection::new(H3Config {
        max_body_bytes: 100,
        ..H3Config::default()
    });
    let mut w = headers(GOOD);
    frame::write_frame(&mut w, frame::DATA, &[0; 101]);
    let id = raw_request(&mut c, &w, true);
    for _ in 0..3 {
        srv.poll(&mut s);
    }
    assert_eq!(c.peer_reset(id), Some(Code::EXCESSIVE_LOAD.0));
    assert!(srv.error().is_none());
}

#[test]
fn concurrency_limit_rejects_excess_requests() {
    let (c, s) = pair();
    let (mut c, mut s) = (c, s);
    let mut srv = ServerConnection::new(H3Config {
        max_concurrent_requests: 2,
        ..H3Config::default()
    });
    let ids: Vec<u64> = (0..3)
        .map(|_| raw_request(&mut c, &headers(GOOD), false))
        .collect();
    srv.poll(&mut s);
    assert_eq!(c.peer_reset(ids[2]), Some(Code::REQUEST_REJECTED.0));
    assert_eq!(srv.stats().rejected, 1);
    assert!(c.peer_reset(ids[0]).is_none());
}

#[test]
fn qpack_blocked_request_waits_for_the_encoder_stream() {
    let (mut srv, mut c, mut s) = raw();
    raw_control(&mut c);
    for _ in 0..2 {
        srv.poll(&mut s);
    }
    // Our client-side encoder, using the server's advertised table.
    let mut enc = Encoder::new(EncoderConfig::default());
    enc.apply_peer_settings(4096, 16);
    let list: Vec<Field> = GOOD
        .iter()
        .map(|(n, v)| Field::new(*n, *v))
        .chain([Field::new("x-custom", "value-1")])
        .collect();
    let id = c.raw_open(false);
    let block = enc.encode(id, &list);
    assert_ne!(block[0], 0, "references the dynamic table");
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &block);
    c.write(id, &w).unwrap();
    c.finish(id).unwrap();
    for _ in 0..3 {
        srv.poll(&mut s);
    }
    assert!(srv.poll_event().is_none(), "blocked");
    assert_eq!(srv.decoder().blocked_streams(), 1);
    assert_eq!(srv.stats().blocked_sections, 1);
    // The encoder stream arrives late: the request is decoded.
    let es = c.raw_open(true);
    let mut w = vec![frame::STREAM_QPACK_ENCODER as u8];
    w.extend(enc.take_encoder_stream());
    c.write(es, &w).unwrap();
    for _ in 0..3 {
        srv.poll(&mut s);
    }
    let Some(Event::Request(r)) = srv.poll_event() else {
        panic!("unblocked request")
    };
    assert!(r.head.fields.contains(&Field::new("x-custom", "value-1")));
    let _ = r.responder.reset(&mut srv, &mut s, Code::NO_ERROR);
    assert!(srv.error().is_none());
}

#[test]
fn blocked_stream_limit_is_enforced() {
    let (c, s) = pair();
    let (mut c, mut s) = (c, s);
    let mut srv = ServerConnection::new(H3Config {
        settings: Settings {
            qpack_max_table_capacity: 4096,
            qpack_blocked_streams: 1,
            max_field_section_size: Some(16 * 1024),
            ..Settings::default()
        },
        ..H3Config::default()
    });
    raw_control(&mut c);
    srv.poll(&mut s);
    // An encoder that ignores the limit (claims 10 blocked streams).
    let mut enc = Encoder::new(EncoderConfig::default());
    enc.apply_peer_settings(4096, 10);
    for i in 0..2 {
        let id = c.raw_open(false);
        let block = enc.encode(
            id,
            &[
                Field::new(":method", "GET"),
                Field::new(format!("x-{i}"), "v"),
            ],
        );
        let mut w = Vec::new();
        frame::write_frame(&mut w, frame::HEADERS, &block);
        c.write(id, &w).unwrap();
    }
    let (e, code) = conn_error(&mut srv, &mut s);
    assert_eq!(code, Code::QPACK_DECOMPRESSION_FAILED, "{e:?}");
}

#[test]
fn peer_reset_after_delivery_is_reported() {
    let mut p = Pair::new();
    let id = p.client.send_request(&mut p.c, &get("/slow")).unwrap();
    let r = p.request();
    p.client.cancel(&mut p.c, id);
    p.step();
    // STOP_SENDING is seen when the response is written: the bytes are
    // dropped and the application is told.
    let mut streaming = r
        .responder
        .send_headers(&mut p.server, &mut p.s, 200, vec![], None)
        .unwrap();
    p.step();
    assert!(matches!(
        p.server.poll_event(),
        Some(Event::ResponseAbandoned { stream_id, code }) if stream_id == id && code == Code::REQUEST_CANCELLED.0
    ));
    // The responder now refuses to send and can only be reset.
    assert_eq!(
        streaming.send_data(&mut p.server, &mut p.s, b"x"),
        Err(SendError::StreamGone)
    );
    let _ = streaming.reset(&mut p.server, &mut p.s, Code::REQUEST_CANCELLED);
    assert_eq!(p.server.stats().leaked_responders, 0);
}

#[test]
fn backpressured_transport_still_delivers() {
    let mut p = Pair::new();
    p.c.write_limit = 7;
    p.s.write_limit = 5;
    let mut req = get("/big");
    req.method = "PUT".into();
    req.body = (0..20_000u32).map(|i| i as u8).collect();
    p.client.send_request(&mut p.c, &req).unwrap();
    let mut got = None;
    for _ in 0..20_000 {
        p.step();
        if let Some(Event::Request(r)) = p.server.poll_event() {
            got = Some(r);
            break;
        }
    }
    let r = got.expect("request");
    assert_eq!(r.body, req.body);
    let body = r.body.clone();
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 200,
                fields: vec![],
                body,
            },
        )
        .unwrap();
    for _ in 0..20_000 {
        p.step();
        if let Some(ClientEvent::Response(resp)) = p.client.poll_event() {
            assert_eq!(resp.body, req.body);
            return;
        }
    }
    panic!("no response");
}

#[test]
fn fin_behind_a_blocked_section_waits_for_the_frames_after_it() {
    // HEADERS that block on the encoder stream, then DATA and FIN in the
    // same read: the frame boundary is checked only once DATA is processed.
    let (mut srv, mut c, mut s) = raw();
    raw_control(&mut c);
    for _ in 0..2 {
        srv.poll(&mut s);
    }
    let mut enc = Encoder::new(EncoderConfig::default());
    enc.apply_peer_settings(4096, 16);
    let list: Vec<Field> = [
        (":method", "POST"),
        (":scheme", "https"),
        (":authority", "a.test"),
        (":path", "/upload"),
        ("content-length", "5"),
        ("x-custom", "value-1"),
    ]
    .iter()
    .map(|(n, v)| Field::new(*n, *v))
    .collect();
    let id = c.raw_open(false);
    let block = enc.encode(id, &list);
    assert_ne!(block[0], 0, "references the dynamic table");
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &block);
    frame::write_frame(&mut w, frame::DATA, b"hello");
    c.write(id, &w).unwrap();
    c.finish(id).unwrap();
    for _ in 0..3 {
        srv.poll(&mut s);
    }
    assert!(srv.error().is_none(), "{:?}", srv.error());
    assert!(srv.poll_event().is_none(), "blocked");
    let es = c.raw_open(true);
    let mut w = vec![frame::STREAM_QPACK_ENCODER as u8];
    w.extend(enc.take_encoder_stream());
    c.write(es, &w).unwrap();
    for _ in 0..3 {
        srv.poll(&mut s);
    }
    assert!(srv.error().is_none(), "{:?}", srv.error());
    let Some(Event::Request(r)) = srv.poll_event() else {
        panic!("unblocked request")
    };
    assert_eq!(r.body, b"hello");
    let _ = r.responder.reset(&mut srv, &mut s, Code::NO_ERROR).unwrap();
}

#[test]
fn client_fin_behind_a_blocked_response_waits_for_its_data() {
    let (mut client, mut c, mut s, _) = raw_server();
    let id = client.send_request(&mut c, &get("/")).unwrap();
    for _ in 0..3 {
        client.poll(&mut c);
    }
    // A response that blocks on the dynamic table, with DATA and FIN
    // behind it in the same read.
    let mut enc = Encoder::new(EncoderConfig::default());
    enc.apply_peer_settings(4096, 16);
    let list = [
        Field::new(":status", "200"),
        Field::new("content-length", "2"),
        Field::new("x-custom", "value-1"),
    ];
    let block = enc.encode(id, &list);
    assert_ne!(block[0], 0, "references the dynamic table");
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &block);
    frame::write_frame(&mut w, frame::DATA, b"ok");
    s.write(id, &w).unwrap();
    s.finish(id).unwrap();
    for _ in 0..3 {
        client.poll(&mut c);
    }
    assert!(client.error().is_none(), "{:?}", client.error());
    assert!(client.poll_event().is_none(), "blocked");
    let es = s.raw_open(true);
    let mut w = vec![frame::STREAM_QPACK_ENCODER as u8];
    w.extend(enc.take_encoder_stream());
    s.write(es, &w).unwrap();
    for _ in 0..3 {
        client.poll(&mut c);
    }
    assert!(client.error().is_none(), "{:?}", client.error());
    let Some(ClientEvent::Response(resp)) = client.poll_event() else {
        panic!("unblocked response")
    };
    assert_eq!(resp.body, b"ok");
}

#[test]
fn reset_refuses_a_foreign_responder() {
    let mut a = Pair::new();
    let mut b = Pair::new();
    let ida = a.client.send_request(&mut a.c, &get("/a")).unwrap();
    let idb = b.client.send_request(&mut b.c, &get("/b")).unwrap();
    assert_eq!(ida, idb, "stream ids repeat across connections");
    let ra = a.request();
    let rb = b.request();
    // Resetting `a`'s responder through `b` touches neither request.
    let Err((responder, SendError::Foreign)) =
        ra.responder.reset(&mut b.server, &mut b.s, Code::NO_ERROR)
    else {
        panic!("foreign reset accepted")
    };
    assert!(b.s.peer_reset(idb).is_none());
    assert!(b.c.peer_reset(idb).is_none());
    let ok = Response {
        status: 200,
        fields: vec![],
        body: b"b".to_vec(),
    };
    let _ = rb.responder.respond(&mut b.server, &mut b.s, ok).unwrap();
    assert!(matches!(b.response(), ClientEvent::Response(r) if r.body == b"b"));
    // The original responder is still armed and answers its own request.
    let ok = Response {
        status: 200,
        fields: vec![],
        body: b"a".to_vec(),
    };
    let _ = responder.respond(&mut a.server, &mut a.s, ok).unwrap();
    assert!(matches!(a.response(), ClientEvent::Response(r) if r.body == b"a"));
    assert_eq!(a.server.stats().leaked_responders, 0);
    assert_eq!(b.server.stats().leaked_responders, 0);
}

#[test]
fn critical_streams_wait_for_stream_credit() {
    let mut p = Pair::new();
    p.c.uni_credit = Some(1);
    p.s.uni_credit = Some(2);
    p.step();
    assert!(p.client.error().is_none(), "{:?}", p.client.error());
    assert!(p.server.error().is_none(), "{:?}", p.server.error());
    assert!(matches!(
        p.client.send_request(&mut p.c, &get("/")),
        Err(vesper_prism_h3::client::ClientError::NotReady)
    ));
    // The peer raises the limit: initialisation resumes where it stopped.
    p.c.uni_credit = Some(2);
    p.s.uni_credit = Some(1);
    p.step();
    let id = p.client.send_request(&mut p.c, &get("/")).unwrap();
    let r = p.request();
    assert_eq!(r.stream_id, id);
    let ok = Response {
        status: 200,
        fields: vec![],
        body: b"up".to_vec(),
    };
    let _ = r.responder.respond(&mut p.server, &mut p.s, ok).unwrap();
    assert!(matches!(p.response(), ClientEvent::Response(r) if r.body == b"up"));
    assert_eq!(p.c.uni_credit, Some(0));
    assert_eq!(p.s.uni_credit, Some(0));
}

#[test]
fn respond_reserves_the_whole_response_under_backpressure() {
    let mut p = Pair::with(
        H3Config::default(),
        H3Config {
            max_stream_buffer: 200,
            ..H3Config::default()
        },
    );
    let id = p.client.send_request(&mut p.c, &get("/")).unwrap();
    let r = p.request();
    p.s.write_limit = 1;
    let big = Response {
        status: 200,
        fields: vec![],
        body: vec![7; 190],
    };
    // Body fits on its own, HEADERS plus body do not: nothing is queued.
    let Err((responder, SendError::BufferFull)) = r.responder.respond(&mut p.server, &mut p.s, big)
    else {
        panic!("over budget accepted")
    };
    p.s.write_limit = 0;
    p.step();
    assert!(p.client.poll_event().is_none(), "nothing was sent");
    let small = Response {
        status: 200,
        fields: vec![],
        body: vec![7; 40],
    };
    let _ = responder.respond(&mut p.server, &mut p.s, small).unwrap();
    let ClientEvent::Response(resp) = p.response() else {
        panic!("one well-formed response")
    };
    assert_eq!(resp.stream_id, id);
    assert_eq!(resp.body, vec![7; 40]);
}

/// A client and a raw server with SETTINGS sent; returns the client, its
/// transport, the server transport and the server's control stream.
fn raw_server() -> (ClientConnection, Mem, Mem, u64) {
    let (c, mut s) = pair();
    let client = ClientConnection::new(H3Config::default());
    let ctl = s.raw_open(true);
    let mut w = Vec::new();
    frame::put_varint(&mut w, frame::STREAM_CONTROL);
    Settings::default().encode(&mut w);
    s.write(ctl, &w).unwrap();
    (client, c, s, ctl)
}

#[test]
fn not_modified_keeps_the_representation_length() {
    let (mut client, mut c, mut s, _) = raw_server();
    let id = client.send_request(&mut c, &get("/cached")).unwrap();
    client.poll(&mut c);
    s.write(
        id,
        &headers(&[(":status", "304"), ("content-length", "100")]),
    )
    .unwrap();
    s.finish(id).unwrap();
    for _ in 0..3 {
        client.poll(&mut c);
    }
    let Some(ClientEvent::Response(resp)) = client.poll_event() else {
        panic!("304 delivered")
    };
    assert_eq!(resp.head.status, 304);
    assert!(resp.body.is_empty());
    // A 304 that does carry content is malformed.
    let id = client.send_request(&mut c, &get("/cached")).unwrap();
    client.poll(&mut c);
    let mut w = headers(&[(":status", "304"), ("content-length", "1")]);
    frame::write_frame(&mut w, frame::DATA, b"x");
    s.write(id, &w).unwrap();
    s.finish(id).unwrap();
    for _ in 0..3 {
        client.poll(&mut c);
    }
    assert_eq!(
        client.poll_event(),
        Some(ClientEvent::Failed {
            stream_id: id,
            code: Code::MESSAGE_ERROR
        })
    );
}

#[test]
fn each_lower_goaway_rejects_newly_excluded_requests() {
    let (mut client, mut c, mut s, ctl) = raw_server();
    let first = client.send_request(&mut c, &get("/1")).unwrap();
    let second = client.send_request(&mut c, &get("/2")).unwrap();
    client.poll(&mut c);
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::GOAWAY, (1 << 62) - 4);
    s.write(ctl, &w).unwrap();
    client.poll(&mut c);
    assert_eq!(
        client.poll_event(),
        Some(ClientEvent::GoAway((1 << 62) - 4))
    );
    assert!(client.poll_event().is_none());
    // The final cutoff excludes the second request only.
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::GOAWAY, second);
    s.write(ctl, &w).unwrap();
    client.poll(&mut c);
    assert_eq!(client.poll_event(), Some(ClientEvent::GoAway(second)));
    assert_eq!(
        client.poll_event(),
        Some(ClientEvent::Failed {
            stream_id: second,
            code: Code::REQUEST_REJECTED
        })
    );
    // The same cutoff again changes nothing.
    let mut w = Vec::new();
    frame::write_varint_frame(&mut w, frame::GOAWAY, second);
    s.write(ctl, &w).unwrap();
    client.poll(&mut c);
    assert!(client.poll_event().is_none());
    assert!(client.error().is_none());
    assert!(first < second);
}
