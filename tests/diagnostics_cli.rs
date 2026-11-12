//! Actual CLI and framed stdio acceptance of compiler-produced causal evidence.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    ops::Range,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_DIAGNOSTICS_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_vesper")));
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}
struct File {
    dir: PathBuf,
    path: PathBuf,
    source: String,
}
impl File {
    fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-causal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("diagnostic.ves");
        fs::write(&path, source.as_bytes()).unwrap();
        Self {
            dir,
            path,
            source: source.into(),
        }
    }
    fn check(&self, json: bool) -> Output {
        let mut command = Command::new(binary());
        command.arg("check").arg(&self.path);
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(
            fs::read(&self.path).unwrap(),
            self.source.as_bytes(),
            "checking modified source"
        );
        output
    }
    fn diagnostics(&self) -> Vec<Value> {
        let output = self.check(true);
        assert!(!output.status.success(), "expected a compiler rejection");
        assert!(output.stdout.len() < 1024 * 1024);
        serde_json::from_slice(&output.stdout).expect("exactly one diagnostic JSON array")
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const MOVE: &str = "// 🦀\r\nlinear struct Tx{}\r\nfn bad(/* 😀 */sink tx:Tx){\r\n/*🦀*/let Tx{}=tx;\r\n/*😀*/let Tx{}=tx;\r\n}\r\n";
const ALIAS: &str = "// 🦀\r\ncopy struct Pair{left:i64,right:i64}\r\nfn both(inout a:Pair,in b:Pair){}\r\nfn bad(/*😀*/inout p:Pair){/*🦀*/both(inout p,/*😀*/in p);}\r\n";
const ABORT: &str = "// 🦀\r\nlinear struct Tx{}\r\neffect Ask{fn ask();}\r\nfn bad(/*😀*/sink tx:Tx)eff[Ask]{/*🦀*/Ask::ask();let Tx{}=tx;}\r\n";
const IR_TEMP: &str = "// 🦀\r\nlinear struct Tx {} fn consume(sink tx:Tx,number:i64){let Tx{}=tx;}\r\nfn bad()eff[Fail[i64]]{/*😀*/consume(Tx {},/*🦀*/Fail::raise(1));}\r\n";

struct Cause {
    source: &'static str,
    code: &'static str,
    message: &'static str,
    primary: Range<usize>,
    related: Vec<Range<usize>>,
    labels: Vec<&'static str>,
    help: &'static str,
}
fn span(source: &str, text: &str) -> Range<usize> {
    let start = source.find(text).expect("fixture needle");
    start..start + text.len()
}
fn initializer(source: &str, n: usize) -> Range<usize> {
    let start = source.match_indices("=tx").nth(n).unwrap().0 + 1;
    start..start + 2
}
fn cases() -> Vec<Cause> {
    vec![
        Cause {
            source: MOVE,
            code: "E0303",
            message: "use of moved place 'tx'",
            primary: initializer(MOVE, 1),
            related: vec![span(MOVE, "sink tx:Tx"), initializer(MOVE, 0)],
            labels: vec![
                "binding introduced here",
                "earlier ownership-taking use of this place",
            ],
            help: "A moved non-copy place cannot be used again, including through an overlapping projection.",
        },
        Cause {
            source: ALIAS,
            code: "E0301",
            message: "overlapping arguments 'p' and 'p' violate alias XOR mutability",
            primary: {
                let start = ALIAS.rfind("in p").unwrap() + 3;
                start..start + 1
            },
            related: {
                let start = ALIAS.find("both(inout p").unwrap() + "both(inout ".len();
                std::iter::once(start..start + 1).collect()
            },
            labels: vec!["earlier overlapping argument evaluated here"],
            help: "Overlapping argument places are allowed only when both arguments use shared in passing.",
        },
        Cause {
            source: ABORT,
            code: "E0402",
            message: "potentially abortable effect 'Ask' may discard live linear value 'tx'",
            primary: span(ABORT, "Ask::ask()"),
            related: vec![span(ABORT, "sink tx:Tx")],
            labels: vec!["live linear binding introduced here"],
            help: "An abortable checkpoint may discard a continuation that still owns this linear binding.",
        },
    ]
}
fn select<'a>(diagnostics: &'a [Value], code: &str, message: &str) -> &'a Value {
    let found: Vec<_> = diagnostics
        .iter()
        .filter(|d| d["code"] == code && d["message"] == message)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "missing/duplicate genuine cause {code}: {diagnostics:?}"
    );
    found[0]
}
fn byte_span(span: &Range<usize>) -> Value {
    json!({"start":span.start,"end":span.end})
}
fn assert_core(diagnostic: &Value, case: &Cause) {
    assert_eq!(diagnostic["span"], byte_span(&case.primary));
    let related = diagnostic["related"]
        .as_array()
        .expect("real compiler secondary labels required");
    assert_eq!(related.len(), case.related.len());
    for ((label, span), message) in related.iter().zip(&case.related).zip(&case.labels) {
        assert_eq!(label["span"], byte_span(span));
        assert_eq!(label["message"], *message);
    }
    assert_eq!(diagnostic["help"], case.help);
}

#[test]
fn cli_json_and_terminal_preserve_actual_causes_codes_and_primary_spans() {
    for case in cases() {
        let file = File::new(case.source);
        let diagnostics = file.diagnostics();
        let diagnostic = select(&diagnostics, case.code, case.message);
        assert_core(diagnostic, &case);
        let terminal = file.check(false);
        assert!(!terminal.status.success());
        assert!(terminal.stdout.is_empty());
        let text = String::from_utf8(terminal.stderr).unwrap();
        assert!(
            text.contains(case.code) && text.contains(case.message),
            "{text}"
        );
        for message in case.labels {
            assert!(text.contains(message), "{text}");
        }
        assert!(text.contains(case.help), "{text}");
        assert!(text.contains('😀') && text.contains('🦀'), "{text}");
        assert!(
            !text.contains("\u{1b}["),
            "terminal renderer should be plain"
        );
    }
}

#[test]
fn ir_temporary_cause_reaches_actual_cli_without_fabricated_ownership_origin() {
    let file = File::new(IR_TEMP);
    let diagnostics = file.diagnostics();
    let diagnostic = select(
        &diagnostics,
        "E0410",
        "abortable effect may abandon linear temporary %0",
    );
    assert_eq!(
        diagnostic["span"],
        byte_span(&span(IR_TEMP, "Fail::raise(1)"))
    );
    let start = IR_TEMP.rfind("Tx {}").unwrap();
    assert_eq!(
        diagnostic["related"][0]["span"],
        byte_span(&(start..start + "Tx {}".len()))
    );
    assert_eq!(
        diagnostic["related"][0]["message"],
        "this IR temporary is defined here"
    );
    assert_eq!(diagnostic["related"].as_array().unwrap().len(), 1);
    assert!(
        diagnostic["help"]
            .as_str()
            .is_some_and(|help| !help.is_empty())
    );
    let terminal = file.check(false);
    assert!(!terminal.status.success() && terminal.stdout.is_empty());
    let terminal = String::from_utf8(terminal.stderr).unwrap();
    assert!(terminal.contains("this IR temporary is defined here"));
    assert!(terminal.contains(diagnostic["help"].as_str().unwrap()));
    let uri = "untitled:ir-temporary/🦀";
    let messages = lsp(vec![initialize(), open(uri, IR_TEMP)]);
    assert_lsp(publications(&messages, uri)[0], IR_TEMP, diagnostic, uri, 1);
}

#[test]
fn legacy_empty_metadata_shape_and_eof_locations_remain_exact() {
    let file = File::new("// 🦀\r\nfn main(){missing}");
    let diagnostics = file.diagnostics();
    let diagnostic = select(&diagnostics, "E0201", "unknown name 'missing'");
    assert_eq!(diagnostic.as_object().unwrap().len(), 3);
    assert!(diagnostic.get("related").is_none() && diagnostic.get("help").is_none());
    let source = "// 🦀\r\nfn main(){\r\n";
    let diagnostics = File::new(source).diagnostics();
    let eof = select(&diagnostics, "E0002", "expected RBrace, found Eof");
    assert_eq!(eof["span"], byte_span(&(source.len()..source.len())));
    let separator = "/*\u{2028}*/fn main(){missing}";
    let file = File::new(separator);
    let diagnostics = file.diagnostics();
    let diagnostic = select(&diagnostics, "E0201", "unknown name 'missing'");
    let primary = span(separator, "missing");
    assert_eq!(diagnostic["span"], byte_span(&primary));
    let output = file.check(false);
    assert!(!output.status.success() && output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("diagnostic source rendering unavailable:"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "E0201: unknown name 'missing' (UTF-8 bytes {}..{})",
            primary.start, primary.end
        )),
        "{stderr}"
    );
}

fn position(source: &str, offset: usize) -> Value {
    assert!(source.is_char_boundary(offset));
    let mut line = 0u32;
    let mut character = 0u32;
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
    assert!(
        !(offset > 0
            && source.as_bytes()[offset - 1] == b'\r'
            && source.as_bytes().get(offset) == Some(&b'\n'))
    );
    json!({"line":line,"character":character})
}
fn range(source: &str, span: &Range<usize>) -> Value {
    json!({"start":position(source,span.start),"end":position(source,span.end)})
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
        "workspace":{"workspaceEdit":{"documentChanges":true}},"textDocument":{"publishDiagnostics":{"relatedInformation":true,"dataSupport":true},"codeAction":{"codeActionLiteralSupport":{}}}}}),
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
fn framed(messages: &[Value]) -> Vec<u8> {
    let mut output = Vec::new();
    for message in messages {
        let bytes = serde_json::to_vec(message).unwrap();
        write!(output, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
        output.extend(bytes);
    }
    output
}
fn lsp(mut messages: Vec<Value>) -> Vec<Value> {
    messages.extend([
        request(900, "shutdown", json!({})),
        notice("exit", json!({})),
    ]);
    let input = framed(&messages);
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
            .take(8 * 1024 * 1024 + 1)
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
        if start.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("causal stdio timeout");
        }
        thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap().unwrap();
    let bytes = reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&errors));
    assert!(bytes.len() <= 8 * 1024 * 1024 && errors.len() <= 1024 * 1024);
    let mut input = std::io::Cursor::new(bytes);
    let mut messages = Vec::new();
    while let Some(bytes) =
        vesper_lsp::read_frame(&mut input, vesper_lsp::ProtocolLimits::default()).unwrap()
    {
        vesper_lsp::parse_envelope(&bytes, vesper_lsp::ProtocolLimits::default()).unwrap();
        messages.push(serde_json::from_slice(&bytes).unwrap());
    }
    messages
}
fn publications<'a>(messages: &'a [Value], uri: &str) -> Vec<&'a Value> {
    messages
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .map(|m| &m["params"])
        .collect()
}
fn assert_lsp(publication: &Value, source: &str, core: &Value, uri: &str, version: i32) {
    assert_eq!(publication["version"], version);
    assert_eq!(
        publication["vesper"]["source_blake3"],
        blake3::hash(source.as_bytes()).to_hex().as_str()
    );
    let diagnostics = publication["diagnostics"].as_array().unwrap();
    let d = select(
        diagnostics,
        core["code"].as_str().unwrap(),
        core["message"].as_str().unwrap(),
    );
    let span = &core["span"];
    assert_eq!(
        d["range"],
        range(
            source,
            &(span["start"].as_u64().unwrap() as usize..span["end"].as_u64().unwrap() as usize)
        )
    );
    assert_eq!(d["severity"], 1);
    if let Some(labels) = core.get("related").and_then(Value::as_array) {
        let related = d["relatedInformation"]
            .as_array()
            .expect("standard causal LSP labels");
        assert_eq!(related.len(), labels.len());
        for (actual, label) in related.iter().zip(labels) {
            let span = &label["span"];
            assert_eq!(actual["location"]["uri"], uri);
            assert_eq!(
                actual["location"]["range"],
                range(
                    source,
                    &(span["start"].as_u64().unwrap() as usize
                        ..span["end"].as_u64().unwrap() as usize)
                )
            );
            assert_eq!(actual["message"], label["message"]);
            assert!(actual.get("uri").is_none() && actual.get("range").is_none());
        }
    } else {
        assert!(d.get("relatedInformation").is_none());
    }
    if let Some(help) = core.get("help") {
        assert_eq!(d["data"]["help"], *help);
    } else {
        assert!(d.get("data").and_then(|data| data.get("help")).is_none());
    }
    assert!(d.get("edit").is_none() && d.get("fixes").is_none());
}

#[test]
fn framed_related_information_uses_current_unicode_crlf_source_and_clears_after_recovery() {
    for case in cases() {
        let uri = format!("untitled:causal/🦀/{}", case.code);
        let before = File::new(case.source);
        let first = before.diagnostics();
        let edited = format!("// longer 😀🦀 revision\r\n{}", case.source);
        let after = File::new(&edited);
        let second = after.diagnostics();
        let first = select(&first, case.code, case.message);
        let second = select(&second, case.code, case.message);
        let messages = lsp(vec![
            initialize(),
            open(&uri, case.source),
            change(&uri, &edited, 2),
            change(&uri, "fn main(){}", 3),
        ]);
        let publications = publications(&messages, &uri);
        assert_eq!(publications.len(), 3);
        assert_lsp(publications[0], case.source, first, &uri, 1);
        assert_lsp(publications[1], &edited, second, &uri, 2);
        assert_ne!(
            publications[0]["diagnostics"][0]["range"],
            publications[1]["diagnostics"][0]["range"]
        );
        assert_eq!(publications[2]["diagnostics"], json!([]));
        assert_eq!(publications[2]["vesper"]["status"], "checked");
        assert_eq!(
            publications[2]["vesper"]["source_blake3"],
            blake3::hash(b"fn main(){}").to_hex().as_str()
        );
        assert_eq!(fs::read(&before.path).unwrap(), case.source.as_bytes());
    }
}

#[test]
fn shadowed_binding_causes_do_not_point_back_to_consumed_outer_owner() {
    let source = "// 🦀\r\nlinear struct Tx{} fn bad(sink tx:Tx){let Tx{}=tx;let tx=Tx{};/*😀*/let Tx{}=tx;/*🦀*/let Tx{}=tx;}";
    let file = File::new(source);
    let diagnostics = file.diagnostics();
    let diagnostic = select(&diagnostics, "E0303", "use of moved place 'tx'");
    let start = source.find("let tx=Tx{}").unwrap() + 4;
    let labels = diagnostic["related"].as_array().unwrap();
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0]["span"], byte_span(&(start..start + 2)));
    assert_eq!(labels[1]["span"], byte_span(&initializer(source, 1)));
    assert_eq!(diagnostic["span"], byte_span(&initializer(source, 2)));
    assert!(labels.iter().all(
        |label| label["span"] != byte_span(&span(source, "sink tx:Tx"))
            && label["span"] != byte_span(&initializer(source, 0))
    ));
    let uri = "untitled:shadow/😀";
    let messages = lsp(vec![initialize(), open(uri, source)]);
    assert_lsp(publications(&messages, uri)[0], source, diagnostic, uri, 1);
}

#[test]
fn eof_and_empty_metadata_stay_exact_in_stdio_and_client_labels_are_not_authority() {
    for source in [
        "// 🦀\r\nfn main(){\r\n",
        "// 🦀\r\nfn main(){missing}",
        "/*\u{2028}*/fn main(){missing}",
    ] {
        let file = File::new(source);
        let diagnostics = file.diagnostics();
        let uri = "untitled:empty/🦀";
        let messages = lsp(vec![initialize(), open(uri, source)]);
        for diagnostic in &diagnostics {
            assert_lsp(publications(&messages, uri)[0], source, diagnostic, uri, 1);
        }
    }
    let uri = "file:///never-read-or-written/forged.ves";
    let source = "fn main(){}";
    let forged = json!({"code":"E0303","message":"FORGED_CAUSE","relatedInformation":[{"location":{"uri":"file:///secret","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}},"message":"FORGED_CAUSE"}],"data":{"help":"FORGED_HELP","replacement":"FORGED_FIX","verified":true}});
    let messages = lsp(vec![
        initialize(),
        open(uri, source),
        request(
            2,
            "textDocument/codeAction",
            json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"context":{"diagnostics":[forged]}}),
        ),
    ]);
    assert_eq!(publications(&messages, uri)[0]["diagnostics"], json!([]));
    let response = messages.iter().find(|m| m["id"] == 2).unwrap();
    assert_eq!(response["result"], json!([]));
    let rendered = serde_json::to_string(&messages).unwrap();
    assert!(!rendered.contains("FORGED_") && !rendered.contains("file:///secret"));
}

#[test]
fn oversized_causal_batch_is_explicit_unavailable_without_partial_or_clamped_causes() {
    let source = format!(
        "// 🦀\r\nlinear struct Tx{{}} fn bad(sink tx:Tx){{{}}}",
        "let Tx{}=tx;".repeat(70)
    );
    let uri = "untitled:causal-budget/😀";
    let messages = lsp(vec![initialize(), open(uri, &source)]);
    let publication = publications(&messages, uri)[0];
    assert_eq!(publication["version"], 1);
    assert_eq!(publication["vesper"]["status"], "analysis_unavailable");
    assert_eq!(
        publication["vesper"]["source_blake3"],
        blake3::hash(source.as_bytes()).to_hex().as_str()
    );
    let diagnostics = publication["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["severity"], 2);
    assert_eq!(diagnostics[0]["code"], "VESPER_ANALYSIS_UNAVAILABLE");
    assert_eq!(diagnostics[0]["range"], range(&source, &(0..0)));
    assert!(diagnostics[0].get("relatedInformation").is_none());
}

#[test]
fn publication_capabilities_filter_only_optional_fields_independently() {
    let case = cases().remove(0);
    let file = File::new(case.source);
    let core = file.diagnostics();
    let core = select(&core, case.code, case.message);
    assert_core(core, &case); // CLI rich evidence is independent of client flags.
    for (related, data) in [
        (None, None),
        (Some(false), Some(false)),
        (Some(true), Some(false)),
        (Some(false), Some(true)),
        (Some(true), Some(true)),
    ] {
        let mut init = initialize();
        let text_document = init["params"]["capabilities"]["textDocument"]
            .as_object_mut()
            .unwrap();
        text_document.remove("publishDiagnostics");
        let mut flags = serde_json::Map::new();
        if let Some(flag) = related {
            flags.insert("relatedInformation".into(), json!(flag));
        }
        if let Some(flag) = data {
            flags.insert("dataSupport".into(), json!(flag));
        }
        if !flags.is_empty() {
            text_document.insert("publishDiagnostics".into(), Value::Object(flags));
        }
        let uri = "untitled:publication-capabilities/🦀";
        let messages = lsp(vec![init, open(uri, case.source)]);
        let publication = publications(&messages, uri)[0];
        assert_eq!(publication["version"], 1);
        assert_eq!(
            publication["vesper"]["source_blake3"],
            blake3::hash(case.source.as_bytes()).to_hex().as_str()
        );
        let diagnostic = select(
            publication["diagnostics"].as_array().unwrap(),
            case.code,
            case.message,
        );
        assert_eq!(diagnostic["range"], range(case.source, &case.primary));
        assert_eq!(diagnostic["severity"], 1);
        assert_eq!(
            diagnostic.get("relatedInformation").is_some(),
            related == Some(true)
        );
        let help = diagnostic.get("data").and_then(|value| value.get("help"));
        assert_eq!(help.is_some(), data == Some(true));
        if let Some(help) = help {
            assert_eq!(help, case.help);
        }
        if related == Some(true) {
            assert_eq!(
                diagnostic["relatedInformation"].as_array().unwrap().len(),
                case.related.len()
            );
            for ((label, span), message) in diagnostic["relatedInformation"]
                .as_array()
                .unwrap()
                .iter()
                .zip(&case.related)
                .zip(&case.labels)
            {
                assert_eq!(label["location"]["uri"], uri);
                assert_eq!(label["location"]["range"], range(case.source, span));
                assert_eq!(label["message"], *message);
            }
        }
    }
    // Legacy warning status/kind data remains visible even without dataSupport.
    let mut legacy = initialize();
    legacy["params"]["capabilities"]["textDocument"]
        .as_object_mut()
        .unwrap()
        .remove("publishDiagnostics");
    let source = format!(
        "linear struct Tx{{}} fn bad(sink tx:Tx){{{}}}",
        "let Tx{}=tx;".repeat(70)
    );
    let uri = "untitled:legacy-unavailable";
    let messages = lsp(vec![legacy, open(uri, &source)]);
    let publication = publications(&messages, uri)[0];
    assert_eq!(publication["vesper"]["status"], "analysis_unavailable");
    assert_eq!(
        publication["diagnostics"][0]["data"]["status"],
        "analysis_unavailable"
    );
    assert_eq!(publication["diagnostics"][0]["data"]["kind"], "resource");
}
