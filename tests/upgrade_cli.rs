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
            "vesper-upgrade-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let f = Self(dir);
        f.manifest("@test/app", true);
        f.catalog("1.0.0", false, true);
        f
    }
    fn manifest(&self, name: &str, grant_net: bool) {
        fs::write(self.0.join("Loom.toml"),format!(
            "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Net\"]\n[dependencies]\n\"@test/lib\" = {{ version = \"^1\", effects = {} }}\n",
            if grant_net {"[\"Net\"]"} else {"[]"}
        )).unwrap();
    }
    fn catalog(&self, version: &str, net: bool, forbid: bool) {
        fs::write(self.0.join("catalog.json"),serde_json::to_vec(&json!({
            "schema_version":1,"packages":[{"package":{
                "name":"@test/lib","version":version,"edition":"2026",
                "capability_ceiling":if net {vec!["Net"]} else {vec![]},"forbid_unsafe_ffi":forbid
            }}]
        })).unwrap()).unwrap();
    }
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .args(["resolve", "--catalog", "catalog.json"])
            .args(extra)
            .output()
            .unwrap()
    }
    fn baseline(&self) -> Vec<u8> {
        let output = self.run(&["-o", "baseline.lock"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read(self.0.join("baseline.lock")).unwrap()
    }
    fn guarded(&self) -> Output {
        self.run(&[
            "--baseline",
            "baseline.lock",
            "-o",
            "baseline.lock",
            "--json",
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn pure_upgrades_and_capability_reductions_can_replace_the_snapshot() {
    let f = Fixture::new();
    f.baseline();
    f.catalog("1.1.0", false, true);
    let output = f.guarded();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = report(&output);
    assert_eq!(result["status"], "resolved");
    assert_eq!(result["upgrade"]["allowed"], true);
    let saved: Value =
        serde_json::from_slice(&fs::read(f.0.join("baseline.lock")).unwrap()).unwrap();
    assert_eq!(saved, result["solution"]);
    assert!(
        saved["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["manifest"]["package"]["version"] == "1.1.0")
    );

    f.catalog("1.2.0", true, true);
    f.baseline();
    f.catalog("1.3.0", false, true);
    let output = f.guarded();
    assert!(output.status.success());
    let result = report(&output);
    assert!(
        result["upgrade"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["removed"] == json!(["Net"]))
    );
}

#[test]
fn a_dependency_cannot_gain_net_even_if_the_root_already_has_net() {
    let f = Fixture::new();
    let original = f.baseline();
    f.catalog("1.1.0", true, true);
    let output = f.guarded();
    assert!(!output.status.success());
    let denied = report(&output);
    assert_eq!(denied["error"]["kind"], "capability_escalation");
    assert_eq!(denied["upgrade"]["allowed"], false);
    assert!(
        denied["upgrade"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["package"] == "@test/lib" && change["added"] == json!(["Net"]))
    );
    assert!(
        !denied["upgrade"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["package"] == "@test/app"
                && change["scope"] == "required_capabilities")
    );
    assert_eq!(fs::read(f.0.join("baseline.lock")).unwrap(), original);
    assert!(denied.get("solution").is_none());
    // This is an explicit opt-in guard; ordinary resolution is unchanged.
    let unguarded = f.run(&["--json"]);
    assert!(unguarded.status.success());
    assert!(report(&unguarded).get("upgrade").is_none());
}

#[test]
fn widening_an_unused_edge_budget_or_relaxing_ffi_policy_is_rejected() {
    let f = Fixture::new();
    f.manifest("@test/app", false);
    let before = f.baseline();
    f.manifest("@test/app", true);
    let output = f.guarded();
    assert!(!output.status.success());
    let denied = report(&output);
    assert!(
        denied["upgrade"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["scope"] == "dependency_ceiling"
                && change["added"] == json!(["Net"]))
    );
    assert_eq!(fs::read(f.0.join("baseline.lock")).unwrap(), before);

    f.manifest("@test/app", false);
    f.catalog("1.1.0", false, false);
    let output = f.guarded();
    assert!(!output.status.success());
    let denied = report(&output);
    assert!(
        denied["upgrade"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "unsafe_ffi_policy_relaxed")
    );
    assert_eq!(fs::read(f.0.join("baseline.lock")).unwrap(), before);
}

#[test]
fn invalid_baselines_and_unrelated_roots_never_replace_existing_output() {
    let f = Fixture::new();
    let original = f.baseline();
    f.manifest("@other/app", true);
    let output = f.guarded();
    assert!(!output.status.success());
    assert_eq!(report(&output)["error"]["kind"], "upgrade_mismatch");
    assert_eq!(fs::read(f.0.join("baseline.lock")).unwrap(), original);
    f.manifest("@test/app", true);
    let mut invalid: Value = serde_json::from_slice(&original).unwrap();
    invalid["schema_version"] = json!(999);
    let bytes = serde_json::to_vec(&invalid).unwrap();
    fs::write(f.0.join("baseline.lock"), &bytes).unwrap();
    let output = f.guarded();
    assert!(!output.status.success());
    assert_eq!(report(&output)["error"]["kind"], "invalid_baseline");
    assert_eq!(fs::read(f.0.join("baseline.lock")).unwrap(), bytes);
}

#[test]
fn baseline_options_are_explicit_and_do_not_mix_with_locked_mode() {
    let f = Fixture::new();
    let before = f.baseline();
    for flags in [
        vec!["--baseline"],
        vec!["--baseline", "baseline.lock", "--locked"],
        vec!["--baseline", "baseline.lock", "--baseline", "baseline.lock"],
        vec!["--baseline", "missing.lock", "--json"],
    ] {
        let output = f.run(&flags);
        assert!(!output.status.success(), "{flags:?}");
        assert!(output.stdout.is_empty());
    }
    assert_eq!(fs::read(f.0.join("baseline.lock")).unwrap(), before);
    assert!(!f.0.join("missing.lock").exists());
}
