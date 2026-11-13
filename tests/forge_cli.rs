//! Real source parser/typed-host/resolved-lowering/checker/authority CLI pipeline.
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const KEEP: &str = include_str!("fixtures/source_macros/keep.ves");
const REPEATED: &str = include_str!("fixtures/source_macros/repeated.ves");
const TRANSITIVE: &str = include_str!("fixtures/source_macros/transitive_effect.ves");
const PURE_ROW: &str = include_str!("fixtures/source_macros/pure_declared_row.ves");
const OVERFLOW: &str = include_str!("fixtures/source_macros/implicit_overflow.ves");
const RUNTIME_FAIL: &str = include_str!("fixtures/source_macros/implicit_runtime_fail.ves");
fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_SOURCE_MACROS_BINARY")
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
            panic!("source macro CLI timed out");
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
    source: String,
}
impl Fixture {
    fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-source-macros-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("original.ves");
        fs::write(&path, source.as_bytes()).unwrap();
        Self {
            dir,
            path,
            source: source.into(),
        }
    }
    fn command(&self, mode: &str, args: &[&str]) -> Output {
        let output = output(
            Command::new(binary())
                .args(["forge", mode])
                .arg(&self.path)
                .args(args),
        );
        assert_eq!(
            fs::read(&self.path).unwrap(),
            self.source.as_bytes(),
            "forge changed original input"
        );
        assert!(output.stdout.len() <= 2 * 1024 * 1024 && output.stderr.len() <= 2 * 1024 * 1024);
        output
    }
    fn check(&self, args: &[&str]) -> Value {
        let output = self.command("check", args);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let report: Value = serde_json::from_slice(&output.stdout).expect("single JSON summary");
        self.validate(&report);
        report
    }
    fn reject(&self, args: &[&str]) -> Value {
        let output = self.command("check", args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value =
            serde_json::from_slice(&output.stdout).expect("single rejected JSON summary");
        assert_eq!(report["schema"], "vesper.forge.source/1");
        assert_eq!(report["status"], "rejected");
        assert_eq!(report["source_map"], json!([]));
        assert!(report["message"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(matches!(
            report["stage"].as_str(),
            Some(
                "parse"
                    | "expand"
                    | "resolve"
                    | "lower"
                    | "check"
                    | "authority"
                    | "ir"
                    | "resource"
                    | "unsupported"
                    | "config"
                    | "io"
            )
        ));
        let expanded = self.command("expand", args);
        assert_eq!(expanded.status.code(), Some(1));
        assert!(
            expanded.stdout.is_empty(),
            "partial failed expansion leaked to stdout"
        );
        report
    }
    fn validate(&self, report: &Value) {
        assert_eq!(report["schema"], "vesper.forge.source/1");
        assert_eq!(report["status"], "expanded_checked");
        assert_eq!(report["scope"], "tier1_source");
        assert_eq!(
            report["source_blake3"],
            blake3::hash(self.source.as_bytes()).to_hex().as_str()
        );
        assert_eq!(report["source_bytes"], self.source.len());
        let expanded = report["expanded_source"].as_str().unwrap();
        assert_eq!(report["expanded_bytes"], expanded.len());
        assert_eq!(
            report["expanded_blake3"],
            blake3::hash(expanded.as_bytes()).to_hex().as_str()
        );
        assert_ne!(report["source_blake3"], report["expanded_blake3"]);
        assert!(report["macro_definitions"].as_u64().unwrap() > 0);
        let count = usize::try_from(report["invocations"].as_u64().unwrap()).unwrap();
        assert!(count > 0);
        assert_eq!(report["audits"].as_array().unwrap().len(), count);
        assert!(!report["source_map"].as_array().unwrap().is_empty());
        for mapping in report["source_map"].as_array().unwrap() {
            range(&mapping["original"], &self.source);
            range(&mapping["expanded"], expanded);
            assert!(matches!(
                mapping["kind"].as_str(),
                Some("source" | "generated")
            ));
            assert!(mapping["macro"].is_null() || mapping["macro"].is_string());
        }
        for audit in report["audits"].as_array().unwrap() {
            assert_eq!(audit["scope"], "expanded_function");
            let invocation = range(&audit["invocation"], &self.source);
            assert!(self.source[invocation].contains("!("));
            assert!(audit["macro"].as_str().is_some_and(|s| !s.is_empty()));
        }
        for forbidden in [
            "world",
            "original_checked",
            "source_checked",
            "checked_original",
        ] {
            assert!(report.get(forbidden).is_none());
        }
    }
    fn expand(&self, args: &[&str]) -> String {
        let output = self.command("expand", args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn run_expanded(&self, expanded: &str) {
        self.run_value(expanded, "42");
    }
    fn run_value(&self, expanded: &str, expected: &str) {
        let path = self.dir.join("expanded.ves");
        fs::write(&path, expanded.as_bytes()).unwrap();
        let checked = output(Command::new(binary()).arg("check").arg(&path).arg("--json"));
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&checked.stdout).unwrap(),
            json!([])
        );
        let run = output(Command::new(binary()).arg("run").arg(&path));
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8(run.stdout).unwrap().trim(), expected);
        assert_eq!(fs::read(&path).unwrap(), expanded.as_bytes());
    }
    fn policy(&self, text: &str) -> PathBuf {
        let path = self.dir.join("policy.json");
        fs::write(&path, text.as_bytes()).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn range(value: &Value, source: &str) -> std::ops::Range<usize> {
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
fn policy(macro_name: &str, effect: &str, emits: bool, ceiling: bool) -> String {
    json!({"schema":"vesper.forge.policy/1","macro_emits":if emits{json!([{"name":macro_name,"effects":[effect]}])}else{json!([])},"caller_ceiling":if ceiling{json!([effect])}else{json!([])}}).to_string()
}

#[test]
fn actual_tier1_expansion_runs_to_42_with_two_way_hygiene_and_real_globals() {
    for source in [
        KEEP,
        include_str!("fixtures/source_macros/addone.ves"),
        include_str!("fixtures/source_macros/argument_capture.ves"),
        include_str!("fixtures/source_macros/reference_capture.ves"),
        include_str!("fixtures/source_macros/initializer.ves"),
        include_str!("fixtures/source_macros/global_helper.ves"),
        REPEATED,
        include_str!("fixtures/source_macros/block_argument.ves"),
        include_str!("fixtures/source_macros/literal_context.ves"),
    ] {
        let fixture = Fixture::new(source);
        let report = fixture.check(&["--json"]);
        assert_eq!(report["package"], "@local/source");
        assert_eq!(report["macro_definitions"], 1);
        assert_eq!(report["invocations"], 1);
        let expanded = fixture.expand(&[]);
        assert_eq!(
            expanded,
            report["expanded_source"].as_str().unwrap(),
            "expand must output exact checked bytes with no extra newline"
        );
        fixture.run_expanded(&expanded);
        if source.contains("fn helper()") {
            assert!(
                expanded.contains("fn helper() -> i64 { 42 }"),
                "support source must remain actual original text"
            );
        }
    }
}

#[test]
fn repeated_antiquotes_have_distinct_expanded_occurrences_and_current_hashes() {
    for (source, expected) in [
        (REPEATED.to_owned(), "42"),
        (
            format!(
                "// 🦀😀 CRLF revision\r\n{}",
                REPEATED.replace("let n = 21", "let n = 20")
            ),
            "40",
        ),
    ] {
        let fixture = Fixture::new(&source);
        let report = fixture.check(&[]);
        let expanded = report["expanded_source"].as_str().unwrap();
        let arg = source.find("twice!(n)").unwrap() + "twice!(".len();
        let spans: BTreeSet<_> = report["source_map"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["original"] == json!({"start":arg,"end":arg+1}))
            .map(|entry| {
                let r = range(&entry["expanded"], expanded);
                (r.start, r.end)
            })
            .filter(|(a, b)| a < b)
            .collect();
        assert!(
            spans.len() >= 2,
            "each copied slot occurrence needs current distinct provenance: {report}"
        );
        let spans: Vec<_> = spans.into_iter().collect();
        assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0));
        fixture.run_value(expanded, expected);
    }
}

#[test]
fn linear_duplication_and_trailing_semicolon_fail_the_actual_normal_checker() {
    for (source, code) in [
        (
            include_str!("fixtures/source_macros/linear_duplicate.ves"),
            "E0303",
        ),
        (
            include_str!("fixtures/source_macros/trailing_unit.ves"),
            "E0200",
        ),
    ] {
        let fixture = Fixture::new(source);
        let report = fixture.reject(&[]);
        assert_eq!(report["stage"], "check", "{report}");
        let candidate = report["expanded_source"].as_str().expect(
            "a check-stage rejection retains its unaccepted candidate for diagnostic positions",
        );
        let diagnostics = report["diagnostics"].as_array().unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|d| d["code"] == code)
            .expect("genuine compiler error required");
        let primary = range(&diagnostic["span"], candidate);
        assert!(!primary.is_empty());
        if code == "E0303" {
            let earlier = diagnostic["related"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["message"] == "earlier ownership-taking use of this place")
                .unwrap();
            let earlier = range(&earlier["span"], candidate);
            assert!(earlier.end <= primary.start);
            assert_eq!(&candidate[earlier], &candidate[primary]);
        }
    }
}

#[test]
fn transitive_effects_require_both_actual_macro_emits_and_caller_ceiling() {
    let fixture = Fixture::new(TRANSITIVE);
    assert_eq!(fixture.reject(&[])["stage"], "authority");
    for (emits, ceiling) in [(false, true), (true, false)] {
        let text = policy("leaky", "Log", emits, ceiling);
        let path = fixture.policy(&text);
        assert_eq!(
            fixture.reject(&["--policy", path.to_str().unwrap()])["stage"],
            "authority"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }
    let path = fixture.policy(&policy("leaky", "Log", true, true));
    let args = ["--policy", path.to_str().unwrap()];
    let checked = fixture.check(&args);
    assert_eq!(
        fixture.expand(&args),
        checked["expanded_source"].as_str().unwrap()
    );
    // No runtime call here: the expanded main has an unhandled Log capability.
}

#[test]
fn unchanged_declared_rows_cannot_hide_current_implicit_failure_requirements() {
    let pure = Fixture::new(PURE_ROW);
    pure.check(&[]);
    for (source, name, argument) in [
        (OVERFLOW, "bump", "ArithOverflow"),
        (RUNTIME_FAIL, "division", "RuntimeError"),
    ] {
        let fixture = Fixture::new(source);
        let denied = fixture.reject(&[]);
        assert_eq!(denied["stage"], "authority", "{denied}");
        let path = fixture.policy(&policy(name, "builtin::Fail", false, true));
        assert_eq!(
            fixture.reject(&["--policy", path.to_str().unwrap()])["stage"],
            "authority"
        );
        let path = fixture.policy(&policy(name, "builtin::Fail", true, true));
        let checked = fixture.check(&["--policy", path.to_str().unwrap()]);
        assert!(
            checked["expanded_source"]
                .as_str()
                .unwrap()
                .contains(argument)
        );
    }
}

#[test]
fn malformed_unsupported_and_resource_inputs_never_publish_partial_acceptance() {
    for source in [
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{unknown!(42)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{keep!(keep!(42))}",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{keep!()}",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{keep!(1,2)}",
        "macro bad($x:expr)->expr{quote{let $x=42;$x}} fn main()->i64{bad!(name)}",
        "macro bad($x:ty)->expr{quote{42}} fn main()->i64{bad!(i64)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(in n:i64)->i64{keep!(n)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main[T:mod copy]()->i64{keep!(42)}",
        "macro call($x:expr)->expr{quote{$x(2)}} fn main()->i64{call!(let a=1;a)}",
        "#[wasm_macro(component=\"file:///must-not-be-loaded.wasm\")] macro x()->expr{quote{42}} fn main()->i64{x!()}",
        "effect E[T:mod copy]{fn take(in x:T);} macro emit()->expr{quote{E::take(1)}} fn main()eff[E[u64]]{emit!()}",
    ] {
        Fixture::new(source).reject(&[]);
    }
    let source = format!("//{}\n{KEEP}", "x".repeat(128 * 1024));
    assert_eq!(Fixture::new(&source).reject(&[])["stage"], "resource");
}

#[test]
fn package_and_bounded_policy_configuration_cannot_supply_approval_or_unknown_grants() {
    let fixture = Fixture::new(KEEP);
    assert_eq!(
        fixture.check(&["--package", "@acceptance/macros"])["package"],
        "@acceptance/macros"
    );
    for package in ["@vesper/builtin", "bad package"] {
        assert_eq!(fixture.reject(&["--package", package])["stage"], "config");
    }
    for text in [
        r#"{"schema":"vesper.forge.policy/1","approved":true}"#,
        r#"{"schema":"vesper.forge.policy/1","caller_ceiling":["Unknown"]}"#,
        r#"{"schema":"vesper.forge.policy/1","caller_ceiling":[],"caller_ceiling":["builtin::Fail"]}"#,
        r#"{"schema":"vesper.forge.policy/1","macro_emits":[{"name":"keep","effects":[]},{"name":"keep","effects":[]}]}"#,
        r#"{"schema":"vesper.forge.policy/1","macro_emits":[{"name":"missing","effects":[]}]}"#,
    ] {
        let path = fixture.policy(text);
        assert_eq!(
            fixture.reject(&["--policy", path.to_str().unwrap()])["stage"],
            "config"
        );
    }
    let path = fixture.policy(&" ".repeat(65537));
    let report = fixture.reject(&["--policy", path.to_str().unwrap()]);
    assert!(matches!(
        report["stage"].as_str(),
        Some("config" | "resource")
    ));
    let expanded = fixture.command("expand", &["--json"]);
    assert_eq!(expanded.status.code(), Some(1));
    assert!(expanded.stdout.is_empty());
}

#[test]
fn ordinary_check_and_lsp_never_implicitly_expand_unchecked_original_source() {
    let fixture = Fixture::new(KEEP);
    let old = Command::new(binary())
        .arg("check")
        .arg(&fixture.path)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(old.status.code(), Some(1));
    assert!(
        !serde_json::from_slice::<Vec<Value>>(&old.stdout)
            .unwrap()
            .is_empty()
    );
    let uri = "file:///not-implicitly-loaded/source-macros.ves";
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"version":1,"languageId":"vesper","text":KEEP}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":5},"approved":true}}),
        json!({"jsonrpc":"2.0","id":900,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit","params":{}}),
    ];
    let mut input = Vec::new();
    for message in messages {
        let body = serde_json::to_vec(&message).unwrap();
        write!(input, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        input.extend(body);
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
            .take(4 * 1024 * 1024 + 1)
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
            panic!("macro-default stdio timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap().unwrap();
    let bytes = reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&errors));
    assert!(bytes.len() <= 4 * 1024 * 1024 && errors.len() <= 1024 * 1024);
    let mut cursor = std::io::Cursor::new(bytes);
    let mut replies = Vec::<Value>::new();
    while let Some(bytes) =
        vesper_lsp::read_frame(&mut cursor, vesper_lsp::ProtocolLimits::default()).unwrap()
    {
        vesper_lsp::parse_envelope(&bytes, vesper_lsp::ProtocolLimits::default()).unwrap();
        replies.push(serde_json::from_slice(&bytes).unwrap());
    }
    let publication = replies
        .iter()
        .find(|r| r["method"] == "textDocument/publishDiagnostics")
        .unwrap();
    assert_eq!(publication["params"]["vesper"]["status"], "parse_error");
    assert_eq!(
        publication["params"]["vesper"]["source_blake3"],
        blake3::hash(KEEP.as_bytes()).to_hex().as_str()
    );
    assert_eq!(
        replies.iter().find(|r| r["id"] == 2).unwrap()["error"]["code"],
        -32003
    );
}
