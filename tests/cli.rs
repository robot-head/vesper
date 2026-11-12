use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn temp(ext: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "vesper-cli-{}-{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        ext
    ))
}
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}
#[test]
fn examples_execute() {
    for (file, want) in [
        ("transaction", "42"),
        ("projections", "42"),
        ("counter", "3"),
        ("fail", "42"),
        ("fibonacci", "55"),
        ("closures", "42"),
        ("resumptions", "42"),
        ("checked_math", "42"),
    ] {
        let o = cli()
            .args(["run", &format!("examples/{file}.ves")])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), want);
    }
}
#[test]
fn diagnostics_exit_nonzero() {
    let o = cli()
        .args(["check", "examples/rejected/error_unwind.ves", "--json"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let d: Vec<Value> = serde_json::from_slice(&o.stdout).unwrap();
    assert!(d.iter().any(|d| d["code"] == "E0402"));
}
#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "vesper build uses a Unix cc linker; MSVC executable linking is not implemented"
)]
fn native_executable() {
    let path = temp("exe");
    let o = cli()
        .args(["build", "examples/fibonacci.ves", "-o"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let status = Command::new(&path).status().unwrap();
    assert_eq!(status.code(), Some(55));
    fs::remove_file(path).unwrap();
}
#[test]
fn outputs() {
    for kind in ["cst", "hir", "ir", "clif", "lean"] {
        let o = cli()
            .args(["emit", "examples/fibonacci.ves", "--emit", kind])
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(!o.stdout.is_empty());
    }
}
#[test]
fn evaluation_fuel() {
    let path = temp("ves");
    fs::write(
        &path,
        "fn forever() -> i64 { forever() } fn main() -> i64 { forever() }",
    )
    .unwrap();
    let o = cli()
        .arg("run")
        .arg(&path)
        .args(["--fuel", "100"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("fuel exhausted"));
    fs::remove_file(path).unwrap();
}
fn frame(v: Value) -> Vec<u8> {
    let text = v.to_string();
    format!("Content-Length: {}\r\n\r\n{text}", text.len()).into_bytes()
}
#[test]
fn lsp_diagnostics_and_updates() {
    let mut child = cli()
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for v in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///test.ves","version":1,"text":"fn main() { unknown }"}}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///test.ves","version":2},"contentChanges":[{"range":{"start":{"line":0,"character":12},"end":{"line":0,"character":19}},"text":"()"}]}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown"}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ] {
        input.write_all(&frame(v)).unwrap();
    }
    drop(input);
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    let s = String::from_utf8(result.stdout).unwrap();
    assert!(s.contains("E0201"));
    assert!(s.contains("\"diagnostics\":[]"));
    assert!(s.contains("\"version\":2"));
    assert!(s.contains("\"id\":2,\"result\":null"));
    assert!(!s.contains("\"error\":"));
}

#[test]
fn aarch64_object() {
    let path = temp("o");
    let o = cli()
        .args([
            "emit",
            "examples/fibonacci.ves",
            "--emit",
            "object",
            "--target",
            "aarch64-unknown-linux-gnu",
            "-o",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let object = fs::read(&path).unwrap();
    assert_eq!(&object[..4], b"\x7fELF");
    assert_eq!(u16::from_le_bytes([object[18], object[19]]), 183);
    fs::remove_file(path).unwrap();
}
