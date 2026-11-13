//! Wave-six lane 14: path-less `vesper lint` / `vesper test --doc`, hermetic
//! SimPulse doctests, §40.3 diagrams and proof badges through the real CLI.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::doctest::{self, DocTestLimits};

const MANIFEST: &str = include_str!("../crates/vesper-doc/examples/package/Loom.toml");
const LEDGER: &str = include_str!("../crates/vesper-doc/examples/package/src/ledger.ves");
const WIRE: &str = include_str!("../crates/vesper-doc/examples/package/wire.json");
const AUDIT: &str = include_str!("../crates/vesper-doc/tests/fixtures/proof-audit-ledger.json");
const SNAPSHOT: &str = include_str!("../crates/vesper-doc/tests/fixtures/ledger-codex.html");

/// A private copy of files as regular files (Bazel runfiles are symlinks,
/// which package discovery deliberately never follows).
struct Scratch(PathBuf);
impl Scratch {
    fn new(files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-codex-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        Self(root)
    }
    fn package() -> Self {
        Self::new(&[("Loom.toml", MANIFEST), ("src/ledger.ves", LEDGER)])
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}
impl Drop for Scratch {
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
fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lint_without_a_path_finds_the_package_root_from_a_subdirectory() {
    let package = Scratch::package();
    let out = vesper(&package.path("src"), &["lint", "--json"]);
    ok(&out);
    let report = json(&out);
    assert_eq!(report["status"], "passed");
    assert_eq!(report["root"], package.0.display().to_string());
    assert_eq!(
        report["manifest"],
        package.path("Loom.toml").display().to_string()
    );
    assert_eq!(report["files"].as_array().unwrap().len(), 1);
    assert_eq!(report["files"][0]["path"], "src/ledger.ves");
    assert_eq!(report["files"][0]["report"]["status"], "checked");

    let text = vesper(&package.0, &["lint"]);
    ok(&text);
    assert!(String::from_utf8_lossy(&text.stdout).contains("lint: 1 file(s) under"));

    // An explicit directory is the same package; an explicit file keeps the
    // original single-file report shape.
    let dir = vesper(
        &std::env::temp_dir(),
        &["lint", package.0.to_str().unwrap(), "--json"],
    );
    ok(&dir);
    assert_eq!(json(&dir)["root"], package.0.display().to_string());
    let file = vesper(&package.0, &["lint", "src/ledger.ves", "--json"]);
    ok(&file);
    let file = json(&file);
    assert!(file.get("files").is_none());
    assert_eq!(file["status"], "checked");
}

#[test]
fn package_lint_reports_every_file_and_fails_after_all_of_them() {
    let package = Scratch::new(&[
        ("a_good.ves", "fn one() -> u64 {\n    1\n}\n"),
        ("b_bad.ves", "fn broken( -> u64 {\n"),
        ("c_good.ves", "fn two() -> u64 {\n    2\n}\n"),
        ("target/ignored.ves", "fn broken( -> u64 {\n"),
        ("nested/Loom.toml", "[package]\n"),
        ("nested/ignored.ves", "fn broken( -> u64 {\n"),
    ]);
    let out = vesper(&package.0, &["lint", "--json"]);
    assert!(!out.status.success());
    let report = json(&out);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["manifest"], Value::Null);
    let files = report["files"].as_array().unwrap();
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["a_good.ves", "b_bad.ves", "c_good.ves"]);
    assert!(files[1]["error"].is_string());
    assert_eq!(files[2]["report"]["status"], "checked");

    let empty = Scratch::new(&[("notes.txt", "")]);
    for args in [vec!["lint", "--json"], vec!["test", "--doc", "--json"]] {
        let out = vesper(&empty.0, &args);
        assert!(!out.status.success());
        let value = json(&out);
        assert!(
            value["error"].as_str().unwrap().contains("no .ves sources"),
            "{value}"
        );
    }
}

#[test]
fn doctests_without_a_path_run_native_compile_fail_and_simulated_examples() {
    let package = Scratch::package();
    let out = vesper(&package.path("src"), &["test", "--doc", "--json"]);
    ok(&out);
    let report = json(&out);
    assert_eq!(report["passed"], true);
    let file = &report["files"][0];
    assert_eq!(file["file"], "src/ledger.ves");
    let examples = file["report"]["examples"].as_array().unwrap();
    assert_eq!(examples.len(), 3);
    assert_eq!(examples[0]["expected_code"], "E0302");
    assert_eq!(examples[1]["result"], "42");
    assert!(examples[1].get("simulation").is_none());
    let sim = &examples[2]["simulation"];
    assert_eq!(sim["replayed"], true);
    assert_eq!(sim["spec"]["seed"], 0x5EED_CAFE_u64);
    assert_eq!(sim["outcome"]["semantics"], "vesper.codex.sim/1");
    let ops: Vec<_> = sim["outcome"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["operation"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["send", "recv", "write", "flush", "read"]);
    assert_eq!(
        examples[2]["result"], "43",
        "read 42 plus one durable block"
    );
    assert_eq!(sim["outcome"]["durable_blocks"]["0"], 42);

    // Determinism across processes: identical outcome, journal included.
    let again = json(&vesper(&package.0, &["test", "--doc", "--json"]));
    assert_eq!(
        again["files"][0]["report"]["examples"][2]["simulation"],
        *sim
    );

    let text = vesper(&package.0, &["test", "--doc"]);
    ok(&text);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("PASS doctest 2: simulated twice under SimPulse"),
        "{text}"
    );
    assert!(
        text.contains("doctests: 1 file(s) tested, 0 failed"),
        "{text}"
    );
}

#[test]
fn package_doctests_skip_files_without_examples_and_require_at_least_one() {
    let package = Scratch::new(&[
        ("Loom.toml", MANIFEST),
        ("src/ledger.ves", LEDGER),
        ("src/plain.ves", "fn plain() -> u64 {\n    1\n}\n"),
    ]);
    let out = vesper(&package.0, &["test", "--doc", "--json"]);
    ok(&out);
    let report = json(&out);
    assert_eq!(report["files"][1]["file"], "src/plain.ves");
    assert_eq!(report["files"][1]["examples"], false);

    let bare = Scratch::new(&[
        ("Loom.toml", MANIFEST),
        ("src/plain.ves", "fn plain() -> u64 {\n    1\n}\n"),
    ]);
    let out = vesper(&bare.0, &["test", "--doc", "--json"]);
    assert!(!out.status.success());
    assert!(
        json(&out)["error"]
            .as_str()
            .unwrap()
            .contains("no Vesper documentation examples found in package")
    );
}

const DEVICES: &str = "linear effect Clock {\n    fn now() -> u64;\n    fn sleep(ns: u64) -> u64;\n}\nlinear effect Net {\n    fn send(port: u64, value: u64) -> u64;\n    fn recv(port: u64, timeout_ns: u64) -> u64;\n}\n";

fn run_doc(source: &str) -> doctest::DocTestReport {
    doctest::run(source, DocTestLimits::default()).unwrap()
}
fn sim_source(seed: &str) -> String {
    format!(
        "{DEVICES}/// ```vesper,sim(seed={seed},clock,net),entry=ping\n/// fn ping() -> u64 eff [Clock, Net] {{\n///     let a = Net::send(1, 5);\n///     let b = Net::send(1, 6);\n///     let x = Net::recv(1, 500);\n///     let y = Net::recv(1, 500);\n///     Clock::now()\n/// }}\n/// ```\nfn anchor() -> u64 {{\n    0\n}}\n"
    )
}

#[test]
fn simulated_doctests_are_seed_deterministic_and_seed_sensitive() {
    let first = run_doc(&sim_source("7"));
    let second = run_doc(&sim_source("7"));
    assert!(first.passed, "{first:?}");
    let a = serde_json::to_value(&first.examples[0]).unwrap();
    let b = serde_json::to_value(&second.examples[0]).unwrap();
    assert_eq!(a, b);
    let times: std::collections::BTreeSet<_> = (0..12)
        .map(|seed| {
            run_doc(&sim_source(&seed.to_string())).examples[0]
                .result
                .clone()
        })
        .collect();
    assert!(
        times.len() > 1,
        "seeded network latency must change virtual time"
    );
}

#[test]
fn simulated_doctests_reject_undeclared_devices_and_contract_mismatches() {
    // The entry uses Net but the fence declares only the clock.
    let undeclared = format!(
        "{DEVICES}/// ```vesper,sim(seed=1,clock),entry=leak\n/// fn leak() -> u64 eff [Net] {{\n///     Net::send(1, 2)\n/// }}\n/// ```\nfn anchor() -> u64 {{\n    0\n}}\n"
    );
    let report = run_doc(&undeclared);
    assert!(!report.passed);
    assert!(
        report.examples[0]
            .message
            .contains("not a simulated device declared"),
        "{}",
        report.examples[0].message
    );
    // A body fence gets exactly the declared row, so using Net fails checking.
    let body = format!(
        "{DEVICES}/// ```vesper,sim(seed=1,clock)\n/// let x = Net::send(1, 2);\n/// ```\nfn anchor() -> u64 {{\n    0\n}}\n"
    );
    let report = run_doc(&body);
    assert!(!report.passed);
    assert!(!report.examples[0].diagnostics.is_empty());
    // Operation signatures must match the Codex contract.
    let wrong = "linear effect Clock {\n    fn now() -> i64;\n}\n/// ```vesper,sim(seed=1,clock),entry=t\n/// fn t() -> i64 eff [Clock] {\n///     Clock::now()\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = run_doc(wrong);
    assert!(
        report.examples[0]
            .message
            .contains("must take 0 u64 argument(s)")
    );
    for bad in [
        "/// ```vesper,sim(clock)\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
        "/// ```vesper,sim(seed=1,gpu)\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
        "/// ```vesper,compile_fail(E0302),sim(seed=1,clock)\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
        "/// ```vesper,sim(seed=1,clock\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
    ] {
        assert!(
            doctest::run(bad, DocTestLimits::default()).is_err(),
            "{bad}"
        );
    }
}

fn codex_sections(html: &str) -> &str {
    let start = html
        .find("<section id=\"codex-badges\">")
        .expect("codex sections");
    &html[start..]
}

#[test]
fn rendered_doc_snapshot_has_badges_wire_rulers_and_typestate_graph() {
    let package = Scratch::new(&[
        ("src/ledger.ves", LEDGER),
        ("wire.json", WIRE),
        ("audit.json", AUDIT),
    ]);
    let out = vesper(
        &package.0,
        &[
            "doc",
            "src/ledger.ves",
            "--wire",
            "wire.json",
            "--proof-audit",
            "audit.json",
        ],
    );
    ok(&out);
    let html = String::from_utf8(out.stdout).unwrap();
    assert!(
        html.contains("Badges come only from 1 supplied kernel-replayed proof-audit report(s)")
    );
    assert!(!html.contains("No audited artifact-bound proof evidence supplied"));
    let actual = codex_sections(&html);
    if std::env::var_os("VESPER_UPDATE_SNAPSHOTS").is_some() {
        fs::write(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("crates/vesper-doc/tests/fixtures/ledger-codex.html"),
            actual,
        )
        .unwrap();
        return;
    }
    assert_eq!(
        actual, SNAPSHOT,
        "rerun with VESPER_UPDATE_SNAPSHOTS=1 after review"
    );

    // Without an audit every item is unverified; the anchor alone grants nothing.
    let plain = vesper(&package.0, &["doc", "src/ledger.ves", "--json"]);
    ok(&plain);
    let report = json(&plain);
    assert!(
        report["badges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["level"] == "unverified")
    );
    assert_eq!(report["visuals"]["layouts"][0]["source"], "native");

    // Invalid wire layouts and forged audits fail generation.
    let bad = Scratch::new(&[
        ("src/ledger.ves", LEDGER),
        (
            "wire.json",
            r#"{"format":"vesper-wire-layouts","version":1,"declarations":[{"name":"H","fields":[{"name":"a","type":"u8"},{"name":"b","type":"u32"}]}],"roots":["H"]}"#,
        ),
        (
            "audit.json",
            &AUDIT.replace("\"accepted\": true", "\"accepted\": false"),
        ),
    ]);
    for args in [
        ["doc", "src/ledger.ves", "--wire", "wire.json"],
        ["doc", "src/ledger.ves", "--proof-audit", "audit.json"],
    ] {
        let out = vesper(&bad.0, &args);
        assert!(!out.status.success(), "{args:?}");
    }
}

/// End to end with real `vesper proof-audit` output. Runs when VESPER_LEAN
/// names Lean 4.24.0 (CI sets it); Linux only, since kernel replay of the
/// prelude is slow on shared macOS and Windows runners.
#[cfg(target_os = "linux")]
#[test]
fn badges_from_a_real_kernel_replayed_proof_audit() {
    let Some(lean) = std::env::var_os("VESPER_LEAN") else {
        eprintln!("skipping real proof-audit badge test: VESPER_LEAN is unset");
        return;
    };
    let lean = lean.to_string_lossy().into_owned();
    // Resolve the elan proxy to the pinned toolchain binary, as proof_audit_cli does.
    let lean = Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()).join("bin/lean"))
        .filter(|p| p.is_file())
        .map_or(lean, |p| p.display().to_string());
    let version = Command::new(&lean).arg("--version").output().unwrap();
    if !String::from_utf8_lossy(&version.stdout).contains("version 4.24.0,") {
        eprintln!("skipping real proof-audit badge test: Lean 4.24.0 unavailable");
        return;
    }
    let model = "namespace Codex.Ledger\n\nstructure Tx where\n  id : Nat\n  total : Nat\n\ndef commit (tx : Tx) : Nat := tx.total\n\ntheorem commit_total (id total : Nat) : commit { id := id, total := total } = total := rfl\n\ntheorem decided (p : Prop) : p ∨ ¬p := Classical.em p\n\nend Codex.Ledger\n";
    let source = "/// Commits.\n///\n/// @proof(Codex.Ledger.commit_total)\nfn commit(x: u64) -> u64 {\n    x\n}\n/// Classical.\n///\n/// @proof(Codex.Ledger.decided)\nfn decide(x: u64) -> u64 {\n    x\n}\n/// Unaudited.\n///\n/// @proof(Codex.Ledger.missing)\nfn other(x: u64) -> u64 {\n    x\n}\n";
    let dir = Scratch::new(&[("CodexLedger.lean", model), ("api.ves", source)]);
    let compiled = Command::new(&lean)
        .current_dir(&dir.0)
        .env("LEAN_PATH", &dir.0)
        .args(["-o", "CodexLedger.olean", "CodexLedger.lean"])
        .output()
        .unwrap();
    ok(&compiled);
    let audit = vesper(
        &dir.0,
        &[
            "proof-audit",
            "CodexLedger",
            "--root",
            dir.0.to_str().unwrap(),
            "--theorem",
            "Codex.Ledger.commit_total",
            "--theorem",
            "Codex.Ledger.decided",
            "--allow-axiom",
            "propext",
            "--allow-axiom",
            "Classical.choice",
            "--allow-axiom",
            "Quot.sound",
            "--lean",
            &lean,
            "--timeout",
            "300",
            "--json",
        ],
    );
    ok(&audit);
    fs::write(dir.path("audit.json"), &audit.stdout).unwrap();
    let out = vesper(
        &dir.0,
        &["doc", "api.ves", "--json", "--proof-audit", "audit.json"],
    );
    ok(&out);
    let report = json(&out);
    let level = |name: &str| {
        report["badges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["item"] == name)
            .unwrap()["level"]
            .clone()
    };
    assert_eq!(level("commit"), "proved");
    assert_eq!(level("decide"), "audited");
    assert_eq!(level("other"), "unverified");
}
