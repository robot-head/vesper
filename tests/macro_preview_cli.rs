//! Actual opt-in stdio previews remain separate from original-source semantics.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const REPEATED: &str = include_str!("fixtures/macro_preview/repeated.ves");
const PURE: &str = include_str!("fixtures/macro_preview/helper_pure.ves");
const FALLIBLE: &str = include_str!("fixtures/macro_preview/helper_fallible.ves");
const LINEAR: &str = include_str!("fixtures/macro_preview/linear_duplicate.ves");
const MALFORMED: &str = include_str!("fixtures/macro_preview/malformed.ves");

fn request(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn open(uri: &str, version: i32, source: &str) -> Value {
    notice(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"version":version,"languageId":"vesper","text":source}}),
    )
}
fn change(uri: &str, version: i32, source: &str) -> Value {
    notice(
        "textDocument/didChange",
        json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":source}]}),
    )
}
fn preview(id: i32, uri: &str, version: i32) -> Value {
    request(
        id,
        "vesper/macroPreview",
        json!({"textDocument":{"uri":uri,"version":version}}),
    )
}
fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_MACRO_PREVIEW_BINARY")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_vesper").map(PathBuf::from))
        .expect("actual vesper executable or VESPER_MACRO_PREVIEW_BINARY required");
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}
fn run(mut messages: Vec<Value>) -> Vec<Value> {
    messages.insert(0, request(1, "initialize", json!({"capabilities":{}})));
    messages.extend([
        request(9999, "shutdown", json!({})),
        notice("exit", json!({})),
    ]);
    let input = messages.iter().fold(Vec::new(), |mut bytes, message| {
        let body = serde_json::to_vec(message).unwrap();
        bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        bytes.extend_from_slice(&body);
        bytes
    });
    let mut child = Command::new(binary())
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
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("macro preview stdio timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap().unwrap();
    let bytes = reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(bytes.len() <= 16 * 1024 * 1024 && errors.len() <= 1024 * 1024);
    assert!(status.success(), "{}", String::from_utf8_lossy(&errors));
    let mut rest = bytes.as_slice();
    let mut replies = Vec::new();
    while !rest.is_empty() {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("frame header");
        assert!(end <= 8192);
        let header = std::str::from_utf8(&rest[..end]).unwrap();
        let sizes = header
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(sizes.len(), 1);
        let size = sizes[0];
        assert!(size <= 2 * 1024 * 1024 && rest.len() >= end + 4 + size);
        let reply: Value = serde_json::from_slice(&rest[end + 4..end + 4 + size]).unwrap();
        assert_eq!(reply["jsonrpc"], "2.0");
        replies.push(reply);
        rest = &rest[end + 4 + size..];
    }
    assert!(result(&replies, 9999).is_null());
    replies
}
fn response(replies: &[Value], id: i32) -> &Value {
    let found = replies
        .iter()
        .filter(|r| r.get("id") == Some(&json!(id)))
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "id {id}: {replies:?}");
    found[0]
}
fn result(replies: &[Value], id: i32) -> &Value {
    let response = response(replies, id);
    assert!(response.get("error").is_none(), "{response:?}");
    response.get("result").unwrap()
}
fn error(replies: &[Value], id: i32, code: i32) {
    assert_eq!(response(replies, id)["error"]["code"], code);
    assert!(response(replies, id).get("result").is_none());
}
fn envelope(value: &Value, uri: &str, version: i32, source: &str) {
    assert_eq!(value["schema"], "vesper.editor.macro-preview/1");
    assert_eq!(value["scope"], "expanded_source_only");
    assert_eq!(value["tier"], 1);
    assert_eq!(value["uri"], uri);
    assert_eq!(value["version"], version);
    assert_eq!(
        value["source_blake3"],
        blake3::hash(source.as_bytes()).to_hex().as_str()
    );
    assert_eq!(value["range_unit"], "utf8_bytes");
    for field in ["world", "checked_source", "approved", "original_checked"] {
        assert!(value.get(field).is_none(), "unexpected {field}");
    }
}
fn accepted<'a>(value: &'a Value, uri: &str, version: i32, source: &str) -> &'a str {
    envelope(value, uri, version, source);
    assert_eq!(value["status"], "expanded_checked");
    let expanded = value["expanded_source"].as_str().unwrap();
    assert_eq!(
        value["expanded_blake3"],
        blake3::hash(expanded.as_bytes()).to_hex().as_str()
    );
    assert_eq!(value["source_bytes"], source.len());
    assert_eq!(value["expanded_bytes"], expanded.len());
    assert_eq!(value["package"], "@local/source");
    assert!(!value["source_map"].as_array().unwrap().is_empty());
    assert_eq!(
        value["audits"].as_array().unwrap().len() as u64,
        value["invocations"].as_u64().unwrap()
    );
    expanded
}
fn rejected(value: &Value, uri: &str, version: i32, source: &str) {
    envelope(value, uri, version, source);
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["source_map"], json!([]));
    assert_eq!(value["audits"], json!([]));
    assert!(!value["message"].as_str().unwrap().is_empty());
    for field in [
        "expanded_blake3",
        "source_bytes",
        "expanded_bytes",
        "macro_definitions",
        "invocations",
        "package",
    ] {
        assert!(
            value.get(field).is_none(),
            "accepted field {field} on rejection"
        );
    }
}

#[test]
fn preview_is_opt_in_and_does_not_promote_original_source_semantics() {
    let uri = "untitled:preview/🦀";
    let replies = run(vec![
        open(uri, 1, REPEATED),
        preview(2, uri, 1),
        preview(3, uri, 1),
        request(
            4,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":{"line":1,"character":3}}),
        ),
    ]);
    assert_eq!(
        result(&replies, 1)["capabilities"]["experimental"]["vesperMacroPreview"],
        json!({"version":1,"tier":1,"scope":"expanded_source_only"})
    );
    accepted(result(&replies, 2), uri, 1, REPEATED);
    assert_eq!(result(&replies, 2), result(&replies, 3));
    error(&replies, 4, -32003);
    let publications = replies
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .collect::<Vec<_>>();
    assert_eq!(
        publications.len(),
        1,
        "preview must not publish or clear diagnostics"
    );
    assert_eq!(publications[0]["params"]["vesper"]["status"], "parse_error");
    assert_eq!(publications[0]["params"]["version"], 1);
}

#[test]
fn helper_body_changes_recheck_authority_and_failure_never_leaks_accepted_maps() {
    let uri = "opaque:authority";
    let replies = run(vec![
        open(uri, 1, PURE),
        preview(2, uri, 1),
        change(uri, 2, FALLIBLE),
        preview(3, uri, 2),
        change(uri, 3, LINEAR),
        preview(4, uri, 3),
        change(uri, 4, PURE),
        preview(5, uri, 4),
    ]);
    accepted(result(&replies, 2), uri, 1, PURE);
    rejected(result(&replies, 3), uri, 2, FALLIBLE);
    assert_eq!(result(&replies, 3)["stage"], "authority");
    rejected(result(&replies, 4), uri, 3, LINEAR);
    assert_eq!(result(&replies, 4)["stage"], "check");
    assert_eq!(result(&replies, 4)["diagnostic_source"], "expanded");
    assert!(
        result(&replies, 4)["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "E0303")
    );
    accepted(result(&replies, 5), uri, 4, PURE);
}

#[test]
fn reset_noop_stale_close_and_reopen_never_return_a_previous_source() {
    let uri = "file:///not-read/macro-preview.ves";
    let foreign = "file:///not-read/foreign.ves";
    let mut messages = vec![open(uri, 1, REPEATED), preview(2, uri, 1)];
    let mut last = String::new();
    for version in 2..=67 {
        last = format!("// revision {version} 😀\r\n{REPEATED}");
        messages.push(change(uri, version, &last));
    }
    messages.extend([
        preview(3, uri, 67),
        change(uri, 68, &last),
        preview(4, uri, 68),
        preview(5, uri, 67),
        change(uri, 68, FALLIBLE),
        preview(6, uri, 68),
        preview(7, foreign, 68),
        notice("textDocument/didClose", json!({"textDocument":{"uri":uri}})),
        preview(8, uri, 68),
        open(uri, 1, PURE),
        preview(9, uri, 1),
    ]);
    let replies = run(messages);
    accepted(result(&replies, 3), uri, 67, &last);
    accepted(result(&replies, 4), uri, 68, &last);
    assert_eq!(
        result(&replies, 3)["expanded_blake3"],
        result(&replies, 4)["expanded_blake3"]
    );
    error(&replies, 5, -32801);
    accepted(result(&replies, 6), uri, 68, &last);
    error(&replies, 7, -32602);
    error(&replies, 8, -32602);
    accepted(result(&replies, 9), uri, 1, PURE);
}

#[test]
fn client_context_unknown_fields_and_invalid_versions_are_not_authority() {
    let uri = "untitled:params";
    let invalid = [
        json!({}),
        json!({"textDocument":{"uri":uri,"version":null}}),
        json!({"textDocument":{"uri":uri,"version":1.5}}),
        json!({"textDocument":{"uri":uri,"version":2147483648_i64}}),
        json!({"textDocument":{"uri":uri},"approved":true}),
        json!({"textDocument":{"uri":uri},"policy":{}}),
        json!({"textDocument":{"uri":uri},"world":{}}),
        json!({"textDocument":{"uri":uri,"source":PURE}}),
        json!({"textDocument":{"uri":uri,"approved":true}}),
        json!({"textDocument":{"uri":uri},"graph":{}}),
        json!({"textDocument":{"uri":uri},"source":PURE}),
        json!({"textDocument":{"uri":uri,"policy":{}}}),
    ];
    let mut messages = vec![open(uri, 1, REPEATED)];
    messages.extend(
        invalid
            .iter()
            .enumerate()
            .map(|(i, p)| request(10 + i as i32, "vesper/macroPreview", p.clone())),
    );
    messages.push(request(
        50,
        "vesper/macroPreview",
        json!({"textDocument":{"uri":uri}}),
    ));
    let replies = run(messages);
    for i in 0..invalid.len() {
        error(&replies, 10 + i as i32, -32602);
    }
    accepted(result(&replies, 50), uri, 1, REPEATED);
}

#[test]
fn malformed_source_has_original_diagnostics_and_oversize_is_unavailable() {
    let uri = "untitled:refusals";
    let oversize = format!("//{}\n{REPEATED}", "x".repeat(128 * 1024));
    let replies = run(vec![
        open(uri, 1, MALFORMED),
        preview(2, uri, 1),
        change(uri, 2, &oversize),
        preview(3, uri, 2),
        change(uri, 3, REPEATED),
        preview(4, uri, 3),
    ]);
    rejected(result(&replies, 2), uri, 1, MALFORMED);
    assert_eq!(result(&replies, 2)["stage"], "parse");
    assert_eq!(result(&replies, 2)["diagnostic_source"], "original");
    error(&replies, 3, -32003);
    accepted(result(&replies, 4), uri, 3, REPEATED);
}
