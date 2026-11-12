//! Actual parameter-aware source compilation and explicit editor preview acceptance.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_SOURCE_MACRO_PARAMETERS_BINARY")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_vesper").map(PathBuf::from))
        .expect("actual vesper or VESPER_SOURCE_MACRO_PARAMETERS_BINARY required");
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
            "vesper-source-macro-parameters-{}-{}",
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
const GLOBAL: &str = include_str!("fixtures/source_macro_parameters/global_capture.ves");
const REPEATED: &str = include_str!("fixtures/source_macro_parameters/repeated.ves");
const PURE: &str = include_str!("fixtures/source_macro_parameters/helper_pure.ves");
const DUPLICATE: &str = include_str!("fixtures/source_macro_parameters/linear_duplicate.ves");
const LEAK: &str = include_str!("fixtures/source_macro_parameters/linear_leak.ves");

#[test]
fn actual_parameter_hygiene_and_supported_passes_expand_check_and_run_42() {
    for source in [
        GLOBAL,
        REPEATED,
        include_str!("fixtures/source_macro_parameters/argument_capture.ves"),
        include_str!("fixtures/source_macro_parameters/reference_capture.ves"),
        include_str!("fixtures/source_macro_parameters/initializer.ves"),
        include_str!("fixtures/source_macro_parameters/passes.ves"),
        include_str!("fixtures/source_macro_parameters/linear_consume.ves"),
        include_str!("fixtures/source_macro_parameters/self_parameter.ves"),
    ] {
        let fixture = Fixture::new(source);
        let checked = fixture.check(&[]);
        let expanded = fixture.expand(&[]);
        assert_eq!(checked["expanded_source"], expanded);
        fixture.run_expanded(&expanded);
    }
}

#[test]
fn renamed_parameter_header_and_repeated_argument_have_actual_byte_anchors() {
    let fixture = Fixture::new(REPEATED);
    let report = fixture.check(&[]);
    let expanded = report["expanded_source"].as_str().unwrap();
    assert!(expanded.contains("/* header λ 🦀 */"));
    let parameter = REPEATED.find("value:").unwrap();
    let maps = report["source_map"].as_array().unwrap();
    let header = maps
        .iter()
        .filter(|m| m["original"]["start"] == parameter && m["original"]["end"] == parameter + 5)
        .collect::<Vec<_>>();
    assert_eq!(header.len(), 1, "{maps:?}");
    assert_eq!(header[0]["kind"], "source");
    let renamed = &expanded[range(&header[0]["expanded"], expanded)];
    assert_ne!(renamed, "value");
    assert!(renamed.starts_with("__forge_local_"));
    assert!(expanded.matches(renamed).count() >= 3);
    let argument = REPEATED.find("twice!(value)").unwrap() + "twice!(".len();
    let repeated = maps
        .iter()
        .filter(|m| m["original"]["start"] == argument && m["original"]["end"] == argument + 5)
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 2, "{maps:?}");
    assert_ne!(repeated[0]["expanded"], repeated[1]["expanded"]);
    for entry in repeated {
        assert_eq!(&expanded[range(&entry["expanded"], expanded)], renamed);
    }
}

#[test]
fn real_linear_errors_and_invalid_original_headers_never_publish_acceptance() {
    for (source, code) in [(DUPLICATE, "E0303"), (LEAK, "E0302")] {
        let rejected = Fixture::new(source).reject(&[]);
        assert_eq!(rejected["stage"], "check", "{rejected}");
        assert!(
            rejected["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == code)
        );
    }
    let duplicate = Fixture::new(include_str!(
        "fixtures/source_macro_parameters/duplicate_names.ves"
    ))
    .reject(&[]);
    assert_eq!(duplicate["stage"], "parse");
    assert!(duplicate["message"].as_str().unwrap().contains("duplicate"));
    for source in [
        include_str!("fixtures/source_macro_parameters/projected.ves"),
        "macro keep($x:expr)->expr{quote{$x}} fn forward[T:mod copy](in n:T)->T{keep!(n)}",
        "macro keep($x:expr)->expr{quote{$x}} fn forward(in n:i64)->i64{keep!(n)",
    ] {
        Fixture::new(source).reject(&[]);
    }
    let parameters = (0..9)
        .map(|n| format!("in p{n}:i64"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "macro keep($x:expr)->expr{{quote{{$x}}}} fn forward({parameters})->i64{{keep!(p0)}}"
    );
    assert_eq!(Fixture::new(&source).reject(&[])["stage"], "resource");
}

#[test]
fn same_parameter_headers_do_not_cache_changed_helper_authority() {
    Fixture::new(PURE).check(&[]);
    let fallible = PURE.replace("wrapping_add(value, 1)", "value + 1");
    assert_ne!(fallible, PURE);
    let failure = Fixture::new(&fallible).reject(&[]);
    assert_eq!(failure["stage"], "authority", "{failure}");
}
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
#[test]
fn actual_unsaved_parameter_preview_keeps_original_semantics_unavailable() {
    let uri = format!("untitled:params/{}", std::process::id());
    let fallible = PURE.replace("wrapping_add(value, 1)", "value + 1");
    let replies = run(vec![
        open(&uri, 1, GLOBAL),
        preview(2, &uri, 1),
        request(
            3,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":{"line":2,"character":3}}),
        ),
        change(&uri, 2, PURE),
        preview(4, &uri, 2),
        change(&uri, 3, &fallible),
        preview(5, &uri, 3),
        preview(6, &uri, 2),
        change(&uri, 4, DUPLICATE),
        preview(7, &uri, 4),
    ]);
    for (id, version, source) in [(2, 1, GLOBAL), (4, 2, PURE)] {
        let value = result(&replies, id);
        assert_eq!(value["status"], "expanded_checked");
        assert_eq!(value["scope"], "expanded_source_only");
        assert_eq!(value["version"], version);
        assert_eq!(
            value["source_blake3"],
            blake3::hash(source.as_bytes()).to_hex().as_str()
        );
        let comparison = Fixture::new(source).check(&[]);
        for key in ["expanded_source", "expanded_blake3", "source_map", "audits"] {
            assert_eq!(value[key], comparison[key], "{key}");
        }
    }
    error(&replies, 3, -32003);
    error(&replies, 6, -32801);
    for (id, stage) in [(5, "authority"), (7, "check")] {
        let value = result(&replies, id);
        assert_eq!(value["status"], "rejected");
        assert_eq!(value["stage"], stage);
        assert_eq!(value["source_map"], json!([]));
        assert_eq!(value["audits"], json!([]));
        assert!(value.get("expanded_blake3").is_none());
    }
    let publishes = replies
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .collect::<Vec<_>>();
    assert_eq!(publishes.len(), 4);
    for publication in publishes {
        assert_eq!(publication["params"]["vesper"]["status"], "parse_error");
        assert!(
            !publication["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
