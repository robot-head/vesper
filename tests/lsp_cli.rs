//! Black-box tests of the actual framed stdio server, including immutable revisions.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
fn request(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn frame(body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    bytes.extend_from_slice(body);
    bytes
}
fn stream(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .flat_map(|m| frame(&serde_json::to_vec(m).unwrap()))
        .collect()
}
fn run(input: Vec<u8>) -> (bool, Vec<Value>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || stdin.write_all(&input));
    let mut stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut b = Vec::new();
        stdout
            .by_ref()
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut b)
            .unwrap();
        b
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = thread::spawn(move || {
        let mut b = Vec::new();
        stderr
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut b)
            .unwrap();
        b
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("stdio server timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let write = writer.join().unwrap();
    if status.success() {
        write.unwrap();
    }
    let bytes = reader.join().unwrap();
    assert!(bytes.len() <= 8 * 1024 * 1024, "unbounded response");
    let stderr = errors.join().unwrap();
    assert!(stderr.len() <= 1024 * 1024);
    let mut cursor = std::io::Cursor::new(bytes);
    let mut messages = Vec::new();
    while let Some(message) =
        vesper_lsp::read_frame(&mut cursor, vesper_lsp::ProtocolLimits::default()).unwrap()
    {
        vesper_lsp::parse_envelope(&message, vesper_lsp::ProtocolLimits::default()).unwrap();
        messages.push(serde_json::from_slice(&message).unwrap());
    }
    (
        status.success(),
        messages,
        String::from_utf8(stderr).unwrap(),
    )
}
fn result(messages: &[Value], id: i32) -> &Value {
    let found = messages
        .iter()
        .filter(|m| m.get("id") == Some(&json!(id)))
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "response {id}: {messages:?}");
    assert!(found[0].get("error").is_none(), "{:?}", found[0]);
    found[0].get("result").expect("response result")
}
fn error(messages: &[Value], id: i32, code: i32) {
    let found = messages
        .iter()
        .filter(|m| m.get("id") == Some(&json!(id)))
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["error"]["code"], code);
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
#[test]
fn actual_stdio_unicode_navigation_follows_only_accepted_revisions() {
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
    let (ok, replies, stderr) = run(stream(&messages));
    assert!(ok, "{stderr}\n{replies:?}");
    let caps = &result(&replies, 1)["capabilities"];
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["textDocumentSync"]["change"], 2);
    for cap in [
        "hoverProvider",
        "definitionProvider",
        "documentSymbolProvider",
        "workspaceSymbolProvider",
        "documentFormattingProvider",
    ] {
        assert_eq!(caps[cap], true, "{cap}");
    }
    assert_eq!(
        result(&replies, 2)[1]["selectionRange"]["start"],
        json!({"line":2,"character":3})
    );
    assert!(
        result(&replies, 3)["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("u64")
    );
    for id in [4, 6] {
        assert_eq!(result(&replies, id)["uri"], uri);
        assert_eq!(
            result(&replies, id)["range"]["start"],
            json!({"line":1,"character":3})
        );
    }
    for id in [5, 7, 8] {
        assert_eq!(result(&replies, id)[1]["name"], "main");
        assert_eq!(
            result(&replies, id)[1]["selectionRange"]["start"],
            json!({"line":4,"character":3})
        );
    }
    error(&replies, 9, -32602);
    assert_eq!(result(&replies, 10), &json!([]));
    let versions = replies
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .filter_map(|m| m["params"]["version"].as_i64())
        .collect::<Vec<_>>();
    assert!(
        versions.contains(&3),
        "valid version3 edit must succeed after invalid version3 edit"
    );
}
#[test]
fn actual_stdio_lifecycle_duplicate_body_recovery_and_unsupported_analysis() {
    let mut bytes = stream(&[
        request(20, "textDocument/hover", at("opaque:x", 0, 0)),
        initialize(),
    ]);
    bytes.extend(frame(
        br#"{"jsonrpc":"2.0","id":77,"method":"shutdown","\u006dethod":"initialize"}"#,
    ));
    bytes.extend(frame(b"{bad JSON"));
    let uri = "opaque:unsupported";
    let messages = vec![
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
    ];
    bytes.extend(stream(&messages));
    let (ok, replies, stderr) = run(bytes);
    assert!(ok, "{stderr}");
    error(&replies, 20, -32002);
    error(&replies, 21, -32601);
    error(&replies, 22, -32003);
    error(&replies, 23, -32602);
    error(&replies, 25, -32600);
    let unavailable = replies.iter().find(|m| m["id"] == 22).unwrap();
    assert_eq!(
        unavailable["error"]["data"]["status"],
        "analysis_unavailable"
    );
    assert_eq!(unavailable["error"]["data"]["uri"], uri);
    for code in [-32600, -32700] {
        assert!(
            replies
                .iter()
                .any(|m| m["id"].is_null() && m["error"]["code"] == code),
            "{replies:?}"
        );
    }
    assert_eq!(result(&replies, 24), &Value::Null);
}
#[test]
fn actual_stdio_framing_errors_and_early_exit_fail_the_process() {
    for input in [
        b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
        b"Content-Length: 9\r\n\r\n{}".to_vec(),
        b"Content-Length: 0\n\n".to_vec(),
        vec![b'x'; 8193],
        stream(&[notice("exit", json!({}))]),
    ] {
        let (ok, replies, _) = run(input);
        assert!(!ok, "fatal input unexpectedly succeeded: {replies:?}");
    }
}

#[test]
fn actual_stdio_document_symbols_respect_legacy_client_shape() {
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
        let (ok, replies, stderr) = run(stream(&messages));
        assert!(ok, "{stderr}");
        let symbols = result(&replies, 2).as_array().unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "main");
        assert_eq!(symbols[0]["location"]["uri"], uri);
        assert!(symbols[0]["location"]["range"]["start"]["line"].is_u64());
        assert!(symbols[0].get("selectionRange").is_none());
    }
}

#[test]
fn actual_stdio_code_actions_require_versioned_literal_client_support() {
    let literal = json!({"codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","source.fixAll.vesper"]}}}});
    let cases = [
        json!({}),
        json!({"workspace":{"workspaceEdit":{"documentChanges":false}},"textDocument":literal}),
        json!({"workspace":{"workspaceEdit":{"documentChanges":true}}}),
        json!({"textDocument":literal}),
        json!({"workspace":{"workspaceEdit":{"documentChanges":true}},"textDocument":{"codeAction":{"codeActionLiteralSupport":false}}}),
    ];
    for caps in cases {
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
        let (ok, replies, stderr) = run(stream(&messages));
        assert!(ok, "{stderr}");
        assert_eq!(
            result(&replies, 1)["capabilities"]["codeActionProvider"],
            false
        );
        error(&replies, 2, -32003);
        let response = replies.iter().find(|m| m["id"] == 2).unwrap();
        assert_eq!(response["error"]["data"]["status"], "analysis_unavailable");
        assert_eq!(response["error"]["data"]["kind"], "client_capability");
        assert!(response.get("result").is_none());
    }
}
