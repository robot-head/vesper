use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-package-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, text: &str) {
        fs::write(self.0.join(name), text).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn export(&self, source: &str, output: &str) {
        let result = self.run(&[
            "emit",
            source,
            "--emit",
            "interface",
            "--package",
            "@example/lib",
            "-o",
            output,
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn source_interfaces_classify_body_changes_modes_and_new_effects() {
    let f = Fixture::new();
    f.write(
        "old.ves",
        "linear struct Receipt { id: u64 } fn receipt(id: u64) -> Receipt { Receipt { id } }",
    );
    f.write("body.ves","linear struct Receipt { id: u64 } fn receipt(number: u64) -> Receipt { let local = number; Receipt { id: local } }");
    f.write(
        "minor.ves",
        "copy struct Receipt { id: u64 } fn receipt(id: u64) -> Receipt { Receipt { id } }",
    );
    f.write("major.ves","copy struct Receipt { id: u64 } fn receipt(id: u64) -> Receipt eff [Fail[u32]] { Receipt { id } }");
    for (source, output) in [
        ("old.ves", "old.vhir"),
        ("body.ves", "body.vhir"),
        ("minor.ves", "minor.vhir"),
        ("major.ves", "major.vhir"),
    ] {
        f.export(source, output);
    }
    assert_eq!(
        fs::read(f.0.join("old.vhir")).unwrap(),
        fs::read(f.0.join("body.vhir")).unwrap()
    );
    for (candidate, classification, success) in [
        ("body.vhir", "patch", true),
        ("minor.vhir", "minor", true),
        ("major.vhir", "major", false),
    ] {
        let result = f.run(&["semver", "old.vhir", candidate, "--json"]);
        assert_eq!(
            result.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["classification"], classification);
    }
}
#[test]
fn unsupported_and_malformed_interfaces_never_certify() {
    let f = Fixture::new();
    f.write(
        "generic.ves",
        "fn invoke[T](sink f: fn(sink T) -> T, sink value: T) -> T { f(value) }",
    );
    f.export("generic.ves", "generic.vhir");
    let unknown = f.run(&["semver", "generic.vhir", "generic.vhir", "--json"]);
    assert!(!unknown.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&unknown.stdout).unwrap()["classification"],
        "unknown"
    );
    f.write("bad.vhir",r#"{"schema_version":1,"semantics_version":1,"exports":{},"exports":{},"external_types":{}}"#);
    assert!(
        !f.run(&["semver", "bad.vhir", "bad.vhir", "--json"])
            .status
            .success()
    );
    let invalid = f.run(&[
        "emit",
        "generic.ves",
        "--emit",
        "interface",
        "--package",
        "../escape",
    ]);
    assert!(!invalid.status.success());
}
fn manifest(name: &str, version: &str) -> Value {
    json!({"package":{"name":name,"version":version,"edition":"2026","capability_ceiling":[]}})
}
#[test]
fn offline_catalog_resolves_and_writes_a_validated_lockfile() {
    let f = Fixture::new();
    f.write("Loom.toml","[package]\nname='@example/app'\nversion='0.1.0'\nedition='2026'\ncapability_ceiling=[]\n[dependencies]\n'@example/lib'={version='^1',effects=[]}\n");
    let catalog = json!({"schema_version":1,"packages":[manifest("@example/lib","1.0.0"),manifest("@example/lib","1.1.0")],"system_packages":[]});
    f.write("catalog.json", &catalog.to_string());
    let result = f.run(&[
        "resolve",
        "--catalog",
        "catalog.json",
        "-o",
        "Loom.lock",
        "--json",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["status"], "resolved");
    let solution: vesper_loom_calculus::Solution =
        serde_json::from_slice(&fs::read(f.0.join("Loom.lock")).unwrap()).unwrap();
    assert!(
        vesper_loom_calculus::validate_solution(&solution)
            .graph
            .is_some()
    );
    assert_eq!(
        solution
            .packages
            .iter()
            .find(|p| p.manifest.package.name.as_str() == "@example/lib")
            .unwrap()
            .manifest
            .package
            .version
            .to_string(),
        "1.1.0"
    );
}
#[test]
fn resolution_errors_preserve_existing_lockfiles() {
    let f = Fixture::new();
    f.write("Loom.toml","[package]\nname='@example/app'\nversion='0.1.0'\nedition='2026'\n[dependencies]\n'@example/missing'={version='^1',effects=[]}\n");
    f.write(
        "catalog.json",
        r#"{"schema_version":1,"packages":[],"system_packages":[]}"#,
    );
    f.write("Loom.lock", "keep existing lockfile");
    let result = f.run(&[
        "resolve",
        "Loom.toml",
        "--catalog",
        "catalog.json",
        "-o",
        "Loom.lock",
        "--json",
    ]);
    assert!(!result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["status"],
        "error"
    );
    assert_eq!(
        fs::read_to_string(f.0.join("Loom.lock")).unwrap(),
        "keep existing lockfile"
    );
    assert!(fs::read_dir(&f.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".vesper-tmp-")
    }));
}
#[test]
fn package_command_flags_fail_closed() {
    let f = Fixture::new();
    assert!(
        !f.run(&["resolve", "--catalog", "missing", "--max-decisions", "0"])
            .status
            .success()
    );
    assert!(
        !f.run(&["resolve", "--catalog", "missing", "--features", ","])
            .status
            .success()
    );
    assert!(!f.run(&["semver", "--approve-unknown"]).status.success());
    assert!(!f.run(&["resolve", "--network"]).status.success());
}

#[test]
fn resolver_reports_budget_and_unenforced_policy_separately() {
    let f = Fixture::new();
    f.write("Loom.toml", "[package]\nname='@example/app'\nversion='0.1.0'\nedition='2026'\n[dependencies]\n'@example/lib'={version='^1',effects=[]}\n");
    let mut newest = manifest("@example/lib", "1.1.0");
    newest["dependencies"] = json!({"@example/missing":{"version":"^1","effects":[]}});
    f.write("catalog.json", &json!({"schema_version":1,"packages":[manifest("@example/lib","1.0.0"),newest],"system_packages":[]}).to_string());
    let result = f.run(&[
        "resolve",
        "--catalog",
        "catalog.json",
        "--max-decisions",
        "1",
        "--json",
    ]);
    assert!(!result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["error"]["kind"],
        "budget_exhausted"
    );
    // Without the budget restriction, the older supported candidate resolves.
    assert!(
        f.run(&["resolve", "--catalog", "catalog.json"])
            .status
            .success()
    );
    f.write("Loom.toml", "[package]\nname='@example/app'\nversion='0.1.0'\nedition='2026'\n[resolver.policy]\nblock_unvetted_namespaces=true\n");
    let result = f.run(&["resolve", "--catalog", "catalog.json", "--json"]);
    assert!(!result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["error"]["kind"],
        "unsupported_policy"
    );
}

#[test]
fn lockfile_write_failure_cleans_temporary_output() {
    let f = Fixture::new();
    f.write(
        "Loom.toml",
        "[package]\nname='@example/app'\nversion='0.1.0'\nedition='2026'\n",
    );
    f.write(
        "catalog.json",
        r#"{"schema_version":1,"packages":[],"system_packages":[]}"#,
    );
    fs::create_dir(f.0.join("lock-dir")).unwrap();
    fs::write(f.0.join("lock-dir/keep"), b"preserve").unwrap();
    let result = f.run(&[
        "resolve",
        "--catalog",
        "catalog.json",
        "-o",
        "lock-dir",
        "--json",
    ]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert_eq!(fs::read(f.0.join("lock-dir/keep")).unwrap(), b"preserve");
    assert!(fs::read_dir(&f.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".vesper-tmp-")
    }));
}
