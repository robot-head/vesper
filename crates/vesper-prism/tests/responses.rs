use vesper_prism::response::{
    ConnectionPolicy, ResponseBuilder, ResponseContext, ResponseError, ResponseLimits,
};

fn plan(status: u16, context: ResponseContext, length: usize) -> ResponseBuilder {
    ResponseBuilder::new(
        status,
        context,
        ConnectionPolicy::Close,
        length,
        ResponseLimits::default(),
    )
    .unwrap()
}
fn encode(status: u16, context: ResponseContext, body: &[u8]) -> Vec<u8> {
    plan(status, context, body.len())
        .finish(body)
        .unwrap()
        .into_bytes()
}

#[test]
fn exact_binary_response_and_repeated_fields_are_not_combined() {
    let response = plan(200, ResponseContext::Ordinary, 4)
        .header("Content-Type", b"application/octet-stream")
        .unwrap()
        .header("Set-Cookie", b"a=1; Path=/")
        .unwrap()
        .header("set-COOKIE", b"b=2; HttpOnly")
        .unwrap()
        .header("X-Opaque", &[128, 255])
        .unwrap()
        .finish(&[0, 128, 255, 10])
        .unwrap();
    assert_eq!(response.connection_policy(), ConnectionPolicy::Close);
    assert!(!response.is_empty());
    let length = response.len();
    let bytes = response.into_bytes();
    assert_eq!(length, bytes.len());
    assert_eq!(bytes, [
        b"HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\nset-cookie: a=1; Path=/\r\nset-cookie: b=2; HttpOnly\r\nx-opaque: ".as_slice(),
        &[128, 255], b"\r\nContent-Length: 4\r\nConnection: close\r\n\r\n", &[0, 128, 255, 10],
    ].concat());
}

#[test]
fn head_emits_verified_representation_length_without_payload() {
    let body = b"abc\0\xff";
    let ordinary = encode(200, ResponseContext::Ordinary, body);
    let head = encode(200, ResponseContext::Head, body);
    assert_eq!(ordinary, [head.as_slice(), body].concat());
    assert_eq!(
        head,
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n"
    );
    assert_eq!(
        plan(200, ResponseContext::Head, 5).finish(b"").unwrap_err(),
        ResponseError::LengthMismatch {
            expected: 5,
            actual: 0
        }
    );
}

#[test]
fn body_forbidden_statuses_omit_length_and_never_accept_a_payload() {
    for (status, reason) in [(204, "No Content"), (304, "Not Modified")] {
        for context in [ResponseContext::Ordinary, ResponseContext::Head] {
            assert_eq!(
                encode(status, context, b""),
                format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n\r\n").as_bytes()
            );
            assert_eq!(
                ResponseBuilder::new(
                    status,
                    context,
                    ConnectionPolicy::Close,
                    1,
                    ResponseLimits::default()
                )
                .unwrap_err(),
                ResponseError::BodyForbidden
            );
            assert_eq!(
                plan(status, context, 0).finish(b"x").unwrap_err(),
                ResponseError::BodyForbidden
            );
        }
    }
    assert_eq!(
        encode(205, ResponseContext::Ordinary, b""),
        b"HTTP/1.1 205 Reset Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert_eq!(
        ResponseBuilder::new(
            205,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            1,
            ResponseLimits::default()
        )
        .unwrap_err(),
        ResponseError::BodyForbidden
    );
}

#[test]
fn status_is_numeric_final_and_reason_is_never_caller_text() {
    for status in [0, 99, 100, 101, 199, 600, 1000, u16::MAX] {
        assert_eq!(
            ResponseBuilder::new(
                status,
                ResponseContext::Ordinary,
                ConnectionPolicy::Close,
                0,
                ResponseLimits::default()
            )
            .unwrap_err(),
            ResponseError::UnsupportedStatus
        );
    }
    assert_eq!(
        ResponseBuilder::new(
            426,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            0,
            ResponseLimits::default()
        )
        .unwrap_err(),
        ResponseError::UnsupportedUpgrade
    );
    assert_eq!(
        encode(404, ResponseContext::Ordinary, b""),
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert_eq!(
        encode(299, ResponseContext::Ordinary, b""),
        b"HTTP/1.1 299 \r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert!(encode(599, ResponseContext::Ordinary, b"").starts_with(b"HTTP/1.1 599 \r\n"));
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Connect,
            ConnectionPolicy::Close,
            0,
            ResponseLimits::default()
        )
        .unwrap_err(),
        ResponseError::UnsupportedConnect
    );
}

#[test]
fn encoder_exclusively_owns_framing_and_connection_fields() {
    for name in [
        "Content-Length",
        "content-LENGTH",
        "Connection",
        "CONNECTION",
        "Keep-Alive",
        "Proxy-Connection",
    ] {
        assert_eq!(
            plan(200, ResponseContext::Ordinary, 0)
                .header(name, b"0")
                .unwrap_err(),
            ResponseError::ReservedHeader,
            "{name}"
        );
    }
    for name in ["Transfer-Encoding", "TRANSFER-encoding", "Trailer", "TE"] {
        assert_eq!(
            plan(200, ResponseContext::Ordinary, 0)
                .header(name, b"chunked")
                .unwrap_err(),
            ResponseError::UnsupportedTransferCoding,
            "{name}"
        );
    }
    assert_eq!(
        plan(200, ResponseContext::Ordinary, 0)
            .header("UpGrAdE", b"websocket")
            .unwrap_err(),
        ResponseError::UnsupportedUpgrade
    );
}

#[test]
fn all_header_injection_and_control_forms_are_rejected() {
    for name in [
        "",
        "Bad Name",
        "Name:",
        "Name\r\nX",
        "\tName",
        "Náme",
        "Bad(Name)",
    ] {
        assert_eq!(
            plan(200, ResponseContext::Ordinary, 0)
                .header(name, b"value")
                .unwrap_err(),
            ResponseError::InvalidHeaderName,
            "{name:?}"
        );
    }
    for value in [
        b"a\r\nContent-Length: 0".as_slice(),
        b"a\nX: b",
        b"\0",
        b"\t",
        b"\x0b",
        b"\x0c",
        b"\x1f",
        b"\x7f",
    ] {
        assert_eq!(
            plan(200, ResponseContext::Ordinary, 0)
                .header("X-Test", value)
                .unwrap_err(),
            ResponseError::InvalidHeaderValue,
            "{value:?}"
        );
    }
    let bytes = plan(200, ResponseContext::Ordinary, 0)
        .header("!#$%&'*+-.^_`|~", b" leading and trailing ")
        .unwrap()
        .finish(b"")
        .unwrap()
        .into_bytes();
    assert!(
        bytes
            .windows(b":  leading and trailing \r\n".len())
            .any(|window| window == b":  leading and trailing \r\n")
    );
}

#[test]
fn exact_representation_length_is_required_before_any_output() {
    for context in [ResponseContext::Ordinary, ResponseContext::Head] {
        assert_eq!(
            plan(200, context, 3).finish(b"ab").unwrap_err(),
            ResponseError::LengthMismatch {
                expected: 3,
                actual: 2
            }
        );
        assert_eq!(
            plan(200, context, 3).finish(b"abcd").unwrap_err(),
            ResponseError::LengthMismatch {
                expected: 3,
                actual: 4
            }
        );
        assert!(plan(200, context, 3).finish(b"abc").is_ok());
    }
}

#[test]
fn close_and_keep_alive_are_explicit_and_deterministic() {
    let create = || {
        ResponseBuilder::new(
            201,
            ResponseContext::Ordinary,
            ConnectionPolicy::KeepAlive,
            0,
            ResponseLimits::default(),
        )
        .unwrap()
        .finish(b"")
        .unwrap()
    };
    let response = create();
    assert_eq!(response.connection_policy(), ConnectionPolicy::KeepAlive);
    assert_eq!(
        response.into_bytes(),
        b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
    );
    assert_eq!(create().into_bytes(), create().into_bytes());
}

#[test]
fn independent_head_body_total_and_header_count_limits_are_exact() {
    let baseline = encode(200, ResponseContext::Ordinary, b"abc");
    let head_length = baseline.len() - 3;
    let exact = ResponseLimits {
        max_head_bytes: head_length,
        max_headers: 2,
        max_body_bytes: 3,
        max_total_bytes: baseline.len(),
    };
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            3,
            exact
        )
        .unwrap()
        .finish(b"abc")
        .unwrap()
        .into_bytes(),
        baseline
    );
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            3,
            ResponseLimits {
                max_head_bytes: head_length - 1,
                ..exact
            }
        )
        .unwrap_err(),
        ResponseError::HeadLimit
    );
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            3,
            ResponseLimits {
                max_headers: 1,
                ..exact
            }
        )
        .unwrap_err(),
        ResponseError::HeaderCountLimit
    );
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            3,
            ResponseLimits {
                max_body_bytes: 2,
                ..exact
            }
        )
        .unwrap_err(),
        ResponseError::BodyLimit
    );
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            3,
            ResponseLimits {
                max_total_bytes: baseline.len() - 1,
                ..exact
            }
        )
        .unwrap_err(),
        ResponseError::TotalLimit
    );
    // A HEAD body is still bounded independently, but does not count as wire bytes.
    let head_limits = ResponseLimits {
        max_total_bytes: head_length,
        ..exact
    };
    assert!(
        ResponseBuilder::new(
            200,
            ResponseContext::Head,
            ConnectionPolicy::Close,
            3,
            head_limits
        )
        .unwrap()
        .finish(b"abc")
        .is_ok()
    );
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Head,
            ConnectionPolicy::Close,
            4,
            head_limits
        )
        .unwrap_err(),
        ResponseError::BodyLimit
    );
    // 204/304 have one generated header rather than two.
    assert!(
        ResponseBuilder::new(
            204,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            0,
            ResponseLimits {
                max_headers: 1,
                ..ResponseLimits::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn added_fields_are_size_checked_before_retaining_copies() {
    let create = |limits| {
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            0,
            limits,
        )
        .unwrap()
    };
    let with_field = create(ResponseLimits::default())
        .header("X", b"ab")
        .unwrap()
        .finish(b"")
        .unwrap()
        .into_bytes();
    let exact = ResponseLimits {
        max_head_bytes: with_field.len(),
        max_headers: 3,
        max_body_bytes: 0,
        max_total_bytes: with_field.len(),
    };
    assert_eq!(
        create(exact)
            .header("X", b"ab")
            .unwrap()
            .finish(b"")
            .unwrap()
            .into_bytes(),
        with_field
    );
    assert_eq!(
        create(ResponseLimits {
            max_head_bytes: with_field.len() - 1,
            ..exact
        })
        .header("X", b"ab")
        .unwrap_err(),
        ResponseError::HeadLimit
    );
    assert_eq!(
        create(ResponseLimits {
            max_headers: 2,
            ..exact
        })
        .header("X", b"ab")
        .unwrap_err(),
        ResponseError::HeaderCountLimit
    );
    assert_eq!(
        create(ResponseLimits {
            max_total_bytes: with_field.len() - 1,
            ..exact
        })
        .header("X", b"ab")
        .unwrap_err(),
        ResponseError::TotalLimit
    );
}

#[test]
fn arithmetic_overflow_is_rejected_without_body_allocation() {
    let unbounded = ResponseLimits {
        max_head_bytes: usize::MAX,
        max_headers: usize::MAX,
        max_body_bytes: usize::MAX,
        max_total_bytes: usize::MAX,
    };
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Ordinary,
            ConnectionPolicy::Close,
            usize::MAX,
            unbounded
        )
        .unwrap_err(),
        ResponseError::LengthOverflow
    );
    // HEAD can represent the number without allocating a body, but finish still
    // requires the actual representation rather than trusting a forged length.
    assert_eq!(
        ResponseBuilder::new(
            200,
            ResponseContext::Head,
            ConnectionPolicy::Close,
            usize::MAX,
            unbounded
        )
        .unwrap()
        .finish(b"")
        .unwrap_err(),
        ResponseError::LengthMismatch {
            expected: usize::MAX,
            actual: 0
        }
    );
}
