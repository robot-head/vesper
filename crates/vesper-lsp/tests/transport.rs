use serde_json::json;
use std::io::{self, BufRead, Cursor, Read, Write};
use vesper_lsp::*;
fn limits() -> ProtocolLimits {
    ProtocolLimits::default()
}
fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    v.extend(body);
    v
}
struct Chunk<'a> {
    bytes: &'a [u8],
    at: usize,
    chunk: usize,
}
impl Read for Chunk<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.bytes.len() - self.at).min(self.chunk);
        out[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}
impl BufRead for Chunk<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Ok(&self.bytes[self.at..(self.at + self.chunk).min(self.bytes.len())])
    }
    fn consume(&mut self, n: usize) {
        self.at += n;
    }
}
#[test]
fn actual_frames_at_every_chunk_and_truncation() {
    let bodies = [
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","method":"exit"}"#,
        br#"{"jsonrpc":"2.0","id":1,"result":null}"#,
    ];
    let stream = bodies.iter().flat_map(|b| frame(b)).collect::<Vec<_>>();
    for chunk in 1..=stream.len() {
        let mut reader = Chunk {
            bytes: &stream,
            at: 0,
            chunk,
        };
        for body in bodies {
            assert_eq!(
                read_message(&mut reader, limits()).unwrap().unwrap(),
                parse_envelope(body, limits()).unwrap()
            );
        }
        assert!(read_message(&mut reader, limits()).unwrap().is_none());
    }
    let bytes = frame(bodies[0]);
    for end in 0..bytes.len() {
        let mut reader = Chunk {
            bytes: &bytes[..end],
            at: 0,
            chunk: 1,
        };
        let result = read_frame(&mut reader, limits());
        if end == 0 {
            assert!(result.unwrap().is_none())
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                ProtocolErrorKind::Truncated,
                "end {end}"
            );
        }
    }
}
#[test]
fn header_validation_and_encoding() {
    for (header, kind) in [
        (
            "Content-Length: 0\r\ncontent-length: 0\r\n\r\n",
            ProtocolErrorKind::DuplicateLength,
        ),
        (
            "Content-Length: 0\r\nContent-Length: 1\r\n\r\n",
            ProtocolErrorKind::DuplicateLength,
        ),
        (
            "Content-Length: -1\r\n\r\n",
            ProtocolErrorKind::InvalidLength,
        ),
        (
            "Content-Length: +1\r\n\r\n",
            ProtocolErrorKind::InvalidLength,
        ),
        (
            "Content-Length: 99999999999999999999999999999999\r\n\r\n",
            ProtocolErrorKind::BodyLimit,
        ),
        ("Content-Length: 0\n\n", ProtocolErrorKind::InvalidHeader),
        (
            " Content-Length: 0\r\n\r\n",
            ProtocolErrorKind::InvalidHeader,
        ),
        ("X: y\r\n\r\n", ProtocolErrorKind::MissingLength),
        (
            "Content-Length: 0\r\nContent-Type: application/vscode-jsonrpc; charset=latin1\r\n\r\n",
            ProtocolErrorKind::UnsupportedEncoding,
        ),
        (
            "Content-Length: 0\r\nContent-Encoding: gzip\r\n\r\n",
            ProtocolErrorKind::UnsupportedEncoding,
        ),
        (
            "Content-Length: 0\r\nContent-Type: application/json; charset=\"utf-8\r\n\r\n",
            ProtocolErrorKind::InvalidHeader,
        ),
    ] {
        assert_eq!(
            read_frame(&mut Cursor::new(header), limits())
                .unwrap_err()
                .kind,
            kind,
            "{header:?}"
        );
    }
    for charset in ["utf-8", "UTF8", "\"utf-8\""] {
        let bytes = format!(
            "cOnTeNt-LeNgTh: 0\r\nContent-Type: application/vscode-jsonrpc; charset={charset}\r\n\r\n"
        );
        assert_eq!(
            read_frame(&mut Cursor::new(bytes), limits())
                .unwrap()
                .unwrap(),
            b""
        );
    }
}
#[test]
fn header_caps_apply_before_unbounded_copy_or_scan() {
    let bytes = vec![b'a'; 1_000_000];
    let mut reader = Cursor::new(bytes);
    let l = ProtocolLimits {
        max_header_line_bytes: 16,
        ..limits()
    };
    assert_eq!(
        read_frame(&mut reader, l).unwrap_err().kind,
        ProtocolErrorKind::HeaderLimit
    );
    assert_eq!(reader.position(), 0);
    let mut reader = Chunk {
        bytes: b"X: y\r\nX: y\r\nX: y\r\n\r\n",
        at: 0,
        chunk: 1,
    };
    let l = ProtocolLimits {
        max_header_bytes: 10,
        ..limits()
    };
    assert_eq!(
        read_frame(&mut reader, l).unwrap_err().kind,
        ProtocolErrorKind::HeaderLimit
    );
    assert_eq!(reader.at, 10);
    let l = ProtocolLimits {
        max_headers: 1,
        ..limits()
    };
    assert_eq!(
        read_frame(&mut Cursor::new(b"X: y\r\nX: y\r\n\r\n"), l)
            .unwrap_err()
            .kind,
        ProtocolErrorKind::HeaderLimit
    );
    let body = br#"{"jsonrpc":"2.0","method":"x"}"#;
    let f = frame(body);
    let header = f.len() - body.len();
    let l = ProtocolLimits {
        max_header_bytes: header,
        max_header_line_bytes: header - 2,
        max_headers: 1,
        max_body_bytes: body.len(),
        ..limits()
    };
    assert!(read_message(&mut Cursor::new(&f), l).unwrap().is_some());
    assert_eq!(
        read_frame(
            &mut Cursor::new(f),
            ProtocolLimits {
                max_body_bytes: body.len() - 1,
                ..l
            }
        )
        .unwrap_err()
        .kind,
        ProtocolErrorKind::BodyLimit
    );
}
#[test]
fn strict_envelopes_and_decoded_duplicates_before_dom() {
    for body in [
        r#"{"jsonrpc":"2.0","method":"x","method":"y"}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":{"x":1,"\u0078":2}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":[{"a":0,"a":1}]}"#,
    ] {
        assert_eq!(
            parse_envelope(body.as_bytes(), limits()).unwrap_err().kind,
            ProtocolErrorKind::DuplicateJsonKey
        );
    }
    for body in [
        r#"[]"#,
        r#"{"jsonrpc":"1.0","method":"x"}"#,
        r#"{"jsonrpc":"2.0","method":"x","id":null}"#,
        r#"{"jsonrpc":"2.0","method":"x","id":1.0}"#,
        r#"{"jsonrpc":"2.0","method":"x","id":1e0}"#,
        r#"{"jsonrpc":"2.0","method":"x","id":2147483648}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":null}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":4}"#,
        r#"{"jsonrpc":"2.0","method":"x","result":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":0,"error":{}}"#,
        r#"{"jsonrpc":"2.0","result":0}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":1.1,"message":"bad"}}"#,
        r#"{"jsonrpc":"2.0","method":"x","extension":0}"#,
    ] {
        assert!(parse_envelope(body.as_bytes(), limits()).is_err(), "{body}");
    }
    for body in [
        r#"{"jsonrpc":"2.0","id":null,"result":null}"#,
        r#"{"jsonrpc":"2.0","id":-2147483648,"error":{"code":-32600,"message":"bad","data":null}}"#,
        r#"{"jsonrpc":"2.0","id":"abc","method":"x","params":[]}"#,
        r#"{"jsonrpc":"2.0","\u006dethod":"x"}"#,
    ] {
        let env = parse_envelope(body.as_bytes(), limits()).unwrap();
        assert_eq!(
            read_message(
                &mut Cursor::new(encode_frame(&env, limits()).unwrap()),
                limits()
            )
            .unwrap()
            .unwrap(),
            env
        );
    }
    let mut bad = br#"{"jsonrpc":"2.0","method":"x","params":["X"]}"#.to_vec();
    let i = bad.iter().position(|b| *b == b'X').unwrap();
    bad[i] = 0xff;
    assert_eq!(
        parse_envelope(&bad, limits()).unwrap_err().kind,
        ProtocolErrorKind::Utf8
    );
}
#[test]
fn input_budgets_and_malformed_unknown_subtrees() {
    let b = br#"{"jsonrpc":"2.0","method":"x","params":{"a":[0,1]}}"#;
    for l in [
        ProtocolLimits {
            max_json_nodes: 5,
            ..limits()
        },
        ProtocolLimits {
            max_json_depth: 1,
            ..limits()
        },
        ProtocolLimits {
            max_json_string_bytes: 2,
            ..limits()
        },
        ProtocolLimits {
            max_json_total_string_bytes: 4,
            ..limits()
        },
        ProtocolLimits {
            max_json_index_bytes: 1,
            ..limits()
        },
        ProtocolLimits {
            max_json_work: 1,
            ..limits()
        },
    ] {
        assert!(parse_envelope(b, l).is_err());
    }
    for b in [
        r#"{"jsonrpc":"2.0","method":"x","params":{"ignored":[1,]}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":["\uD800"]}"#,
    ] {
        assert!(parse_envelope(b.as_bytes(), limits()).is_err());
    }
    assert_eq!(
        parse_envelope(
            b,
            ProtocolLimits {
                max_method_bytes: 0,
                ..limits()
            }
        )
        .unwrap_err()
        .kind,
        ProtocolErrorKind::InvalidEnvelope
    );
    assert_eq!(
        parse_envelope(
            br#"{"jsonrpc":"2.0","method":"x","id":"xx"}"#,
            ProtocolLimits {
                max_id_bytes: 1,
                ..limits()
            }
        )
        .unwrap_err()
        .kind,
        ProtocolErrorKind::InvalidId
    );
    assert!(
        ProtocolLimits {
            max_body_bytes: usize::MAX,
            ..limits()
        }
        .validate()
        .is_err()
    );
}
#[test]
fn utf8_wire_length_output_bounds_and_transactionality() {
    let env = Envelope::Response {
        id: Some(Id::String("😀".into())),
        payload: ResponsePayload::Result(json!({"é":"☃😀"})),
    };
    let bytes = encode_frame(&env, limits()).unwrap();
    let split = bytes.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(
        String::from_utf8_lossy(&bytes[..split]),
        format!("Content-Length: {}\r\n\r\n", bytes.len() - split)
    );
    assert_eq!(
        read_message(&mut Cursor::new(&bytes), limits())
            .unwrap()
            .unwrap(),
        env
    );
    let mut writer = b"sentinel".to_vec();
    for l in [
        ProtocolLimits {
            max_reply_bytes: 1,
            ..limits()
        },
        ProtocolLimits {
            max_json_work: 1,
            ..limits()
        },
        ProtocolLimits {
            max_json_nodes: 1,
            ..limits()
        },
        ProtocolLimits {
            max_header_bytes: 1,
            ..limits()
        },
        ProtocolLimits {
            max_id_bytes: 1,
            ..limits()
        },
    ] {
        assert!(write_message(&mut writer, &env, l).is_err());
        assert_eq!(writer, b"sentinel");
    }
    let invalid = Envelope::Notification {
        method: "x".into(),
        params: Some(json!(null)),
    };
    assert!(write_message(&mut writer, &invalid, limits()).is_err());
    assert_eq!(writer, b"sentinel");
    let mut deep = json!(null);
    for _ in 0..100 {
        deep = ValueArray::wrap(deep);
    }
    let env = Envelope::Response {
        id: None,
        payload: ResponsePayload::Result(deep),
    };
    assert_eq!(
        encode_frame(&env, limits()).unwrap_err().kind,
        ProtocolErrorKind::JsonLimit
    );
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("broken"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        write_message(&mut Broken, &invalid, limits())
            .unwrap_err()
            .kind,
        ProtocolErrorKind::InvalidEnvelope
    );
    let valid = Envelope::Notification {
        method: "exit".into(),
        params: None,
    };
    assert_eq!(
        write_message(&mut Broken, &valid, limits())
            .unwrap_err()
            .kind,
        ProtocolErrorKind::Io
    );
}
struct ValueArray;
impl ValueArray {
    fn wrap(v: serde_json::Value) -> serde_json::Value {
        serde_json::Value::Array(vec![v])
    }
}
#[test]
fn output_node_count_exact_and_depth_edges() {
    let env = Envelope::Notification {
        method: "x".into(),
        params: Some(json!([0, 1])),
    };
    let l = ProtocolLimits {
        max_json_nodes: 9,
        max_json_depth: 2,
        ..limits()
    };
    let bytes = encode_frame(&env, l).unwrap();
    assert!(read_message(&mut Cursor::new(bytes), l).unwrap().is_some());
    assert!(
        encode_frame(
            &env,
            ProtocolLimits {
                max_json_nodes: 8,
                ..l
            }
        )
        .is_err()
    );
    assert!(
        encode_frame(
            &env,
            ProtocolLimits {
                max_json_depth: 1,
                ..l
            }
        )
        .is_err()
    );
}

#[test]
fn body_boundary_io_errors_and_json_storms() {
    let mut stream = frame(b"{}");
    stream.extend_from_slice(b"next");
    let mut reader = Cursor::new(stream);
    assert_eq!(read_frame(&mut reader, limits()).unwrap().unwrap(), b"{}");
    assert_eq!(reader.position(), 23); // independent literal header/body byte count
    struct Failed;
    impl Read for Failed {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }
    impl BufRead for Failed {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Err(io::Error::other("fill failed"))
        }
        fn consume(&mut self, _: usize) {}
    }
    assert_eq!(
        read_frame(&mut Failed, limits()).unwrap_err().kind,
        ProtocolErrorKind::Io
    );
    let mut body = String::from("{\"jsonrpc\":\"2.0\",\"method\":\"x\",\"params\":[");
    body.push_str(&vec!["0"; 1000].join(","));
    body.push_str("]}");
    assert_eq!(
        parse_envelope(
            body.as_bytes(),
            ProtocolLimits {
                max_json_nodes: 64,
                ..limits()
            }
        )
        .unwrap_err()
        .kind,
        ProtocolErrorKind::JsonLimit
    );
    let bad = b"Content-Length: 0\r\nX: \xff\r\n\r\n";
    assert_eq!(
        read_frame(&mut Cursor::new(bad), limits())
            .unwrap_err()
            .kind,
        ProtocolErrorKind::InvalidHeader
    );
    assert_eq!(
        read_message(&mut Cursor::new(frame(b"")), limits())
            .unwrap_err()
            .kind,
        ProtocolErrorKind::Json
    );
}

#[test]
fn sixty_four_container_levels_are_real_and_exact() {
    let mut value = serde_json::Value::Null;
    for _ in 0..63 {
        value = serde_json::Value::Array(vec![value]);
    }
    let env = Envelope::Response {
        id: None,
        payload: ResponsePayload::Result(value),
    };
    let frame = encode_frame(&env, limits()).unwrap();
    assert_eq!(
        read_message(&mut Cursor::new(frame), limits())
            .unwrap()
            .unwrap(),
        env
    );
    assert_eq!(
        encode_frame(
            &env,
            ProtocolLimits {
                max_json_depth: 63,
                ..limits()
            }
        )
        .unwrap_err()
        .kind,
        ProtocolErrorKind::JsonLimit
    );
    let body = br#"{"jsonrpc":"2.0","method":"\u00e9"}"#;
    assert!(
        parse_envelope(
            body,
            ProtocolLimits {
                max_method_bytes: 2,
                ..limits()
            }
        )
        .is_ok()
    );
    assert!(
        parse_envelope(
            body,
            ProtocolLimits {
                max_method_bytes: 1,
                ..limits()
            }
        )
        .is_err()
    );
    assert!(parse_envelope(br#"{"jsonrpc":"2.0","method":"x","id":true}"#, limits()).is_err());
}
