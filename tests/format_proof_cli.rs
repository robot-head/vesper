//! Explicit real-Lean formatter document-model verification, never source semantics.
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
const COMMENTED: &str = include_str!("fixtures/format_proof/commented.ves");
const HOSTILE: &str = include_str!("fixtures/format_proof/hostile.ves");
const MACRO: &str = include_str!("fixtures/format_proof/macro.ves");
static SERIAL: Mutex<()> = Mutex::new(());
fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_FORMAT_PROOF_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_vesper")));
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}
fn output(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = thread::spawn(move || {
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
            panic!("formatter proof command timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = reader.join().unwrap();
    let stderr = errors.join().unwrap();
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
        v.status.success() && String::from_utf8_lossy(&v.stdout).contains("version 4.24.0,")
    });
    if !available {
        assert!(
            configured.is_none(),
            "configured actual Lean4.24.0 unavailable: {version:?}"
        );
        eprintln!("Lean4.24.0 unavailable; configure VESPER_LEAN for actual kernel acceptance");
        return None;
    }
    Some(binary)
}
struct Fixture {
    root: PathBuf,
    path: PathBuf,
    original: Vec<u8>,
}
impl Fixture {
    fn new(source: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-format-proof-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("source.ves");
        fs::write(&path, source).unwrap();
        Self {
            root,
            path,
            original: source.to_vec(),
        }
    }
    fn verify(&self, extra: &[&str]) -> Output {
        let result = output(
            Command::new(binary())
                .arg("verify")
                .arg(&self.path)
                .args(extra),
        );
        self.unchanged();
        result
    }
    fn unchanged(&self) {
        assert_eq!(fs::read(&self.path).unwrap(), self.original);
        assert!(!fs::read_dir(&self.root).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".vesper-")
        }));
    }
    fn checked(&self, lean: &std::path::Path) -> Value {
        let result = self.verify(&[
            "--format",
            "--json",
            "--lean",
            lean.to_str().unwrap(),
            "--timeout",
            "300",
        ]);
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["schema"], "vesper.format-proof/1");
        assert_eq!(value["status"], "kernel_checked");
        assert_eq!(value["scope"], "formatter_document_model");
        assert_eq!(
            value["source_blake3"],
            blake3::hash(&self.original).to_hex().as_str()
        );
        assert_eq!(value["input_bytes"], self.original.len());
        assert!(value["document_nodes"].as_u64().unwrap() > 0);
        assert!(value["lean_version"].as_str().unwrap().contains("4.24.0"));
        let audit = &value["audit"];
        assert_eq!(audit["accepted"], true);
        assert_eq!(audit["kernel_replayed"], true);
        assert_eq!(audit["forbidden_axioms"], serde_json::json!([]));
        let theorems = audit["theorems"].as_array().unwrap();
        assert!(
            theorems
                .iter()
                .any(|t| t["name"] == "Vesper.StylusGenerated.comments_preserved")
        );
        assert!(theorems.iter().any(
            |t| t["name"] == "Vesper.StylusSafety.projection_narrowing_preserves_disjointness"
        ));
        for t in theorems {
            assert_eq!(t["axioms"], serde_json::json!([]));
        }
        for denied in [
            "world",
            "checked_source",
            "semantic_checked",
            "rust_renderer_verified",
            "program_verified",
        ] {
            assert!(value.get(denied).is_none());
        }
        value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn reject(result: Output) {
    assert_eq!(result.status.code(), Some(1));
    {
        let value: Value = serde_json::from_slice(&result.stdout)
            .expect("--json rejection must be one bounded JSON report");
        assert_eq!(value["status"], "rejected");
        for forbidden in [
            "audit",
            "source_blake3",
            "output_blake3",
            "lean_version",
            "document_nodes",
        ] {
            assert!(
                value.get(forbidden).is_none(),
                "partial accepted field {forbidden}: {value:?}"
            );
        }
        assert!(
            !String::from_utf8_lossy(&result.stdout).contains("namespace Vesper.StylusGenerated")
        );
    }
}
#[test]
fn actual_kernel_audit_binds_current_source_output_and_ordered_comments() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let f = Fixture::new(COMMENTED.as_bytes());
    let first = f.checked(&lean);
    assert_eq!(first["comment_tokens"], 3);
    let candidate = f.root.join("formatted.ves");
    fs::write(&candidate, COMMENTED).unwrap();
    let formatted = output(
        Command::new(binary())
            .arg("fmt")
            .arg(&candidate)
            .arg("--json"),
    );
    assert!(formatted.status.success());
    let bytes = fs::read(&candidate).unwrap();
    assert_eq!(
        first["output_blake3"],
        blake3::hash(&bytes).to_hex().as_str()
    );
    assert_eq!(first["output_bytes"], bytes.len());
    let edited = COMMENTED.replace("final 🦀", "current 🦀");
    let next = Fixture::new(edited.as_bytes()).checked(&lean);
    assert_ne!(first["source_blake3"], next["source_blake3"]);
    assert_ne!(first["output_blake3"], next["output_blake3"]);
    assert_eq!(first["comment_tokens"], next["comment_tokens"]);
}
#[test]
fn emitted_ast_witness_compiles_independently_with_hostile_payload_as_literals() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let f = Fixture::new(HOSTILE.as_bytes());
    let emitted = f.verify(&[
        "--format",
        "--emit-lean",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
    ]);
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let text = String::from_utf8(emitted.stdout).unwrap();
    assert!(text.contains("comments_preserved"));
    assert!(text.contains("Vesper.StylusGenerated"));
    assert!(!text.trim_start().starts_with('{'));
    let model = std::env::var_os("VESPER_FORMAT_PROOF_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lean/StylusAndLintSafety.lean")
        });
    fs::copy(model, f.root.join("StylusAndLintSafety.lean")).unwrap();
    fs::write(f.root.join("Witness.lean"), text).unwrap();
    let compile = |file: &str, out: &str| {
        let result = output(
            Command::new(&lean)
                .current_dir(&f.root)
                .env("LEAN_PATH", &f.root)
                .env_remove("LEAN_SRC_PATH")
                .env_remove("LEAN_PLUGIN_PATH")
                .args(["--trust=0", "-o", out, file]),
        );
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    };
    compile("StylusAndLintSafety.lean", "StylusAndLintSafety.olean");
    compile("Witness.lean", "Witness.olean");
    fs::write(f.root.join("Inspect.lean"),"import Witness\n#print axioms Vesper.StylusGenerated.comments_preserved\n#print axioms Vesper.StylusSafety.projection_narrowing_preserves_disjointness\n").unwrap();
    let inspect = output(
        Command::new(&lean)
            .current_dir(&f.root)
            .env("LEAN_PATH", &f.root)
            .env_remove("LEAN_SRC_PATH")
            .env_remove("LEAN_PLUGIN_PATH")
            .args(["--trust=0", "Inspect.lean"]),
    );
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let out = String::from_utf8(inspect.stdout).unwrap();
    assert_eq!(out.matches("does not depend on any axioms").count(), 2);
    assert!(!out.contains("sorryAx"));
    fs::write(
        f.root.join("NoInjection.lean"),
        "import Witness\n#check injected\n",
    )
    .unwrap();
    let probe = output(
        Command::new(&lean)
            .current_dir(&f.root)
            .env("LEAN_PATH", &f.root)
            .env_remove("LEAN_SRC_PATH")
            .env_remove("LEAN_PLUGIN_PATH")
            .args(["--trust=0", "NoInjection.lean"]),
    );
    assert!(
        !probe.status.success(),
        "hostile payload introduced an actual Lean declaration"
    );
    f.unchanged();
}
#[test]
fn actual_fixed_expr_macro_document_is_proved_without_expansion_or_source_checking() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let f = Fixture::new(MACRO.as_bytes());
    let value = f.checked(&lean);
    assert_eq!(value["comment_tokens"], 2);
    let ordinary = output(
        Command::new(binary())
            .arg("check")
            .arg(&f.path)
            .arg("--json"),
    );
    assert_eq!(ordinary.status.code(), Some(1));
    f.unchanged();
}
#[test]
fn invalid_unsupported_oversized_and_failed_tool_inputs_publish_no_certificate() {
    for source in [
        b"fn main(){let x=;".as_slice(),
        b"fn main(){/* unclosed",
        b"fn main(){unknown!{raw}}",
        b"fn main(){\xff}",
    ] {
        let f = Fixture::new(source);
        reject(f.verify(&["--format", "--json"]));
        let out = f.verify(&["--format", "--emit-lean"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
    }
    let large = vec![b' '; 128 * 1024 + 1];
    reject(Fixture::new(&large).verify(&["--format", "--json"]));
    let f = Fixture::new(COMMENTED.as_bytes());
    let missing = f.root.join("nonexistent-real-lean");
    reject(f.verify(&["--format", "--json", "--lean", missing.to_str().unwrap()]));
    let out = f.verify(&[
        "--format",
        "--emit-lean",
        "--lean",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    for flags in [
        &["--json"][..],
        &["--format", "--json", "--emit-lean"][..],
        &["--format", "--json", "--timeout", "0"][..],
        &["--format", "--json", "--timeout", "301"][..],
        &["--format", "--json", "--unknown"][..],
    ] {
        reject(f.verify(flags));
    }
    assert!(
        !output(
            Command::new(binary())
                .arg("verify")
                .arg(&f.root)
                .args(["--format", "--json"])
        )
        .status
        .success()
    );
}
#[cfg(unix)]
#[test]
fn verify_rejects_final_symlinks_and_preserves_target() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new(COMMENTED.as_bytes());
    let target = f.root.join("target.ves");
    fs::rename(&f.path, &target).unwrap();
    symlink(&target, &f.path).unwrap();
    reject(f.verify(&["--format", "--json"]));
    assert!(
        fs::symlink_metadata(&f.path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target).unwrap(), COMMENTED.as_bytes());
}
