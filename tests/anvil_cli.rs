use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct TempDir(PathBuf);
impl TempDir {
    fn new(files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-anvil-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        for (rel, content) in files {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        Self(dir)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn parse_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid json ({e})\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

const ANVIL_SUITE: &str = r#"
linear struct WriteTxn { id: i64 }

fn commit_txn(sink tx: WriteTxn) -> i64 {
    let WriteTxn { id } = tx;
    id
}

fn clamp_zero(x: i64) -> i64 {
    if x < 0 { 0 } else { x }
}

@test
fn anvil_unit_jit() -> bool {
    clamp_zero(-5) == 0 && clamp_zero(9) == 9
}

@test(cases = 32)
@test_case(0, 0)
@test_case(a = 7, b = -3)
fn anvil_property_comm(a: i64, b: i64) -> bool {
    (a == b) == (b == a) && ((a < b) == (b > a))
}

@test(sim_schedules = 4, fault_profile = TornWalSector)
fn anvil_simpulse_dpor() -> bool {
    clamp_zero(42) == 42
}

@test(expect_error = E0302, primary = "tx", related = "linear binding introduced here")
fn anvil_negative_linear_leak() -> i64 {
    let tx = WriteTxn { id: 1 };
    0
}
"#;

#[test]
fn anvil_unifies_unit_property_simpulse_and_expect_error_via_cli() {
    let temp = TempDir::new(&[("anvil.ves", ANVIL_SUITE)]);
    let file = temp.path("anvil.ves");
    let out = vesper(
        &temp.0,
        &["test", file.to_str().unwrap(), "--no-cache", "--json"],
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report = parse_json(&out);
    assert_eq!(report["passed"], true);
    assert_eq!(report["total"], 4);
    assert_eq!(report["failed"], 0);

    let tests = report["files"][0]["tests"].as_array().unwrap();
    assert_eq!(tests[0]["name"], "anvil_unit_jit");
    assert_eq!(tests[0]["engine"], "jit");
    assert_eq!(tests[0]["anvil_mode"], "Unit");

    assert_eq!(tests[1]["name"], "anvil_property_comm");
    assert_eq!(tests[1]["engine"], "crucible");
    assert_eq!(tests[1]["anvil_mode"], "Property");

    assert_eq!(tests[2]["name"], "anvil_simpulse_dpor");
    assert_eq!(tests[2]["engine"], "sim_pulse");
    assert_eq!(tests[2]["anvil_mode"], "SimPulse");

    assert_eq!(tests[3]["name"], "anvil_negative_linear_leak");
    assert_eq!(tests[3]["engine"], "checker");
    assert_eq!(tests[3]["anvil_mode"], "ExpectError");
    assert_eq!(tests[3]["passed"], true);
}

#[test]
fn anvil_property_failure_emits_shrunk_pin_and_vchrono_capsule() {
    let src = r#"
@test(cases = 64)
fn prop_must_be_small(x: i64) -> bool {
    x < 10
}
"#;
    let temp = TempDir::new(&[("fail_prop.ves", src)]);
    let file = temp.path("fail_prop.ves");
    let out = vesper(
        &temp.0,
        &["test", file.to_str().unwrap(), "--no-cache", "--json"],
    );
    assert!(!out.status.success());
    let report = parse_json(&out);
    assert_eq!(report["passed"], false);
    let test = &report["files"][0]["tests"][0];
    assert_eq!(test["name"], "prop_must_be_small");
    assert_eq!(test["engine"], "crucible");
    assert_eq!(test["test_case_pin"], "@test_case(10)");
    let capsule = test["vchrono_capsule"]
        .as_str()
        .expect("vchrono capsule path");
    assert!(
        temp.0.join(capsule).exists(),
        "expected capsule file at {}",
        temp.0.join(capsule).display()
    );
}
