//! Existing default-enabled optimizer versus --no-opt through the actual CLI.
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_REWRITE_WITNESS_BINARY")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_vesper").map(PathBuf::from))
        .expect("actual vesper or VESPER_REWRITE_WITNESS_BINARY required");
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
        if started.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("rewrite CLI timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = reader.join().unwrap();
    let stderr = errors.join().unwrap();
    assert!(stdout.len() <= 4 * 1024 * 1024 && stderr.len() <= 1024 * 1024);
    Output {
        status,
        stdout,
        stderr,
    }
}

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    source: String,
}
impl Fixture {
    fn new(source: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "vesper-rewrite-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("current.ves");
        fs::write(&path, source).unwrap();
        Self {
            directory,
            path,
            source: source.into(),
        }
    }
    fn command(&self, mode: &str, options: &[&str]) -> Output {
        let result = output(
            Command::new(binary())
                .arg(mode)
                .arg(&self.path)
                .args(options),
        );
        assert_eq!(fs::read(&self.path).unwrap(), self.source.as_bytes());
        result
    }
    fn success(&self, mode: &str, options: &[&str]) -> Vec<u8> {
        let output = self.command(mode, options);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn hir(&self, unoptimized: bool) -> Value {
        let options = if unoptimized {
            vec!["--emit", "hir", "--no-opt"]
        } else {
            vec!["--emit", "hir"]
        };
        serde_json::from_slice(&self.success("emit", &options)).unwrap()
    }
    fn agrees(&self, expected: &str) {
        assert_eq!(self.success("check", &["--json"]), b"[]\n");
        for mode in ["run", "jit"] {
            for options in [vec![], vec!["--no-opt"]] {
                assert_eq!(
                    std::str::from_utf8(&self.success(mode, &options))
                        .unwrap()
                        .trim(),
                    expected,
                    "{mode} {options:?}: {}",
                    self.source
                );
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn body<'a>(module: &'a Value, name: &str) -> &'a Value {
    module["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.get("Function"))
        .find(|function| function["name"] == name)
        .unwrap()
        .get("body")
        .unwrap()
}
fn tail(mut expression: &Value) -> &Value {
    while let Some(block) = expression["kind"].get("Block") {
        expression = &block["tail"];
    }
    expression
}

#[test]
fn actual_cli_folds_all_six_families_for_all_four_integer_kinds() {
    for kind in ["u8", "u32", "u64", "i64"] {
        for (expression, expected) in [
            ("2 + 3", 5),
            ("9 - 4", 5),
            ("2 * 3", 6),
            ("wrapping_add(2, 3)", 5),
            ("wrapping_sub(9, 4)", 5),
            ("wrapping_mul(2, 3)", 6),
        ] {
            let fixture = Fixture::new(&format!("fn main() -> {kind} {{ {expression} }}"));
            fixture.agrees(&expected.to_string());
            let original = fixture.hir(true);
            let optimized = fixture.hir(false);
            assert_ne!(
                tail(body(&original, "main"))["kind"],
                tail(body(&optimized, "main"))["kind"]
            );
            assert_eq!(
                tail(body(&optimized, "main"))["kind"]["Literal"]["Int"],
                expected
            );
            assert_eq!(optimized, fixture.hir(false));
        }
    }
}

#[test]
fn actual_cli_preserves_overflow_totality_high_unsigned_values_and_barrier_order() {
    for (source, expected) in [
        (
            "fn f(x: u8) -> u8 eff [Fail[ArithOverflow]] { (x + 1) * 0 } fn main() -> u8 { handle Fail[ArithOverflow] in { f(255) } with { return v => v, raise(e,k) => 42, } }",
            "42",
        ),
        (
            "fn main() -> u64 { wrapping_mul(18446744073709551615, 18446744073709551615) }",
            "1",
        ),
        (
            "fn main() -> u64 { 9223372036854775808 * 1 }",
            "9223372036854775808",
        ),
        (
            "fn main() -> u64 { handle Fail[ArithOverflow] in { 18446744073709551615 * 18446744073709551615 } with { return v => v, raise(e,k) => 42, } }",
            "42",
        ),
        (
            "fn next(inout n: i64) -> i64 { n = wrapping_add(n, 1); n } fn main() -> i64 { let n = 0; let a = wrapping_mul(next(inout n), 0); let b = next(inout n); wrapping_add(wrapping_mul(n, 100), wrapping_add(wrapping_mul(a, 10), b)) }",
            "202",
        ),
    ] {
        let fixture = Fixture::new(source);
        fixture.agrees(expected);
        assert_eq!(fixture.hir(false), fixture.hir(false));
    }
}

#[test]
fn actual_shadowed_callee_identity_and_span_survive_child_folds() {
    let fixture = Fixture::new(
        "fn main() -> i64 { let wrapping_add = |a: i64, b: i64| wrapping_mul(a, b); wrapping_add(6 + 0, 7 + 0) }",
    );
    fixture.agrees("42");
    let original = fixture.hir(true);
    let optimized = fixture.hir(false);
    let before = &tail(body(&original, "main"))["kind"]["Call"];
    let after = &tail(body(&optimized, "main"))["kind"]["Call"];
    assert_eq!(before["callee"], after["callee"]);
    assert_eq!(after["args"][0]["expr"]["kind"]["Literal"]["Int"], 6);
    assert_eq!(after["args"][1]["expr"]["kind"]["Literal"]["Int"], 7);
}

#[test]
fn actual_depth_refusal_keeps_original_hir_bytes_and_program_behavior() {
    let expression = "x".to_owned() + &" + 0".repeat(70);
    let fixture = Fixture::new(&format!(
        "fn f(x: i64) -> i64 {{ {expression} }} fn main() -> i64 {{ f(42) }}"
    ));
    fixture.agrees("42");
    let before = fixture.success("emit", &["--emit", "hir", "--no-opt"]);
    let after = fixture.success("emit", &["--emit", "hir"]);
    assert_eq!(after, before);
    assert_eq!(after, fixture.success("emit", &["--emit", "hir"]));
}

#[test]
fn original_semantic_errors_precede_optimization_and_leave_source_unchanged() {
    let fixture = Fixture::new("fn main() -> u8 { 255 + 1 }");
    let before = fixture.command("run", &["--json", "--no-opt"]);
    let after = fixture.command("run", &["--json"]);
    assert_eq!(before.status.code(), Some(1));
    assert_eq!(after.status.code(), Some(1));
    assert_eq!(before.stdout, after.stdout);
    assert_eq!(before.stderr, after.stderr);
    let diagnostics: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert!(
        diagnostics
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "E0401")
    );
}
