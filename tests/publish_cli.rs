use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-publish-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let trust_fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/vesper-loom-trust/tests/fixtures");
        for name in [
            "identity.json",
            "policy.json",
            "bundle.json",
            "artifact.bin",
        ] {
            fs::copy(trust_fixtures.join(name), path.join(name)).unwrap();
        }
        Self(path)
    }

    fn write_str(&self, name: &str, contents: &str) {
        fs::write(self.0.join(name), contents).unwrap();
    }

    fn write_json(&self, name: &str, value: &Value) {
        fs::write(self.0.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .arg("publish")
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest_toml(name: &str, version: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "{version}"
edition = "2026"
license = "Apache-2.0"
capability_ceiling = []
forbid_unsafe_ffi = true
"#
    )
}

fn parse_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON stdout ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn publish_initial_with_sbom_audit_attestation_and_cas_succeeds() {
    let f = Fixture::new();
    f.write_str("Loom.toml", &manifest_toml("@test/package", "1.2.3+source"));
    f.write_str("main.ves", "fn answer() -> i64 { 42 }\n");
    f.write_json(
        "Loom.lock",
        &json!({
            "schema_version": 1,
            "roots": [{"name": "@test/package", "version": "^1"}],
            "packages": [{
                "manifest": {
                    "package": {
                        "name": "@test/package",
                        "version": "1.2.3+source",
                        "edition": "2026",
                        "license": "Apache-2.0",
                        "capability_ceiling": []
                    },
                    "dependencies": {}
                }
            }],
            "system_packages": []
        }),
    );
    f.write_json(
        "advisories.json",
        &json!({
            "schema_version": 1,
            "catalog_id": "test-osv-catalog",
            "advisories": [],
            "native_licenses": []
        }),
    );
    f.write_json(
        "audit-policy.json",
        &json!({
            "schema_version": 1,
            "minimum_severity": "low",
            "allowed_licenses": ["Apache-2.0"]
        }),
    );

    let out = f.run(&[
        "main.ves",
        "--initial",
        "--manifest",
        "Loom.toml",
        "--solution",
        "Loom.lock",
        "--advisories",
        "advisories.json",
        "--audit-policy",
        "audit-policy.json",
        "--expected",
        "identity.json",
        "--policy",
        "policy.json",
        "--bundle",
        "bundle.json",
        "--artifact",
        "artifact.bin",
        "--at",
        "150",
        "--store",
        "cas-store",
        "-o",
        "publish-receipt.json",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "publish failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report = parse_json(&out);
    assert_eq!(report["status"], "published_locally");
    assert_eq!(report["package"], "@test/package");
    assert_eq!(report["version"], "1.2.3+source");
    assert!(
        report["interface_mif"]
            .as_str()
            .unwrap()
            .starts_with("mif:blake3:")
    );
    assert_eq!(report["gates"]["semver"]["mode"], "initial");
    assert_eq!(report["gates"]["sbom"]["bomFormat"], "CycloneDX");
    assert_eq!(report["gates"]["audit"]["outcome"], "clean");
    assert_eq!(report["gates"]["attestation"]["accepted"], true);
    assert_eq!(report["cas"]["source_newly_stored"], true);
    assert_eq!(report["cas"]["interface_newly_stored"], true);
    assert!(f.0.join("publish-receipt.json").is_file());
    assert!(f.0.join("cas-store/LAYOUT").is_file());
}

#[test]
fn publish_semver_rejects_breaking_change_without_major_bump_and_keeps_cas_clean() {
    let f = Fixture::new();
    f.write_str("Loom.toml", &manifest_toml("@test/package", "1.2.3"));
    f.write_str("old.ves", "fn transform(x: i64) -> i64 {\n    x\n}\n");
    // Emit baseline .vhir interface.
    let emit_out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(&f.0)
        .args([
            "emit",
            "old.ves",
            "--emit",
            "interface",
            "--package",
            "@test/package",
            "-o",
            "old.vhir",
        ])
        .output()
        .unwrap();
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );

    // Breaking candidate: different return type, only minor bump 1.2.3 -> 1.3.0.
    f.write_str("Loom.toml", &manifest_toml("@test/package", "1.3.0"));
    f.write_str("new.ves", "fn transform(x: i64) -> bool {\n    true\n}\n");

    let rejected = f.run(&[
        "new.ves",
        "--baseline",
        "old.vhir",
        "--old-version",
        "1.2.3",
        "--store",
        "cas-store",
        "-o",
        "publish-receipt.json",
        "--json",
    ]);
    assert!(!rejected.status.success());
    let report = parse_json(&rejected);
    assert_eq!(report["status"], "rejected");
    assert_eq!(report["error"]["kind"], "semver_rejected");
    assert!(
        !report["details"]["caller_witnesses"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Must not initialize CAS or write receipt when gate fails.
    assert!(!f.0.join("cas-store").exists());
    assert!(!f.0.join("publish-receipt.json").exists());

    // Major bump 1.2.3 -> 2.0.0 is accepted.
    f.write_str("Loom.toml", &manifest_toml("@test/package", "2.0.0"));
    let accepted = f.run(&[
        "new.ves",
        "--baseline",
        "old.vhir",
        "--old-version",
        "1.2.3",
        "--store",
        "cas-store",
        "-o",
        "publish-receipt.json",
        "--json",
    ]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let ok = parse_json(&accepted);
    assert_eq!(ok["status"], "published_locally");
    assert_eq!(ok["gates"]["semver"]["accepted"], true);
}

#[test]
fn publish_rejects_sorry_axiom_allow_flag() {
    let f = Fixture::new();
    f.write_str("Loom.toml", &manifest_toml("@test/package", "0.1.0"));
    f.write_str("lib.ves", "fn id(x: i64) -> i64 {\n    x\n}\n");
    let out = f.run(&["lib.ves", "--initial", "--allow-axiom", "sorryAx"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sorryAx"), "stderr was: {stderr}");
}

#[test]
fn publish_enforces_capability_ceiling_and_canonical_ffi_names() {
    let f = Fixture::new();
    f.write_str("Loom.toml", &manifest_toml("@test/package", "0.1.0"));
    f.write_str(
        "fail.ves",
        "fn oops(x: i64) -> i64 eff [Fail[ArithOverflow]] {\n    x\n}\n",
    );
    let out = f.run(&["fail.ves", "--initial", "--json"]);
    assert!(!out.status.success());
    let report = parse_json(&out);
    assert_eq!(report["error"]["kind"], "capability_violation");

    for src in [
        "effect Ffi[L] { fn call() -> i64; }\nstruct LibA {}\nfn foreign() -> i64 eff [Ffi[LibA]] { 1 }\n",
        "effect UnsafeFfi { fn call() -> i64; }\nfn unsafe_foreign() -> i64 eff [UnsafeFfi] { 1 }\n",
    ] {
        f.write_str("ffi.ves", src);
        let out = f.run(&["ffi.ves", "--initial", "--json"]);
        assert!(!out.status.success());
        let report = parse_json(&out);
        assert_eq!(report["error"]["kind"], "capability_violation");
    }

    // Allowing the declared capability lets non-FFI effects publish cleanly.
    f.write_str(
        "Loom.toml",
        "[package]\nname = \"@test/package\"\nversion = \"0.1.0\"\nedition = \"2026\"\nlicense = \"Apache-2.0\"\ncapability_ceiling = [\"Fail[ArithOverflow]\"]\nforbid_unsafe_ffi = true\n",
    );
    let ok = f.run(&["fail.ves", "--initial", "--json"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

#[test]
fn publish_rejects_stale_solution_when_root_manifest_differs() {
    let f = Fixture::new();
    f.write_str(
        "Loom.toml",
        r#"[package]
name = "@test/package"
version = "1.2.3+source"
edition = "2026"
license = "Apache-2.0"
capability_ceiling = ["Net"]
forbid_unsafe_ffi = true

[dependencies."@other/dep"]
version = "^1.0"
effects = ["Net"]
"#,
    );
    f.write_str("main.ves", "fn answer() -> i64 { 42 }\n");
    // Stale lockfile matches name/version but omits @other/dep and Net ceiling.
    f.write_json(
        "Loom.lock",
        &json!({
            "schema_version": 1,
            "roots": [{"name": "@test/package", "version": "^1"}],
            "packages": [{
                "manifest": {
                    "package": {
                        "name": "@test/package",
                        "version": "1.2.3+source",
                        "edition": "2026",
                        "license": "Apache-2.0",
                        "capability_ceiling": []
                    },
                    "dependencies": {}
                }
            }],
            "system_packages": []
        }),
    );

    let out = f.run(&[
        "main.ves",
        "--initial",
        "--manifest",
        "Loom.toml",
        "--solution",
        "Loom.lock",
        "--store",
        "cas-store",
        "--json",
    ]);
    assert!(!out.status.success());
    let report = parse_json(&out);
    assert_eq!(report["error"]["kind"], "solution_root_mismatch");
    assert!(!f.0.join("cas-store").exists());
}

#[test]
fn publish_rejects_attestation_bundle_when_source_differs_from_source_tree() {
    let f = Fixture::new();
    f.write_str("Loom.toml", &manifest_toml("@test/package", "1.2.3+source"));
    // Different source bytes than the attested `fn answer() -> i64 { 42 }\n`.
    f.write_str("main.ves", "fn answer() -> i64 { 99 }\n");

    let out = f.run(&[
        "main.ves",
        "--initial",
        "--manifest",
        "Loom.toml",
        "--expected",
        "identity.json",
        "--policy",
        "policy.json",
        "--bundle",
        "bundle.json",
        "--artifact",
        "artifact.bin",
        "--at",
        "150",
        "--store",
        "cas-store",
        "--json",
    ]);
    assert!(!out.status.success());
    let report = parse_json(&out);
    assert_eq!(report["error"]["kind"], "attestation_source_mismatch");
    assert!(!f.0.join("cas-store").exists());
}
