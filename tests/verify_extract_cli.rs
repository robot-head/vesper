//! CLI integration tests for `vesper verify --extract` (§8.3–§8.5, §10.4).

use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const PURE_VES: &str = "fn add1(x: u64) -> u64 { x }\nfn main() -> u64 { add1(1) }\n";

static SERIAL: Mutex<()> = Mutex::new(());

fn binary() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper"));
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}

fn run(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    });
    let mut stderr = child.stderr.take().unwrap();
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(360) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("verify --extract command timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader.join().unwrap();
    let stderr = stderr_reader.join().unwrap();
    assert!(stdout.len() <= 4 * 1024 * 1024 && stderr.len() <= 4 * 1024 * 1024);
    Output {
        status,
        stdout,
        stderr,
    }
}

fn lean() -> Option<PathBuf> {
    let configured = std::env::var_os("VESPER_LEAN");
    let selected = configured.clone().unwrap_or_else(|| "lean".into());
    let prefix = Command::new(&selected)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output();
    let binary = prefix
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            PathBuf::from(String::from_utf8(o.stdout).unwrap().trim())
                .join("bin")
                .join(format!("lean{}", std::env::consts::EXE_SUFFIX))
        })
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(selected));
    let version = Command::new(&binary).arg("--version").output();
    let available = version.as_ref().is_ok_and(|v| {
        v.status.success() && String::from_utf8_lossy(&v.stdout).contains("version 4.35.0,")
    });
    if !available {
        assert!(
            configured.is_none(),
            "configured actual Lean 4.35.0 unavailable: {version:?}"
        );
        eprintln!("Lean 4.35.0 unavailable; skipping real-kernel --extract checks");
        return None;
    }
    Some(binary)
}

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    original: Vec<u8>,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-verify-extract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.ves");
        fs::write(&source, bytes).unwrap();
        Self {
            root,
            source,
            original: bytes.to_vec(),
        }
    }

    fn verify(&self, args: &[&str]) -> Output {
        let out = run(Command::new(binary())
            .arg("verify")
            .arg(&self.source)
            .args(args));
        assert_eq!(fs::read(&self.source).unwrap(), self.original);
        assert!(!fs::read_dir(&self.root).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".vesper-")
        }));
        out
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn reject_json(out: Output, expected_stage: Option<&str>) {
    assert_eq!(out.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "expected rejection JSON: {e}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(value["schema"], "vesper.verify-extract/1");
    assert_eq!(value["status"], "rejected");
    if let Some(stage) = expected_stage {
        assert_eq!(value["stage"], stage, "unexpected stage: {value:?}");
    }
    for forbidden in [
        "audit",
        "lean_model_blake3",
        "olean_certificate",
        "totality",
    ] {
        assert!(
            value.get(forbidden).is_none(),
            "rejected report leaked {forbidden}: {value:?}"
        );
    }
}

#[test]
fn extract_rejects_valid_unrelated_artifact_under_current_source_cache_key() {
    use vesper_loom_cas::{DerivationInputs, Domain, Store};
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let f = Fixture::new(PURE_VES.as_bytes());
    let parsed = vesper::parser::parse(PURE_VES);
    let checked = vesper::check::check(&parsed.module);
    let extracted = vesper::lean::extract_report(&checked.world)
        .unwrap()
        .module
        .render()
        .unwrap();
    let bundle = format!(
        "{extracted}\nnamespace VesperVerify\ntheorem model_verified : True := trivial\nend VesperVerify\n"
    );
    let version = run(Command::new(&lean).arg("--version"));
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap().trim().to_owned();
    let store_dir = f.root.join("poisoned-cache");
    let store = Store::open(&store_dir).unwrap();
    let source = store.put_blob(Domain::Source, bundle.as_bytes()).unwrap();
    let inputs = DerivationInputs::new(Domain::Artifact, "vesper-verify-extract", "1")
        .with_input("lean_bundle", source.id)
        .with_option(
            "source_blake3",
            blake3::hash(PURE_VES.as_bytes()).to_hex().to_string(),
        )
        .with_option(
            "lean_model_blake3",
            blake3::hash(extracted.as_bytes()).to_hex().to_string(),
        )
        .with_option("proofs_blake3", "")
        .with_option("lean_version", version)
        .with_option("theorems", "VesperVerify.model_verified")
        .with_option("allowed_axioms", "");
    // This is a real, valid theorem with the expected name. It has no relation
    // to the current Vesper source; CAS integrity and kernel replay both pass.
    fs::write(
        f.root.join("VesperExtract.lean"),
        "namespace VesperVerify\ntheorem model_verified : True := trivial\nend VesperVerify\n",
    )
    .unwrap();
    let compiled = run(Command::new(&lean).current_dir(&f.root).args([
        "-o",
        "VesperExtract.olean",
        "VesperExtract.lean",
    ]));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stdout)
    );
    let bytes = fs::read(f.root.join("VesperExtract.olean")).unwrap();
    let artifact = store.put_blob(Domain::Artifact, &bytes).unwrap();
    store.publish_derivation(&inputs, artifact.id).unwrap();
    let output = f.verify(&[
        "--extract",
        "--olean-store",
        store_dir.to_str().unwrap(),
        "--json",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
    ]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["message"]
            .as_str()
            .unwrap()
            .contains("does not match a fresh compilation"),
        "{report}"
    );
    reject_json(output, Some("store"));
}
