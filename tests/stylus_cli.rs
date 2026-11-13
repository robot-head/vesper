//! Actual macro/partial-buffer formatting, atomic CLI and current stdio edits.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const MACRO: &str = include_str!("fixtures/stylus/macro.ves");
const RECOVERY: &str = include_str!("fixtures/stylus/recovery.ves");
const OPAQUE: &str = include_str!("fixtures/stylus/opaque.ves");
fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_STYLUS_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_vesper")));
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}
fn output(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .by_ref()
            .take(2 * 1024 * 1024 + 1)
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
            panic!("Stylus CLI timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = reader.join().unwrap();
    let stderr = errors.join().unwrap();
    assert!(stdout.len() <= 2 * 1024 * 1024 && stderr.len() <= 2 * 1024 * 1024);
    Output {
        status,
        stdout,
        stderr,
    }
}

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(source: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-stylus-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("source.ves");
        fs::write(&path, source).unwrap();
        Self { dir, path }
    }
    fn fmt(&self, flags: &[&str]) -> Output {
        output(
            Command::new(binary())
                .arg("fmt")
                .arg(&self.path)
                .args(flags),
        )
    }
    fn unchanged(&self, source: &[u8]) {
        assert_eq!(fs::read(&self.path).unwrap(), source);
        assert_eq!(fs::read_dir(&self.dir).unwrap().count(), 1);
    }
    fn formatted(&self, mode: &str) -> String {
        let flags = if mode == "recovery" {
            vec!["--recovery", "--json"]
        } else {
            vec!["--json"]
        };
        let out = self.fmt(&flags);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["mode"], mode);
        if mode == "recovery" {
            assert!(value["edits"].is_u64());
        }
        let after = fs::read_to_string(&self.path).unwrap();
        let stable = self.fmt(&flags);
        assert!(stable.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&stable.stdout).unwrap()["changed"],
            false
        );
        self.unchanged(after.as_bytes());
        after
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn tokens(source: &str) -> Vec<(vesper::lexer::TokenKind, String)> {
    let (tokens, errors) = vesper::lexer::lex(source);
    assert!(errors.is_empty());
    tokens
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                vesper::lexer::TokenKind::Whitespace | vesper::lexer::TokenKind::Eof
            )
        })
        .map(|t| (t.kind, source[t.span].to_owned()))
        .collect()
}
#[test]
fn recognized_macro_formats_without_execution_then_expands_and_runs_42() {
    let f = Fixture::new(MACRO.as_bytes());
    let check = f.fmt(&["--check", "--json"]);
    assert_eq!(check.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&check.stdout).unwrap()["mode"],
        "source"
    );
    f.unchanged(MACRO.as_bytes());
    let after = f.formatted("source");
    assert_ne!(after, MACRO);
    assert_eq!(tokens(MACRO), tokens(&after));
    let expansion = output(
        Command::new(binary())
            .args(["forge", "expand"])
            .arg(&f.path),
    );
    assert!(
        expansion.status.success(),
        "{}",
        String::from_utf8_lossy(&expansion.stderr)
    );
    let expanded = f.dir.join("expanded.ves");
    fs::write(&expanded, &expansion.stdout).unwrap();
    let run = output(Command::new(binary()).arg("run").arg(&expanded));
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap().trim(), "42");
    assert_eq!(fs::read_to_string(&f.path).unwrap(), after);
}
#[test]
fn explicit_recovery_preserves_damaged_gaps_and_does_not_make_source_checked() {
    let f = Fixture::new(RECOVERY.as_bytes());
    assert!(!f.fmt(&["--json"]).status.success());
    f.unchanged(RECOVERY.as_bytes());
    let check = f.fmt(&["--recovery", "--check", "--json"]);
    assert_eq!(check.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(value["mode"], "recovery");
    assert!(value["edits"].as_u64().unwrap() > 0);
    f.unchanged(RECOVERY.as_bytes());
    let after = f.formatted("recovery");
    let before = RECOVERY.split("fn main").next().unwrap();
    assert!(after.starts_with(before));
    assert_eq!(tokens(RECOVERY), tokens(&after));
    assert!(after.ends_with(
        "

"
    ));
    let ordinary = output(
        Command::new(binary())
            .arg("check")
            .arg(&f.path)
            .arg("--json"),
    );
    assert_eq!(ordinary.status.code(), Some(1));
    assert!(
        !serde_json::from_slice::<Vec<Value>>(&ordinary.stdout)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn opaque_and_fake_declarations_are_never_selected_as_recovery_items() {
    let f = Fixture::new(OPAQUE.as_bytes());
    assert!(!f.fmt(&["--json"]).status.success());
    f.unchanged(OPAQUE.as_bytes());
    let after = f.formatted("recovery");
    assert!(after.starts_with(
        "fn opaque(){unknown!{ fn fake( )->i64{42} }}\n// fn comment_fake( )->i64{42}\n"
    ));
    assert!(after.contains("\"fn string_fake( )->i64{42}\""));
    assert_eq!(tokens(OPAQUE), tokens(&after));
    let only = "fn opaque(){unknown!{ fn fake( )->i64{42} }}";
    let f = Fixture::new(only.as_bytes());
    assert_eq!(f.formatted("recovery"), only);
}
#[test]
fn malformed_unsupported_and_resource_failures_leave_inputs_unchanged() {
    for source in [
        b"fn main(){/* unclosed".as_slice(),
        b"fn main(){\xff}",
        b"macro m($x:ty)->expr{quote{$x}} fn main(){m!(i64)}",
        b"macro m($x:expr)->expr{quote{$x}} fn main(){m!(m!(42))}",
    ] {
        let f = Fixture::new(source);
        assert!(!f.fmt(&["--json"]).status.success());
        f.unchanged(source);
    }
    let large = vec![b' '; 256 * 1024 + 1];
    let f = Fixture::new(&large);
    for flags in [&["--json"][..], &["--recovery", "--json"][..]] {
        assert!(!f.fmt(flags).status.success());
        f.unchanged(&large);
    }
    let f = Fixture::new(b"fn main(){42}");
    assert!(!f.fmt(&["--unknown"]).status.success());
    f.unchanged(b"fn main(){42}");
    assert!(
        !output(
            Command::new(binary())
                .arg("fmt")
                .arg(&f.dir)
                .arg("--recovery")
                .arg("--check")
        )
        .status
        .success()
    );
    f.unchanged(b"fn main(){42}");
}
#[test]
fn formatting_never_repairs_actual_linear_exhaustion_or_duplicate_use() {
    for (source, code) in [
        ("linear struct Tx{} fn leak(sink tx:Tx){}", "E0302"),
        (
            "linear struct Tx{} fn make()->Tx{Tx{}} fn consume(sink tx:Tx){let Tx{}=tx;} fn main(){let tx=make();consume(tx);consume(tx)}",
            "E0303",
        ),
    ] {
        let f = Fixture::new(source.as_bytes());
        let after = f.formatted("source");
        assert_eq!(tokens(source), tokens(&after));
        let check = output(
            Command::new(binary())
                .arg("check")
                .arg(&f.path)
                .arg("--json"),
        );
        assert_eq!(check.status.code(), Some(1));
        let diagnostics: Vec<Value> = serde_json::from_slice(&check.stdout).unwrap();
        assert!(
            diagnostics.iter().any(|d| d["code"] == code),
            "{diagnostics:?}"
        );
    }
}
fn request(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn session(mut messages: Vec<Value>) -> Vec<Value> {
    messages.extend([
        request(900, "shutdown", json!({})),
        notice("exit", json!({})),
    ]);
    let mut input = Vec::new();
    for m in messages {
        let bytes = serde_json::to_vec(&m).unwrap();
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
        let mut b = Vec::new();
        stdout
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
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
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Stylus stdio timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap().unwrap();
    let bytes = reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&errors));
    assert!(bytes.len() <= 4 * 1024 * 1024 && errors.len() <= 1024 * 1024);
    let mut cursor = std::io::Cursor::new(bytes);
    let mut replies = Vec::new();
    while let Some(b) =
        vesper_lsp::read_frame(&mut cursor, vesper_lsp::ProtocolLimits::default()).unwrap()
    {
        vesper_lsp::parse_envelope(&b, vesper_lsp::ProtocolLimits::default()).unwrap();
        replies.push(serde_json::from_slice(&b).unwrap());
    }
    replies
}
fn response(messages: &[Value], id: i32) -> &Value {
    let reply = messages.iter().find(|m| m["id"] == id).unwrap();
    assert!(reply.get("error").is_none(), "{reply:?}");
    &reply["result"]
}
fn apply(source: &str, edits: &Value) -> String {
    let idx =
        vesper_span::LineIndex::new(Arc::from(source), vesper_span::Limits::default()).unwrap();
    let mut decoded = Vec::new();
    for edit in edits.as_array().unwrap() {
        let decode = |p: &Value| vesper_span::Position {
            line: p["line"].as_u64().unwrap().try_into().unwrap(),
            character: p["character"].as_u64().unwrap().try_into().unwrap(),
        };
        let range = vesper_span::Range {
            start: decode(&edit["range"]["start"]),
            end: decode(&edit["range"]["end"]),
        };
        let bytes = idx.span(range).unwrap();
        decoded.push((bytes, edit["newText"].as_str().unwrap()));
    }
    decoded.sort_by_key(|(span, _)| span.start);
    assert!(decoded.windows(2).all(|w| w[0].0.end <= w[1].0.start));
    let mut result = source.to_owned();
    for (span, text) in decoded.into_iter().rev() {
        result.replace_range(span.start..span.end, text);
    }
    result
}
#[test]
fn stdio_partial_edits_use_current_utf16_crlf_buffer_and_keep_parse_diagnostics() {
    let uri = "untitled:stylus/🦀";
    let first = "// 😀\r\nfn damaged(){let x=;}\r\nfn main( )->i64{42}\r\n";
    let current = "// 😀 updated\r\n\r\nfn damaged(){let x=;}\r\n// 🦀\r\nfn main( )->i64{41}\r\n";
    let format = |id| {
        request(
            id,
            "textDocument/formatting",
            json!({"textDocument":{"uri":uri},"options":{"tabSize":4,"insertSpaces":true}}),
        )
    };
    let replies = session(vec![
        request(
            1,
            "initialize",
            json!({"capabilities":{"general":{"positionEncodings":["utf-16"]}}}),
        ),
        notice(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"version":1,"languageId":"vesper","text":first}}),
        ),
        format(2),
        notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":current}]}),
        ),
        format(3),
        request(
            4,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":{"line":5,"character":4}}),
        ),
    ]);
    for (source, id) in [(first, 2), (current, 3)] {
        let edits = response(&replies, id);
        assert!(!edits.as_array().unwrap().is_empty());
        let after = apply(source, edits);
        let prefix = source.split("fn main").next().unwrap();
        assert!(after.starts_with(prefix));
        assert_eq!(tokens(source), tokens(&after));
        assert_ne!(after, source);
    }
    let publications: Vec<_> = replies
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .collect();
    assert_eq!(publications.len(), 2);
    for (p, source, version) in [(publications[0], first, 1), (publications[1], current, 2)] {
        assert_eq!(p["params"]["version"], version);
        assert_eq!(p["params"]["vesper"]["status"], "parse_error");
        assert_eq!(
            p["params"]["vesper"]["source_blake3"],
            blake3::hash(source.as_bytes()).to_hex().as_str()
        );
        assert!(!p["params"]["diagnostics"].as_array().unwrap().is_empty());
    }
    assert_eq!(
        replies.iter().find(|m| m["id"] == 4).unwrap()["error"]["code"],
        -32003
    );
}
#[cfg(unix)]
#[test]
fn recovery_symlink_input_preserves_link_and_target() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new(RECOVERY.as_bytes());
    let target = f.dir.join("target.ves");
    fs::rename(&f.path, &target).unwrap();
    symlink(&target, &f.path).unwrap();
    assert!(!f.fmt(&["--recovery", "--json"]).status.success());
    assert!(
        fs::symlink_metadata(&f.path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target).unwrap(), RECOVERY.as_bytes());
}
