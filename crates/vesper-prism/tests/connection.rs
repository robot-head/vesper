use std::io;

use vesper_prism::{
    Error, Limits,
    connection::{
        Connection, ConnectionError, ConnectionLimits, ConnectionState, Inbound, RequestEvent,
        RequestId, Response, SubmitErrorKind,
    },
    response::{ResponseError, ResponseLimits},
};

const A: &[u8] = b"GET /a HTTP/1.1\r\nHost: x\r\n\r\n";
const B: &[u8] = b"HEAD /a HTTP/1.1\r\nHost: x\r\n\r\n";
// The body is a CRLFCRLF probe: it must never be read as a request boundary.
const C: &[u8] = b"POST /b HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\n\r\n\r\n";
const D: &[u8] = b"GET /c HTTP/1.1\r\nHost: x\r\nConnection: keep-alive, close\r\n\r\n";
const NEVER: &[u8] = b"GET /never HTTP/1.1\r\nHost: x\r\n\r\n";

const RA: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\nContent-Length: 3\r\nConnection: keep-alive\r\n\r\n\x00\xffA";
const RB: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\n";
const RC: &[u8] = b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n";
const RD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbye";

fn limits() -> ConnectionLimits {
    ConnectionLimits {
        request: Limits {
            max_head_bytes: 256,
            max_headers: 8,
            max_body_bytes: 64,
            max_buffered_bytes: 512,
        },
        response: ResponseLimits {
            max_head_bytes: 256,
            max_headers: 8,
            max_body_bytes: 64,
            max_total_bytes: 512,
        },
        max_in_flight: 8,
        max_outbound_bytes: 2048,
    }
}
fn connection() -> Connection {
    Connection::new(limits()).unwrap()
}

/// Accepts at most `limit` bytes per call; optionally fails after `fail_after`.
struct ShortWriter {
    limit: usize,
    fail_after: Option<usize>,
    written: Vec<u8>,
}
impl io::Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_after.is_some_and(|n| self.written.len() >= n) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut count = bytes.len().min(self.limit);
        if let Some(n) = self.fail_after {
            count = count.min(n - self.written.len());
        }
        self.written.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn writer(limit: usize) -> ShortWriter {
    ShortWriter {
        limit,
        fail_after: None,
        written: Vec::new(),
    }
}
fn drain(connection: &mut Connection, out: &mut ShortWriter) {
    while !connection.pending_output().is_empty() {
        connection.write_to(out).unwrap();
    }
}
fn poll_all(connection: &mut Connection) -> Vec<Inbound> {
    let mut issued = Vec::new();
    while let RequestEvent::Request(inbound) = connection.poll_request() {
        issued.push(inbound);
    }
    issued
}
fn feed(connection: &mut Connection, mut bytes: &[u8], issued: &mut Vec<Inbound>) {
    while !bytes.is_empty() {
        let accepted = connection.receive(bytes).unwrap();
        bytes = &bytes[accepted..];
        issued.extend(poll_all(connection));
    }
    issued.extend(poll_all(connection));
}
fn response_for(inbound: &Inbound) -> Response {
    match (
        inbound.request.head.method.as_str(),
        inbound.request.head.target.as_str(),
    ) {
        ("GET", "/a") => Response::new(200, vec![0, 255, b'A'])
            .header("Content-Type", b"application/octet-stream"),
        ("HEAD", "/a") => Response::new(200, b"hello".to_vec()),
        ("POST", "/b") => {
            assert_eq!(inbound.request.body, b"\r\n\r\n");
            Response::new(204, Vec::new())
        }
        ("GET", "/c") => Response::new(200, b"bye".to_vec()),
        other => panic!("unexpected request {other:?}"),
    }
}

#[test]
fn every_input_split_and_write_size_produces_exact_ordered_bytes() {
    let input = [A, B, C, D, NEVER].concat();
    let expected = [RA, RB, RC, RD].concat();
    for split in 0..=input.len() {
        for write_size in (1..=48).chain([expected.len()]) {
            let mut connection = connection();
            let mut issued = Vec::new();
            feed(&mut connection, &input[..split], &mut issued);
            feed(&mut connection, &input[split..], &mut issued);
            assert_eq!(issued.len(), 4, "split {split}");
            assert_eq!(connection.state(), ConnectionState::Closing);
            let mut out = writer(write_size);
            // Complete out of order, writing between each submission.
            for inbound in issued.iter().rev() {
                connection
                    .submit(inbound.id, response_for(inbound))
                    .unwrap();
                if !connection.pending_output().is_empty() {
                    connection.write_to(&mut out).unwrap();
                }
            }
            drain(&mut connection, &mut out);
            assert_eq!(out.written, expected, "split {split}, write {write_size}");
            assert_eq!(connection.state(), ConnectionState::Closed);
            assert_eq!(connection.retained_outbound_bytes(), 0);
            let shutdown = connection.shutdown();
            assert!(shutdown.unsent.is_empty() && shutdown.awaiting.is_empty());
            // The request after `Connection: close` is never issued. Bytes that
            // arrived before the close was seen are retained; later ones are
            // counted as discarded. Together they are exactly the suffix.
            assert!(NEVER.starts_with(&shutdown.unprocessed_input));
            assert_eq!(
                shutdown.unprocessed_input.len() + shutdown.discarded_input,
                NEVER.len()
            );
        }
    }
}

#[test]
fn single_byte_feed_issues_each_request_once() {
    let input = [A, B, C].concat();
    let mut connection = connection();
    let mut issued = Vec::new();
    for byte in &input {
        feed(&mut connection, std::slice::from_ref(byte), &mut issued);
    }
    let sequences: Vec<u64> = issued.iter().map(|i| i.id.sequence()).collect();
    assert_eq!(sequences, [0, 1, 2]);
    assert_eq!(connection.state(), ConnectionState::Open);
}

#[test]
fn out_of_order_completion_waits_for_earlier_responses() {
    let mut connection = connection();
    let mut issued = Vec::new();
    feed(&mut connection, &[A, B, C].concat(), &mut issued);
    connection
        .submit(issued[2].id, response_for(&issued[2]))
        .unwrap();
    connection
        .submit(issued[1].id, response_for(&issued[1]))
        .unwrap();
    assert!(connection.pending_output().is_empty());
    assert_eq!(connection.retained_outbound_bytes(), RB.len() + RC.len());
    connection
        .submit(issued[0].id, response_for(&issued[0]))
        .unwrap();
    let mut out = writer(usize::MAX);
    drain(&mut connection, &mut out);
    assert_eq!(out.written, [RA, RB, RC].concat());
    assert_eq!(connection.state(), ConnectionState::Open);
}

#[test]
fn stale_duplicate_and_foreign_submissions_return_the_response_unchanged() {
    let mut connection = connection();
    let mut other = self::connection();
    let mut issued = Vec::new();
    feed(&mut connection, &[A, B, C].concat(), &mut issued);
    let mut foreign = Vec::new();
    feed(&mut other, A, &mut foreign);

    let probe = Response::new(200, b"probe".to_vec()).header("X-Probe", b"1");
    let error = connection.submit(foreign[0].id, probe.clone()).unwrap_err();
    assert_eq!(error.kind, SubmitErrorKind::Foreign);
    assert_eq!(error.response, probe);

    connection
        .submit(issued[2].id, response_for(&issued[2]))
        .unwrap();
    let retained = connection.retained_outbound_bytes();
    let error = connection.submit(issued[2].id, probe.clone()).unwrap_err();
    assert_eq!(error.kind, SubmitErrorKind::Duplicate);
    assert_eq!(error.response, probe);
    assert_eq!(connection.retained_outbound_bytes(), retained);

    connection
        .submit(issued[0].id, response_for(&issued[0]))
        .unwrap();
    let error = connection.submit(issued[0].id, probe.clone()).unwrap_err();
    assert_eq!(error.kind, SubmitErrorKind::Stale);
    assert_eq!(error.response, probe);
    connection
        .submit(issued[1].id, response_for(&issued[1]))
        .unwrap();
    let error = connection.submit(issued[1].id, probe.clone()).unwrap_err();
    assert_eq!(error.kind, SubmitErrorKind::Stale);

    let mut out = writer(7);
    drain(&mut connection, &mut out);
    assert_eq!(out.written, [RA, RB, RC].concat());
    // The other connection is untouched by the foreign attempt.
    assert_eq!(other.in_flight(), 1);
}

#[test]
fn application_close_in_a_pipeline_abandons_later_requests_with_owners() {
    let mut connection = connection();
    let mut issued = Vec::new();
    feed(&mut connection, &[A, A, A, A].concat(), &mut issued);
    assert_eq!(issued.len(), 4);
    // seq 3 completes before the close, seq 2 after; both are cut off.
    connection
        .submit(issued[3].id, Response::new(200, b"late".to_vec()))
        .unwrap();
    connection
        .submit(issued[1].id, Response::new(503, Vec::new()).with_close())
        .unwrap();
    assert_eq!(connection.state(), ConnectionState::Closing);
    let abandoned = connection.take_abandoned();
    assert_eq!(abandoned.len(), 2);
    assert_eq!(abandoned[0].id, issued[2].id);
    assert!(abandoned[0].response.is_none());
    assert_eq!(abandoned[1].id, issued[3].id);
    assert!(abandoned[1].response.as_ref().unwrap().len() > 4);
    let error = connection
        .submit(issued[2].id, Response::new(200, Vec::new()))
        .unwrap_err();
    assert_eq!(error.kind, SubmitErrorKind::Abandoned);
    // Later input is discarded, never decoded.
    assert_eq!(connection.receive(A).unwrap(), A.len());
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::NoMoreRequests
    ));

    connection
        .submit(issued[0].id, Response::new(200, b"ok".to_vec()))
        .unwrap();
    let mut out = writer(3);
    drain(&mut connection, &mut out);
    assert_eq!(
        out.written,
        [
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok".as_slice(),
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]
        .concat()
    );
    assert_eq!(connection.state(), ConnectionState::Closed);
    let shutdown = connection.shutdown();
    assert_eq!(shutdown.discarded_input, A.len());
    assert!(shutdown.unsent.is_empty() && shutdown.blocked.is_empty());
    assert!(shutdown.awaiting.is_empty() && shutdown.abandoned.is_empty());
}

#[test]
fn malformed_later_request_is_answered_in_order_and_never_rescanned() {
    let smuggled = b"GET /smuggled HTTP/1.1\r\nHost: x\r\n\r\n";
    let cases: [(&[u8], Error, &[u8]); 5] = [
        (
            b"GET / HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nX",
            Error::DuplicateContentLength,
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
        (
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n",
            Error::AmbiguousFraming,
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
        (
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n",
            Error::UnsupportedTransferCoding,
            b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
        (
            b"GET / HTTP/1.0\r\nHost: x\r\n\r\n",
            Error::UnsupportedVersion,
            b"HTTP/1.1 505 HTTP Version Not Supported\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
        (
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 65\r\n\r\n",
            Error::BodyLimit,
            b"HTTP/1.1 413 Content Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
    ];
    for (bad, error, error_response) in cases {
        let mut connection = connection();
        let input = [A, bad, smuggled].concat();
        assert_eq!(connection.receive(&input).unwrap(), input.len());
        let RequestEvent::Request(first) = connection.poll_request() else {
            panic!("first request")
        };
        assert!(matches!(connection.poll_request(), RequestEvent::Failed(e) if e == error));
        assert!(matches!(
            connection.poll_request(),
            RequestEvent::NoMoreRequests
        ));
        assert_eq!(connection.failure(), Some(error));
        // The error response waits behind the outstanding first response.
        assert!(connection.pending_output().is_empty());
        connection.submit(first.id, response_for(&first)).unwrap();
        let mut out = writer(5);
        drain(&mut connection, &mut out);
        assert_eq!(out.written, [RA, error_response].concat());
        assert_eq!(connection.state(), ConnectionState::Closed);
        let shutdown = connection.shutdown();
        assert_eq!(shutdown.failure, Some(error));
        // The suffix stays opaque: no attempt was made to find `/smuggled`.
        assert!(shutdown.unprocessed_input.ends_with(smuggled));
    }
}

#[test]
fn eof_drains_complete_requests_and_closes_without_resynchronizing() {
    // Clean EOF: the final response is marked close.
    let mut connection = connection();
    let mut issued = Vec::new();
    feed(&mut connection, &[A, B].concat(), &mut issued);
    connection.end_input();
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::NoMoreRequests
    ));
    assert_eq!(connection.receive(A), Err(ConnectionError::InputEnded));
    connection
        .submit(issued[1].id, Response::new(200, b"hello".to_vec()))
        .unwrap();
    connection
        .submit(issued[0].id, response_for(&issued[0]))
        .unwrap();
    let mut out = writer(9);
    drain(&mut connection, &mut out);
    let close_b = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n";
    assert_eq!(out.written, [RA, close_b.as_slice()].concat());
    assert_eq!(connection.state(), ConnectionState::Closed);

    // EOF inside a partial frame: earlier responses still go out, no error body.
    let mut connection = self::connection();
    let mut issued = Vec::new();
    feed(
        &mut connection,
        &[A, &C[..C.len() - 1]].concat(),
        &mut issued,
    );
    assert_eq!(issued.len(), 1);
    connection.end_input();
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::Failed(Error::UnexpectedEof)
    ));
    connection
        .submit(issued[0].id, response_for(&issued[0]))
        .unwrap();
    let mut out = writer(1);
    drain(&mut connection, &mut out);
    assert_eq!(out.written, RA);
    assert_eq!(connection.state(), ConnectionState::Closed);
    assert_eq!(connection.shutdown().unprocessed_input, &C[..C.len() - 1]);

    // EOF with nothing outstanding closes immediately.
    let mut connection = self::connection();
    connection.end_input();
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::NoMoreRequests
    ));
    assert_eq!(connection.state(), ConnectionState::Closed);
}

#[test]
fn write_failure_shutdown_returns_every_unsent_byte_exactly_once() {
    let expected = [RA, RB, RC].concat();
    for fail_after in 0..expected.len() {
        let mut connection = connection();
        let mut issued = Vec::new();
        feed(&mut connection, &[A, B, C, A].concat(), &mut issued);
        for inbound in &issued[..3] {
            connection
                .submit(inbound.id, response_for(inbound))
                .unwrap();
        }
        let mut out = ShortWriter {
            limit: 4,
            fail_after: Some(fail_after),
            written: Vec::new(),
        };
        let error = loop {
            match connection.write_to(&mut out) {
                Ok(_) => {}
                Err(error) => break error,
            }
        };
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        let shutdown = connection.shutdown();
        assert_eq!(shutdown.awaiting, [issued[3].id]);
        let mut reconstructed = out.written.clone();
        for (index, unsent) in shutdown.unsent.iter().enumerate() {
            if index > 0 {
                assert_eq!(unsent.written, 0);
            }
            reconstructed.extend_from_slice(&unsent.bytes[unsent.written..]);
            assert_eq!(
                &unsent.bytes[..unsent.written],
                &out.written[out.written.len() - unsent.written..]
            );
        }
        assert_eq!(reconstructed, expected, "fail after {fail_after}");
    }
}

#[test]
fn shutdown_retains_blocked_responses_and_awaiting_ids() {
    let mut connection = connection();
    let mut issued = Vec::new();
    feed(&mut connection, &[A, B, C].concat(), &mut issued);
    connection
        .submit(issued[2].id, response_for(&issued[2]))
        .unwrap();
    let shutdown = connection.shutdown();
    let awaiting: Vec<RequestId> = vec![issued[0].id, issued[1].id];
    assert_eq!(shutdown.awaiting, awaiting);
    assert_eq!(shutdown.blocked.len(), 1);
    assert_eq!(shutdown.blocked[0].0, issued[2].id);
    let blocked = shutdown.blocked.into_iter().next().unwrap().1;
    assert_eq!(blocked.into_bytes(), RC);
}

#[test]
fn encode_errors_and_outbound_limit_return_the_response_and_keep_the_slot() {
    let mut limits = limits();
    limits.max_outbound_bytes = limits.response.max_total_bytes;
    let mut connection = Connection::new(limits).unwrap();
    let mut issued = Vec::new();
    feed(&mut connection, &[A, A, B].concat(), &mut issued);
    for (response, kind) in [
        (
            Response::new(204, b"x".to_vec()),
            SubmitErrorKind::Encode(ResponseError::BodyForbidden),
        ),
        (
            Response::new(101, Vec::new()),
            SubmitErrorKind::Encode(ResponseError::UnsupportedStatus),
        ),
        (
            Response::new(200, Vec::new()).header("Content-Length", b"0"),
            SubmitErrorKind::Encode(ResponseError::ReservedHeader),
        ),
        (
            Response::new(200, Vec::new()).header("Transfer-Encoding", b"chunked"),
            SubmitErrorKind::Encode(ResponseError::UnsupportedTransferCoding),
        ),
        (
            Response::new(200, vec![0; 65]),
            SubmitErrorKind::Encode(ResponseError::BodyLimit),
        ),
    ] {
        let error = connection
            .submit(issued[0].id, response.clone())
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.response, response);
    }
    // Two ~290-byte responses exceed the 512-byte retained budget together.
    // The rejected one keeps its slot and its owner can retry after writing.
    let big = || Response::new(200, vec![7; 64]).header("X-Pad", &[b'p'; 150]);
    connection.submit(issued[1].id, big()).unwrap();
    let error = connection.submit(issued[0].id, big()).unwrap_err();
    assert_eq!(error.kind, SubmitErrorKind::OutboundLimit);
    assert_eq!(error.response, big());
    assert!(connection.pending_output().is_empty());
    connection
        .submit(issued[0].id, Response::new(200, Vec::new()))
        .unwrap();
    let mut out = writer(usize::MAX);
    drain(&mut connection, &mut out);
    assert_eq!(connection.retained_outbound_bytes(), 0);
    connection.submit(issued[2].id, big()).unwrap();
    drain(&mut connection, &mut out);
    assert!(
        out.written
            .starts_with(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n")
    );
    // HEAD suppressed the third response's body but kept its length.
    assert!(
        out.written
            .ends_with(b"Content-Length: 64\r\nConnection: keep-alive\r\n\r\n")
    );
}

#[test]
fn head_and_body_forbidden_statuses_follow_the_associated_request() {
    let mut connection = connection();
    let mut issued = Vec::new();
    feed(&mut connection, &[B, B, A].concat(), &mut issued);
    connection
        .submit(issued[0].id, Response::new(304, Vec::new()))
        .unwrap();
    connection
        .submit(issued[1].id, Response::new(404, b"missing".to_vec()))
        .unwrap();
    // The same 404 representation on GET is sent in full.
    connection
        .submit(issued[2].id, Response::new(404, b"missing".to_vec()))
        .unwrap();
    let mut out = writer(usize::MAX);
    drain(&mut connection, &mut out);
    assert_eq!(
        out.written,
        [
            b"HTTP/1.1 304 Not Modified\r\nConnection: keep-alive\r\n\r\n".as_slice(),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 7\r\nConnection: keep-alive\r\n\r\n",
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 7\r\nConnection: keep-alive\r\n\r\nmissing",
        ]
        .concat()
    );
}

#[test]
fn input_and_in_flight_bounds_apply_backpressure_without_poisoning() {
    let mut limits = limits();
    limits.max_in_flight = 2;
    let mut connection = Connection::new(limits).unwrap();
    let input = [A; 30].concat();
    // Only the configured queue capacity is accepted; the rest stays with the caller.
    let accepted = connection.receive(&input).unwrap();
    assert_eq!(accepted, limits.request.max_buffered_bytes);
    assert_eq!(connection.receive(&input[accepted..]).unwrap(), 0);
    let RequestEvent::Request(first) = connection.poll_request() else {
        panic!()
    };
    let RequestEvent::Request(_second) = connection.poll_request() else {
        panic!()
    };
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::Backpressure
    ));
    // Draining two requests freed exactly their bytes of input capacity.
    assert_eq!(connection.receive(&input[accepted..]).unwrap(), 2 * A.len());
    connection
        .submit(first.id, Response::new(200, Vec::new()))
        .unwrap();
    // The wire queue holds the response; one in-flight slot is free again.
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::Request(_)
    ));
    assert!(matches!(
        connection.poll_request(),
        RequestEvent::Backpressure
    ));
    assert_eq!(connection.state(), ConnectionState::Open);
}

#[test]
fn output_overrun_is_rejected_without_changing_state() {
    let mut connection = connection();
    let mut issued = Vec::new();
    feed(&mut connection, A, &mut issued);
    connection
        .submit(issued[0].id, response_for(&issued[0]))
        .unwrap();
    let pending = connection.pending_output().len();
    assert_eq!(
        connection.advance_output(pending + 1),
        Err(ConnectionError::OutputOverrun {
            pending,
            advanced: pending + 1
        })
    );
    connection.advance_output(10).unwrap();
    assert_eq!(connection.pending_output(), &RA[10..]);
    assert_eq!(connection.pending_output_len(), RA.len() - 10);
    connection.advance_output(RA.len() - 10).unwrap();
    assert!(connection.pending_output().is_empty());
    assert_eq!(connection.advance_output(0), Ok(()));
}

#[test]
fn invalid_connection_limits_are_rejected() {
    let mut bad = limits();
    bad.max_in_flight = 0;
    assert!(matches!(
        Connection::new(bad),
        Err(ConnectionError::InvalidLimits)
    ));
    let mut bad = limits();
    bad.max_outbound_bytes = bad.response.max_total_bytes - 1;
    assert!(matches!(
        Connection::new(bad),
        Err(ConnectionError::InvalidLimits)
    ));
    let mut bad = limits();
    bad.request.max_buffered_bytes = 1;
    assert!(matches!(
        Connection::new(bad),
        Err(ConnectionError::Decoder(Error::InvalidLimits))
    ));
}
