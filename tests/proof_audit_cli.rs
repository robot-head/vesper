use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static SERIAL: Mutex<()> = Mutex::new(());

/// Resolve `lean` to the binary of the toolchain this checkout pins.
///
/// The fixtures run Lean from a temporary directory. Through the elan proxy,
/// `lean` there finds no `lean-toolchain` file and fails unless a default
/// toolchain is configured, which CI deliberately does not do. Asking the
/// proxy for its prefix from the repository root pins the binary instead.
fn resolve_lean(lean: String) -> String {
    let Ok(output) = Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output()
    else {
        return lean;
    };
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let binary = PathBuf::from(prefix)
        .join("bin")
        .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
    if output.status.success() && binary.is_file() {
        binary.to_string_lossy().into_owned()
    } else {
        lean
    }
}
struct Fixture {
    root: PathBuf,
    lean: String,
}
impl Fixture {
    fn new() -> Option<Self> {
        let lean = resolve_lean(std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into()));
        let version = match Command::new(&lean).arg("--version").output() {
            Ok(output) => output,
            Err(e) => {
                eprintln!("skipping real Lean artifact audit: {e}");
                return None;
            }
        };
        if !version.status.success()
            || !String::from_utf8_lossy(&version.stdout).contains("version 4.24.0,")
        {
            eprintln!("skipping real Lean artifact audit: Lean 4.24.0 is unavailable");
            return None;
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-proof-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Some(Self { root, lean })
    }
    fn compile(&self, module: &str, source: &str) {
        fs::write(self.root.join(format!("{module}.lean")), source).unwrap();
        let output = Command::new(&self.lean)
            .current_dir(&self.root)
            .env("LEAN_PATH", &self.root)
            .args(["-o", &format!("{module}.olean"), &format!("{module}.lean")])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn audit(&self, module: &str, theorem: &str, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(["proof-audit", module, "--root"])
            .arg(&self.root)
            .args(["--theorem", theorem, "--lean", &self.lean, "--json"])
            .args(extra)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = report(output);
    assert_eq!(report["accepted"], true);
    assert_eq!(report["kernel_replayed"], true);
    assert_eq!(report["cache_eligible"], false);
    report
}

#[test]
fn kernel_replays_actual_foundation_theorems_and_records_real_inputs() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(f) = Fixture::new() else { return };
    f.compile("Model", include_str!("../lean/Foundations.lean"));
    let before = fs::read(f.root.join("Model.olean")).unwrap();
    let report = success(&f.audit(
        "Model",
        "Vesper.Foundations.join_comm",
        &["--theorem", "Vesper.Foundations.linear_exactly_once"],
    ));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["theorems"].as_array().unwrap().len(), 2);
    assert!(report["checked_constants"].as_u64().unwrap() > 0);
    for theorem in report["theorems"].as_array().unwrap() {
        assert_eq!(theorem["axioms"], serde_json::json!([]));
    }
    assert_eq!(report["lean_binary_blake3"].as_str().unwrap().len(), 64);
    assert_eq!(report["auditor_blake3"].as_str().unwrap().len(), 64);
    assert!(
        report["artifact_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "Model.olean")
    );
    assert!(
        report["imports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|module| module["name"] == "Model" && module["scope"] == "artifact")
    );
    assert!(
        report["imports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|module| module["scope"] == "toolchain")
    );
    assert_eq!(fs::read(f.root.join("Model.olean")).unwrap(), before);
}

#[test]
fn transitive_axioms_need_explicit_policy_and_definition_changes_alter_inventory() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(f) = Fixture::new() else { return };
    f.compile(
        "Base",
        "namespace Example\naxiom assumption : True\ndef seed : Nat := 1\nend Example\n",
    );
    f.compile("Model","import Base\nnamespace Example\ntheorem exposed : True := assumption\ntheorem seed_refl : seed = seed := rfl\nend Example\n");
    let rejected = f.audit("Model", "Example.exposed", &[]);
    assert!(!rejected.status.success());
    let denied = report(&rejected);
    assert_eq!(denied["kernel_replayed"], true);
    assert_eq!(
        denied["forbidden_axioms"],
        serde_json::json!(["Example.assumption"])
    );
    let accepted = success(&f.audit(
        "Model",
        "Example.exposed",
        &["--allow-axiom", "Example.assumption"],
    ));
    assert_eq!(
        accepted["theorems"][0]["axioms"],
        serde_json::json!(["Example.assumption"])
    );
    f.compile(
        "Base",
        "namespace Example\naxiom assumption : True\ndef seed : Nat := 2\nend Example\n",
    );
    f.compile("Model","import Base\nnamespace Example\ntheorem exposed : True := assumption\ntheorem seed_refl : seed = seed := rfl\nend Example\n");
    let changed = success(&f.audit(
        "Model",
        "Example.exposed",
        &["--allow-axiom", "Example.assumption"],
    ));
    assert_ne!(
        accepted["artifact_inventory_blake3"],
        changed["artifact_inventory_blake3"]
    );
}

#[test]
fn forged_axiom_free_olean_does_not_pass_kernel_replay() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(f) = Fixture::new() else { return };
    f.compile(
        "Bad",
        r#"
import Lean
open Lean Elab Command in
set_option debug.skipKernelTC true in
run_cmd do
  liftCoreM <| addDecl <| .thmDecl {
    name := `Example.forged
    levelParams := []
    type := mkConst ``False
    value := mkConst ``True.intro
  }
"#,
    );
    fs::write(
        f.root.join("Probe.lean"),
        "import Bad\n#print axioms Example.forged\n#check Example.forged\n",
    )
    .unwrap();
    let probe = Command::new(&f.lean)
        .current_dir(&f.root)
        .env("LEAN_PATH", &f.root)
        .args(["--trust=0", "Probe.lean"])
        .output()
        .unwrap();
    assert!(probe.status.success());
    assert!(String::from_utf8_lossy(&probe.stdout).contains("does not depend on any axioms"));
    let output = f.audit("Bad", "Example.forged", &[]);
    assert!(!output.status.success());
    let error = report(&output);
    assert_eq!(error["status"], "error");
    assert_eq!(error["error"]["kind"], "audit_failed");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("mismatch")
    );
    assert!(error.get("accepted").is_none());
}

#[test]
fn missing_non_theorem_and_partial_artifacts_cannot_claim_success() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(f) = Fixture::new() else { return };
    let missing = f.audit("Absent", "Example.missing", &[]);
    assert!(!missing.status.success());
    assert_eq!(report(&missing)["error"]["kind"], "audit_failed");
    f.compile(
        "Model",
        "def Example.value : Nat := 1\ntheorem Example.ok : True := True.intro\n",
    );
    for name in ["Example.missing", "Example.value"] {
        let output = f.audit("Model", name, &[]);
        assert!(!output.status.success());
        assert_eq!(report(&output)["error"]["kind"], "audit_failed");
    }
    f.compile("Model","partial def Example.loop (n : Nat) : Nat := Example.loop n\ntheorem Example.ok : True := True.intro\n");
    let output = f.audit("Model", "Example.ok", &[]);
    assert!(!output.status.success());
    assert_eq!(report(&output)["error"]["kind"], "audit_failed");
}

#[test]
fn sorry_is_rejected_and_package_initializers_are_not_executed() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(f) = Fixture::new() else { return };
    f.compile("Model", "theorem Example.incomplete : False := by sorry\n");
    let output = f.audit("Model", "Example.incomplete", &[]);
    assert!(!output.status.success());
    let denied = report(&output);
    assert_eq!(denied["kernel_replayed"], true);
    assert_eq!(denied["forbidden_axioms"], serde_json::json!(["sorryAx"]));

    let sentinel = f.root.join("initializer-must-not-run.txt");
    let quoted = serde_json::to_string(sentinel.to_str().unwrap()).unwrap();
    f.compile("Model", &format!(
        "initialize do IO.FS.writeFile {quoted} \"ran\"\ntheorem Example.ok : True := True.intro\n"
    ));
    // Source elaboration is a separate, explicitly trusted test setup step.
    // The audit itself imports artifact data without running its initializer.
    if sentinel.exists() {
        fs::remove_file(&sentinel).unwrap();
    }
    let output = f.audit("Model", "Example.ok", &[]);
    let _ = report(&output);
    assert!(!sentinel.exists());
}

#[test]
fn invalid_names_options_and_missing_artifacts_fail_before_any_certificate() {
    for args in [
        vec![
            "proof-audit",
            "../Escape",
            "--root",
            ".",
            "--theorem",
            "Example.ok",
        ],
        vec![
            "proof-audit",
            "Lean",
            "--root",
            ".",
            "--theorem",
            "True.intro",
        ],
        vec!["proof-audit", "Model", "--root", "."],
        vec![
            "proof-audit",
            "Model",
            "--root",
            ".",
            "--theorem",
            "bad;run_cmd",
        ],
        vec![
            "proof-audit",
            "Model",
            "--root",
            ".",
            "--theorem",
            "Example.ok",
            "--allow-axiom",
            "sorryAx",
        ],
        vec![
            "proof-audit",
            "Model",
            "--root",
            ".",
            "--theorem",
            "Example.ok",
            "--timeout",
            "0",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    let mut args = vec![
        "proof-audit".to_string(),
        "Model".into(),
        "--root".into(),
        ".".into(),
    ];
    for index in 0..129 {
        args.extend(["--theorem".into(), format!("Example.theorem{index}")]);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("at most 128"));
}
