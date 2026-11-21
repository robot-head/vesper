//! Self-hosting SH5 lane 16 (#1339): `@vesper/lsp` (packages/vesper-lsp)
//! against crates/vesper-lsp, and `@vesper/memo` (packages/vesper-memo)
//! against the salsa database of src/database.rs.
//!
//! - `frames_match_rust`: `read_frame` over every recorded session's client
//!   and server streams, the framing cases of crates/vesper-lsp's tests and
//!   every truncation and limit edge of them.
//!
//! - `envelopes_match_rust`: `parse_envelope` on every body of every
//!   recorded session (both directions) and the envelope cases of
//!   crates/vesper-lsp's tests, under their limits.
//! - `responses_match_byte_for_byte`: every envelope the server wrote,
//!   encoded by `@vesper/lsp`, is the server's frame byte for byte; the
//!   encoder cases of crates/vesper-lsp's tests give the same frame or error.
//!
//! - `documents_match_rust`: `DocumentStore` against
//!   `@vesper/lsp-documents` (packages/vesper-lsp/documents) on every
//!   didOpen/didChange/didClose of the recorded sessions and the cases of
//!   crates/vesper-lsp/tests/documents.rs: every result, count, total,
//!   revision, version and text after every op.
//!
//! - `memo_invalidation_matches_salsa`: the salsa events (executions and
//!   validated memos, in order) of src/database.rs's queries on every
//!   recorded session's edits and on `memo_scripts`, against `memo_replay`.
//!
//! - `stdio_loop_matches_server`: `lsp_serve`, the transport half of the
//!   stdio loop (src/lsp.rs `dispatch`), against the same decisions made
//!   with crates/vesper-lsp's functions on every session and framing case;
//!   each refusal it writes is a frame the live server wrote.
//!
//! The recorded sessions are the stdio sessions of tests/lsp_cli.rs, run
//! through the actual `vesper lsp` server (`sessions`).

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use serde_json::{Value, json};
use std::sync::OnceLock;
use vesper_lsp::{
    ContentChange, DocumentStore, Envelope, Id, Position, ProtocolErrorKind, ProtocolLimits, Range,
    ResponsePayload, StoreError, StoreLimits,
};

fn documents() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-lsp/documents"))
}

fn memo() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-memo"))
}

fn lsp() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-lsp"))
}

fn optimize() -> bool {
    std::env::var_os("LSP_CORE_NO_OPT").is_none()
}

fn push_word(out: &mut Vec<u8>, w: u64) {
    out.extend_from_slice(&w.to_le_bytes());
}

/// `ProtocolLimits` as the thirteen words of `read_limits` (limits.ves).
fn limit_words(out: &mut Vec<u8>, l: ProtocolLimits) {
    for w in [
        l.max_body_bytes,
        l.max_reply_bytes,
        l.max_header_bytes,
        l.max_header_line_bytes,
        l.max_headers,
        l.max_json_depth,
        l.max_json_nodes,
        l.max_json_string_bytes,
        l.max_json_total_string_bytes,
        l.max_json_index_bytes,
        l.max_json_work,
        l.max_id_bytes,
        l.max_method_bytes,
    ] {
        push_word(out, w as u64);
    }
}

fn kind_index(k: ProtocolErrorKind) -> u64 {
    k as u64
}

// ------------------------------------------------------------------ frames

/// Rust's frames of `stream`, in the words of `lsp_frames` (lib.ves).
fn rust_frames(stream: &[u8], l: ProtocolLimits) -> Vec<u64> {
    let mut cursor = std::io::Cursor::new(stream);
    let mut out = vec![];
    loop {
        let before = cursor.position();
        match vesper_lsp::read_frame(&mut cursor, l) {
            Ok(Some(body)) => {
                let end = cursor.position();
                out.extend([0, end - body.len() as u64, body.len() as u64]);
                let _ = before;
            }
            Ok(None) => {
                out.extend([1, 0, 0]);
                return out;
            }
            Err(e) => {
                out.extend([2, kind_index(e.kind), cursor.position()]);
                return out;
            }
        }
    }
}

fn vesper_frames(stream: &[u8], l: ProtocolLimits) -> Vec<u64> {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    let entry = ENTRY.get_or_init(|| lsp().entry("lsp_frames", optimize()));
    let mut input = vec![];
    limit_words(&mut input, l);
    input.extend_from_slice(stream);
    entry.words(&input)
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    v.extend(body);
    v
}

/// The framing cases of crates/vesper-lsp/tests/transport.rs and
/// tests/lsp_cli.rs, with their limits.
fn frame_cases() -> Vec<(Vec<u8>, ProtocolLimits)> {
    let d = ProtocolLimits::default();
    let mut cases: Vec<(Vec<u8>, ProtocolLimits)> = vec![];
    let bodies = [
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","method":"exit"}"#,
        br#"{"jsonrpc":"2.0","id":1,"result":null}"#,
    ];
    let stream: Vec<u8> = bodies.iter().flat_map(|b| frame(b)).collect();
    cases.push((stream, d));
    let one = frame(bodies[0]);
    for end in 0..one.len() {
        cases.push((one[..end].to_vec(), d));
    }
    for header in [
        "Content-Length: 0\r\ncontent-length: 0\r\n\r\n",
        "Content-Length: 0\r\nContent-Length: 1\r\n\r\n",
        "Content-Length: -1\r\n\r\n",
        "Content-Length: +1\r\n\r\n",
        "Content-Length: 99999999999999999999999999999999\r\n\r\n",
        "Content-Length: 0\n\n",
        " Content-Length: 0\r\n\r\n",
        "X: y\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/vscode-jsonrpc; charset=latin1\r\n\r\n",
        "Content-Length: 0\r\nContent-Encoding: gzip\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json; charset=\"utf-8\r\n\r\n",
        "cOnTeNt-LeNgTh: 0\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
        "cOnTeNt-LeNgTh: 0\r\nContent-Type: application/vscode-jsonrpc; charset=UTF8\r\n\r\n",
        "cOnTeNt-LeNgTh: 0\r\nContent-Type: application/vscode-jsonrpc; charset=\"utf-8\"\r\n\r\n",
        // Edges of the content type and the header grammar.
        "Content-Length: 0\r\nContent-Type: application/json\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: text/plain\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json; charset=utf-8; charset=utf-8\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json; foo=bar\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json; charset\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json; charset=\"\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json;\r\n\r\n",
        "Content-Length: 0\r\nContent-Type: application/json\r\nContent-Type: application/json\r\n\r\n",
        "Content-Length: 0\r\nContent-Encoding: IDENTITY\r\n\r\n",
        "Content-Length:\t 3 \t\r\n\r\nabc",
        "Content-Length: \r\n\r\n",
        "Content-Length 3\r\n\r\nabc",
        ": 3\r\n\r\n",
        "Content_Length: 3\r\n\r\nabc",
        "Content-Length: 3\r\nX-\u{7f}: y\r\n\r\nabc",
        "Content-Length: 3\r\n\r\nab",
        "Content-Length: 3\r\r\n\r\nabc",
        "Content-Length: 3\r\n\r",
        "\r\n",
        "Content-Length: 0\r\n\r\nContent-Length: 1\r\n\r\nx",
    ] {
        cases.push((header.as_bytes().to_vec(), d));
    }
    cases.push((
        b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
        d,
    ));
    cases.push((b"Content-Length: 9\r\n\r\n{}".to_vec(), d));
    cases.push((vec![b'x'; 8193], d));
    cases.push((
        vec![b'a'; 1_000_000],
        ProtocolLimits {
            max_header_line_bytes: 16,
            ..d
        },
    ));
    cases.push((
        b"X: y\r\nX: y\r\nX: y\r\n\r\n".to_vec(),
        ProtocolLimits {
            max_header_bytes: 10,
            ..d
        },
    ));
    cases.push((
        b"X: y\r\nX: y\r\n\r\n".to_vec(),
        ProtocolLimits {
            max_headers: 1,
            ..d
        },
    ));
    let body = br#"{"jsonrpc":"2.0","method":"x"}"#;
    let f = frame(body);
    let header = f.len() - body.len();
    let tight = ProtocolLimits {
        max_header_bytes: header,
        max_header_line_bytes: header - 2,
        max_headers: 1,
        max_body_bytes: body.len(),
        ..d
    };
    cases.push((f.clone(), tight));
    cases.push((
        f.clone(),
        ProtocolLimits {
            max_body_bytes: body.len() - 1,
            ..tight
        },
    ));
    for (dh, dl) in [(0, 1), (1, 0), (1, 1), (0, 0)] {
        cases.push((
            f.clone(),
            ProtocolLimits {
                max_header_bytes: header - dh,
                max_header_line_bytes: header - 2 - dl,
                ..tight
            },
        ));
    }
    cases.push((
        f.clone(),
        ProtocolLimits {
            max_json_depth: ProtocolLimits::HARD.max_json_depth + 1,
            ..d
        },
    ));
    cases
}

#[test]
fn frames_match_rust() {
    let mut cases = frame_cases();
    for s in sessions() {
        cases.push((s.input.clone(), ProtocolLimits::default()));
        cases.push((s.output.clone(), ProtocolLimits::default()));
    }
    let mut differ = vec![];
    for (i, (stream, l)) in cases.iter().enumerate() {
        let r = rust_frames(stream, *l);
        let v = vesper_frames(stream, *l);
        if r != v {
            differ.push(format!(
                "case {i} {:?}: rust {r:?} vesper {v:?}",
                String::from_utf8_lossy(&stream[..stream.len().min(80)])
            ));
        }
    }
    eprintln!("frames: {} cases, {} differ", cases.len(), differ.len());
    assert!(differ.is_empty(), "{differ:#?}");
}

// --------------------------------------------------------------- envelopes

/// Reads `lsp_envelope`'s packed text: a length word, then the bytes.
fn take_text(words: &[u64], at: &mut usize) -> Vec<u8> {
    let len = words[*at] as usize;
    *at += 1;
    let mut bytes = vec![];
    for _ in 0..len.div_ceil(8) {
        bytes.extend_from_slice(&words[*at].to_le_bytes());
        *at += 1;
    }
    bytes.truncate(len);
    bytes
}

fn vesper_envelope(body: &[u8], l: ProtocolLimits) -> Vec<u64> {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    let entry = ENTRY.get_or_init(|| lsp().entry("lsp_envelope", optimize()));
    let mut input = vec![];
    limit_words(&mut input, l);
    input.extend_from_slice(body);
    entry.words(&input)
}

/// Compares `parse_envelope` with `lsp_envelope`; `None` when they agree.
fn envelope_difference(body: &[u8], l: ProtocolLimits) -> Option<String> {
    let v = vesper_envelope(body, l);
    let r = vesper_lsp::parse_envelope(body, l);
    let env = match r {
        Err(e) => {
            return (v != [1, kind_index(e.kind)])
                .then(|| format!("rust {:?}, vesper {v:?}", e.kind));
        }
        Ok(env) => env,
    };
    if v[0] != 0 {
        return Some(format!("rust {env:?}, vesper error {v:?}"));
    }
    let span = |at: usize| -> Option<Value> {
        (v[at] == 1)
            .then(|| serde_json::from_slice(&body[v[at + 1] as usize..v[at + 2] as usize]).unwrap())
    };
    let mut at = 14;
    let id_text = take_text(&v, &mut at);
    let method = take_text(&v, &mut at);
    let message = take_text(&v, &mut at);
    let id_of = |id: Option<&Id>| -> (u64, u64, Vec<u8>) {
        match id {
            None => (0, 0, vec![]),
            Some(Id::Integer(n)) => (1, *n as i64 as u64, vec![]),
            Some(Id::String(s)) => (2, 0, s.as_bytes().to_vec()),
        }
    };
    let got_id = (v[2], v[3], id_text);
    let same = match &env {
        Envelope::Request {
            id,
            method: m,
            params,
        } => v[1] == 0 && got_id == id_of(Some(id)) && method == m.as_bytes() && span(4) == *params,
        Envelope::Notification { method: m, params } => {
            v[1] == 1 && got_id == id_of(None) && method == m.as_bytes() && span(4) == *params
        }
        Envelope::Response { id, payload } => {
            v[1] == 2
                && got_id == id_of(id.as_ref())
                && match payload {
                    ResponsePayload::Result(value) => span(7).as_ref() == Some(value),
                    ResponsePayload::Error {
                        code,
                        message: m,
                        data,
                    } => {
                        v[7] == 0
                            && v[10] == *code as i64 as u64
                            && message == m.as_bytes()
                            && span(11) == *data
                    }
                }
        }
    };
    (!same).then(|| format!("rust {env:?}, vesper {v:?}"))
}

/// The envelope cases of crates/vesper-lsp/tests/transport.rs, and edges
/// of the same rules.
fn envelope_cases() -> Vec<(Vec<u8>, ProtocolLimits)> {
    let d = ProtocolLimits::default();
    let mut cases: Vec<(Vec<u8>, ProtocolLimits)> = vec![];
    for body in [
        r#"{"jsonrpc":"2.0","method":"x","method":"y"}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":{"x":1,"\u0078":2}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":[{"a":0,"a":1}]}"#,
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
        r#"{"jsonrpc":"2.0","id":null,"result":null}"#,
        r#"{"jsonrpc":"2.0","id":-2147483648,"error":{"code":-32600,"message":"bad","data":null}}"#,
        r#"{"jsonrpc":"2.0","id":"abc","method":"x","params":[]}"#,
        r#"{"jsonrpc":"2.0","\u006dethod":"x"}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":{"ignored":[1,]}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":["\uD800"]}"#,
        r#"{"jsonrpc":"2.0","method":"x","id":true}"#,
        r#"{"jsonrpc":"2.0","method":"\u00e9"}"#,
        "",
        "{bad JSON",
        r#"{"jsonrpc":"2.0","id":77,"method":"shutdown","\u006dethod":"initialize"}"#,
        // Edges: ids, codes, numbers serde_json refuses, escapes.
        r#"{"jsonrpc":"2.0","method":"x","id":-0}"#,
        r#"{"jsonrpc":"2.0","id":-0,"result":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-0,"message":"m"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":2147483647,"message":"m","data":{"k":[1,2.5,"s"]}}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":1,"message":2}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":1}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"message":"m"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":1,"message":"m","x":0}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":[]}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":"1","message":"m"}}"#,
        r#"{"jsonrpc":"2.0","id":-2147483649,"result":1}"#,
        r#"{"jsonrpc":"2.0","id":[],"result":1}"#,
        r#"{"jsonrpc":"2.0","method":"","id":1}"#,
        r#"{"jsonrpc":"2.0","method":7}"#,
        r#"{"jsonrpc":2.0,"method":"x"}"#,
        r#"{"method":"x"}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":{"big":1e400}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":{"big":-1.7976931348623159e308}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":{"max":1.7976931348623157e308,"tiny":1e-400}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":[123456789012345678901234567890,-9223372036854775809]}"#,
        r#"{"jsonrpc":"2.0","id":"\ud83d\ude00\n\"\\","method":"a\/b","params":{"\u00e9":"\u2603"}}"#,
        r#"{"jsonrpc":"2.0","method":"x","params":[] , "id" : 5 }"#,
        r#"  {"jsonrpc":"2.0","method":"x"}  "#,
        r#"{"jsonrpc":"2.0","method":"x"} x"#,
    ] {
        cases.push((body.as_bytes().to_vec(), d));
    }
    let mut bad = br#"{"jsonrpc":"2.0","method":"x","params":["X"]}"#.to_vec();
    let i = bad.iter().position(|b| *b == b'X').unwrap();
    bad[i] = 0xff;
    cases.push((bad, d));
    let b = br#"{"jsonrpc":"2.0","method":"x","params":{"a":[0,1]}}"#.to_vec();
    for l in [
        ProtocolLimits {
            max_json_nodes: 5,
            ..d
        },
        ProtocolLimits {
            max_json_depth: 1,
            ..d
        },
        ProtocolLimits {
            max_json_string_bytes: 2,
            ..d
        },
        ProtocolLimits {
            max_json_total_string_bytes: 4,
            ..d
        },
        ProtocolLimits {
            max_json_index_bytes: 1,
            ..d
        },
        ProtocolLimits {
            max_json_work: 1,
            ..d
        },
        ProtocolLimits {
            max_json_work: 100,
            ..d
        },
        ProtocolLimits {
            max_method_bytes: 0,
            ..d
        },
        ProtocolLimits {
            max_body_bytes: 10,
            ..d
        },
        ProtocolLimits {
            max_body_bytes: usize::MAX,
            ..d
        },
    ] {
        cases.push((b.clone(), l));
    }
    cases.push((
        br#"{"jsonrpc":"2.0","method":"x","id":"xx"}"#.to_vec(),
        ProtocolLimits {
            max_id_bytes: 1,
            ..d
        },
    ));
    let mut storm = String::from("{\"jsonrpc\":\"2.0\",\"method\":\"x\",\"params\":[");
    storm.push_str(&vec!["0"; 1000].join(","));
    storm.push_str("]}");
    cases.push((
        storm.clone().into_bytes(),
        ProtocolLimits {
            max_json_nodes: 64,
            ..d
        },
    ));
    cases.push((storm.into_bytes(), d));
    for m in [1, 2] {
        cases.push((
            br#"{"jsonrpc":"2.0","method":"\u00e9"}"#.to_vec(),
            ProtocolLimits {
                max_method_bytes: m,
                ..d
            },
        ));
    }
    cases
}

#[test]
fn envelopes_match_rust() {
    let mut cases = envelope_cases();
    for s in sessions() {
        for stream in [&s.input, &s.output] {
            let mut cursor = std::io::Cursor::new(stream);
            while let Ok(Some(body)) =
                vesper_lsp::read_frame(&mut cursor, ProtocolLimits::default())
            {
                cases.push((body, ProtocolLimits::default()));
            }
        }
    }
    let mut differ = vec![];
    for (body, l) in &cases {
        if let Some(d) = envelope_difference(body, *l) {
            differ.push(format!(
                "{:?}: {d}",
                String::from_utf8_lossy(&body[..body.len().min(120)])
            ));
        }
    }
    eprintln!("envelopes: {} bodies, {} differ", cases.len(), differ.len());
    assert!(differ.is_empty(), "{differ:#?}");
}

// ----------------------------------------------------------------- encoding

/// A `serde_json::Value` in encode.ves's prefix words, text in `pool`.
fn flatten(value: &Value, words: &mut Vec<u64>, pool: &mut Vec<u8>) {
    let text = |s: &str, words: &mut Vec<u64>, pool: &mut Vec<u8>| {
        words.extend([5, pool.len() as u64, s.len() as u64]);
        pool.extend_from_slice(s.as_bytes());
    };
    match value {
        Value::Null => words.push(0),
        Value::Bool(b) => words.extend([1, *b as u64]),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                words.extend([2, u]);
            } else if let Some(i) = n.as_i64() {
                words.extend([3, i as u64]);
            } else {
                words.extend([4, n.as_f64().unwrap().to_bits()]);
            }
        }
        Value::String(s) => text(s, words, pool),
        Value::Array(values) => {
            words.extend([6, values.len() as u64]);
            for v in values {
                flatten(v, words, pool);
            }
        }
        Value::Object(map) => {
            words.extend([7, map.len() as u64]);
            for (k, v) in map {
                text(k, words, pool);
                flatten(v, words, pool);
            }
        }
    }
}

fn flatten_envelope(env: &Envelope) -> (Vec<u64>, Vec<u8>) {
    let mut words = vec![];
    let mut pool = vec![];
    let id = |id: Option<&Id>, words: &mut Vec<u64>, pool: &mut Vec<u8>| match id {
        None => words.extend([0, 0, 0, 0]),
        Some(Id::Integer(n)) => words.extend([1, *n as i64 as u64, 0, 0]),
        Some(Id::String(s)) => {
            words.extend([2, 0, pool.len() as u64, s.len() as u64]);
            pool.extend_from_slice(s.as_bytes());
        }
    };
    let text = |s: &str, words: &mut Vec<u64>, pool: &mut Vec<u8>| {
        words.extend([pool.len() as u64, s.len() as u64]);
        pool.extend_from_slice(s.as_bytes());
    };
    let params = |p: &Option<Value>, words: &mut Vec<u64>, pool: &mut Vec<u8>| match p {
        None => words.push(0),
        Some(v) => {
            words.push(1);
            flatten(v, words, pool);
        }
    };
    match env {
        Envelope::Request {
            id: i,
            method,
            params: p,
        } => {
            words.push(0);
            id(Some(i), &mut words, &mut pool);
            text(method, &mut words, &mut pool);
            params(p, &mut words, &mut pool);
        }
        Envelope::Notification { method, params: p } => {
            words.push(1);
            text(method, &mut words, &mut pool);
            params(p, &mut words, &mut pool);
        }
        Envelope::Response { id: i, payload } => {
            words.push(2);
            id(i.as_ref(), &mut words, &mut pool);
            match payload {
                ResponsePayload::Result(v) => {
                    words.push(0);
                    flatten(v, &mut words, &mut pool);
                }
                ResponsePayload::Error {
                    code,
                    message,
                    data,
                } => {
                    words.extend([1, *code as i64 as u64]);
                    text(message, &mut words, &mut pool);
                    params(data, &mut words, &mut pool);
                }
            }
        }
    }
    (words, pool)
}

/// `lsp_encode` of `env`: the frame, or the error's index.
fn encode_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| lsp().entry("lsp_encode", optimize()))
}

/// The input of `lsp_encode`.
fn encode_input(env: &Envelope, l: ProtocolLimits) -> Vec<u8> {
    let (words, pool) = flatten_envelope(env);
    let mut input = vec![];
    limit_words(&mut input, l);
    push_word(&mut input, words.len() as u64);
    for w in &words {
        push_word(&mut input, *w);
    }
    input.extend_from_slice(&pool);
    input
}

fn vesper_encode(env: &Envelope, l: ProtocolLimits) -> Result<Vec<u8>, u64> {
    let out = encode_entry().words(&encode_input(env, l));
    if out[0] == 0 {
        let mut at = 1;
        Ok(take_text(&out, &mut at))
    } else {
        Err(out[1])
    }
}

fn encode_difference(env: &Envelope, l: ProtocolLimits) -> Option<String> {
    let r = vesper_lsp::encode_frame(env, l).map_err(|e| kind_index(e.kind));
    let v = vesper_encode(env, l);
    (r != v).then(|| {
        let show = |x: &Result<Vec<u8>, u64>| match x {
            Ok(b) => String::from_utf8_lossy(b).into_owned(),
            Err(k) => format!("error {k}"),
        };
        format!("{env:?}: rust {} vesper {}", show(&r), show(&v))
    })
}

/// The encoder cases of crates/vesper-lsp's tests and src/lsp.rs's, and
/// values that exercise every writer path.
fn encode_cases() -> Vec<(Envelope, ProtocolLimits)> {
    let d = ProtocolLimits::default();
    let mut cases = vec![];
    let unicode = Envelope::Response {
        id: Some(Id::String("😀".into())),
        payload: ResponsePayload::Result(json!({"é":"☃😀"})),
    };
    cases.push((unicode, d));
    for l in [
        ProtocolLimits {
            max_reply_bytes: 1,
            ..d
        },
        ProtocolLimits {
            max_json_work: 1,
            ..d
        },
        ProtocolLimits {
            max_json_nodes: 1,
            ..d
        },
        ProtocolLimits {
            max_header_bytes: 1,
            ..d
        },
        ProtocolLimits {
            max_id_bytes: 1,
            ..d
        },
        ProtocolLimits {
            max_reply_bytes: 40,
            ..d
        },
        ProtocolLimits {
            max_json_work: 30,
            ..d
        },
        ProtocolLimits {
            max_json_work: 60,
            ..d
        },
        ProtocolLimits {
            max_header_line_bytes: 19,
            ..d
        },
        ProtocolLimits {
            max_headers: 0,
            ..d
        },
        ProtocolLimits {
            max_json_string_bytes: 3,
            ..d
        },
        ProtocolLimits {
            max_json_total_string_bytes: 12,
            ..d
        },
        ProtocolLimits {
            max_json_depth: 0,
            ..d
        },
    ] {
        cases.push((
            Envelope::Response {
                id: Some(Id::String("😀".into())),
                payload: ResponsePayload::Result(json!({"é":"☃😀"})),
            },
            l,
        ));
    }
    cases.push((
        Envelope::Notification {
            method: "x".into(),
            params: Some(json!(null)),
        },
        d,
    ));
    cases.push((
        Envelope::Notification {
            method: "".into(),
            params: None,
        },
        d,
    ));
    cases.push((
        Envelope::Notification {
            method: "exit".into(),
            params: None,
        },
        d,
    ));
    let mut deep = json!(null);
    for _ in 0..100 {
        deep = Value::Array(vec![deep]);
    }
    cases.push((
        Envelope::Response {
            id: None,
            payload: ResponsePayload::Result(deep),
        },
        d,
    ));
    let mut deep = json!(null);
    for _ in 0..63 {
        deep = Value::Array(vec![deep]);
    }
    for depth in [64, 63] {
        cases.push((
            Envelope::Response {
                id: None,
                payload: ResponsePayload::Result(deep.clone()),
            },
            ProtocolLimits {
                max_json_depth: depth,
                ..d
            },
        ));
    }
    let pair = Envelope::Notification {
        method: "x".into(),
        params: Some(json!([0, 1])),
    };
    let l = ProtocolLimits {
        max_json_nodes: 9,
        max_json_depth: 2,
        ..d
    };
    for l in [
        l,
        ProtocolLimits {
            max_json_nodes: 8,
            ..l
        },
        ProtocolLimits {
            max_json_depth: 1,
            ..l
        },
    ] {
        cases.push((
            Envelope::Notification {
                method: "x".into(),
                params: Some(json!([0, 1])),
            },
            l,
        ));
    }
    drop(pair);
    for l in [
        ProtocolLimits {
            max_reply_bytes: 256,
            ..d
        },
        d,
    ] {
        cases.push((
            Envelope::Response {
                id: Some(Id::String("request".into())),
                payload: ResponsePayload::Result(json!("x".repeat(2048))),
            },
            l,
        ));
        cases.push((
            Envelope::Notification {
                method: "window/logMessage".into(),
                params: Some(json!({"message": "x".repeat(2048)})),
            },
            l,
        ));
    }
    let floats = json!([
        0.0,
        -0.0,
        1.0,
        -1.5,
        0.1,
        1e15,
        1e16,
        1.5e16,
        123456.789,
        1e-5,
        1e-7,
        0.000123,
        1.7976931348623157e308,
        5e-324,
        2.2250738585072014e-308,
        9007199254740993.0,
        3.0e-10,
        12345678901234567.0
    ]);
    let strings = json!([
        "",
        "plain",
        "q\"b\\s/",
        "\u{0}\u{1}\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{1f}\u{7f}",
        "é☃😀",
        "a\nb\nc"
    ]);
    let ints = json!([0, 1, -1, u64::MAX, i64::MIN, i64::MAX, 4294967296u64]);
    let objects =
        json!({"": {}, "b": [], "a": [{}, [[]], null, true, false], "\n": {"x": {"y": "z"}}});
    for v in [floats, strings, ints, objects] {
        cases.push((
            Envelope::Request {
                id: Id::Integer(-7),
                method: "m".into(),
                params: Some(v.clone()),
            },
            d,
        ));
        cases.push((
            Envelope::Response {
                id: Some(Id::Integer(i32::MIN)),
                payload: ResponsePayload::Error {
                    code: -32603,
                    message: "m\u{1}".into(),
                    data: Some(v),
                },
            },
            d,
        ));
    }
    cases.push((
        Envelope::Response {
            id: Some(Id::Integer(i32::MAX)),
            payload: ResponsePayload::Error {
                code: 0,
                message: "".into(),
                data: None,
            },
        },
        d,
    ));
    cases.push((
        Envelope::Request {
            id: Id::String("".into()),
            method: "m".into(),
            params: Some(json!(7)),
        },
        d,
    ));
    cases
}

#[test]
fn responses_match_byte_for_byte() {
    let mut differ = vec![];
    let mut frames = 0;
    for s in sessions() {
        let mut cursor = std::io::Cursor::new(&s.output);
        loop {
            let start = cursor.position() as usize;
            let Ok(Some(body)) = vesper_lsp::read_frame(&mut cursor, ProtocolLimits::default())
            else {
                break;
            };
            let end = cursor.position() as usize;
            let env = vesper_lsp::parse_envelope(&body, ProtocolLimits::default()).unwrap();
            frames += 1;
            match vesper_encode(&env, ProtocolLimits::default()) {
                Ok(bytes) if bytes == s.output[start..end] => {}
                other => differ.push(format!(
                    "{}: server {:?}, vesper {:?}",
                    s.name,
                    String::from_utf8_lossy(&s.output[start..end]),
                    other.map(|b| String::from_utf8_lossy(&b).into_owned())
                )),
            }
        }
    }
    let cases = encode_cases();
    for (env, l) in &cases {
        if let Some(d) = encode_difference(env, *l) {
            differ.push(d);
        }
    }
    eprintln!(
        "responses: {frames} server frames, {} encoder cases, {} differ",
        cases.len(),
        differ.len()
    );
    assert!(frames > 0);
    assert!(differ.is_empty(), "{differ:#?}");
}

// ---------------------------------------------------------------- documents

/// One store operation, as lib.ves of @vesper/lsp-documents reads it.
#[derive(Clone, Debug)]
enum Op {
    Open(String, i32, String),
    Change(String, i32, Vec<(Option<Range>, Option<u32>, String)>),
    Close(String),
    Get(String),
    SetRevision(u64),
}

fn span_code(e: vesper_span::Error) -> u64 {
    use vesper_span::Error::*;
    match e {
        InvalidLimits => 0,
        Budget(r) => 20 + r as u64,
        LineOutOfBounds => 2,
        CharacterOutOfBounds => 3,
        ByteOutOfBounds => 4,
        Utf8Boundary => 5,
        SurrogateInterior => 6,
        LineEndingInterior => 7,
        ReversedRange => 8,
        OverlappingEdits => 9,
        SourceMismatch => 10,
        InvalidDiagnostic => 11,
        Allocation => 12,
    }
}

fn store_code(e: StoreError) -> u64 {
    1 + match e {
        StoreError::InvalidLimits => 0,
        StoreError::InvalidUri => 1,
        StoreError::AlreadyOpen => 2,
        StoreError::NotOpen => 3,
        StoreError::Version => 4,
        StoreError::DocumentLimit => 5,
        StoreError::TotalLimit => 6,
        StoreError::ChangeLimit => 7,
        StoreError::WorkLimit => 8,
        StoreError::RevisionOverflow => 9,
        StoreError::RangeLength => 10,
        StoreError::Allocation => 11,
        StoreError::Index(e) => 100 + span_code(e),
    }
}

fn pack_text_words(out: &mut Vec<u64>, bytes: &[u8]) {
    out.push(bytes.len() as u64);
    for chunk in bytes.chunks(8) {
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        out.push(u64::from_le_bytes(w));
    }
}

/// Rust's results in the words of `lsp_documents`.
fn rust_documents(l: StoreLimits, ops: &[Op]) -> Vec<u64> {
    let mut s = match DocumentStore::new(l) {
        Ok(s) => s,
        Err(e) => return vec![store_code(e)],
    };
    let mut out = vec![0];
    let state =
        |out: &mut Vec<u64>, s: &DocumentStore, status: u64, doc: Option<(i32, u64, &str)>| {
            out.extend([status, s.len() as u64, s.total_bytes() as u64, s.revision()]);
            match doc {
                Some((version, revision, text)) => {
                    out.extend([1, version as i64 as u64, revision]);
                    pack_text_words(out, text.as_bytes());
                }
                None => out.push(0),
            }
        };
    for op in ops {
        match op {
            Op::Open(uri, version, text) => match s.open(uri, *version, text) {
                Ok(d) => {
                    let d = (d.version(), d.revision(), d.text().to_owned());
                    state(&mut out, &s, 0, Some((d.0, d.1, &d.2)))
                }
                Err(e) => state(&mut out, &s, store_code(e), None),
            },
            Op::Change(uri, version, changes) => {
                let changes: Vec<ContentChange<'_>> = changes
                    .iter()
                    .map(|(range, range_length, text)| ContentChange {
                        range: *range,
                        range_length: *range_length,
                        text,
                    })
                    .collect();
                match s.change(uri, *version, &changes) {
                    Ok(d) => {
                        let d = (d.version(), d.revision(), d.text().to_owned());
                        state(&mut out, &s, 0, Some((d.0, d.1, &d.2)))
                    }
                    Err(e) => state(&mut out, &s, store_code(e), None),
                }
            }
            Op::Close(uri) => match s.close(uri) {
                Ok(d) => state(&mut out, &s, 0, Some((d.version(), d.revision(), d.text()))),
                Err(e) => state(&mut out, &s, store_code(e), None),
            },
            Op::Get(uri) => {
                let d = s
                    .get(uri)
                    .map(|d| (d.version(), d.revision(), d.text().to_owned()));
                match d {
                    Some(d) => state(&mut out, &s, 0, Some((d.0, d.1, &d.2))),
                    None => state(&mut out, &s, 1000, None),
                }
            }
            Op::SetRevision(_) => unreachable!("only the Vesper store can force its revision"),
        }
    }
    out
}

fn vesper_documents(l: StoreLimits, ops: &[Op]) -> Vec<u64> {
    documents_entry().words(&documents_input(l, ops))
}

fn documents_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| documents().entry("lsp_documents", optimize()))
}

/// The input of `lsp_documents`.
fn documents_input(l: StoreLimits, ops: &[Op]) -> Vec<u8> {
    let mut words: Vec<u64> = vec![];
    let mut pool: Vec<u8> = vec![];
    let mut text = |s: &str, words: &mut Vec<u64>| {
        words.extend([pool.len() as u64, s.len() as u64]);
        pool.extend_from_slice(s.as_bytes());
    };
    for op in ops {
        match op {
            Op::Open(uri, version, t) => {
                words.push(1);
                text(uri, &mut words);
                words.push(*version as i64 as u64);
                text(t, &mut words);
            }
            Op::Change(uri, version, changes) => {
                words.push(2);
                text(uri, &mut words);
                words.extend([*version as i64 as u64, changes.len() as u64]);
                for (range, len, t) in changes {
                    let r = range.unwrap_or(Range {
                        start: Position {
                            line: 0,
                            character: 0,
                        },
                        end: Position {
                            line: 0,
                            character: 0,
                        },
                    });
                    words.extend([
                        range.is_some() as u64,
                        r.start.line as u64,
                        r.start.character as u64,
                        r.end.line as u64,
                        r.end.character as u64,
                        len.is_some() as u64,
                        len.unwrap_or(0) as u64,
                    ]);
                    text(t, &mut words);
                }
            }
            Op::Close(uri) => {
                words.push(3);
                text(uri, &mut words);
            }
            Op::Get(uri) => {
                words.push(4);
                text(uri, &mut words);
            }
            Op::SetRevision(r) => words.extend([5, *r]),
        }
    }
    let mut input = vec![];
    let i = l.index_limits;
    for w in [
        l.max_document_bytes,
        l.max_documents,
        l.max_total_bytes,
        l.max_uri_bytes,
        l.max_changes,
        l.max_work,
        i.max_source_bytes,
        i.max_lines,
        i.max_metadata_bytes,
        i.max_work,
        i.max_edits,
        i.max_output_bytes,
        i.max_diagnostics,
        i.max_labels,
        i.max_fixes,
        i.max_text_bytes,
    ] {
        push_word(&mut input, w as u64);
    }
    push_word(&mut input, words.len() as u64);
    for w in &words {
        push_word(&mut input, *w);
    }
    input.extend_from_slice(&pool);
    input
}

fn pos(line: u32, character: u32) -> Position {
    Position { line, character }
}

fn edit(
    start: Position,
    end: Position,
    length: Option<u32>,
    text: &str,
) -> (Option<Range>, Option<u32>, String) {
    (Some(Range { start, end }), length, text.to_string())
}

fn full(text: &str) -> (Option<Range>, Option<u32>, String) {
    (None, None, text.to_string())
}

/// The scripts of crates/vesper-lsp/tests/documents.rs.
fn document_scripts() -> Vec<(StoreLimits, Vec<Op>)> {
    use Op::*;
    let d = StoreLimits::default();
    let s = |x: &str| x.to_string();
    let mut scripts = vec![];
    scripts.push((
        d,
        vec![
            Open(s("untitled:opaque/path"), -3, s("a😀b\r\ncafé\rZ\n")),
            Change(
                s("untitled:opaque/path"),
                0,
                vec![
                    edit(pos(0, 1), pos(0, 3), Some(2), "☃"),
                    edit(pos(1, 3), pos(1, 4), Some(1), "é"),
                    edit(pos(2, 0), pos(3, 0), Some(2), "end"),
                ],
            ),
            Get(s("untitled:opaque/path")),
            Change(
                s("untitled:opaque/path"),
                1,
                vec![full("😀\r\ny"), edit(pos(0, 2), pos(1, 0), Some(2), "!")],
            ),
            Get(s("untitled:opaque/path")),
        ],
    ));
    let mut ops = vec![Open(s("opaque:x"), 10, s("a😀b\r\nc"))];
    for changes in [
        vec![edit(pos(0, 2), pos(0, 3), None, "")],
        vec![edit(pos(0, 3), pos(0, 1), None, "")],
        vec![edit(pos(9, 0), pos(9, 0), None, "")],
        vec![edit(pos(0, 5), pos(1, 0), None, "")],
        vec![edit(pos(0, 1), pos(0, 3), Some(1), "")],
        vec![(None, Some(0), s("new"))],
        vec![
            full("replacement"),
            edit(pos(99, 0), pos(99, 0), None, "bad"),
        ],
        vec![edit(pos(0, 4), pos(0, 6), None, "")],
        vec![edit(pos(1, 0), pos(1, 2), None, "")],
        vec![edit(pos(0, 6), pos(0, 6), None, "")],
    ] {
        ops.push(Change(s("opaque:x"), 11, changes));
        ops.push(Get(s("opaque:x")));
    }
    scripts.push((d, ops));
    scripts.push((
        d,
        vec![
            Change(s("unknown"), 1, vec![]),
            Close(s("unknown")),
            Open(s("file:///does-not-exist"), i32::MIN, s("x")),
            Open(s("file:///does-not-exist"), 0, s("different")),
            Get(s("file:///does-not-exist")),
            Change(s("file:///does-not-exist"), i32::MIN, vec![full("y")]),
            Change(s("file:///does-not-exist"), i32::MAX, vec![]),
            Change(s("file:///does-not-exist"), i32::MAX, vec![]),
            Close(s("file:///does-not-exist")),
            Close(s("file:///does-not-exist")),
            Open(s("file:///does-not-exist"), 1, s("again")),
        ],
    ));
    let caps = StoreLimits {
        max_document_bytes: 4,
        max_documents: 2,
        max_total_bytes: 6,
        max_uri_bytes: 3,
        ..d
    };
    let mut ops = vec![
        Open(s("one"), 1, s("😀")),
        Open(s("two"), 1, s("ab")),
        Open(s("tri"), 1, s("")),
        Change(s("two"), 2, vec![full("abc")]),
        Change(s("one"), 2, vec![full("abcde")]),
        Change(s("two"), 2, vec![full("abc"), full("")]),
        Get(s("two")),
    ];
    for uri in ["", "long", "a\nb", "a\u{85}", "a\u{7f}", "é"] {
        ops.push(Close(s(uri)));
        ops.push(Get(s(uri)));
    }
    ops.extend([Close(s("one")), Change(s("two"), 2, vec![full("abcd")])]);
    scripts.push((caps, ops));
    scripts.push((
        StoreLimits {
            max_changes: 1,
            ..d
        },
        vec![
            Open(s("x"), 1, s("text")),
            Change(s("x"), 2, vec![full("a"), full("b")]),
            Get(s("x")),
        ],
    ));
    scripts.push((
        StoreLimits {
            max_work: 12,
            max_uri_bytes: 1,
            ..d
        },
        vec![
            Open(s("x"), 1, s("abc")),
            Change(s("x"), 2, vec![full("abc")]),
            Get(s("x")),
        ],
    ));
    let mut l = d;
    l.index_limits.max_lines = 1;
    scripts.push((
        l,
        vec![
            Open(s("x"), 1, s("abc")),
            Change(s("x"), 2, vec![full("\n")]),
            Get(s("x")),
            Open(s("y"), 1, s("\n")),
        ],
    ));
    let mut l = d;
    l.index_limits.max_metadata_bytes = 0;
    scripts.push((l, vec![Open(s("x"), 1, s(""))]));
    let mut l = d;
    l.index_limits.max_metadata_bytes = 31;
    scripts.push((l, vec![Open(s("x"), 1, s("a")), Open(s("y"), 1, s("a\nb"))]));
    let mut l = d;
    l.index_limits.max_work = 3;
    scripts.push((
        l,
        vec![
            Open(s("x"), 1, s("ab")),
            Change(s("x"), 2, vec![edit(pos(0, 1), pos(0, 2), None, "z")]),
        ],
    ));
    for l in [
        StoreLimits {
            max_document_bytes: usize::MAX,
            ..d
        },
        StoreLimits {
            max_documents: 257,
            ..d
        },
        StoreLimits {
            max_changes: usize::MAX,
            ..d
        },
        StoreLimits {
            index_limits: vesper_span::Limits {
                max_source_bytes: 1,
                ..vesper_span::Limits::default()
            },
            ..d
        },
        StoreLimits {
            index_limits: vesper_span::Limits {
                max_lines: usize::MAX,
                ..vesper_span::Limits::default()
            },
            ..d
        },
        StoreLimits::HARD,
    ] {
        scripts.push((l, vec![]));
    }
    scripts.push((
        StoreLimits {
            max_document_bytes: 0,
            max_total_bytes: 0,
            ..d
        },
        vec![
            Open(s("x"), 1, s("")),
            Change(s("x"), 2, vec![full("a")]),
            Change(s("x"), 2, vec![]),
            Get(s("x")),
        ],
    ));
    scripts.push((
        StoreLimits {
            max_documents: 0,
            ..d
        },
        vec![Open(s("x"), 1, s(""))],
    ));
    scripts
}

/// Every didOpen, didChange and didClose of a session's client stream, with
/// a `get` after each, read the way src/lsp/session.rs reads them.
fn session_ops(input: &[u8]) -> Vec<Op> {
    let mut ops = vec![];
    let mut cursor = std::io::Cursor::new(input);
    while let Ok(Some(body)) = vesper_lsp::read_frame(&mut cursor, ProtocolLimits::default()) {
        let Ok(Envelope::Notification {
            method,
            params: Some(p),
        }) = vesper_lsp::parse_envelope(&body, ProtocolLimits::default())
        else {
            continue;
        };
        let uri = p["textDocument"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let version = p["textDocument"]["version"].as_i64().unwrap_or_default() as i32;
        match method.as_str() {
            "textDocument/didOpen" => {
                let text = p["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                ops.push(Op::Open(uri.clone(), version, text));
            }
            "textDocument/didChange" => {
                let changes = p["contentChanges"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let range = c.get("range").filter(|r| !r.is_null()).map(|r| {
                            let p = |v: &Value| {
                                pos(
                                    v["line"].as_u64().unwrap() as u32,
                                    v["character"].as_u64().unwrap() as u32,
                                )
                            };
                            Range {
                                start: p(&r["start"]),
                                end: p(&r["end"]),
                            }
                        });
                        let len = c
                            .get("rangeLength")
                            .and_then(Value::as_u64)
                            .map(|n| n as u32);
                        (
                            range,
                            len,
                            c["text"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                ops.push(Op::Change(uri.clone(), version, changes));
            }
            "textDocument/didClose" => ops.push(Op::Close(uri.clone())),
            _ => continue,
        }
        ops.push(Op::Get(uri));
    }
    ops
}

#[test]
fn documents_match_rust() {
    let mut scripts = document_scripts();
    for s in sessions() {
        let ops = session_ops(&s.input);
        if !ops.is_empty() {
            scripts.push((StoreLimits::default(), ops));
        }
    }
    let mut differ = vec![];
    let mut ops = 0;
    for (i, (l, script)) in scripts.iter().enumerate() {
        ops += script.len();
        let r = rust_documents(*l, script);
        let v = vesper_documents(*l, script);
        if r != v {
            differ.push(format!(
                "script {i} {script:?}:\n rust {r:?}\n vesper {v:?}"
            ));
        }
    }
    // The revision overflow of document.rs's unit test, through the
    // Vesper store's forced revision: every mutation is refused whole.
    let v = vesper_documents(
        StoreLimits::default(),
        &[
            Op::Open("opaque:a".into(), 1, "x".into()),
            Op::SetRevision(u64::MAX),
            Op::Close("opaque:a".into()),
            Op::Change("opaque:a".into(), 2, vec![]),
            Op::Open("opaque:b".into(), 1, "".into()),
            Op::Get("opaque:a".into()),
        ],
    );
    let overflow = store_code(StoreError::RevisionOverflow);
    let statuses: Vec<u64> = {
        let mut at = 1;
        let mut s = vec![];
        while at < v.len() {
            s.push(v[at]);
            at += 4;
            if v[at] == 1 {
                at += 3;
                let len = v[at] as usize;
                at += 1 + len.div_ceil(8);
            } else {
                at += 1;
            }
        }
        s
    };
    assert_eq!(statuses, [0, 0, overflow, overflow, overflow, 0], "{v:?}");
    eprintln!(
        "documents: {} scripts, {ops} ops, {} differ",
        scripts.len(),
        differ.len()
    );
    assert!(differ.is_empty(), "{differ:#?}");
}

// ---------------------------------------------------------------- sessions

/// A recorded stdio session: the client's bytes and the server's.
struct Session {
    name: String,
    input: Vec<u8>,
    output: Vec<u8>,
}

fn sessions() -> &'static [Session] {
    static SESSIONS: OnceLock<Vec<Session>> = OnceLock::new();
    SESSIONS.get_or_init(record_sessions)
}

fn request(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}

fn stream(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .flat_map(|m| frame(&serde_json::to_vec(m).unwrap()))
        .collect()
}

/// Runs the actual `vesper lsp` server on `input` and returns its stdout.
fn serve(input: &[u8]) -> Vec<u8> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut out = vec![];
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    writer.join().unwrap();
    child.wait().unwrap();
    out
}

fn initialize() -> Value {
    request(
        1,
        "initialize",
        json!({"capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"workspaceEdit":{"documentChanges":true}},"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":true},"codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","source.fixAll.vesper"]}}}}}}),
    )
}

fn shutdown() -> [Value; 2] {
    [
        request(900, "shutdown", json!({})),
        notice("exit", json!({})),
    ]
}

fn doc(uri: &str) -> Value {
    json!({"textDocument":{"uri":uri}})
}

fn at(uri: &str, line: u32, character: u32) -> Value {
    json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}})
}

/// The client streams of tests/lsp_cli.rs, message for message.
fn cli_inputs() -> Vec<(String, Vec<u8>)> {
    let mut inputs = vec![];
    let uri = "untitled:stdio/🦀";
    let source = "// 😀\r\nfn first()->u64{1}\r\nfn main()->u64{first()}\r\n";
    let mut messages = vec![
        initialize(),
        notice("initialized", json!({})),
        notice(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"vesper","version":1,"text":source}}),
        ),
        request(2, "textDocument/documentSymbol", doc(uri)),
        request(3, "textDocument/hover", at(uri, 1, 16)),
        request(4, "textDocument/definition", at(uri, 2, 15)),
        notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"range":{"start":{"line":1,"character":16},"end":{"line":1,"character":17}},"rangeLength":1,"text":"\r\n1\r\n"}]}),
        ),
        request(5, "textDocument/documentSymbol", doc(uri)),
        request(6, "textDocument/definition", at(uri, 4, 15)),
        notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":3},"contentChanges":[{"range":{"start":{"line":0,"character":4},"end":{"line":0,"character":5}},"text":"x"}]}),
        ),
        notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"fn forged()->u64{9}"}]}),
        ),
        request(7, "textDocument/documentSymbol", doc(uri)),
        notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":3},"contentChanges":[{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":5}},"rangeLength":2,"text":"🦀"}]}),
        ),
        request(8, "textDocument/documentSymbol", doc(uri)),
        notice("textDocument/didClose", doc(uri)),
        request(9, "textDocument/hover", at(uri, 1, 3)),
        request(10, "workspace/symbol", json!({"query":"main"})),
    ];
    messages.extend(shutdown());
    inputs.push(("unicode_navigation".to_string(), stream(&messages)));

    let mut bytes = stream(&[
        request(20, "textDocument/hover", at("opaque:x", 0, 0)),
        initialize(),
    ]);
    bytes.extend(frame(
        br#"{"jsonrpc":"2.0","id":77,"method":"shutdown","\u006dethod":"initialize"}"#,
    ));
    bytes.extend(frame(b"{bad JSON"));
    let uri = "opaque:unsupported";
    bytes.extend(stream(&[
        request(21, "unknown/method", json!({})),
        notice(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"version":1,"text":"fn broken("}}),
        ),
        request(22, "textDocument/hover", at(uri, 0, 3)),
        request(23, "vesper/signatureSearch", json!({})),
        request(24, "shutdown", json!({})),
        request(25, "workspace/symbol", json!({"query":""})),
        notice("exit", json!({})),
    ]));
    inputs.push(("lifecycle_recovery".to_string(), bytes));

    for (i, input) in [
        b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
        b"Content-Length: 9\r\n\r\n{}".to_vec(),
        b"Content-Length: 0\n\n".to_vec(),
        vec![b'x'; 8193],
        stream(&[notice("exit", json!({}))]),
    ]
    .into_iter()
    .enumerate()
    {
        inputs.push((format!("framing_error_{i}"), input));
    }

    for flag in [None, Some(false)] {
        let mut init = initialize();
        match flag {
            None => {
                init["params"]["capabilities"]
                    .as_object_mut()
                    .unwrap()
                    .remove("textDocument");
            }
            Some(flag) => {
                init["params"]["capabilities"]["textDocument"]["documentSymbol"]["hierarchicalDocumentSymbolSupport"] =
                    json!(flag)
            }
        }
        let uri = "opaque:legacy";
        let mut messages = vec![
            init,
            notice(
                "textDocument/didOpen",
                json!({"textDocument":{"uri":uri,"version":1,"text":"fn main()->u64{42}"}}),
            ),
            request(2, "textDocument/documentSymbol", doc(uri)),
        ];
        messages.extend(shutdown());
        inputs.push((format!("legacy_symbols_{flag:?}"), stream(&messages)));
    }

    let literal = json!({"codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","source.fixAll.vesper"]}}}});
    let cases = [
        json!({}),
        json!({"workspace":{"workspaceEdit":{"documentChanges":false}},"textDocument":literal}),
        json!({"workspace":{"workspaceEdit":{"documentChanges":true}}}),
        json!({"textDocument":literal}),
        json!({"workspace":{"workspaceEdit":{"documentChanges":true}},"textDocument":{"codeAction":{"codeActionLiteralSupport":false}}}),
    ];
    for (i, caps) in cases.into_iter().enumerate() {
        let uri = "opaque:legacy-actions";
        let source = "effect Audit{fn event();} fn main()->u64 eff[Audit]{42}";
        let mut init = initialize();
        init["params"]["capabilities"] = caps;
        let mut messages = vec![
            init,
            notice(
                "textDocument/didOpen",
                json!({"textDocument":{"uri":uri,"version":1,"text":source}}),
            ),
            request(
                2,
                "textDocument/codeAction",
                json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":source.len()}},"context":{"only":["quickfix"],"diagnostics":[]}}),
            ),
        ];
        messages.extend(shutdown());
        inputs.push((format!("legacy_actions_{i}"), stream(&messages)));
    }
    inputs
}

/// Editing sessions over corpus programs: open each, edit it in several
/// ways (insertions, deletions and replacements at UTF-16 positions, a
/// whole-text change, a stale version, a version-only change), and ask for
/// symbols, hover and formatting between edits.
fn editing_inputs() -> Vec<(String, Vec<u8>)> {
    let files = [
        "examples/checked_math.ves",
        "examples/closures.ves",
        "examples/counter.ves",
        "examples/fail.ves",
        "examples/fibonacci.ves",
        "examples/projections.ves",
        "examples/resumptions.ves",
        "examples/transaction.ves",
    ];
    let mut inputs = vec![];
    let mut sources: Vec<(String, String)> = files
        .iter()
        .filter_map(|f| {
            std::fs::read_to_string(repo().join(f))
                .ok()
                .map(|s| (f.to_string(), s))
        })
        .collect();
    sources.push((
        "inline".into(),
        "// 😀 ünïcödé\nfn helper(x: u64) -> u64 {\n    x + 1\n}\n\nfn main() -> u64 {\n    helper(41)\n}\n".into(),
    ));
    for (name, source) in sources {
        let uri = format!("untitled:{name}");
        let lines: Vec<&str> = source.split('\n').collect();
        let last = (lines.len() - 1) as u32;
        let mut m = vec![
            initialize(),
            notice("initialized", json!({})),
            notice(
                "textDocument/didOpen",
                json!({"textDocument":{"uri":uri,"languageId":"vesper","version":1,"text":source}}),
            ),
            request(2, "textDocument/documentSymbol", doc(&uri)),
            request(3, "textDocument/hover", at(&uri, 1, 4)),
            // A body edit: append a comment at the end of line 1.
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":0}},"text":"// edit\n"}]}),
            ),
            request(4, "textDocument/documentSymbol", doc(&uri)),
            // Two changes in one notification, applied in order.
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":3},"contentChanges":[
                    {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"text":"\n"},
                    {"range":{"start":{"line":0,"character":0},"end":{"line":1,"character":0}},"rangeLength":1,"text":""}
                ]}),
            ),
            // A stale version and an out-of-range edit are refused.
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":3},"contentChanges":[{"text":"stale"}]}),
            ),
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":4},"contentChanges":[{"range":{"start":{"line":last + 5,"character":0},"end":{"line":last + 5,"character":0}},"text":"x"}]}),
            ),
            // A version-only change keeps the text.
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":5},"contentChanges":[]}),
            ),
            request(
                5,
                "textDocument/formatting",
                json!({"textDocument":{"uri":uri},"options":{"tabSize":4,"insertSpaces":true}}),
            ),
            // Delete the edit comment again: the text is the opened text.
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":6},"contentChanges":[{"range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},"text":""}]}),
            ),
            request(6, "textDocument/documentSymbol", doc(&uri)),
            notice(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":7},"contentChanges":[{"text":"fn main() -> u64 { 1 }\n"}]}),
            ),
            request(7, "textDocument/hover", at(&uri, 0, 4)),
            notice("textDocument/didClose", doc(&uri)),
        ];
        m.extend(shutdown());
        inputs.push((format!("editing_{name}"), stream(&m)));
    }
    inputs
}

/// Bodies the transport refuses before the session sees them, between
/// requests, cancellations and a client response.
fn transport_inputs() -> Vec<(String, Vec<u8>)> {
    let mut input = stream(&[initialize(), notice("initialized", json!({}))]);
    for body in [
        &b"{not json"[..],
        b"[1,2]",
        b"{\"jsonrpc\":\"2.0\",\"id\":-0,\"method\":\"x\"}",
        b"{\"jsonrpc\":\"1.0\",\"method\":\"x\"}",
        b"{\"jsonrpc\":\"2.0\",\"method\":\"x\",\"method\":\"y\"}",
        b"{\"jsonrpc\":\"2.0\",\"method\":\"\xff\"}",
        b"{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"x\"}",
        b"{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":null}",
    ] {
        input.extend(frame(body));
    }
    input.extend(stream(&[
        notice("$/cancelRequest", json!({"id": 99})),
        request(2, "workspace/symbol", json!({"query": ""})),
        notice("$/cancelRequest", json!({"id": 2})),
    ]));
    input.extend(stream(&shutdown()));
    vec![("transport_refusals".into(), input)]
}

fn record_sessions() -> Vec<Session> {
    let mut inputs = cli_inputs();
    inputs.extend(editing_inputs());
    inputs.extend(transport_inputs());
    inputs
        .into_iter()
        .map(|(name, input)| {
            let output = serve(&input);
            Session {
                name,
                input,
                output,
            }
        })
        .collect()
}

// -------------------------------------------------------------------- serve

/// `dispatch`'s transport decisions (src/lsp.rs) in `lsp_serve`'s words,
/// from crates/vesper-lsp's functions.
fn rust_serve(stream: &[u8], l: ProtocolLimits) -> Vec<u64> {
    let mut out = vec![];
    let mut cursor = std::io::Cursor::new(stream);
    loop {
        let body = match vesper_lsp::read_frame(&mut cursor, l) {
            Ok(Some(body)) => body,
            Ok(None) => {
                out.push(6);
                return out;
            }
            Err(e) => {
                out.extend([5, kind_index(e.kind)]);
                return out;
            }
        };
        let end = cursor.position();
        let start = end - body.len() as u64;
        match vesper_lsp::parse_envelope(&body, l) {
            Err(error) => {
                let code = match error.kind {
                    ProtocolErrorKind::Utf8 | ProtocolErrorKind::Json => -32700,
                    _ => -32600,
                };
                let reply = Envelope::Response {
                    id: None,
                    payload: ResponsePayload::Error {
                        code,
                        message: error.to_string(),
                        data: None,
                    },
                };
                match vesper_lsp::encode_frame(&reply, l) {
                    Ok(bytes) => {
                        out.push(0);
                        pack_text_words(&mut out, &bytes);
                    }
                    Err(e) => {
                        out.extend([5, 100 + kind_index(e.kind)]);
                        return out;
                    }
                }
            }
            Ok(Envelope::Request { .. }) => out.extend([1, start, body.len() as u64]),
            Ok(Envelope::Notification { method, .. }) if method == "$/cancelRequest" => {
                out.extend([3, start, body.len() as u64])
            }
            Ok(Envelope::Notification { .. }) => out.extend([2, start, body.len() as u64]),
            Ok(Envelope::Response { .. }) => out.extend([4, start, body.len() as u64]),
        }
    }
}

fn serve_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| lsp().entry("lsp_serve", optimize()))
}

fn serve_input(stream: &[u8], l: ProtocolLimits) -> Vec<u8> {
    let mut input = vec![];
    limit_words(&mut input, l);
    input.extend_from_slice(stream);
    input
}

/// The frames of a server stream.
fn server_frames(output: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = vec![];
    let mut cursor = std::io::Cursor::new(output);
    let mut start = 0;
    while let Ok(Some(_)) = vesper_lsp::read_frame(&mut cursor, ProtocolLimits::default()) {
        let end = cursor.position() as usize;
        frames.push(output[start..end].to_vec());
        start = end;
    }
    frames
}

#[test]
fn stdio_loop_matches_server() {
    let mut differ = vec![];
    let (mut records, mut replies) = (0, 0);
    // (name, client stream, limits, the server's stream if recorded)
    type Stream<'a> = (String, Vec<u8>, ProtocolLimits, Option<&'a [u8]>);
    let mut streams: Vec<Stream<'_>> = sessions()
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.input.clone(),
                ProtocolLimits::default(),
                Some(s.output.as_slice()),
            )
        })
        .collect();
    for (i, (stream, l)) in frame_cases().into_iter().enumerate() {
        streams.push((format!("frame case {i}"), stream, l, None));
    }
    for (name, stream, l, output) in &streams {
        let r = rust_serve(stream, *l);
        let v = serve_entry().words(&serve_input(stream, *l));
        if r != v {
            differ.push(format!("{name}:\n rust {r:?}\n vesper {v:?}"));
            continue;
        }
        // Every refusal is the one the live server wrote, byte for byte.
        let mut at = 0;
        while at < v.len() {
            records += 1;
            match v[at] {
                0 => {
                    at += 1;
                    let reply = take_text(&v, &mut at);
                    replies += 1;
                    if let Some(output) = output
                        && !server_frames(output).contains(&reply)
                    {
                        differ.push(format!(
                            "{name}: the server did not write {:?}",
                            String::from_utf8_lossy(&reply)
                        ));
                    }
                }
                5 => at += 2,
                6 => at += 1,
                _ => at += 3,
            }
        }
    }
    eprintln!(
        "serve: {} streams, {records} records, {replies} refusals, {} differ",
        streams.len(),
        differ.len()
    );
    assert!(replies > 0);
    assert!(differ.is_empty(), "{differ:#?}");
}

// --------------------------------------------------------------------- memo

/// The queries of src/database.rs in @vesper/memo's numbering.
const QUERIES: [(&str, bool); 9] = [
    ("parse_file", false),
    ("check_file", false),
    ("diagnostics", true),
    ("guarded_analysis", false),
    ("signature_snapshot", true),
    ("signature_inventory", true),
    ("preview", false),
    ("recovery_input", true),
    ("recovery_query", false),
];
const Q_DIAGNOSTICS: u64 = 2;
const Q_GUARDED: u64 = 3;
const Q_INVENTORY: u64 = 5;
const Q_PREVIEW: u64 = 6;
const Q_RECOVERY: u64 = 8;
/// What a session asks for, in rotation.
const FETCHED: [u64; 5] = [Q_GUARDED, Q_DIAGNOSTICS, Q_INVENTORY, Q_PREVIEW, Q_RECOVERY];

/// A database like src/database.rs's, recording salsa's events.
#[salsa::db]
#[derive(Clone)]
struct Recorder {
    storage: salsa::Storage<Self>,
}
#[salsa::db]
impl salsa::Database for Recorder {}

type Events = std::sync::Arc<std::sync::Mutex<Vec<(u64, String)>>>;

impl Recorder {
    fn new(events: Events) -> Self {
        let callback = move |event: salsa::Event| {
            let record = match event.kind {
                salsa::EventKind::WillExecute { database_key } => (1, format!("{database_key:?}")),
                salsa::EventKind::DidValidateMemoizedValue { database_key } => {
                    (2, format!("{database_key:?}"))
                }
                _ => return,
            };
            events.lock().unwrap().push(record);
        };
        Recorder {
            storage: salsa::Storage::new(Some(Box::new(callback))),
        }
    }
}

/// An input row: a source text or a recovery snapshot, one per state.
enum RowStates {
    Source(Vec<String>),
    Recovery(Vec<std::sync::Arc<vesper_syntax::recovery::RecoverySyntax>>),
}

/// Distinct values of a query that compares them, so that a fingerprint is
/// the index of the first equal value (the value's own `PartialEq`).
#[derive(Default)]
struct Distinct {
    diagnostics: Vec<Vec<vesper::ast::Diagnostic>>,
    snapshots: Vec<vesper::database::SignatureSnapshot>,
    inventories: Vec<vesper::database::SignatureInventory>,
    recovery: Vec<std::sync::Arc<vesper_syntax::recovery::RecoverySyntax>>,
}

fn index_of<T: Clone>(seen: &mut Vec<T>, value: &T, eq: impl Fn(&T, &T) -> bool) -> u64 {
    match seen.iter().position(|v| eq(v, value)) {
        Some(i) => i as u64,
        None => {
            seen.push(value.clone());
            (seen.len() - 1) as u64
        }
    }
}

/// A recorded query result: input row, query, input state, fingerprint and
/// reads (tag, row, query).
type OracleRow = (u64, u64, u64, u64, Vec<[u64; 3]>);

/// One database lifetime of src/lsp/session.rs (it starts over when a
/// document closes and every 64 accepted changes): the inputs, salsa's
/// events, the script for `memo_replay` and the fingerprints fetched.
struct Epoch {
    db: Recorder,
    events: Events,
    files: std::collections::BTreeMap<String, (vesper::database::SourceFile, usize)>,
    recovery: std::collections::BTreeMap<
        String,
        Option<(
            vesper_syntax::incremental::SyntaxDocument,
            vesper::database::RecoveryFile,
            usize,
        )>,
    >,
    ids: std::collections::HashMap<String, usize>,
    rows: Vec<RowStates>,
    ops: Vec<u64>,
    fetches: Vec<(u64, Option<u64>)>,
    accepted: usize,
}

fn salsa_id(id: impl salsa::plumbing::AsId) -> String {
    format!("{:?}", id.as_id())
}

impl Epoch {
    fn new() -> Self {
        let events: Events = Default::default();
        Epoch {
            db: Recorder::new(events.clone()),
            events,
            files: Default::default(),
            recovery: Default::default(),
            ids: Default::default(),
            rows: vec![],
            ops: vec![],
            fetches: vec![],
            accepted: 0,
        }
    }
    fn new_source(&mut self, text: &str) -> (vesper::database::SourceFile, usize) {
        let file = vesper::database::SourceFile::new(&self.db, text.to_owned());
        let row = self.rows.len();
        self.rows.push(RowStates::Source(vec![text.to_owned()]));
        self.ids.insert(salsa_id(file), row);
        self.ops.push(1);
        (file, row)
    }
    fn open_recovery(
        &mut self,
        text: &str,
    ) -> Option<(
        vesper_syntax::incremental::SyntaxDocument,
        vesper::database::RecoveryFile,
        usize,
    )> {
        let document = vesper_syntax::incremental::SyntaxDocument::open(
            text,
            vesper_syntax::recovery::Limits::default(),
        )
        .ok()?;
        let arc = std::sync::Arc::clone(document.snapshot());
        let file = vesper::database::RecoveryFile::new(
            &self.db,
            vesper::database::SyntaxSnapshot::new(std::sync::Arc::clone(&arc)),
        );
        let row = self.rows.len();
        self.rows.push(RowStates::Recovery(vec![arc]));
        self.ids.insert(salsa_id(file), row);
        self.ops.push(1);
        Some((document, file, row))
    }
    /// `Session::accepted` for `uri`'s new text.
    fn accepted(&mut self, uri: &str, text: &str) {
        use salsa::Setter;
        self.accepted += 1;
        if let Some(&(file, row)) = self.files.get(uri) {
            if file.text(&self.db) != text {
                file.set_text(&mut self.db).to(text.to_owned());
                if let RowStates::Source(states) = &mut self.rows[row] {
                    states.push(text.to_owned());
                }
                self.ops.extend([2, row as u64]);
            }
        } else {
            let entry = self.new_source(text);
            self.files.insert(uri.to_owned(), entry);
        }
        let mirror = match self.recovery.remove(uri) {
            Some(Some((mut document, file, row))) => match document.update(text) {
                Ok(_) => {
                    let arc = std::sync::Arc::clone(document.snapshot());
                    file.set_snapshot(&mut self.db)
                        .to(vesper::database::SyntaxSnapshot::new(
                            std::sync::Arc::clone(&arc),
                        ));
                    if let RowStates::Recovery(states) = &mut self.rows[row] {
                        states.push(arc);
                    }
                    self.ops.extend([2, row as u64]);
                    Some((document, file, row))
                }
                Err(_) => None,
            },
            _ => self.open_recovery(text),
        };
        self.recovery.insert(uri.to_owned(), mirror);
    }
    /// What the session's requests ask for after step `k`, in rotation.
    fn fetch(&mut self, k: usize, seen: &mut Distinct) {
        let uris: Vec<String> = self.files.keys().cloned().collect();
        for uri in uris {
            for (j, &q) in FETCHED.iter().enumerate() {
                if (k + j).is_multiple_of(4) {
                    continue;
                }
                let (file, row) = self.files[&uri];
                let fp = match q {
                    Q_GUARDED => {
                        let _ = vesper::database::guarded_analysis(&self.db, file);
                        None
                    }
                    Q_DIAGNOSTICS => Some(index_of(
                        &mut seen.diagnostics,
                        vesper::database::diagnostics(&self.db, file),
                        |a, b| a == b,
                    )),
                    Q_INVENTORY => Some(index_of(
                        &mut seen.inventories,
                        vesper::database::signature_inventory(&self.db, file),
                        |a, b| a == b,
                    )),
                    Q_PREVIEW => {
                        let _ = vesper::database::macro_preview::preview(&self.db, file);
                        None
                    }
                    _ => {
                        let Some(Some((_, rfile, rrow))) = self.recovery.get(&uri) else {
                            continue;
                        };
                        let _ = vesper::database::recovery_diagnostics(&self.db, *rfile);
                        self.ops.extend([3, q, *rrow as u64]);
                        self.fetches.push((q, None));
                        continue;
                    }
                };
                self.ops.extend([3, q, row as u64]);
                self.fetches.push((q, fp));
            }
        }
    }
    /// salsa's events as `memo_replay`'s log records.
    fn log(&self) -> Vec<u64> {
        let mut out = vec![];
        for (kind, key) in self.events.lock().unwrap().iter() {
            let open = key.find('(').unwrap_or_else(|| panic!("event key {key}"));
            let name = key[..open].rsplit("::").next().unwrap();
            let q = QUERIES
                .iter()
                .position(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("unknown query {key}"));
            let id = &key[open + 1..key.len() - 1];
            let row = *self
                .ids
                .get(id)
                .unwrap_or_else(|| panic!("unknown input {key}"));
            out.extend([*kind, q as u64, row as u64]);
        }
        out
    }
    /// Every query's reads and value fingerprint on every input state, each
    /// from a fresh database (the oracle of memo.ves).
    fn oracle(&self, seen: &mut Distinct) -> Vec<OracleRow> {
        use vesper::database as d;
        let mut out = vec![];
        for (row, states) in self.rows.iter().enumerate() {
            let row = row as u64;
            let input = [0, row, 0];
            let query = |q: u64| [1, row, q];
            match states {
                RowStates::Source(texts) => {
                    // Per query: (state, fingerprint, reads).
                    type Result = (u64, u64, Vec<[u64; 3]>);
                    let mut per_query: Vec<Vec<Result>> = vec![vec![]; 7];
                    for (s, text) in texts.iter().enumerate() {
                        let s = s as u64;
                        let db = d::Database::default();
                        let file = d::SourceFile::new(&db, text.clone());
                        let parsed = d::parse_file(&db, file).diagnostics.is_empty();
                        let diagnostics =
                            index_of(&mut seen.diagnostics, d::diagnostics(&db, file), |a, b| {
                                a == b
                            });
                        let snapshot = index_of(
                            &mut seen.snapshots,
                            d::signature_snapshot(&db, file),
                            |a, b| a == b,
                        );
                        let inventory = index_of(
                            &mut seen.inventories,
                            d::signature_inventory(&db, file),
                            |a, b| a == b,
                        );
                        per_query[0].push((s, 0, vec![input]));
                        per_query[1].push((s, 0, vec![query(0)]));
                        let mut reads = vec![query(0)];
                        if parsed {
                            reads.push(query(1));
                        }
                        per_query[2].push((s, diagnostics, reads));
                        per_query[3].push((s, 0, vec![input]));
                        per_query[4].push((s, snapshot, vec![query(Q_GUARDED)]));
                        per_query[5].push((s, inventory, vec![query(4)]));
                        per_query[6].push((s, 0, vec![input]));
                    }
                    for (q, results) in per_query.into_iter().enumerate() {
                        for (s, fp, reads) in results {
                            out.push((row, q as u64, s, fp, reads));
                        }
                    }
                }
                RowStates::Recovery(arcs) => {
                    let fps: Vec<u64> = arcs
                        .iter()
                        .map(|a| index_of(&mut seen.recovery, a, std::sync::Arc::ptr_eq))
                        .collect();
                    for (s, fp) in fps.iter().enumerate() {
                        out.push((row, 7, s as u64, *fp, vec![input]));
                    }
                    for s in 0..arcs.len() {
                        out.push((row, Q_RECOVERY, s as u64, 0, vec![query(7)]));
                    }
                }
            }
        }
        out
    }
    /// The input of `memo_replay`.
    fn replay_input(&self, seen: &mut Distinct, cutoff: bool) -> Vec<u8> {
        let mut b = vec![];
        push_word(&mut b, QUERIES.len() as u64);
        for (_, eq) in QUERIES {
            push_word(&mut b, (eq && cutoff) as u64);
        }
        let oracle = self.oracle(seen);
        push_word(&mut b, oracle.len() as u64);
        for (f, q, s, fp, reads) in &oracle {
            for w in [*f, *q, *s, *fp, reads.len() as u64] {
                push_word(&mut b, w);
            }
            for r in reads {
                for w in r {
                    push_word(&mut b, *w);
                }
            }
        }
        // Each op is one to three words: count the ops, not the words.
        let mut count = 0;
        let mut at = 0;
        while at < self.ops.len() {
            at += match self.ops[at] {
                1 => 1,
                2 => 2,
                _ => 3,
            };
            count += 1;
        }
        push_word(&mut b, count);
        for w in &self.ops {
            push_word(&mut b, *w);
        }
        b
    }
}

/// salsa's events and fetched fingerprints against `memo_replay`'s, for
/// one epoch: the first difference, if any.
fn memo_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| memo().entry("memo_replay", optimize()))
}

fn memo_difference(epoch: &Epoch, seen: &mut Distinct, cutoff: bool) -> Option<String> {
    let entry = memo_entry();
    let input = epoch.replay_input(seen, cutoff);
    let words = entry.words(&input);
    let log = epoch.log();
    let fetched = epoch.fetches.len();
    if words.len() < fetched + 1 {
        return Some(format!("short result {words:?}"));
    }
    let (vlog, rest) = words.split_at(words.len() - fetched - 1);
    if vlog != log.as_slice() {
        let at = vlog.iter().zip(&log).take_while(|(a, b)| a == b).count();
        return Some(format!(
            "log differs at word {at}:\n salsa  {:?}\n vesper {:?}",
            &log[at.saturating_sub(6)..log.len().min(at + 12)],
            &vlog[at.saturating_sub(6)..vlog.len().min(at + 12)]
        ));
    }
    if rest[0] != fetched as u64 {
        return Some(format!("fetch count {} != {fetched}", rest[0]));
    }
    for (i, (q, fp)) in epoch.fetches.iter().enumerate() {
        if let Some(fp) = fp
            && rest[1 + i] != *fp
        {
            return Some(format!("fetch {i} of query {q}: {} != {fp}", rest[1 + i]));
        }
    }
    None
}

/// Runs a document script the way src/lsp/session.rs does: accepted texts
/// update the inputs, a close (or 64 accepted changes) starts a new
/// database with the open documents.
fn memo_epochs(ops: &[Op], seen: &mut Distinct) -> Vec<Epoch> {
    let mut store = DocumentStore::new(StoreLimits::default()).unwrap();
    let mut epochs = vec![Epoch::new()];
    let reset = |store: &DocumentStore, epochs: &mut Vec<Epoch>, open: &[String]| {
        let mut e = Epoch::new();
        for uri in open {
            if let Some(d) = store.get(uri) {
                let entry = e.new_source(d.text());
                e.files.insert(uri.clone(), entry);
                let mirror = e.open_recovery(d.text());
                e.recovery.insert(uri.clone(), mirror);
            }
        }
        epochs.push(e);
    };
    let mut open: Vec<String> = vec![];
    for (k, op) in ops.iter().enumerate() {
        let accepted = match op {
            Op::Open(uri, version, text) => store
                .open(uri, *version, text)
                .ok()
                .map(|d| (uri, d.text().to_owned())),
            Op::Change(uri, version, changes) => {
                let changes: Vec<ContentChange<'_>> = changes
                    .iter()
                    .map(|(range, range_length, text)| ContentChange {
                        range: *range,
                        range_length: *range_length,
                        text,
                    })
                    .collect();
                store
                    .change(uri, *version, &changes)
                    .ok()
                    .map(|d| (uri, d.text().to_owned()))
            }
            Op::Close(uri) => {
                if store.close(uri).is_ok() {
                    open.retain(|u| u != uri);
                    reset(&store, &mut epochs, &open);
                }
                None
            }
            Op::Get(_) | Op::SetRevision(_) => None,
        };
        if let Some((uri, text)) = accepted {
            if !open.contains(uri) {
                open.push(uri.clone());
            }
            let e = epochs.last_mut().unwrap();
            if e.accepted + 1 >= 64 {
                reset(&store, &mut epochs, &open);
            } else {
                e.accepted(uri, &text);
            }
        }
        epochs.last_mut().unwrap().fetch(k, seen);
    }
    epochs
}

/// Edits that exercise early cutoff across two documents: body-only edits
/// keep the signature (signature_inventory is backdated), comments, an
/// error and its fix, a reverted text, version-only changes and a close.
fn memo_scripts() -> Vec<Vec<Op>> {
    let s = |x: &str| x.to_string();
    let a = "untitled:a";
    let b = "untitled:b";
    let src = "fn helper(x: u64) -> u64 {\n    x + 1\n}\n\nfn main() -> u64 {\n    helper(41)\n}\n";
    let mut ops = vec![
        Op::Open(s(a), 1, s(src)),
        Op::Open(s(b), 1, s("fn b() -> u64 { 2 }\n")),
    ];
    let edits = [
        // A body edit: same signature.
        (a, edit(pos(1, 8), pos(1, 9), None, "2")),
        // A comment.
        (b, edit(pos(0, 0), pos(0, 0), None, "// b\n")),
        // A signature edit.
        (a, edit(pos(0, 13), pos(0, 16), None, "u32")),
        // A parse error, then its fix.
        (b, edit(pos(1, 0), pos(1, 2), None, "fn(")),
        (b, edit(pos(1, 0), pos(1, 3), None, "fn")),
        // Back to the signature, then the opened text.
        (a, edit(pos(0, 13), pos(0, 16), None, "u64")),
        (a, edit(pos(1, 8), pos(1, 9), None, "1")),
        // A version-only change.
        (b, (None, None, s("// b\nfn b() -> u64 { 2 }\n"))),
        // A type error in a body.
        (a, edit(pos(1, 4), pos(1, 5), None, "y")),
    ];
    for (i, (uri, change)) in edits.into_iter().enumerate() {
        ops.push(Op::Change(s(uri), 2 + i as i32, vec![change]));
    }
    ops.push(Op::Close(s(b)));
    ops.push(Op::Change(s(a), 20, vec![full("fn main() -> u64 { 1 }\n")]));
    let mut many = vec![Op::Open(s(a), 1, s(src))];
    // Past 64 accepted changes the session starts a new database.
    for v in 2..80 {
        let text = if v % 2 == 0 {
            "fn main() -> u64 { 1 }\n"
        } else {
            src
        };
        many.push(Op::Change(s(a), v, vec![full(text)]));
    }
    vec![ops, many]
}

#[test]
fn memo_invalidation_matches_salsa() {
    let mut scripts = memo_scripts();
    for s in sessions() {
        let ops = session_ops(&s.input);
        if !ops.is_empty() {
            scripts.push(ops);
        }
    }
    let mut differ = vec![];
    let (mut epochs, mut fetches, mut without_cutoff) = (0, 0, 0);
    let mut kinds = [0usize; 3];
    let mut backdated = 0;
    for (i, script) in scripts.iter().enumerate() {
        let mut seen = Distinct::default();
        for (j, epoch) in memo_epochs(script, &mut seen).iter().enumerate() {
            epochs += 1;
            fetches += epoch.fetches.len();
            let log = epoch.log();
            for r in log.chunks(3) {
                kinds[r[0] as usize] += 1;
            }
            // An equal signature stops propagation: signature_snapshot
            // executes again, signature_inventory is only validated.
            backdated += log
                .chunks(3)
                .zip(log.chunks(3).skip(1))
                .filter(|(a, b)| a[..2] == [1, 4] && b[..2] == [2, 5] && a[2] == b[2])
                .count();
            if let Some(d) = memo_difference(epoch, &mut seen, true) {
                differ.push(format!("script {i} epoch {j}: {d}"));
            }
            // The same replay without early cutoff must differ wherever
            // salsa backdated: the comparison sees cutoff.
            without_cutoff += memo_difference(epoch, &mut seen, false).is_some() as usize;
        }
    }
    eprintln!(
        "memo: {} scripts, {epochs} databases, {} executions, {} validations, {backdated} \
         signature cutoffs, {fetches} fetches, {} differ ({without_cutoff} databases differ \
         without cutoff)",
        scripts.len(),
        kinds[1],
        kinds[2],
        differ.len()
    );
    assert!(backdated > 0 && without_cutoff > 0);
    assert!(differ.is_empty(), "{differ:#?}");
}

// -------------------------------------------------------------------- speed

/// ABBA wall time of each layer over every recorded session, both sides
/// with their inputs built up front: framing (both streams of each
/// session), envelope parsing (every body), encoding (every server
/// response), the document store (every session's edits) and the
/// transport loop (every client stream: framing, parsing, refusals). The memo
/// bookkeeping has no Rust counterpart without salsa's query bodies, so
/// it is timed alone, per fetch. `CHECK_HARNESS_SPEED=1`.
#[test]
#[ignore]
fn lsp_core_speed() {
    if !speed() {
        return;
    }
    let l = ProtocolLimits::default();
    let streams: Vec<&[u8]> = sessions()
        .iter()
        .flat_map(|s| [s.input.as_slice(), s.output.as_slice()])
        .collect();
    let frame_inputs: Vec<Vec<u8>> = streams
        .iter()
        .map(|st| {
            let mut i = vec![];
            limit_words(&mut i, l);
            i.extend_from_slice(st);
            i
        })
        .collect();
    let frames = lsp().entry("lsp_frames", true);
    let mut bodies = vec![];
    let mut envelopes = vec![];
    for (k, st) in streams.iter().enumerate() {
        let mut cursor = std::io::Cursor::new(*st);
        while let Ok(Some(body)) = vesper_lsp::read_frame(&mut cursor, l) {
            if k % 2 == 1
                && let Ok(env) = vesper_lsp::parse_envelope(&body, l)
            {
                envelopes.push(env);
            }
            bodies.push(body);
        }
    }
    let body_inputs: Vec<Vec<u8>> = bodies
        .iter()
        .map(|b| {
            let mut i = vec![];
            limit_words(&mut i, l);
            i.extend_from_slice(b);
            i
        })
        .collect();
    let envelope = lsp().entry("lsp_envelope", true);
    let encode_inputs: Vec<Vec<u8>> = envelopes.iter().map(|e| encode_input(e, l)).collect();
    let encode = lsp().entry("lsp_encode", true);
    let scripts: Vec<Vec<Op>> = sessions()
        .iter()
        .map(|s| session_ops(&s.input))
        .filter(|o| !o.is_empty())
        .collect();
    let store_inputs: Vec<Vec<u8>> = scripts
        .iter()
        .map(|o| documents_input(StoreLimits::default(), o))
        .collect();
    let store = documents().entry("lsp_documents", true);
    let rounds = 20;
    let per = |d: std::time::Duration, n: usize| d / (rounds * n) as u32;
    let (r, v) = abba(
        "frames",
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for st in &streams {
                        std::hint::black_box(rust_frames(st, l));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for i in &frame_inputs {
                        std::hint::black_box(frames.words(i));
                    }
                }
            })
        },
    );
    eprintln!(
        "  {} streams: per stream rust {:?} vesper {:?}",
        streams.len(),
        per(r, streams.len()),
        per(v, streams.len())
    );
    let (r, v) = abba(
        "envelopes",
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for b in &bodies {
                        std::hint::black_box(vesper_lsp::parse_envelope(b, l).ok());
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for i in &body_inputs {
                        std::hint::black_box(envelope.words(i));
                    }
                }
            })
        },
    );
    eprintln!(
        "  {} bodies: per body rust {:?} vesper {:?}",
        bodies.len(),
        per(r, bodies.len()),
        per(v, bodies.len())
    );
    let (r, v) = abba(
        "encode",
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for e in &envelopes {
                        std::hint::black_box(vesper_lsp::encode_frame(e, l).ok());
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for i in &encode_inputs {
                        std::hint::black_box(encode.words(i));
                    }
                }
            })
        },
    );
    eprintln!(
        "  {} responses: per response rust {:?} vesper {:?}",
        envelopes.len(),
        per(r, envelopes.len()),
        per(v, envelopes.len())
    );
    let (r, v) = abba(
        "documents",
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for o in &scripts {
                        std::hint::black_box(rust_documents(StoreLimits::default(), o));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for i in &store_inputs {
                        std::hint::black_box(store.words(i));
                    }
                }
            })
        },
    );
    let ops: usize = scripts.iter().map(Vec::len).sum();
    eprintln!(
        "  {ops} store ops: per op rust {:?} vesper {:?}",
        per(r, ops),
        per(v, ops)
    );
    let clients: Vec<&[u8]> = sessions().iter().map(|s| s.input.as_slice()).collect();
    let serve_inputs: Vec<Vec<u8>> = clients.iter().map(|c| serve_input(c, l)).collect();
    let serve = lsp().entry("lsp_serve", true);
    let (r, v) = abba(
        "serve",
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for c in &clients {
                        std::hint::black_box(rust_serve(c, l));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..rounds {
                    for i in &serve_inputs {
                        std::hint::black_box(serve.words(i));
                    }
                }
            })
        },
    );
    let messages: usize = clients.iter().map(|c| server_frames(c).len()).sum();
    eprintln!(
        "  {messages} client messages: per message rust {:?} vesper {:?}",
        per(r, messages),
        per(v, messages)
    );
    let mut memo_inputs = vec![];
    let mut fetches = 0;
    for script in memo_scripts().iter().chain(&scripts) {
        let mut seen = Distinct::default();
        for epoch in memo_epochs(script, &mut seen) {
            fetches += epoch.fetches.len();
            memo_inputs.push(epoch.replay_input(&mut seen, true));
        }
    }
    let replay = memo().entry("memo_replay", true);
    let best = (0..6)
        .map(|_| {
            time(|| {
                for _ in 0..rounds {
                    for i in &memo_inputs {
                        std::hint::black_box(replay.words(i));
                    }
                }
            })
        })
        .min()
        .unwrap();
    eprintln!(
        "memo: {fetches} fetches, per fetch {:?} (replay including oracle decoding)",
        per(best, fetches)
    );
}
