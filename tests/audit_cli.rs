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
        let path = std::env::temp_dir().join(format!(
            "vesper-audit-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path);
        fixture.write(
            "Loom.lock",
            &json!({
                "schema_version":1,
                "roots":[{"name":"@test/app","version":"=1.0.0","features":[]}],
                "packages":[
                    selection("app", "MIT", json!({"@test/lib":{"version":"^1","effects":[]}})),
                    selection("lib", "MIT", json!({})),
                    selection("unused", "GPL-3.0-only", json!({}))
                ],
                "system_packages":[]
            }),
        );
        fixture.write("catalog.json", &json!({"schema_version":1,"catalog_id":"local-test","advisories":[],"native_licenses":[]}));
        fixture.write("policy.json", &json!({"schema_version":1,"minimum_severity":"moderate","allowed_licenses":["MIT","Apache-2.0"]}));
        fixture
    }
    fn write(&self, name: &str, value: &Value) {
        fs::write(self.0.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.0.join(name)).unwrap()).unwrap()
    }
    fn run(&self) -> Output {
        self.args(&[
            "audit",
            "--catalog",
            "catalog.json",
            "--policy",
            "policy.json",
            "--json",
        ])
    }
    fn args(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
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
fn selection(name: &str, license: &str, dependencies: Value) -> Value {
    json!({"manifest":{"package":{"name":format!("@test/{name}"),"version":"1.0.0","edition":"2026","license":license,"capability_ceiling":[]},"dependencies":dependencies}})
}
fn advisory(id: &str, ecosystem: &str, name: &str) -> Value {
    json!({"id":id,"package":{"ecosystem":ecosystem,"name":name},"affected":[{"introduced":"1.0.0","fixed":"1.0.1","include_prerelease":false}],"severity":"high","summary":"local test advisory"})
}
fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn clean_audit_is_read_only_and_excludes_unreachable_selections() {
    let f = Fixture::new();
    let before =
        ["Loom.lock", "catalog.json", "policy.json"].map(|name| fs::read(f.0.join(name)).unwrap());
    let output = f.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let r = report(&output);
    assert_eq!(r["outcome"], "clean");
    assert_eq!(r["reachable_vesper_packages"], 2);
    assert_eq!(r["findings"], json!([]));
    for (name, bytes) in ["Loom.lock", "catalog.json", "policy.json"]
        .into_iter()
        .zip(before)
    {
        assert_eq!(fs::read(f.0.join(name)).unwrap(), bytes);
    }
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 3);
}

#[test]
fn transitive_vulnerability_fails_and_reports_a_root_dependency_path() {
    let f = Fixture::new();
    let mut catalog = f.read("catalog.json");
    catalog["advisories"] = json!([
        advisory("TEST-1", "vesper", "@test/lib"),
        advisory("TEST-unused", "vesper", "@test/unused")
    ]);
    f.write("catalog.json", &catalog);
    let output = f.run();
    assert!(!output.status.success());
    let r = report(&output);
    assert_eq!(r["outcome"], "findings");
    assert_eq!(r["findings"].as_array().unwrap().len(), 1);
    let finding = &r["findings"][0];
    assert_eq!(finding["issue"]["advisory_id"], "TEST-1");
    assert_eq!(finding["paths"][0][0]["identity"]["name"], "@test/app");
    assert_eq!(finding["paths"][0][1]["identity"]["name"], "@test/lib");
}

#[test]
fn unknown_license_and_known_denial_are_distinct_non_successes() {
    for (license, outcome) in [
        ("custom license text", "unknown"),
        ("GPL-3.0-only", "findings"),
    ] {
        let f = Fixture::new();
        let mut solution = f.read("Loom.lock");
        solution["packages"][1]["manifest"]["package"]["license"] = license.into();
        f.write("Loom.lock", &solution);
        let output = f.run();
        assert!(!output.status.success(), "{license}");
        assert_eq!(report(&output)["outcome"], outcome);
    }
}

#[test]
fn native_identity_uses_explicit_license_metadata_and_distinct_paths() {
    let f = Fixture::new();
    let mut solution = f.read("Loom.lock");
    solution["packages"][0]["manifest"]["dependencies"]["@test/lib"]["features"] =
        json!(["native"]);
    solution["packages"][1]["manifest"]["features"] = json!({"native":{"sys_deps":["zlib ^1"]}});
    solution["packages"][1]["features"] = json!(["native"]);
    solution["system_packages"] = json!([{"name":"zlib","version":"1.0.0"}]);
    f.write("Loom.lock", &solution);
    let mut catalog = f.read("catalog.json");
    catalog["advisories"] = json!([advisory("NATIVE-1", "native", "zlib")]);
    catalog["native_licenses"] = json!([{"name":"zlib","version":"1.0.0","expression":"MIT"}]);
    f.write("catalog.json", &catalog);
    let output = f.run();
    assert!(!output.status.success());
    let r = report(&output);
    assert_eq!(r["outcome"], "findings", "{r}");
    assert_eq!(r["reachable_native_packages"], 1);
    assert_eq!(
        r["findings"][0]["package"]["identity"]["ecosystem"],
        "native"
    );
    assert_eq!(r["findings"][0]["paths"][0].as_array().unwrap().len(), 3);
}

#[test]
fn invalid_catalog_snapshot_duplicate_keys_and_size_never_report_clean() {
    for kind in ["catalog", "snapshot", "duplicate", "oversized"] {
        let f = Fixture::new();
        match kind {
            "catalog" => {
                let mut c = f.read("catalog.json");
                c["schema_version"] = 2.into();
                f.write("catalog.json", &c);
            }
            "snapshot" => {
                let mut s = f.read("Loom.lock");
                s["roots"] = json!([]);
                f.write("Loom.lock", &s);
            }
            "duplicate" => fs::write(
                f.0.join("catalog.json"),
                br#"{"schema_version":1,"schema_version":1}"#,
            )
            .unwrap(),
            "oversized" => fs::File::create(f.0.join("catalog.json"))
                .unwrap()
                .set_len(1024 * 1024 + 1)
                .unwrap(),
            _ => unreachable!(),
        }
        let output = f.run();
        assert!(!output.status.success(), "{kind}");
        assert_eq!(report(&output)["status"], "error", "{kind}");
    }
    let f = Fixture::new();
    for args in [
        vec!["audit"],
        vec!["audit", "--catalog"],
        vec!["audit", "--policy", "x", "--policy", "y"],
        vec!["audit", "--unknown"],
    ] {
        assert!(!f.args(&args).status.success());
    }
    assert!(f.args(&["audit", "--help"]).status.success());
}
