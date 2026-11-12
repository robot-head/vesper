use vesper_prism::{Decoder, Error, Limits, Request};

fn limits() -> Limits {
    Limits {
        max_head_bytes: 512,
        max_headers: 16,
        max_body_bytes: 256,
        max_buffered_bytes: 2048,
    }
}
fn decoder() -> Decoder {
    Decoder::new(limits()).unwrap()
}
fn parse(bytes: &[u8]) -> Result<Request, Error> {
    let mut decoder = decoder();
    decoder.push(bytes)?;
    decoder.end_input();
    Ok(decoder.next_request()?.expect("complete request").request)
}
fn request(extra: &[u8]) -> Vec<u8> {
    let mut bytes = b"POST /events HTTP/1.1\r\nHost: example.test\r\n".to_vec();
    bytes.extend_from_slice(extra);
    bytes.extend_from_slice(b"\r\n");
    bytes
}
fn queued(decoder: &Decoder) -> Vec<u8> {
    let (front, back) = decoder.buffered_slices();
    [front, back].concat()
}

#[test]
fn complete_request_has_exact_head_bytes_and_binary_body() {
    let body = (0..=255).collect::<Vec<u8>>();
    let mut bytes = request(b"Content-Length: 256\r\nX-Bytes: \t\x80\xff a\tb \t\r\n");
    let head_len = bytes.len();
    bytes.extend_from_slice(&body);
    let mut decoder = decoder();
    decoder.push(&bytes).unwrap();
    let decoded = decoder.next_request().unwrap().unwrap();
    assert_eq!(decoded.consumed, head_len + body.len());
    assert_eq!(decoded.request.head.method, "POST");
    assert_eq!(decoded.request.head.target, "/events");
    assert_eq!(decoded.request.head.content_length, 256);
    assert_eq!(
        decoded.request.head.headers("x-BYTES").collect::<Vec<_>>(),
        vec![b"\x80\xff a\tb".as_slice()]
    );
    assert_eq!(decoded.request.body, body);
    assert_eq!(decoder.buffered_bytes(), 0);
    assert!(decoder.next_request().unwrap().is_none());
}

#[test]
fn every_two_chunk_split_and_every_single_byte_feed_preserve_framing() {
    let mut bytes = request(b"Content-Length: 8\r\nX-Value: text\r\n");
    bytes.extend_from_slice(b"a\0\r\n\r\n\xffz");
    let expected = parse(&bytes).unwrap();
    for split in 0..=bytes.len() {
        let mut decoder = decoder();
        decoder.push(&bytes[..split]).unwrap();
        let first = decoder.next_request().unwrap();
        if split == bytes.len() {
            assert_eq!(first.unwrap().request, expected);
        } else {
            assert!(first.is_none(), "split {split}");
            decoder.push(&bytes[split..]).unwrap();
            assert_eq!(decoder.next_request().unwrap().unwrap().request, expected);
        }
        assert!(decoder.next_request().unwrap().is_none());
    }
    let mut decoder = decoder();
    for (index, byte) in bytes.iter().enumerate() {
        decoder.push(&[*byte]).unwrap();
        let result = decoder.next_request().unwrap();
        if index + 1 == bytes.len() {
            assert_eq!(result.unwrap().request, expected);
        } else {
            assert!(result.is_none());
        }
    }
}

#[test]
fn pipeline_remainder_is_exact_and_body_bytes_cannot_be_requests() {
    let fake = b"GET /in-body HTTP/1.1\r\nHost: fake.test\r\n\r\n";
    let mut first = request(format!("Content-Length: {}\r\n", fake.len()).as_bytes());
    first.extend_from_slice(fake);
    let second = b"GET /second HTTP/1.1\r\nHost: second.test\r\n\r\n";
    let third = b"GET /unfinished HTTP/1.1\r\n";
    let mut decoder = decoder();
    decoder
        .push(&[first.as_slice(), second, third].concat())
        .unwrap();
    let decoded = decoder.next_request().unwrap().unwrap();
    assert_eq!(decoded.request.body, fake);
    assert_eq!(decoded.consumed, first.len());
    assert_eq!(queued(&decoder), [second.as_slice(), third].concat());
    let decoded = decoder.next_request().unwrap().unwrap();
    assert_eq!(decoded.request.head.target, "/second");
    assert_eq!(decoded.consumed, second.len());
    assert_eq!(queued(&decoder), third);
    assert!(decoder.next_request().unwrap().is_none());
    assert_eq!(queued(&decoder), third);
}

#[test]
fn ring_wrap_and_sustained_pipeline_do_not_corrupt_remainder() {
    let bytes = b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n";
    let mut decoder = Decoder::new(Limits {
        max_head_bytes: 64,
        max_headers: 2,
        max_body_bytes: 0,
        max_buffered_bytes: 128,
    })
    .unwrap();
    decoder.push(&[bytes.as_slice(); 3].concat()).unwrap();
    for _ in 0..1000 {
        let decoded = decoder.next_request().unwrap().unwrap();
        assert_eq!(decoded.consumed, bytes.len());
        assert_eq!(decoded.request.head.target, "/");
        decoder.push(bytes).unwrap();
        assert_eq!(queued(&decoder), [bytes.as_slice(); 3].concat());
    }
    decoder.end_input();
    for _ in 0..3 {
        assert!(decoder.next_request().unwrap().is_some());
    }
    assert!(decoder.next_request().unwrap().is_none());
}

#[test]
fn content_length_forms_and_duplicates_fail_closed() {
    for value in [
        "",
        "+1",
        "-1",
        "1,1",
        "1, 1",
        "0x1",
        "1 0",
        "1\t0",
        "18446744073709551616000000",
    ] {
        assert_eq!(
            parse(&request(format!("Content-Length: {value}\r\n").as_bytes())),
            Err(Error::InvalidContentLength),
            "{value:?}"
        );
    }
    for extra in [
        b"Content-Length: 0\r\nContent-Length: 0\r\n".as_slice(),
        b"CONTENT-LENGTH: 0\r\ncOnTeNt-LeNgTh: 1\r\n",
    ] {
        assert_eq!(parse(&request(extra)), Err(Error::DuplicateContentLength));
    }
    assert_eq!(
        parse(&request(b"Content-Length: \t000\t \r\n"))
            .unwrap()
            .head
            .content_length,
        0
    );
}

#[test]
fn transfer_codings_upgrade_connect_and_expectations_are_explicitly_unsupported() {
    for extra in [
        b"Transfer-Encoding: chunked\r\n".as_slice(),
        b"Transfer-Encoding: identity\r\n",
        b"Transfer-Encoding: gzip, chunked\r\n",
        b"Transfer-Encoding:\r\n",
    ] {
        assert_eq!(
            parse(&request(extra)),
            Err(Error::UnsupportedTransferCoding)
        );
    }
    for extra in [
        b"Content-Length: 0\r\nTransfer-Encoding: chunked\r\n".as_slice(),
        b"Transfer-Encoding: chunked\r\nContent-Length: 0\r\n",
    ] {
        assert_eq!(parse(&request(extra)), Err(Error::AmbiguousFraming));
    }
    for extra in [
        b"Upgrade: websocket\r\n".as_slice(),
        b"Connection: keep-alive, UpGrAdE\r\n",
    ] {
        assert_eq!(parse(&request(extra)), Err(Error::UnsupportedUpgrade));
    }
    assert_eq!(
        parse(&request(b"Expect: 100-continue\r\n")),
        Err(Error::UnsupportedExpectation)
    );
    for method in ["CONNECT", "connect"] {
        assert_eq!(
            parse(
                format!("{method} example.test:443 HTTP/1.1\r\nHost: example.test\r\n\r\n")
                    .as_bytes()
            ),
            Err(Error::UnsupportedMethod)
        );
    }
}

#[test]
fn host_must_be_unique_and_unambiguous_with_valid_port_and_address() {
    assert_eq!(parse(b"GET / HTTP/1.1\r\n\r\n"), Err(Error::MissingHost));
    assert_eq!(
        parse(&request(b"HOST: example.test\r\n")),
        Err(Error::DuplicateHost)
    );
    for host in [
        "",
        "a,b",
        "a b",
        "a\tb",
        "user@host",
        "a:65536",
        "a:-1",
        "a:",
        "a::80",
        "a..b",
        "-a.test",
        "a-.test",
        ".",
        "[::gg]",
        "[::1",
        "::1",
        "[::1]x",
        "[::1]:",
        "a%2eb",
        "127.1",
        "2130706433",
        "127.0.0.01",
        "127.0.0.1.",
        "0x7f000001",
        "0X7F000001",
        "0x7f000001.",
        "0x7f.0.0.1",
        "0177.0.0.0x1",
    ] {
        assert_eq!(
            parse(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()),
            Err(Error::InvalidHost),
            "{host:?}"
        );
    }
    for host in [
        "localhost",
        "EXAMPLE.test",
        "0xcorp.example",
        "a.test.",
        "127.0.0.1:80",
        "[::1]",
        "[2001:db8::1]:443",
    ] {
        assert!(
            parse(format!("GET / HTTP/1.1\r\nhOsT: {host}\r\n\r\n").as_bytes()).is_ok(),
            "{host}"
        );
    }
}

#[test]
fn numeric_host_aliases_poison_every_incremental_split() {
    for host in ["0x7f000001", "0X7F000001", "0x7f.0.0.1", "0177.0.0.0x1"] {
        let bytes = format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n");
        for split in 0..=bytes.len() {
            let mut decoder = decoder();
            decoder.push(&bytes.as_bytes()[..split]).unwrap();
            let first = decoder.next_request();
            if split == bytes.len() {
                assert_eq!(first, Err(Error::InvalidHost));
            } else {
                assert_eq!(first, Ok(None));
                decoder.push(&bytes.as_bytes()[split..]).unwrap();
                assert_eq!(decoder.next_request(), Err(Error::InvalidHost));
            }
            assert_eq!(decoder.push(&request(b"")), Err(Error::Poisoned));
            assert_eq!(decoder.next_request(), Err(Error::Poisoned));
        }
    }
}

#[test]
fn whitespace_folding_controls_and_line_ending_smuggling_are_rejected() {
    for extra in [
        b"Content-Length : 0\r\n".as_slice(),
        b" Host: evil.test\r\n",
        b"X: ok\r\n\tcontinued\r\n",
        b"Bad(Name): value\r\n",
        b": empty\r\n",
        b"X: \0\r\n",
        b"X: \x7f\r\n",
        b"X: \x0b\r\n",
        b"X: \x0c\r\n",
    ] {
        assert_eq!(parse(&request(extra)), Err(Error::InvalidHeader));
    }
    for bytes in [
        b"GET / HTTP/1.1\nHost: a.test\n\n".as_slice(),
        b"GET / HTTP/1.1\rHost: a.test\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: a.test\n\r\n",
        b"GET / HTTP/1.1\r\nX: a\rb\r\nHost: a.test\r\n\r\n",
    ] {
        assert_eq!(parse(bytes), Err(Error::InvalidLineEnding));
    }
    // Fatal errors cannot be followed by a resynchronization to a queued request.
    let mut decoder = decoder();
    decoder
        .push(&[b"GET / HTTP/1.1\n".as_slice(), &request(b"")].concat())
        .unwrap();
    assert_eq!(decoder.next_request(), Err(Error::InvalidLineEnding));
    assert_eq!(decoder.next_request(), Err(Error::Poisoned));
    assert_eq!(decoder.push(b"anything"), Err(Error::Poisoned));
}

#[test]
fn request_line_and_target_grammar_are_strict_without_normalization() {
    for line in [
        "GET  / HTTP/1.1",
        " GET / HTTP/1.1",
        "GET / HTTP/1.1 ",
        "GE(T / HTTP/1.1",
        "GET\t/ HTTP/1.1",
        "GET /",
        "",
    ] {
        assert_eq!(
            parse(format!("{line}\r\nHost: a.test\r\n\r\n").as_bytes()),
            Err(Error::InvalidRequestLine),
            "{line:?}"
        );
    }
    for version in ["HTTP/1.0", "HTTP/2.0", "http/1.1", "HTTP/1.10"] {
        assert_eq!(
            parse(format!("GET / {version}\r\nHost: a.test\r\n\r\n").as_bytes()),
            Err(Error::UnsupportedVersion)
        );
    }
    for target in [
        "http://a.test/x",
        "//a.test/x",
        "*",
        "relative",
        "/#fragment",
        "/bad%",
        "/bad%0g",
        "/back\\slash",
        "/raw[bracket]",
        "/\x7f",
    ] {
        assert_eq!(
            parse(format!("GET {target} HTTP/1.1\r\nHost: a.test\r\n\r\n").as_bytes()),
            Err(Error::InvalidTarget),
            "{target:?}"
        );
    }
    let options = parse(b"OPTIONS * HTTP/1.1\r\nHost: a.test\r\n\r\n").unwrap();
    assert_eq!(options.head.target, "*");
    let exact = parse(b"get /a%2fb?q=%00&x=1 HTTP/1.1\r\nHost: a.test\r\n\r\n").unwrap();
    assert_eq!(exact.head.method, "get");
    assert_eq!(exact.head.target, "/a%2fb?q=%00&x=1");
}

#[test]
fn connection_options_cannot_hide_framing_fields() {
    for value in [
        "",
        ",",
        "close,",
        "bad token",
        "Host",
        "content-length",
        "TRANSFER-ENCODING",
    ] {
        assert_eq!(
            parse(&request(format!("Connection: {value}\r\n").as_bytes())),
            Err(Error::InvalidConnection)
        );
    }
    assert!(parse(&request(b"Connection: keep-alive, close\r\n")).is_ok());
}

#[test]
fn exact_limits_accept_and_overruns_poison_without_retaining_excess() {
    let bytes = b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n";
    let mut exact = Decoder::new(Limits {
        max_head_bytes: bytes.len(),
        max_headers: 1,
        max_body_bytes: 0,
        max_buffered_bytes: bytes.len(),
    })
    .unwrap();
    exact.push(bytes).unwrap();
    assert!(exact.next_request().unwrap().is_some());
    let mut short = Decoder::new(Limits {
        max_head_bytes: bytes.len() - 1,
        max_headers: 1,
        max_body_bytes: 0,
        max_buffered_bytes: bytes.len(),
    })
    .unwrap();
    short.push(bytes).unwrap();
    assert_eq!(short.next_request(), Err(Error::HeadLimit));
    let mut count = Decoder::new(Limits {
        max_headers: 1,
        ..limits()
    })
    .unwrap();
    count.push(&request(b"X: 1\r\n")).unwrap();
    assert_eq!(count.next_request(), Err(Error::HeaderCountLimit));
    let mut body = Decoder::new(Limits {
        max_body_bytes: 2,
        ..limits()
    })
    .unwrap();
    body.push(&request(b"Content-Length: 3\r\n")).unwrap();
    assert_eq!(body.next_request(), Err(Error::BodyLimit));
    let mut buffered = Decoder::new(Limits {
        max_head_bytes: 32,
        max_headers: 1,
        max_body_bytes: 0,
        max_buffered_bytes: 64,
    })
    .unwrap();
    buffered.push(&[b'x'; 64]).unwrap();
    assert_eq!(buffered.push(b"y"), Err(Error::BufferedLimit));
    assert_eq!(buffered.buffered_bytes(), 64);
    assert_eq!(buffered.push(b""), Err(Error::Poisoned));
}

#[test]
fn eof_distinguishes_idle_complete_pipeline_and_truncated_head_or_body() {
    let bytes = request(b"Content-Length: 2\r\n");
    for partial in [
        &bytes[..bytes.len() - 1],
        bytes.as_slice(),
        &[bytes.as_slice(), b"a"].concat(),
    ] {
        let mut decoder = decoder();
        decoder.push(partial).unwrap();
        assert!(decoder.next_request().unwrap().is_none());
        decoder.end_input();
        assert_eq!(decoder.next_request(), Err(Error::UnexpectedEof));
        assert_eq!(decoder.next_request(), Err(Error::Poisoned));
    }
    let mut decoder = decoder();
    decoder
        .push(&[request(b"").as_slice(); 2].concat())
        .unwrap();
    decoder.end_input();
    assert!(decoder.next_request().unwrap().is_some());
    assert!(decoder.next_request().unwrap().is_some());
    assert!(decoder.next_request().unwrap().is_none());
    assert_eq!(decoder.push(b""), Err(Error::InputEnded));
    let mut idle = self::decoder();
    idle.end_input();
    assert!(idle.next_request().unwrap().is_none());
}

#[test]
fn invalid_limits_are_rejected_before_reserving_input_storage() {
    for limits in [
        Limits {
            max_head_bytes: 3,
            ..limits()
        },
        Limits {
            max_headers: 0,
            ..limits()
        },
        Limits {
            max_buffered_bytes: 1,
            ..limits()
        },
        Limits {
            max_head_bytes: usize::MAX,
            max_body_bytes: 1,
            max_buffered_bytes: usize::MAX,
            ..limits()
        },
    ] {
        assert!(matches!(Decoder::new(limits), Err(Error::InvalidLimits)));
    }
}

#[test]
fn malformed_line_endings_fail_at_every_transport_split() {
    for bytes in [
        b"GET / HTTP/1.1\rXHost: a.test\r\n\r\n".as_slice(),
        b"GET / HTTP/1.1\r\nHost: a.test\r\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: a.test\n\r\n",
    ] {
        for split in 0..=bytes.len() {
            let mut decoder = decoder();
            decoder.push(&bytes[..split]).unwrap();
            match decoder.next_request() {
                Err(error) => assert_eq!(error, Error::InvalidLineEnding),
                Ok(None) => {
                    decoder.push(&bytes[split..]).unwrap();
                    assert_eq!(decoder.next_request(), Err(Error::InvalidLineEnding));
                }
                Ok(Some(_)) => panic!("malformed head was accepted"),
            }
        }
    }
}
