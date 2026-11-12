use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}
fn report(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn traceparent_preserves_hex_ids_and_reserved_flag_bits() {
    let value = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-ff";
    let out = run(&["traceparent", value, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data = report(&out);
    assert_eq!(data["status"], "valid");
    assert_eq!(data["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(data["parent_span_id"], "00f067aa0ba902b7");
    assert_eq!(data["trace_flags"], "ff");
    assert_eq!(data["sampled"], true);
    assert_eq!(data["canonical"], value);
    assert_eq!(
        String::from_utf8(run(&["traceparent", value]).stdout).unwrap(),
        format!("{value}\n")
    );
}
#[test]
fn traceparent_rejects_bad_versions_ids_case_and_extra_arguments() {
    for header in [
        "",
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
    ] {
        let out = run(&["traceparent", header, "--json"]);
        assert!(!out.status.success(), "{header}");
        assert_eq!(report(&out)["status"], "invalid");
    }
    for args in [
        vec!["traceparent"],
        vec!["traceparent", "--bad"],
        vec!["traceparent", "first", "second"],
    ] {
        assert!(!run(&args).status.success());
    }
    assert!(run(&["traceparent", "--help"]).status.success());
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "vesper-json-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn write(&self, bytes: &[u8]) {
        fs::write(self.0.join("data.json"), bytes).unwrap();
    }
    fn check(&self) -> Output {
        run(&[
            "json",
            "check",
            self.0.join("data.json").to_str().unwrap(),
            "--json",
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn json_validates_whole_input_without_rewriting_it() {
    let f = Fixture::new();
    let data = br#"{"ignored":{"escaped":"\uD834\uDD1E"},"rows":[1,true,null]}"#;
    f.write(data);
    let out = f.check();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = report(&out);
    assert_eq!(r["status"], "valid");
    assert_eq!(r["root_kind"], "object");
    assert_eq!(r["bytes"], data.len());
    assert!(r["tokens"].as_u64().unwrap() > 1);
    assert_eq!(fs::read(f.0.join("data.json")).unwrap(), data);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
}
#[test]
fn json_hidden_errors_duplicate_decoded_keys_and_limits_fail() {
    let f = Fixture::new();
    for data in [
        br#"{"ignored":[0,]}"#.as_slice(),
        br#"{"a":1,"\u0061":2}"#,
        br#"["\uD800"]"#,
        b"[01]",
        b"true false",
        b"\"\xff\"",
        b"{\"x\":",
    ] {
        f.write(data);
        let out = f.check();
        assert!(!out.status.success(), "{data:?}");
        assert_eq!(report(&out)["status"], "invalid");
    }
    fs::File::create(f.0.join("data.json"))
        .unwrap()
        .set_len(1024 * 1024 + 1)
        .unwrap();
    let out = f.check();
    assert!(!out.status.success());
    assert_eq!(report(&out)["status"], "invalid");
    for args in [
        vec!["json"],
        vec!["json", "check"],
        vec!["json", "parse", "file"],
        vec!["json", "check", "--bad"],
        vec!["json", "check", "one", "two"],
    ] {
        assert!(!run(&args).status.success());
    }
    assert!(run(&["json", "--help"]).status.success());
}

#[test]
fn traceparent_json_reports_argument_failures_consistently() {
    for args in [
        vec!["traceparent", "--json"],
        vec!["traceparent", "--json", "--bad"],
        vec!["traceparent", "one", "two", "--json"],
    ] {
        let out = run(&args);
        assert!(!out.status.success());
        assert_eq!(report(&out)["status"], "invalid");
    }
}
