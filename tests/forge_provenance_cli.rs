//! `vesper forge lean --provenance` and the macro provenance admission rule of
//! `vesper proof-audit` on real Lean 4.35 artifacts (skipped without Lean).
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Each audit may use Lean -M8192; run the real replays one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

const MIXED: &str = include_str!("fixtures/source_macros/provenance_mixed.ves");

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-forge-provenance-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ok(output: &Output) -> &Output {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Expand `MIXED`, returning (Lean text, manifest path).
fn forge_lean(dir: &Path) -> (String, PathBuf) {
    let source = dir.join("mixed.ves");
    fs::write(&source, MIXED).unwrap();
    let manifest = dir.join("provenance.json");
    let output = vesper()
        .args(["forge", "lean"])
        .arg(&source)
        .arg("--provenance")
        .arg(&manifest)
        .output()
        .unwrap();
    ok(&output);
    (String::from_utf8(output.stdout).unwrap(), manifest)
}

#[test]
fn forge_lean_writes_the_same_table_that_check_reports() {
    let dir = Dir::new("cli");
    let (lean, manifest) = forge_lean(&dir.0);
    let table: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let check = vesper()
        .args(["forge", "check"])
        .arg(dir.0.join("mixed.ves"))
        .arg("--json")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&ok(&check).stdout).unwrap();
    assert_eq!(report["provenance"], table);
    assert_eq!(table["schema"], "vesper.forge.provenance/1");
    assert!(lean.contains(table["digest"].as_str().unwrap()));
    assert!(lean.contains("«main$macro_provenance»"));
    assert!(lean.contains("«doubled$macro_provenance»"));
    assert!(!lean.contains("«plain$macro_provenance»"));

    // The manifest is never silently replaced, and nothing is emitted then.
    let again = vesper()
        .args(["forge", "lean"])
        .arg(dir.0.join("mixed.ves"))
        .arg("--provenance")
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(again.stdout.is_empty());
    // --provenance belongs to `lean` only.
    let misuse = vesper()
        .args(["forge", "expand"])
        .arg(dir.0.join("mixed.ves"))
        .args(["--provenance", "x.json"])
        .output()
        .unwrap();
    assert!(!misuse.status.success());
}

#[test]
fn rejected_check_reports_macro_origins_for_diagnostics() {
    let dir = Dir::new("reject");
    let source = dir.0.join("bad.ves");
    fs::write(
        &source,
        "macro bad($x: expr) -> expr {\n    quote {\n        wrapping_add($x, true)\n    }\n}\nfn main() -> i64 {\n    let n = 42;\n    bad!(n)\n}\n",
    )
    .unwrap();
    let output = vesper()
        .args(["forge", "check"])
        .arg(&source)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "check");
    assert_eq!(report["origins"][0]["macro"], "bad");
    assert_eq!(report["origins"][0]["kind"], "generated");
    assert_eq!(
        report["origins"].as_array().unwrap().len(),
        report["diagnostics"].as_array().unwrap().len()
    );
}

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

fn lean() -> Option<String> {
    let lean = resolve_lean(std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into()));
    match Command::new(&lean).arg("--version").output() {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("version 4.35.0,") =>
        {
            Some(lean)
        }
        _ => {
            eprintln!("skipping real Lean macro provenance audit: Lean 4.35.0 is unavailable");
            None
        }
    }
}

const PROOF: &str = "import Expanded\n\
theorem Proof.main_eq : Vesper.main = 42 := by decide\n\
theorem Proof.plain_eq : Vesper.plain 7 = 7 := by decide\n\
theorem Proof.block_eq : Vesper.«$block_main_0» = 42 := by decide\n";

fn compile(lean: &str, root: &Path, expanded: &str) {
    fs::write(root.join("Expanded.lean"), expanded).unwrap();
    fs::write(root.join("Proof.lean"), PROOF).unwrap();
    for module in ["Expanded", "Proof"] {
        let output = Command::new(lean)
            .current_dir(root)
            .env("LEAN_PATH", root)
            .args(["-o", &format!("{module}.olean"), &format!("{module}.lean")])
            .output()
            .unwrap();
        ok(&output);
    }
}

fn audit(lean: &str, root: &Path, theorem: &str, manifest: Option<&Path>) -> Value {
    let mut command = vesper();
    command
        .args(["proof-audit", "Proof", "--root"])
        .arg(root)
        .args([
            "--theorem",
            theorem,
            "--lean",
            lean,
            "--timeout",
            "300",
            "--json",
        ]);
    if let Some(manifest) = manifest {
        command.arg("--macro-provenance").arg(manifest);
    }
    let output = command.output().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(output.status.success(), value["status"] == "audited");
    value
}

fn error(value: &Value) -> &str {
    assert_eq!(value["status"], "error", "{value}");
    value["error"]["message"].as_str().unwrap()
}

#[test]
fn proof_audit_admits_expanded_code_only_with_matching_provenance() {
    let Some(lean) = lean() else { return };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let work = Dir::new("audit");
    let (text, manifest) = forge_lean(&work.0);
    let root = Dir::new("audit-root");
    compile(&lean, &root.0, &text);

    // A theorem about expanded code without its manifest is refused.
    let refused = audit(&lean, &root.0, "Proof.main_eq", None);
    let message = error(&refused);
    assert!(
        message.contains("depends on macro-expanded definitions")
            && message.contains("Vesper.main")
            && message.contains("--macro-provenance"),
        "{message}"
    );

    // With the manifest it is admitted, and the report names the macros.
    let admitted = audit(&lean, &root.0, "Proof.main_eq", Some(&manifest));
    assert_eq!(admitted["accepted"], true, "{admitted}");
    let provenance = &admitted["macro_provenance"];
    let table: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(provenance["manifest_digest"], table["digest"]);
    let dependencies = provenance["theorems"][0]["expanded_dependencies"]
        .as_array()
        .unwrap();
    let functions = dependencies
        .iter()
        .map(|d| d["function"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    // main calls doubled, so both expansions reach the theorem.
    assert_eq!(functions, ["doubled", "main"].into_iter().collect());
    assert!(
        dependencies
            .iter()
            .any(|d| d["constant"] == "Vesper.main" && d["macros"] == serde_json::json!(["keep"]))
    );
    assert!(
        dependencies
            .iter()
            .any(|d| d["constant"] == "Vesper.doubled"
                && d["macros"] == serde_json::json!(["twice"]))
    );

    // A theorem about ordinary source in the same module needs no manifest.
    let plain = audit(&lean, &root.0, "Proof.plain_eq", None);
    assert_eq!(plain["accepted"], true, "{plain}");
    assert_eq!(plain["macro_provenance"]["theorems"], serde_json::json!([]));

    // A manifest for different code is refused.
    let mut forged = table.clone();
    forged["expansions"][0]["macro"] = "other".into();
    let forged_path = work.0.join("forged.json");
    fs::write(&forged_path, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(
        error(&audit(&lean, &root.0, "Proof.main_eq", Some(&forged_path)))
            .contains("macro provenance mismatch")
    );
}

#[test]
fn proof_audit_refuses_stripped_provenance_markers() {
    let Some(lean) = lean() else { return };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let work = Dir::new("strip");
    let (text, manifest) = forge_lean(&work.0);

    // Drop only `main`'s markers: the module marker still claims this table.
    let stripped = text
        .lines()
        .filter(|line| {
            !line.contains("«main$macro_provenance»")
                && !line.contains("«$block_main_0$macro_provenance»")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(stripped, text);
    let root = Dir::new("strip-root");
    compile(&lean, &root.0, &stripped);
    let message = error(&audit(&lean, &root.0, "Proof.main_eq", Some(&manifest))).to_owned();
    assert!(
        message.contains("provenance marker is missing"),
        "{message}"
    );
    // A theorem about the stripped `$block_main_0` helper alone is caught by
    // the extractor's ownership rule, not only by bare function names.
    let message = error(&audit(&lean, &root.0, "Proof.block_eq", Some(&manifest))).to_owned();
    assert!(
        message.contains("Vesper.$block_main_0") && message.contains("marker is missing"),
        "{message}"
    );

    // Drop every marker: the manifest no longer matches any module marker.
    let bare = text
        .lines()
        .filter(|line| !line.contains("$macro_provenance»"))
        .collect::<Vec<_>>()
        .join("\n");
    let root = Dir::new("bare-root");
    compile(&lean, &root.0, &bare);
    assert!(
        error(&audit(&lean, &root.0, "Proof.main_eq", Some(&manifest)))
            .contains("has no module marker")
    );
}
