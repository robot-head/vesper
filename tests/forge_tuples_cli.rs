//! Actual tuple-aware source compilation and explicit editor preview acceptance.
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
    let path = std::env::var_os("VESPER_SOURCE_MACRO_TUPLES_BINARY")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_vesper").map(PathBuf::from))
        .expect("actual vesper or VESPER_SOURCE_MACRO_TUPLES_BINARY required");
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
            "vesper-source-macro-tuples-{}-{}",
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
const POSITIVE: &[&str] = &[
    include_str!("fixtures/source_macro_tuples/unit.ves"),
    include_str!("fixtures/source_macro_tuples/grouped.ves"),
    include_str!("fixtures/source_macro_tuples/singleton.ves"),
    include_str!("fixtures/source_macro_tuples/multiple.ves"),
    include_str!("fixtures/source_macro_tuples/nested.ves"),
    include_str!("fixtures/source_macro_tuples/unit_slot.ves"),
    include_str!("fixtures/source_macro_tuples/singleton_slot.ves"),
    include_str!("fixtures/source_macro_tuples/nested_slot.ves"),
    include_str!("fixtures/source_macro_tuples/input_slot.ves"),
    include_str!("fixtures/source_macro_tuples/parameter.ves"),
    include_str!("fixtures/source_macro_tuples/argument_capture.ves"),
    include_str!("fixtures/source_macro_tuples/reference_capture.ves"),
    include_str!("fixtures/source_macro_tuples/global_capture.ves"),
    include_str!("fixtures/source_macro_tuples/initializer.ves"),
    include_str!("fixtures/source_macro_tuples/element_scope.ves"),
    include_str!("fixtures/source_macro_tuples/evaluation_order.ves"),
    include_str!("fixtures/source_macro_tuples/punctuation.ves"),
    include_str!("fixtures/source_macro_tuples/repeated.ves"),
    include_str!("fixtures/source_macro_tuples/linear_consume.ves"),
    include_str!("fixtures/source_macro_tuples/arity_nine.ves"),
];
const REPEATED: &str = include_str!("fixtures/source_macro_tuples/repeated.ves");
const PURE: &str = include_str!("fixtures/source_macro_tuples/generated_pure.ves");
const CALLER: &str = include_str!("fixtures/source_macro_tuples/caller_effect.ves");
const DUPLICATE: &str = include_str!("fixtures/source_macro_tuples/linear_duplicate.ves");
const LEAK: &str = include_str!("fixtures/source_macro_tuples/linear_leak.ves");

#[test]
fn real_unit_group_singleton_multiple_nested_and_slot_values_run_42() {
    for source in POSITIVE {
        let fixture = Fixture::new(source);
        let report = fixture.check(&[]);
        let expanded = fixture.expand(&[]);
        assert_eq!(report["expanded_source"], expanded);
        fixture.run_expanded(&expanded);
    }
}

#[test]
fn tuple_repetition_preserves_distinct_actual_argument_and_header_anchors() {
    let report = Fixture::new(REPEATED).check(&[]);
    let expanded = report["expanded_source"].as_str().unwrap();
    assert!(expanded.contains("/* tuple , ) λ 🦀 */"));
    let parameter = REPEATED.find("value:").unwrap();
    let argument = REPEATED.find("twice!(value)").unwrap() + "twice!(".len();
    let maps = report["source_map"].as_array().unwrap();
    let header = maps
        .iter()
        .filter(|m| m["original"]["start"] == parameter && m["original"]["end"] == parameter + 5)
        .collect::<Vec<_>>();
    assert_eq!(header.len(), 1, "{maps:?}");
    assert_eq!(header[0]["kind"], "source");
    let renamed = &expanded[range(&header[0]["expanded"], expanded)];
    assert_ne!(renamed, "value");
    let entries = maps
        .iter()
        .filter(|m| m["original"]["start"] == argument && m["original"]["end"] == argument + 5)
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2, "{maps:?}");
    let left = range(&entries[0]["expanded"], expanded);
    let right = range(&entries[1]["expanded"], expanded);
    assert_ne!(left, right);
    assert!(left.end <= right.start || right.end <= left.start);
    assert_eq!(&expanded[left], renamed);
    assert_eq!(&expanded[right], renamed);
}

#[test]
fn actual_product_linearity_and_tuple_field_types_remain_checked() {
    for (source, code) in [
        (DUPLICATE, "E0303"),
        (LEAK, "E0302"),
        (
            include_str!("fixtures/source_macro_tuples/field_mismatch.ves"),
            "E0200",
        ),
        (
            "macro make()->expr{quote{(42,)}} fn forward()->i64{make!()} fn main()->i64{42}",
            "E0200",
        ),
        (
            "macro make()->expr{quote{(42)}} fn forward()->(i64,){make!()} fn main()->i64{42}",
            "E0200",
        ),
    ] {
        let failure = Fixture::new(source).reject(&[]);
        assert_eq!(failure["stage"], "check", "{failure}");
        assert!(
            failure["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == code),
            "{failure}"
        );
    }
}

#[test]
fn malformed_and_bounded_tuple_refusals_never_emit_partial_source() {
    for body in ["(,)", "(1,,2)", "(1 2)", "(1,2]", "((1,),,)"] {
        let source = format!("macro make()->expr{{quote{{{body}}}}} fn main(){{make!()}}");
        Fixture::new(&source).reject(&[]);
    }
    for source in [
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!((42,),)}",
        "fn score(in n:(i64,))->i64{let(x,)=n;x} macro make()->expr{quote{(42,)}} fn main()->i64{score(make!(),)}",
    ] {
        Fixture::new(source).reject(&[]);
    }
    let nested = format!("{}42{}", "(".repeat(160), ",)".repeat(160));
    let source = format!("macro make()->expr{{quote{{{nested}}}}} fn main(){{make!()}}");
    assert_eq!(Fixture::new(&source).reject(&[])["stage"], "resource");
    let values = vec!["0"; 7000].join(",");
    let source = format!("macro make()->expr{{quote{{({values})}}}} fn main(){{make!()}}");
    Fixture::new(&source).reject(&[]);
}

#[test]
fn nested_generated_calls_cannot_hide_authority_but_source_slots_keep_their_origin() {
    Fixture::new(PURE).check(&[]);
    for (old, new) in [
        ("wrapping_add(left, 1)", "left + 1"),
        ("wrapping_add(right, 1)", "right + 1"),
    ] {
        let changed = PURE.replace(old, new);
        assert_ne!(changed, PURE);
        assert_eq!(Fixture::new(&changed).reject(&[])["stage"], "authority");
    }
    // This call is caller-owned source in a slot and its real Fail row remains
    // declared. A checked artifact does not grant execution of an unhandled entry.
    Fixture::new(CALLER).check(&[]);
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
fn current_unsaved_tuple_previews_preserve_original_semantic_boundaries() {
    let uri = format!("untitled:tuples/{}", std::process::id());
    let changed = PURE.replace("wrapping_add(left, 1)", "left + 1");
    let replies = run(vec![
        open(&uri, 1, REPEATED),
        preview(2, &uri, 1),
        change(&uri, 2, PURE),
        preview(3, &uri, 2),
        change(&uri, 3, &changed),
        preview(4, &uri, 3),
        change(&uri, 4, CALLER),
        preview(5, &uri, 4),
        preview(6, &uri, 3),
        request(
            7,
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":{"line":1,"character":3}}),
        ),
        change(&uri, 5, DUPLICATE),
        preview(8, &uri, 5),
    ]);
    for (id, version, source) in [(2, 1, REPEATED), (3, 2, PURE), (5, 4, CALLER)] {
        let value = result(&replies, id);
        assert_eq!(value["status"], "expanded_checked");
        assert_eq!(value["scope"], "expanded_source_only");
        assert_eq!(value["version"], version);
        assert_eq!(
            value["source_blake3"],
            blake3::hash(source.as_bytes()).to_hex().as_str()
        );
        let checked = Fixture::new(source).check(&[]);
        for field in ["expanded_source", "expanded_blake3", "source_map", "audits"] {
            assert_eq!(value[field], checked[field], "{field}");
        }
    }
    for (id, stage) in [(4, "authority"), (8, "check")] {
        let value = result(&replies, id);
        assert_eq!(value["status"], "rejected");
        assert_eq!(value["stage"], stage);
        assert_eq!(value["source_map"], json!([]));
        assert_eq!(value["audits"], json!([]));
        assert!(value.get("expanded_blake3").is_none());
    }
    error(&replies, 6, -32801);
    error(&replies, 7, -32003);
    let publications = replies
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .collect::<Vec<_>>();
    assert_eq!(publications.len(), 5);
    for p in publications {
        assert_eq!(p["params"]["vesper"]["status"], "parse_error");
        assert!(!p["params"]["diagnostics"].as_array().unwrap().is_empty());
    }
}
