use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_loom_cas::{ObjectId, Store};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-admission-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        for name in [
            "identity.json",
            "policy.json",
            "bundle.json",
            "artifact.bin",
        ] {
            fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("crates/vesper-loom-trust/tests/fixtures")
                    .join(name),
                path.join(name),
            )
            .unwrap();
        }
        Self(path)
    }
    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.0.join(name)).unwrap()).unwrap()
    }
    fn write(&self, name: &str, value: Value) {
        fs::write(
            self.0.join(name),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
    fn import(&self, at: &str) -> Output {
        self.run(&[
            "attest",
            "import",
            "--expected",
            "identity.json",
            "--policy",
            "policy.json",
            "--bundle",
            "bundle.json",
            "--artifact",
            "artifact.bin",
            "--store",
            "store",
            "--at",
            at,
            "--json",
        ])
    }
    fn run(&self, args: &[&str]) -> Output {
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
fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn imports_exact_signed_bytes_and_leaves_build_cache_unapproved() {
    let f = Fixture::new();
    let output = f.import("150");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = report(&output);
    assert_eq!(result["verification"]["accepted"], true);
    let identity = f.read("identity.json");
    let id: ObjectId = identity["output"].as_str().unwrap().parse().unwrap();
    let store = Store::open(f.0.join("store")).unwrap();
    assert_eq!(
        store.read_blob(&id).unwrap(),
        fs::read(f.0.join("artifact.bin")).unwrap()
    );
    assert_eq!(
        fs::read_dir(f.0.join("store/derivations/v1"))
            .unwrap()
            .count(),
        0
    );
    let repeated = f.import("150");
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    assert_eq!(report(&repeated)["newly_stored"], false);
}

#[test]
fn rejected_bytes_current_policy_clock_and_output_type_never_create_a_store() {
    for scenario in [
        "bytes",
        "revoked",
        "expiry",
        "identity",
        "tree",
        "malformed",
    ] {
        let f = Fixture::new();
        match scenario {
            "bytes" => fs::write(f.0.join("artifact.bin"), b"different unsigned output").unwrap(),
            "revoked" => {
                let mut p = f.read("policy.json");
                p["builders"][0]["revoked"] = true.into();
                f.write("policy.json", p);
            }
            "identity" => {
                let mut p = f.read("identity.json");
                p["target"] = "aarch64-unknown-linux-gnu".into();
                f.write("identity.json", p);
            }
            "tree" => {
                let mut p = f.read("identity.json");
                p["output"] = p["output"]
                    .as_str()
                    .unwrap()
                    .replace(":blob:", ":tree:")
                    .into();
                f.write("identity.json", p);
            }
            "malformed" => fs::write(f.0.join("bundle.json"), b"{invalid}").unwrap(),
            "expiry" => {}
            _ => unreachable!(),
        }
        let output = f.import(if scenario == "expiry" { "900" } else { "150" });
        assert!(!output.status.success(), "{scenario}");
        assert_eq!(
            report(&output)["error"]["kind"],
            "admission_rejected",
            "{scenario}"
        );
        assert!(!f.0.join("store").exists(), "{scenario}");
    }
}

fn inventory(path: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut pending = vec![path.to_owned()];
    let mut files = vec![];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                files.push((entry.path(), fs::read(entry.path()).unwrap()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn prior_admission_cannot_bypass_current_revocation() {
    let f = Fixture::new();
    assert!(f.import("150").status.success());
    let before = inventory(&f.0.join("store"));
    let mut policy = f.read("policy.json");
    policy["builders"][0]["revoked"] = true.into();
    f.write("policy.json", policy);
    let output = f.import("150");
    assert!(!output.status.success());
    assert_eq!(report(&output)["error"]["kind"], "admission_rejected");
    assert_eq!(inventory(&f.0.join("store")), before);
}

#[test]
fn bad_options_and_oversized_imports_fail_without_store_creation() {
    let f = Fixture::new();
    for args in [
        vec!["attest", "import"],
        vec!["attest", "import", "--store"],
        vec!["attest", "verify", "--store", "store"],
    ] {
        assert!(!f.run(&args).status.success());
    }
    fs::File::create(f.0.join("artifact.bin"))
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let output = f.import("150");
    assert!(!output.status.success());
    assert_eq!(report(&output)["error"]["kind"], "artifact_too_large");
    assert!(!f.0.join("store").exists());
}

#[cfg(unix)]
#[test]
fn special_and_symlink_artifacts_fail_promptly_without_creating_store() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::symlink},
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    for kind in ["fifo", "symlink", "directory"] {
        let f = Fixture::new();
        let path = f.0.join("artifact.bin");
        fs::remove_file(&path).unwrap();
        match kind {
            "fifo" => {
                let name = CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: the path is a valid NUL-terminated local fixture path.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "symlink" => symlink("identity.json", &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            _ => unreachable!(),
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&f.0)
            .args([
                "attest",
                "import",
                "--expected",
                "identity.json",
                "--policy",
                "policy.json",
                "--bundle",
                "bundle.json",
                "--artifact",
                "artifact.bin",
                "--store",
                "store",
                "--at",
                "150",
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{kind} artifact blocked the CLI");
        }
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success(), "{kind}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("regular file"),
            "{kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!f.0.join("store").exists());
    }
}
