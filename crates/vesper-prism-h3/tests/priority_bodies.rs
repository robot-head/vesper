//! RFC 9218 priorities in the response scheduler, streaming request bodies
//! with back-pressure, the per-connection request memory budget, and
//! interim (1xx) responses, over the in-memory stream pair.

mod support;

use support::{Mem, pair};
use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{
    ConnectionError, Event, H3Config, Request, Response, SendError, ServerConnection,
    StreamingRequest,
};
use vesper_prism_h3::error::Code;
use vesper_prism_h3::frame;
use vesper_prism_h3::priority::{Priority, PriorityUpdate};
use vesper_prism_h3::qpack::Field;
use vesper_prism_h3::transport::Transport;

struct Pair {
    client: ClientConnection,
    server: ServerConnection,
    c: Mem,
    s: Mem,
}

impl Pair {
    fn with(scfg: H3Config) -> Self {
        let (c, s) = pair();
        let mut p = Self {
            client: ClientConnection::new(H3Config::default()),
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
}

fn get_with(path: &str, fields: &[(&str, &str)]) -> ClientRequest {
    let mut r = ClientRequest::get("prism.test", path);
    r.fields = fields.iter().map(|(n, v)| Field::new(*n, *v)).collect();
    r
}

fn requests(events: Vec<Event>) -> Vec<Request> {
    events
        .into_iter()
        .map(|e| match e {
            Event::Request(r) => *r,
            e => panic!("unexpected {e:?}"),
        })
        .collect()
}

fn body(n: usize) -> Vec<u8> {
    (0..n).map(|i| i as u8).collect()
}

/// Answers every request with a large body while the server's transport
/// has no write budget, then lets bytes out and returns the stream order
/// of every write that carried response bytes.
fn send_order(p: &mut Pair, reqs: Vec<Request>, len: usize) -> Vec<u64> {
    p.s.budget = Some(0);
    for r in reqs {
        let _ = r
            .responder
            .respond(
                &mut p.server,
                &mut p.s,
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: body(len),
                },
            )
            .unwrap();
    }
    p.s.log.clear();
    // Release bytes a little at a time so the scheduler decides.
    for _ in 0..400 {
        p.s.budget = Some(4096);
        p.server.poll(&mut p.s);
    }
    p.s.budget = None;
    p.s.log
        .iter()
        .filter(|(id, _)| id % 4 == 0)
        .map(|(id, _)| *id)
        .collect()
}

/// The order in which streams first and last wrote.
fn spans(order: &[u64], id: u64) -> (usize, usize) {
    let first = order.iter().position(|x| *x == id).unwrap();
    let last = order.iter().rposition(|x| *x == id).unwrap();
    (first, last)
}

#[test]
fn urgency_orders_responses_and_non_incremental_ones_run_to_completion() {
    let mut p = Pair::with(H3Config::default());
    let low = p
        .client
        .send_request(&mut p.c, &get_with("/low", &[("priority", "u=6")]))
        .unwrap();
    let dflt = p
        .client
        .send_request(&mut p.c, &get_with("/d", &[]))
        .unwrap();
    let high = p
        .client
        .send_request(&mut p.c, &get_with("/high", &[("priority", "u=1")]))
        .unwrap();
    let reqs = requests(p.server_events());
    assert_eq!(p.server.priority(high), Priority::new(1, false));
    assert_eq!(p.server.priority(dflt), Priority::default());
    assert_eq!(p.server.priority(low), Priority::new(6, false));
    let order = send_order(&mut p, reqs, 60_000);
    let (h0, h1) = spans(&order, high);
    let (d0, d1) = spans(&order, dflt);
    let (l0, _) = spans(&order, low);
    assert!(h1 < d0 && d1 < l0, "{order:?}");
    assert!(h0 == 0);
    // Every response arrives intact.
    let ev = p.client_events();
    assert_eq!(ev.len(), 3);
    for e in ev {
        let ClientEvent::Response(r) = e else {
            panic!("{e:?}")
        };
        assert_eq!(r.body, body(60_000));
    }
}

#[test]
fn incremental_responses_of_one_urgency_share_capacity() {
    let mut p = Pair::with(H3Config {
        incremental_quantum: 4096,
        ..H3Config::default()
    });
    let a = p
        .client
        .send_request(&mut p.c, &get_with("/a", &[("priority", "u=2, i")]))
        .unwrap();
    let b = p
        .client
        .send_request(&mut p.c, &get_with("/b", &[("priority", "i, u=2")]))
        .unwrap();
    let late = p
        .client
        .send_request(&mut p.c, &get_with("/c", &[("priority", "u=4")]))
        .unwrap();
    let reqs = requests(p.server_events());
    let order = send_order(&mut p, reqs, 40_000);
    let (a0, a1) = spans(&order, a);
    let (b0, b1) = spans(&order, b);
    // Interleaved: each starts before the other finishes.
    assert!(a0 < b1 && b0 < a1, "{order:?}");
    assert!(a1 < spans(&order, late).0 && b1 < spans(&order, late).0);
    for e in p.client_events() {
        let ClientEvent::Response(r) = e else {
            panic!("{e:?}")
        };
        assert_eq!(r.body, body(40_000));
    }
}

#[test]
fn priority_update_overrides_the_header() {
    let mut p = Pair::with(H3Config::default());
    let first = p
        .client
        .send_request(&mut p.c, &get_with("/1", &[("priority", "u=0")]))
        .unwrap();
    let second = p
        .client
        .send_request(&mut p.c, &get_with("/2", &[("priority", "u=5")]))
        .unwrap();
    p.client
        .update_priority(&mut p.c, second, Priority::new(0, false))
        .unwrap();
    p.client
        .update_priority(&mut p.c, first, Priority::new(7, false))
        .unwrap();
    let reqs = requests(p.server_events());
    assert_eq!(p.server.priority(second), Priority::new(0, false));
    assert_eq!(p.server.priority(first), Priority::new(7, false));
    assert_eq!(p.server.stats().priority_updates, 2);
    let order = send_order(&mut p, reqs, 30_000);
    assert!(
        spans(&order, second).1 < spans(&order, first).0,
        "{order:?}"
    );
}

#[test]
fn bad_priority_updates_are_connection_errors() {
    // A server-initiated or unidirectional element ID: H3_ID_ERROR.
    for (push, id, err) in [
        (false, 3, ConnectionError::BadStreamId(3)),
        (false, 2, ConnectionError::BadStreamId(2)),
        // Push is never enabled.
        (true, 0, ConnectionError::BadPushId(0)),
    ] {
        // A fresh transport pair whose client never polls, so the raw
        // control stream below is the client's only control stream.
        let (c, s) = pair();
        let mut p = Pair::with(H3Config::default());
        p.c = c;
        p.s = s;
        p.server = ServerConnection::new(H3Config::default());
        let mut w = Vec::new();
        frame::put_varint(&mut w, frame::STREAM_CONTROL);
        vesper_prism_h3::frame::Settings::default().encode(&mut w);
        PriorityUpdate {
            push,
            element_id: id,
            priority: Priority::new(1, false),
        }
        .encode(&mut w);
        let ctl = p.c.raw_open(true);
        p.c.write(ctl, &w).unwrap();
        for _ in 0..3 {
            p.server.poll(&mut p.s);
        }
        assert_eq!(p.server.error(), Some(err));
        assert_eq!(p.c.peer_closed(), Some(Code::ID_ERROR));
    }
    // PRIORITY_UPDATE on a request stream: H3_FRAME_UNEXPECTED.
    let mut p = Pair::with(H3Config::default());
    let id = p.c.raw_open(false);
    let mut w = Vec::new();
    PriorityUpdate {
        push: false,
        element_id: id,
        priority: Priority::new(1, false),
    }
    .encode(&mut w);
    p.c.write(id, &w).unwrap();
    p.step();
    assert!(matches!(
        p.server.error(),
        Some(ConnectionError::FrameUnexpected { .. })
    ));
}

fn streaming_pair(window: usize, budget: usize) -> Pair {
    Pair::with(H3Config {
        streaming_request_bodies: true,
        stream_body_window: window,
        max_connection_buffer: budget,
        ..H3Config::default()
    })
}

fn post(path: &str, len: Option<usize>) -> ClientRequest {
    let mut r = ClientRequest::get("prism.test", path);
    r.method = "POST".into();
    if let Some(n) = len {
        r.fields.push(Field::new("content-length", n.to_string()));
    }
    r
}

fn streaming(events: Vec<Event>) -> Vec<StreamingRequest> {
    events
        .into_iter()
        .filter_map(|e| match e {
            Event::StreamingRequest(r) => Some(*r),
            Event::BodyReadable { .. } => None,
            e => panic!("unexpected {e:?}"),
        })
        .collect()
}

#[test]
fn streaming_bodies_are_pulled_under_a_window() {
    let window = 16 * 1024;
    let mut p = streaming_pair(window, 1 << 20);
    let total = 200_000;
    let id = p
        .client
        .send_request_streaming(&mut p.c, &post("/upload", Some(total)))
        .unwrap();
    let data = body(total);
    for chunk in data.chunks(10_000) {
        p.client.send_body(&mut p.c, id, chunk).unwrap();
    }
    p.client.finish_body(&mut p.c, id).unwrap();
    let mut reqs = streaming(p.server_events());
    let r = reqs.pop().unwrap();
    assert_eq!(r.head.method, "POST");
    assert_eq!(r.head.content_length, Some(total as u64));
    // Not read: the server holds at most the window; the rest waits in
    // the transport (QUIC flow control on a real link).
    for _ in 0..5 {
        p.step();
    }
    let held = p.server.buffered_request_bytes();
    assert!(held <= window + 64, "{held}");
    assert!(p.s.pending(id) > total - window - 64);
    // Pulling the body releases the rest.
    let mut got = Vec::new();
    let mut buf = vec![0u8; 3000];
    loop {
        let r = p.server.read_body(&mut p.s, id, &mut buf).unwrap();
        got.extend_from_slice(&buf[..r.len]);
        assert!(p.server.buffered_request_bytes() <= window + 64);
        if r.end {
            break;
        }
        if r.len == 0 {
            p.step();
        }
    }
    assert_eq!(got, data);
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 201,
                fields: Vec::new(),
                body: Vec::new(),
            },
        )
        .unwrap();
    let ev = p.client_events();
    assert!(matches!(&ev[..], [ClientEvent::Response(r)] if r.head.status == 201));
    p.step();
    assert_eq!(p.server.open_requests(), 0);
}

#[test]
fn streaming_body_errors_and_early_responses() {
    // Content-length mismatch at FIN: H3_MESSAGE_ERROR after delivery.
    let mut p = streaming_pair(4096, 1 << 20);
    let id = p
        .client
        .send_request_streaming(&mut p.c, &post("/x", Some(10)))
        .unwrap();
    p.client.send_body(&mut p.c, id, b"short").unwrap();
    p.client.finish_body(&mut p.c, id).unwrap();
    let ev = p.server_events();
    let r = ev
        .into_iter()
        .find_map(|e| match e {
            Event::StreamingRequest(r) => Some(r),
            _ => None,
        })
        .unwrap();
    let mut buf = [0u8; 64];
    // Either the bytes are readable before the FIN is checked, or the
    // request already failed; then the body is gone.
    let _ = p.server.read_body(&mut p.s, id, &mut buf);
    p.step();
    assert_eq!(
        p.server.read_body(&mut p.s, id, &mut buf),
        Err(SendError::StreamGone)
    );
    assert_eq!(p.c.peer_reset(id), Some(Code::MESSAGE_ERROR.0));
    let _ = r
        .responder
        .reset(&mut p.server, &mut p.s, Code::NO_ERROR)
        .unwrap();

    // Trailers after a streaming body.
    let mut p = streaming_pair(4096, 1 << 20);
    let id = p
        .client
        .send_request_streaming(&mut p.c, &post("/t", None))
        .unwrap();
    p.client.send_body(&mut p.c, id, b"abc").unwrap();
    // Raw trailers: HEADERS after DATA, encoded by the client's encoder.
    let mut enc = vesper_prism_h3::qpack::Encoder::new(Default::default());
    let block = enc.encode(id, &[Field::new("x-sum", "6")]);
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &block);
    p.client.poll(&mut p.c);
    p.c.write(id, &w).unwrap();
    p.c.finish(id).unwrap();
    let ev = p.server_events();
    let r = streaming(ev).pop().unwrap();
    let mut buf = [0u8; 16];
    let got = p.server.read_body(&mut p.s, id, &mut buf).unwrap();
    assert_eq!(&buf[..got.len], b"abc");
    assert!(got.end);
    assert_eq!(p.server.take_trailers(id), vec![Field::new("x-sum", "6")]);
    let _ = r
        .responder
        .reset(&mut p.server, &mut p.s, Code::NO_ERROR)
        .unwrap();

    // Answering before the body ends: STOP_SENDING with H3_NO_ERROR.
    let mut p = streaming_pair(4096, 1 << 20);
    let id = p
        .client
        .send_request_streaming(&mut p.c, &post("/early", None))
        .unwrap();
    p.client.send_body(&mut p.c, id, &body(1000)).unwrap();
    let r = streaming(p.server_events()).pop().unwrap();
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 413,
                fields: Vec::new(),
                body: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(p.s.stopped(id).or(p.c.stopped(id)), Some(Code::NO_ERROR.0));
    let ev = p.client_events();
    assert!(
        ev.iter()
            .any(|e| matches!(e, ClientEvent::Response(r) if r.head.status == 413)),
        "{ev:?}"
    );
    p.step();
    assert_eq!(p.server.open_requests(), 0);
}

#[test]
fn whole_bodies_that_cannot_fit_the_connection_budget_do_not_deadlock() {
    // Budget 100 KB; three 60 KB uploads, each half sent before any ends:
    // 90 KB held, none can complete. The newest is reset; the others
    // finish within the budget.
    let mut p = Pair::with(H3Config {
        max_connection_buffer: 100_000,
        ..H3Config::default()
    });
    let mut ids = Vec::new();
    for i in 0..3 {
        let id = p
            .client
            .send_request_streaming(&mut p.c, &post(&format!("/{i}"), Some(60_000)))
            .unwrap();
        p.client.send_body(&mut p.c, id, &body(30_000)).unwrap();
        ids.push(id);
    }
    p.step();
    for id in &ids {
        p.client.send_body(&mut p.c, *id, &body(30_000)).unwrap();
        p.client.finish_body(&mut p.c, *id).unwrap();
    }
    let mut delivered = Vec::new();
    let mut failed = Vec::new();
    for _ in 0..10 {
        for e in p.server_events() {
            match e {
                Event::Request(r) => {
                    assert!(p.server.buffered_request_bytes() <= 100_000);
                    delivered.push(r.stream_id);
                    let _ = r.responder.reset(&mut p.server, &mut p.s, Code::NO_ERROR);
                }
                Event::RequestFailed { stream_id, code } => {
                    assert_eq!(code, Code::EXCESSIVE_LOAD);
                    failed.push(stream_id);
                }
                e => panic!("unexpected {e:?}"),
            }
        }
    }
    assert_eq!(failed, vec![ids[2]], "{delivered:?}");
    delivered.sort();
    assert_eq!(delivered, ids[..2]);
    assert_eq!(p.server.stats().budget_resets, 1);
    assert!(p.server.error().is_none());
}

#[test]
fn interim_responses_precede_the_final_one() {
    let mut p = Pair::with(H3Config::default());
    let id = p
        .client
        .send_request(&mut p.c, &get_with("/", &[]))
        .unwrap();
    let mut r = requests(p.server_events()).pop().unwrap();
    r.responder
        .send_interim(
            &mut p.server,
            &mut p.s,
            103,
            vec![Field::new("link", "</style.css>; rel=preload")],
        )
        .unwrap();
    assert_eq!(
        r.responder
            .send_interim(&mut p.server, &mut p.s, 101, Vec::new()),
        Err(SendError::InvalidResponse(
            vesper_prism_h3::message::MessageError::InvalidStatus
        ))
    );
    assert_eq!(
        r.responder
            .send_interim(&mut p.server, &mut p.s, 200, Vec::new()),
        Err(SendError::InvalidResponse(
            vesper_prism_h3::message::MessageError::InvalidStatus
        ))
    );
    assert!(matches!(
        r.responder.send_interim(
            &mut p.server,
            &mut p.s,
            100,
            vec![Field::new(":status", "100")]
        ),
        Err(SendError::InvalidResponse(_))
    ));
    let _ = r
        .responder
        .respond(
            &mut p.server,
            &mut p.s,
            Response {
                status: 200,
                fields: Vec::new(),
                body: b"final".to_vec(),
            },
        )
        .unwrap();
    let ev = p.client_events();
    assert!(
        matches!(&ev[..], [ClientEvent::Interim { stream_id, head }, ClientEvent::Response(r)]
            if *stream_id == id && head.status == 103
                && head.fields == vec![Field::new("link", "</style.css>; rel=preload")]
                && r.body == b"final"),
        "{ev:?}"
    );
    assert_eq!(p.server.stats().interim_responses, 1);
}

#[test]
fn transport_trait_defaults_report_no_datagrams() {
    // A transport that does not override the datagram methods.
    struct Bare(Mem);
    impl Transport for Bare {
        fn open_uni(&mut self) -> Result<u64, vesper_prism_h3::transport::TransportError> {
            self.0.open_uni()
        }
        fn open_bidi(&mut self) -> Result<u64, vesper_prism_h3::transport::TransportError> {
            self.0.open_bidi()
        }
        fn write(
            &mut self,
            id: u64,
            d: &[u8],
        ) -> Result<usize, vesper_prism_h3::transport::TransportError> {
            self.0.write(id, d)
        }
        fn finish(&mut self, id: u64) -> Result<(), vesper_prism_h3::transport::TransportError> {
            self.0.finish(id)
        }
        fn read(
            &mut self,
            id: u64,
            o: &mut [u8],
        ) -> Result<
            vesper_prism_h3::transport::ReadOutcome,
            vesper_prism_h3::transport::TransportError,
        > {
            self.0.read(id, o)
        }
        fn reset(&mut self, id: u64, c: Code) {
            self.0.reset(id, c)
        }
        fn stop_sending(&mut self, id: u64, c: Code) {
            self.0.stop_sending(id, c)
        }
        fn accept(&mut self) -> Option<u64> {
            self.0.accept()
        }
        fn close(&mut self, c: Code) {
            self.0.close(c)
        }
    }
    let (_, s) = pair();
    let mut b = Bare(s);
    assert_eq!(b.max_datagram_size(), None);
    assert_eq!(
        b.send_datagram(b"x"),
        Err(vesper_prism_h3::transport::TransportError::Unsupported)
    );
    assert_eq!(b.recv_datagram(), None);
}

#[test]
fn reset_uploads_release_their_priority_entries() {
    // Whole-body requests with a priority header, reset by the client
    // before FIN: nothing about them may stay behind.
    let mut p = Pair::with(H3Config::default());
    for i in 0..200 {
        let mut r = post(&format!("/u{i}"), Some(1000));
        r.fields.push(Field::new("priority", "u=1"));
        let id = p.client.send_request_streaming(&mut p.c, &r).unwrap();
        p.client.send_body(&mut p.c, id, b"part").unwrap();
        p.step();
        p.client.cancel(&mut p.c, id);
        p.step();
    }
    let ev = p.server_events();
    assert!(
        ev.iter().all(|e| matches!(e, Event::RequestFailed { .. })),
        "{ev:?}"
    );
    assert_eq!(p.server.open_requests(), 0);
    assert_eq!(p.server.tracked_priorities(), 0);
}

#[test]
fn trailers_larger_than_the_body_window_still_arrive() {
    let mut p = streaming_pair(32, 1 << 20);
    let id = p
        .client
        .send_request_streaming(&mut p.c, &post("/t", None))
        .unwrap();
    // Handed out first, so the body window applies to what follows.
    let r = streaming(p.server_events()).pop().unwrap();
    p.client.send_body(&mut p.c, id, b"abc").unwrap();
    let mut enc = vesper_prism_h3::qpack::Encoder::new(Default::default());
    let trailers = vec![Field::new("x-checksum", "a".repeat(200))];
    let block = enc.encode(id, &trailers);
    assert!(block.len() > 32);
    let mut w = Vec::new();
    frame::write_frame(&mut w, frame::HEADERS, &block);
    p.client.poll(&mut p.c);
    p.c.write(id, &w).unwrap();
    p.c.finish(id).unwrap();
    p.step();
    let mut got = Vec::new();
    let mut buf = [0u8; 16];
    let mut end = false;
    for _ in 0..20 {
        let b = p.server.read_body(&mut p.s, id, &mut buf).unwrap();
        got.extend_from_slice(&buf[..b.len]);
        if b.end {
            end = true;
            break;
        }
        p.step();
    }
    assert!(
        end,
        "stuck with {} buffered",
        p.server.buffered_request_bytes()
    );
    assert_eq!(got, b"abc");
    assert_eq!(p.server.take_trailers(id), trailers);
    let _ = r
        .responder
        .reset(&mut p.server, &mut p.s, Code::NO_ERROR)
        .unwrap();
}

#[test]
fn a_refused_initial_upload_leaves_nothing_in_flight() {
    let (c, s) = pair();
    let mut p = Pair {
        client: ClientConnection::new(H3Config {
            max_stream_buffer: 64,
            ..H3Config::default()
        }),
        server: ServerConnection::new(H3Config::default()),
        c,
        s,
    };
    p.step();
    let mut r = post("/big", None);
    r.body = body(128);
    assert!(matches!(
        p.client.send_request_streaming(&mut p.c, &r),
        Err(vesper_prism_h3::client::ClientError::BufferFull)
    ));
    assert_eq!(p.client.in_flight(), 0);
    assert!(p.server_events().is_empty());
}
