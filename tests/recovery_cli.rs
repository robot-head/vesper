//! Actual original-buffer recovery, strict semantic gates and CST update paths.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    ops::Range,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const DAMAGED: &str = "// 😀\r\nlinear struct Tx{}\r\nfn damaged(/*🦀*/sink tx:Tx){tx.}\r\nfn leak(/*😀*/sink other:Tx){}\r\n";
const EMPTY_REPAIR: &str = "// 😀\r\nlinear struct Tx{}\r\nfn damaged(/*🦀*/sink tx:Tx){}\r\nfn leak(/*😀*/sink other:Tx){}\r\n";
const VALID_REPAIR: &str = "// 😀\r\nlinear struct Tx{}\r\nfn damaged(/*🦀*/sink tx:Tx){let Tx{}=tx;}\r\nfn leak(/*😀*/sink other:Tx){let Tx{}=other;}\r\n";

fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_RECOVERY_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_vesper")));
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}
fn request(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn initialize() -> Value {
    request(
        1,
        "initialize",
        json!({"capabilities":{"general":{"positionEncodings":["utf-16"]},
        "workspace":{"workspaceEdit":{"documentChanges":true}},"textDocument":{
            "publishDiagnostics":{"relatedInformation":true,"dataSupport":true},
            "codeAction":{"codeActionLiteralSupport":{}}}}}),
    )
}
fn open(uri: &str, source: &str) -> Value {
    notice(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"version":1,"languageId":"vesper","text":source}}),
    )
}
fn change(uri: &str, source: &str, version: i32) -> Value {
    notice(
        "textDocument/didChange",
        json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":source}]}),
    )
}
fn status_request(id: i32, uri: &str) -> Value {
    request(
        id,
        "vesper/recoveryStatus",
        json!({"textDocument":{"uri":uri}}),
    )
}
fn run(mut messages: Vec<Value>) -> Vec<Value> {
    messages.extend([
        request(900, "shutdown", json!({})),
        notice("exit", json!({})),
    ]);
    let mut input = Vec::new();
    for message in messages {
        let bytes = serde_json::to_vec(&message).unwrap();
        write!(input, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
        input.extend(bytes);
    }
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
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("recovery stdio timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap().unwrap();
    let bytes = reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&errors));
    assert!(bytes.len() <= 16 * 1024 * 1024 && errors.len() <= 1024 * 1024);
    let mut cursor = std::io::Cursor::new(bytes);
    let mut output = Vec::new();
    while let Some(bytes) =
        vesper_lsp::read_frame(&mut cursor, vesper_lsp::ProtocolLimits::default()).unwrap()
    {
        vesper_lsp::parse_envelope(&bytes, vesper_lsp::ProtocolLimits::default()).unwrap();
        output.push(serde_json::from_slice(&bytes).unwrap());
    }
    output
}
fn result(messages: &[Value], id: i32) -> &Value {
    let found: Vec<_> = messages.iter().filter(|m| m["id"] == id).collect();
    assert_eq!(found.len(), 1, "{messages:?}");
    assert!(found[0].get("error").is_none(), "{:?}", found[0]);
    found[0].get("result").unwrap()
}
fn error(messages: &[Value], id: i32, code: i32) -> &Value {
    let found: Vec<_> = messages.iter().filter(|m| m["id"] == id).collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["error"]["code"], code, "{:?}", found[0]);
    &found[0]["error"]
}
fn publications<'a>(messages: &'a [Value], uri: &str) -> Vec<&'a Value> {
    messages
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .map(|m| &m["params"])
        .collect()
}
fn span(source: &str, text: &str) -> Range<usize> {
    let start = source.find(text).unwrap();
    start..start + text.len()
}
fn position(source: &str, offset: usize) -> Value {
    assert!(source.is_char_boundary(offset));
    assert!(
        !(offset > 0
            && source.as_bytes()[offset - 1] == b'\r'
            && source.as_bytes().get(offset) == Some(&b'\n'))
    );
    let (mut line, mut character) = (0u32, 0u32);
    let mut chars = source[..offset].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line += 1;
                character = 0;
            }
            '\n' => {
                line += 1;
                character = 0;
            }
            _ => character += c.len_utf16() as u32,
        }
    }
    json!({"line":line,"character":character})
}
fn lsp_range(source: &str, span: Range<usize>) -> Value {
    json!({"start":position(source,span.start),"end":position(source,span.end)})
}
fn raw_span(value: &Value, source: &str) -> Range<usize> {
    let start = usize::try_from(value["start"].as_u64().unwrap()).unwrap();
    let end = usize::try_from(value["end"].as_u64().unwrap()).unwrap();
    assert!(
        start <= end
            && end <= source.len()
            && source.is_char_boundary(start)
            && source.is_char_boundary(end)
    );
    start..end
}
fn syntax_status<'a>(
    messages: &'a [Value],
    id: i32,
    uri: &str,
    source: &str,
    version: i32,
) -> &'a Value {
    let status = result(messages, id);
    assert_eq!(status["scope"], "syntax_only");
    assert_eq!(status["uri"], uri);
    assert_eq!(status["version"], version);
    assert_eq!(
        status["source_blake3"],
        blake3::hash(source.as_bytes()).to_hex().as_str()
    );
    assert_eq!(status["byte_length"], source.len());
    for forbidden in [
        "source", "world", "types", "checked", "liveness", "proof", "verified",
    ] {
        assert!(
            status.get(forbidden).is_none(),
            "syntax response must not mint {forbidden}"
        );
    }
    let declarations = status["declarations"].as_array().unwrap();
    for d in declarations {
        let declared = raw_span(&d["span"], source);
        let header = raw_span(&d["header_span"], source);
        assert!(declared.start <= header.start && header.end <= declared.end);
        if !d["body_span"].is_null() {
            let body = raw_span(&d["body_span"], source);
            assert!(declared.start <= body.start && body.end <= declared.end);
        }
        assert!(matches!(
            d["status"].as_str(),
            Some("complete" | "recovered" | "opaque")
        ));
    }
    for site in status["sites"].as_array().unwrap() {
        raw_span(&site["span"], source);
        if let Some(owner) = site["declaration_index"].as_u64() {
            assert!(usize::try_from(owner).unwrap() < declarations.len());
        }
        if !site["receiver"].is_null() {
            raw_span(&site["receiver"]["root"], source);
            for p in site["receiver"]["projections"].as_array().unwrap() {
                raw_span(p, source);
            }
        }
    }
    status
}
fn declaration<'a>(status: &'a Value, name: &str) -> (usize, &'a Value) {
    let found: Vec<_> = status["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, d)| d["name"] == name)
        .collect();
    assert_eq!(found.len(), 1, "{status:?}");
    found[0]
}
fn assert_hash(publication: &Value, source: &str, version: i32) {
    assert_eq!(publication["version"], version);
    assert_eq!(
        publication["vesper"]["source_blake3"],
        blake3::hash(source.as_bytes()).to_hex().as_str()
    );
}
fn leak<'a>(publication: &'a Value, name: &str) -> &'a Value {
    let message = format!(
        "linear value '{name}' must be consumed exactly once; destructure it or pass it to a sink parameter"
    );
    let found: Vec<_> = publication["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "E0302" && d["message"] == message)
        .collect();
    assert_eq!(found.len(), 1, "{publication:?}");
    found[0]
}
fn assert_independent_leak(publication: &Value, source: &str, uri: &str, status: &Value) {
    assert_eq!(publication["vesper"]["status"], "parse_error");
    let recovery = &publication["vesper"]["recovery"];
    assert_eq!(recovery["scope"], "independent_functions");
    let (_, damaged) = declaration(status, "damaged");
    let (_, complete) = declaration(status, "leak");
    assert_eq!(damaged["status"], "recovered");
    assert_eq!(complete["status"], "complete");
    assert!(
        recovery["analyzed_functions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["span"] == complete["span"])
    );
    assert!(
        recovery["unavailable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["span"] == damaged["span"] && f["reason"] == "damaged_function")
    );
    assert!(
        !recovery["analyzed_functions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["span"] == damaged["span"])
    );
    let diagnostic = leak(publication, "other");
    assert_eq!(
        diagnostic["range"],
        lsp_range(source, raw_span(&complete["body_span"], source))
    );
    let related = diagnostic["relatedInformation"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["location"]["uri"], uri);
    assert_eq!(
        related[0]["location"]["range"],
        lsp_range(source, span(source, "sink other:Tx"))
    );
    assert_eq!(related[0]["message"], "linear binding introduced here");
    assert_eq!(
        diagnostic["data"]["help"],
        "Every owned linear binding must be consumed exactly once on every returning path."
    );
    assert!(
        !publication["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "E0302" && d["message"].as_str().unwrap().contains("'tx'"))
    );
    assert!(
        publication["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "E0002")
    );
}

#[test]
fn actual_member_recovery_preserves_real_leak_and_repairs_restore_strict_exhaustion() {
    let uri = "untitled:recovery/😀";
    let at = position(DAMAGED, span(DAMAGED, "tx.").start);
    let query = json!({"parameters":[],"result":{"Named":["u64",[]]},"result_max_mode":"Copy","effect_ceiling":{"instances":[],"tail":null}});
    let messages = run(vec![
        initialize(),
        open(uri, DAMAGED),
        status_request(2, uri),
        request(
            3,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":at,"verified":true}),
        ),
        request(
            4,
            "textDocument/definition",
            json!({"textDocument":{"uri":uri},"position":at,"checked":true}),
        ),
        request(
            5,
            "textDocument/codeAction",
            json!({"textDocument":{"uri":uri},"range":{"start":at,"end":at},"context":{"only":["quickfix"],"diagnostics":[{"code":"E0302","verified":true,"fix":"FORGED_FIX"}]}}),
        ),
        request(
            6,
            "vesper/signatureSearch",
            json!({"uri":uri,"query":query,"verified":true}),
        ),
        change(uri, EMPTY_REPAIR, 2),
        status_request(7, uri),
        change(uri, VALID_REPAIR, 3),
        status_request(8, uri),
        request(
            9,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":position(VALID_REPAIR,span(VALID_REPAIR,"=tx").start+1)}),
        ),
    ]);
    let first = syntax_status(&messages, 2, uri, DAMAGED, 1);
    assert_eq!(first["update_kind"], "opened");
    let (owner, _) = declaration(first, "damaged");
    let dot = span(DAMAGED, "tx.");
    let site = first["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "missing_member")
        .expect("actual missing-member site");
    assert_eq!(site["declaration_index"], owner);
    assert_eq!(raw_span(&site["span"], DAMAGED), dot.end..dot.end);
    assert_eq!(&DAMAGED[raw_span(&site["receiver"]["root"], DAMAGED)], "tx");
    assert_eq!(site["receiver"]["projections"], json!([]));
    let published = publications(&messages, uri);
    assert_eq!(published.len(), 3);
    assert_hash(published[0], DAMAGED, 1);
    assert_independent_leak(published[0], DAMAGED, uri, first);
    for id in 3..=6 {
        assert_eq!(
            error(&messages, id, -32003)["data"]["status"],
            "analysis_unavailable"
        );
    }
    syntax_status(&messages, 7, uri, EMPTY_REPAIR, 2);
    assert_hash(published[1], EMPTY_REPAIR, 2);
    assert_eq!(published[1]["vesper"]["status"], "semantic_error");
    leak(published[1], "tx");
    leak(published[1], "other");
    let repaired = syntax_status(&messages, 8, uri, VALID_REPAIR, 3);
    assert_eq!(repaired["sites"], json!([]));
    assert!(
        repaired["declarations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["status"] == "complete")
    );
    assert_hash(published[2], VALID_REPAIR, 3);
    assert_eq!(published[2]["vesper"]["status"], "checked");
    assert_eq!(published[2]["diagnostics"], json!([]));
    assert!(
        result(&messages, 9)["contents"]["value"]
            .as_str()
            .is_some_and(|text| text.contains("Tx"))
    );
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("FORGED_FIX")
    );
    // A clean independent subset is still not checked original source.
    let source = "fn damaged(){receiver.}\r\nfn good()->u64{1}";
    let messages = run(vec![
        initialize(),
        open(uri, source),
        status_request(2, uri),
        request(
            3,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":position(source,span(source,"{1}").start+1),"verified":true}),
        ),
        request(
            4,
            "textDocument/definition",
            json!({"textDocument":{"uri":uri},"position":position(source,span(source,"good").start),"verified":true}),
        ),
        request(
            5,
            "textDocument/codeAction",
            json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"context":{"only":["quickfix"],"diagnostics":[]},"verified":true}),
        ),
        request(
            6,
            "vesper/signatureSearch",
            json!({"uri":uri,"query":query,"verified":true}),
        ),
    ]);
    let status = syntax_status(&messages, 2, uri, source, 1);
    let (_, good) = declaration(status, "good");
    let publication = publications(&messages, uri)[0];
    assert_eq!(publication["vesper"]["status"], "parse_error");
    assert!(
        publication["vesper"]["recovery"]["analyzed_functions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["span"] == good["span"])
    );
    assert!(
        publication["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["code"].as_str().unwrap().starts_with("E000"))
    );
    for id in 3..=6 {
        error(&messages, id, -32003);
    }
}

#[test]
fn incomplete_calls_and_missing_braces_keep_real_independent_siblings() {
    for damaged_body in ["unknown(}", "\r\n"] {
        let source = format!(
            "// 🦀\r\nlinear struct Tx{{}}\r\nfn damaged(sink tx:Tx){{{damaged_body}\r\nfn leak(/*😀*/sink other:Tx){{}}\r\n"
        );
        let uri = "untitled:delimiter-recovery/😀";
        let messages = run(vec![
            initialize(),
            open(uri, &source),
            status_request(2, uri),
        ]);
        let status = syntax_status(&messages, 2, uri, &source, 1);
        let (owner, _) = declaration(status, "damaged");
        assert!(
            status["sites"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["kind"] == "missing_delimiter" && s["declaration_index"] == owner)
        );
        let publication = publications(&messages, uri)[0];
        assert_hash(publication, &source, 1);
        assert_independent_leak(publication, &source, uri, status);
    }
}

#[test]
fn comments_strings_and_opaque_payloads_cannot_mint_sibling_declarations() {
    for (source, expected, opaque_kind) in [
        (
            "/* fn forged(){} tx. */\r\n// fn forged_again(){}\r\nfn real(){}",
            "real",
            None,
        ),
        ("fn text()->String{\"fn forged(){} tx.\"}", "text", None),
        (
            "fn payload(){opaque!(fn forged(){})}\r\nfn real(){}",
            "real",
            Some("opaque_macro"),
        ),
        (
            "fn text()->String{\"unterminated fn forged(){}\r\nfn hidden_in_string(){}",
            "text",
            Some("opaque_lexical"),
        ),
    ] {
        let uri = "untitled:lexical-boundaries/🦀";
        let messages = run(vec![
            initialize(),
            open(uri, source),
            status_request(2, uri),
        ]);
        let status = syntax_status(&messages, 2, uri, source, 1);
        declaration(status, expected);
        assert!(
            !status["declarations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| matches!(
                    d["name"].as_str(),
                    Some("forged" | "forged_again" | "hidden_in_string")
                ))
        );
        if let Some(kind) = opaque_kind {
            assert!(
                status["sites"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["kind"] == kind)
            );
            assert!(
                status["declarations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["status"] == "opaque")
            );
        } else {
            assert_eq!(status["sites"], json!([]));
            assert_eq!(status["declarations"].as_array().unwrap().len(), 1);
        }
    }
}

#[test]
fn lexical_and_generic_context_uncertainty_full_reparse_while_noop_is_unchanged() {
    for (source, next, kind) in [
        (
            "fn f(){0}\r\nfn later(){}",
            "fn f(){/*0}\r\nfn later(){}",
            "full_reparse",
        ),
        (
            "fn id[T:mod copy](sink x:T)->T{x}",
            "fn id[T:mod copy](sink x:T)->T{x /*body*/}",
            "full_reparse",
        ),
        ("fn f()->u64{1}", "fn f()->u64{1}", "unchanged"),
    ] {
        let uri = "untitled:fallback";
        let messages = run(vec![
            initialize(),
            open(uri, source),
            status_request(2, uri),
            change(uri, next, 2),
            status_request(3, uri),
        ]);
        syntax_status(&messages, 2, uri, source, 1);
        let updated = syntax_status(&messages, 3, uri, next, 2);
        assert_eq!(updated["update_kind"], kind);
        if next.contains("/*0") {
            assert!(
                updated["sites"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["kind"] == "opaque_lexical")
            );
            assert!(
                !updated["declarations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["name"] == "later")
            );
        }
    }
}

#[test]
fn periodic_reset_and_closing_other_buffers_do_not_expose_old_recovery_snapshots() {
    let uri = "untitled:periodic";
    let other = "untitled:retained";
    let source = "// 😀\r\nfn value()->u64{0}";
    let mut messages = vec![
        initialize(),
        open(uri, source),
        open(other, "fn retained()->u64{7}"),
    ];
    let mut current = String::new();
    for version in 2..=63 {
        current = format!("// 😀\r\nfn value()->u64{{{version}}}");
        messages.push(change(uri, &current, version));
    }
    messages.extend([
        status_request(2, uri),
        status_request(3, other),
        notice("textDocument/didClose", json!({"textDocument":{"uri":uri}})),
        status_request(4, other),
        status_request(5, uri),
    ]);
    let messages = run(messages);
    let current = syntax_status(&messages, 2, uri, &current, 63);
    declaration(current, "value");
    let retained = syntax_status(&messages, 3, other, "fn retained()->u64{7}", 1);
    declaration(retained, "retained");
    let after = syntax_status(&messages, 4, other, "fn retained()->u64{7}", 1);
    declaration(after, "retained");
    error(&messages, 5, -32602);
}

#[test]
fn valid_body_reparse_header_fallback_and_rejected_edits_have_truthful_current_identity() {
    let uri = "untitled:incremental/🦀";
    let first = "//😀\r\nfn value()->u64{1}\r\nfn sibling()->u64{7}\r\n";
    let second = first.replacen("{1}", "{22}", 1);
    let header = second.replacen("fn value()", "fn renamed()", 1);
    let messages = run(vec![
        initialize(),
        open(uri, first),
        status_request(2, uri),
        change(uri, &second, 2),
        status_request(3, uri),
        change(uri, "fn FORGED(){}", 2),
        status_request(4, uri),
        notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":3},"contentChanges":[{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":3}},"text":"x"}]}),
        ),
        status_request(5, uri),
        change(uri, &header, 3),
        status_request(6, uri),
        notice("textDocument/didClose", json!({"textDocument":{"uri":uri}})),
        status_request(7, uri),
        open(uri, "fn fresh(){}"),
        status_request(8, uri),
    ]);
    assert_eq!(
        syntax_status(&messages, 2, uri, first, 1)["update_kind"],
        "opened"
    );
    let fast = syntax_status(&messages, 3, uri, &second, 2);
    assert_eq!(fast["update_kind"], "body_reparse");
    let (_, sibling) = declaration(fast, "sibling");
    assert_eq!(
        &second[raw_span(&sibling["span"], &second)],
        "fn sibling()->u64{7}"
    );
    assert_eq!(fast, result(&messages, 4));
    assert_eq!(fast, result(&messages, 5));
    let full = syntax_status(&messages, 6, uri, &header, 3);
    assert_eq!(full["update_kind"], "full_reparse");
    declaration(full, "renamed");
    declaration(full, "sibling");
    error(&messages, 7, -32602);
    let reopened = syntax_status(&messages, 8, uri, "fn fresh(){}", 1);
    assert_eq!(reopened["update_kind"], "opened");
    assert_eq!(reopened["declarations"].as_array().unwrap().len(), 1);
    declaration(reopened, "fresh");
    // The fast path also relocates later damaged syntax and real causal spans.
    let original = "// 🦀 é\r\nlinear struct Tx{}\r\nfn good()->u64{1}\r\nfn damaged(sink tx:Tx){tx.}\r\nfn leak(sink other:Tx){}\r\n";
    let changed = original.replacen("{1}", "{12345}", 1);
    let messages = run(vec![
        initialize(),
        open(uri, original),
        status_request(2, uri),
        change(uri, &changed, 2),
        status_request(3, uri),
    ]);
    let old = syntax_status(&messages, 2, uri, original, 1);
    let new = syntax_status(&messages, 3, uri, &changed, 2);
    assert_eq!(new["update_kind"], "body_reparse");
    let old_site = old["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "missing_member")
        .unwrap();
    let new_site = new["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "missing_member")
        .unwrap();
    assert_eq!(
        new_site["span"]["start"].as_u64().unwrap(),
        old_site["span"]["start"].as_u64().unwrap() + 4
    );
    assert_eq!(
        &changed[raw_span(&new_site["receiver"]["root"], &changed)],
        "tx"
    );
    let published = publications(&messages, uri);
    assert_hash(published[1], &changed, 2);
    assert_independent_leak(published[1], &changed, uri, new);
}

#[test]
fn failed_recovery_update_discards_old_snapshot_and_resource_refusal_is_explicit() {
    let uri = "untitled:recovery-budget";
    let storm = format!("fn storm(){{{}}}", ";".repeat(20_000));
    let messages = run(vec![
        initialize(),
        open(uri, "fn old(){}"),
        status_request(2, uri),
        change(uri, &storm, 2),
        status_request(3, uri),
        change(uri, "fn fresh(){}", 3),
        status_request(4, uri),
    ]);
    syntax_status(&messages, 2, uri, "fn old(){}", 1);
    let refusal = error(&messages, 3, -32003);
    assert_eq!(refusal["data"]["status"], "analysis_unavailable");
    assert_eq!(refusal["data"]["kind"], "recovery");
    assert_eq!(refusal["data"]["version"], 2);
    assert_eq!(
        refusal["data"]["source_blake3"],
        blake3::hash(storm.as_bytes()).to_hex().as_str()
    );
    assert!(
        refusal["data"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty())
    );
    let final_status = syntax_status(&messages, 4, uri, "fn fresh(){}", 3);
    assert_eq!(final_status["declarations"].as_array().unwrap().len(), 1);
    declaration(final_status, "fresh");
    let published = publications(&messages, uri);
    assert_eq!(published.len(), 3);
    assert_hash(published[1], &storm, 2);
    assert_eq!(published[1]["vesper"]["status"], "analysis_unavailable");
    assert_eq!(published[1]["diagnostics"][0]["severity"], 2);
    assert_eq!(published[2]["vesper"]["status"], "checked");
    assert_eq!(published[2]["diagnostics"], json!([]));
}
