use serde_json::Value;
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
            "vesper-attest-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/vesper-loom-trust/tests/fixtures");
        for name in [
            "identity.json",
            "policy.json",
            "bundle.json",
            "artifact.bin",
        ] {
            fs::copy(source.join(name), path.join(name)).unwrap();
        }
        Self(path)
    }
    fn value(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.0.join(name)).unwrap()).unwrap()
    }
    fn write(&self, name: &str, value: &Value) {
        fs::write(self.0.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .args([
                "attest",
                "verify",
                "--expected",
                "identity.json",
                "--policy",
                "policy.json",
                "--bundle",
                "bundle.json",
                "--artifact",
                "artifact.bin",
                "--json",
            ])
            .args(extra)
            .output()
            .unwrap()
    }
    fn at150(&self) -> Output {
        self.run(&["--at", "150"])
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
fn verifies_real_signature_quorum_and_actual_artifact_content_without_mutation() {
    let f = Fixture::new();
    let names = [
        "identity.json",
        "policy.json",
        "bundle.json",
        "artifact.bin",
    ];
    let before = names.map(|name| fs::read(f.0.join(name)).unwrap());
    let output = f.at150();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = report(&output);
    assert_eq!(result["status"], "verified");
    assert_eq!(result["accepted"], true);
    assert_eq!(result["artifact"]["matches"], true);
    assert_eq!(result["verification"]["accepted"], true);
    assert_eq!(result["verification"]["evaluated_at"], 150);
    assert_eq!(
        result["verification"]["verified_operators"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(result["cache_eligible"], false);
    for (name, bytes) in names.into_iter().zip(before) {
        assert_eq!(fs::read(f.0.join(name)).unwrap(), bytes);
    }
}

#[test]
fn valid_signatures_cannot_approve_different_artifact_bytes() {
    let f = Fixture::new();
    fs::write(f.0.join("artifact.bin"), b"changed after signing").unwrap();
    let output = f.at150();
    assert!(!output.status.success());
    let result = report(&output);
    assert_eq!(result["accepted"], false);
    assert_eq!(result["verification"]["accepted"], true);
    assert_eq!(result["artifact"]["matches"], false);
    assert_ne!(
        result["artifact"]["expected_id"],
        result["artifact"]["observed_id"]
    );
}

#[test]
fn signature_target_expiry_and_operator_independence_fail_closed() {
    for change in [
        "signature",
        "target",
        "expired",
        "operator",
        "duplicate",
        "revoked",
    ] {
        let f = Fixture::new();
        match change {
            "signature" => {
                let mut bundle = f.value("bundle.json");
                let old = bundle["attestations"][0]["signature"].as_str().unwrap();
                bundle["attestations"][0]["signature"] = Value::String(format!(
                    "{}{}",
                    if old.starts_with('0') { "1" } else { "0" },
                    &old[1..]
                ));
                f.write("bundle.json", &bundle);
            }
            "target" => {
                let mut expected = f.value("identity.json");
                expected["target"] = Value::String("aarch64-unknown-linux-gnu".into());
                f.write("identity.json", &expected);
            }
            "operator" => {
                let mut policy = f.value("policy.json");
                policy["builders"][1]["operator_id"] = policy["builders"][0]["operator_id"].clone();
                f.write("policy.json", &policy);
            }
            "duplicate" => {
                let mut bundle = f.value("bundle.json");
                let duplicate = bundle["attestations"][0].clone();
                bundle["attestations"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
                f.write("bundle.json", &bundle);
            }
            "revoked" => {
                let mut policy = f.value("policy.json");
                policy["builders"][0]["revoked"] = Value::Bool(true);
                f.write("policy.json", &policy);
            }
            "expired" => {}
            _ => unreachable!(),
        }
        let output = if change == "expired" {
            f.run(&["--at", "900"])
        } else {
            f.at150()
        };
        assert!(!output.status.success(), "{change}");
        let result = report(&output);
        assert_eq!(result["accepted"], false, "{change}");
        assert_eq!(result["artifact"]["matches"], true);
        assert!(
            !result["verification"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn wrong_output_domain_or_tree_is_not_verified_as_a_blob() {
    for output_id in ["b3v1:source:blob:", "b3v1:artifact:tree:"] {
        let f = Fixture::new();
        let mut identity = f.value("identity.json");
        let digest = identity["output"]
            .as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap();
        identity["output"] = Value::String(format!("{output_id}{digest}"));
        f.write("identity.json", &identity);
        let output = f.at150();
        assert!(!output.status.success());
        assert_eq!(report(&output)["error"]["kind"], "unsupported_output");
    }
}

#[test]
fn malformed_inputs_missing_files_and_bad_options_never_claim_verification() {
    let f = Fixture::new();
    fs::write(f.0.join("identity.json"), "{not json}").unwrap();
    let malformed = f.at150();
    assert!(!malformed.status.success());
    assert!(malformed.stdout.is_empty());
    for args in [
        vec!["attest"],
        vec!["attest", "sign"],
        vec!["attest", "verify"],
        vec!["attest", "verify", "--expected"],
        vec!["attest", "verify", "--at", "not-time"],
        vec!["attest", "verify", "--artifact", "a", "--artifact", "b"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    let f = Fixture::new();
    fs::remove_file(f.0.join("artifact.bin")).unwrap();
    let missing = f.at150();
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "Uses a POSIX sparse file to exercise the 4 GiB metadata limit"
)]
fn oversized_artifacts_are_rejected_before_reading_the_file() {
    let f = Fixture::new();
    fs::OpenOptions::new()
        .write(true)
        .open(f.0.join("artifact.bin"))
        .unwrap()
        .set_len(4 * 1024 * 1024 * 1024 + 1)
        .unwrap();
    let output = f.at150();
    assert!(!output.status.success());
    assert_eq!(report(&output)["error"]["kind"], "artifact_too_large");
}
