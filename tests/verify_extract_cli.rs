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

const STRUCTS: &str = include_str!("fixtures/lean_extract/structs.ves");
const STRUCTS_PROOFS: &str = include_str!("fixtures/lean_extract/structs.proofs.lean");
const PARTIAL_CONTRACTS: &str = include_str!("fixtures/lean_extract/contracts.ves");
const TOTAL_CONTRACTS: &str = "\
fn clamp(x: u64, hi: u64) -> u64 {
    if x > hi { hi } else { x }
}
fn ensures_clamp(x: u64, hi: u64, r: u64) -> bool {
    r <= hi && r <= x
}
fn main() -> u64 {
    clamp(9, 4)
}
";
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
fn help_documents_extract_mode_and_options() {
    let out = run(Command::new(binary()).args(["verify", "--help"]));
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for flag in [
        "--extract",
        "--proofs",
        "--theorem",
        "--allow-axiom",
        "--olean-store",
        "--emit-lean",
    ] {
        assert!(text.contains(flag), "verify --help missing {flag}");
    }
}

#[test]
fn extract_rejects_invalid_flags_and_forbidden_axiom_policies() {
    let f = Fixture::new(TOTAL_CONTRACTS.as_bytes());
    let proofs = f.root.join("p.lean");
    fs::write(&proofs, STRUCTS_PROOFS).unwrap();
    let store = f.root.join("cas");

    // Bare `vesper verify SOURCE --json` must remain rejected (requires --format or --extract).
    let bare = f.verify(&["--json"]);
    assert_eq!(bare.status.code(), Some(1));
    let bare_json: Value = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(bare_json["schema"], "vesper.format-proof/1");
    assert_eq!(bare_json["status"], "rejected");
    assert_eq!(bare_json["stage"], "config");

    let cases: &[&[&str]] = &[
        &["--format", "--extract", "--json"],
        &["--format", "--proofs", proofs.to_str().unwrap(), "--json"],
        &["--format", "--theorem", "foo", "--json"],
        &["--format", "--allow-axiom", "propext", "--json"],
        &[
            "--format",
            "--olean-store",
            store.to_str().unwrap(),
            "--json",
        ],
        &["--extract", "--json", "--emit-lean"],
        &["--extract", "--extract", "--json"],
        &["--extract", "--timeout", "0", "--json"],
        &["--extract", "--timeout", "301", "--json"],
        &["--extract", "--theorem", "bad name", "--json"],
        &["--extract", "--allow-axiom", "sorryAx", "--json"],
        &["--extract", "--allow-axiom", "Lean.ofReduceBool", "--json"],
        &["--extract", "--allow-axiom", "Lean.trustCompiler", "--json"],
        &["--extract", "--olean-store", "", "--json"],
        &["--extract", "--unknown-flag", "--json"],
    ];
    for args in cases {
        let expect_extract_schema = args.contains(&"--extract") && !args.contains(&"--format");
        let out = f.verify(args);
        if expect_extract_schema && args.contains(&"--json") {
            reject_json(out, Some("config"));
        } else {
            assert_eq!(out.status.code(), Some(1), "args: {args:?}");
        }
    }

    let emit_fail = f.verify(&["--extract", "--allow-axiom", "sorryAx", "--emit-lean"]);
    assert_eq!(emit_fail.status.code(), Some(1));
    assert!(emit_fail.stdout.is_empty());
}

#[test]
fn extract_rejects_bad_source_empty_proofs_missing_lean_and_symlinks_without_writing_store() {
    // Parse error.
    let bad_syntax = Fixture::new(b"fn broken( {");
    reject_json(bad_syntax.verify(&["--extract", "--json"]), Some("parse"));

    // Type/semantic error.
    let bad_type = Fixture::new(b"fn bad(x: u64) -> bool { x }\n");
    reject_json(bad_type.verify(&["--extract", "--json"]), Some("check"));

    // Oversized input (> 128 KiB).
    let oversized = vec![b' '; 128 * 1024 + 1];
    reject_json(
        Fixture::new(&oversized).verify(&["--extract", "--json"]),
        Some("resource"),
    );

    // Non-UTF8 input.
    reject_json(
        Fixture::new(b"fn main() {\xff}\n").verify(&["--extract", "--json"]),
        Some("parse"),
    );

    // Empty proofs file (comments only) with no --theorem override.
    let f = Fixture::new(PURE_VES.as_bytes());
    let empty_proofs = f.root.join("empty.proofs.lean");
    fs::write(
        &empty_proofs,
        "-- line comment\n/- theorem commented_out : True := trivial -/\n",
    )
    .unwrap();
    reject_json(
        f.verify(&[
            "--extract",
            "--proofs",
            empty_proofs.to_str().unwrap(),
            "--json",
        ]),
        Some("proofs"),
    );

    // Missing --proofs file.
    let missing_proofs = f.root.join("missing.lean");
    reject_json(
        f.verify(&[
            "--extract",
            "--proofs",
            missing_proofs.to_str().unwrap(),
            "--json",
        ]),
        Some("io"),
    );

    // Missing Lean toolchain must fail cleanly before creating/recording CAS entries.
    let missing_lean = f.root.join("nonexistent-lean");
    let store_dir = f.root.join("cas-store");
    reject_json(
        f.verify(&[
            "--extract",
            "--lean",
            missing_lean.to_str().unwrap(),
            "--olean-store",
            store_dir.to_str().unwrap(),
            "--json",
        ]),
        Some("toolchain"),
    );
    assert!(
        !store_dir.exists(),
        "failed toolchain must not create or populate --olean-store"
    );
}

#[cfg(unix)]
#[test]
fn extract_rejects_final_symlinks_for_source_and_proofs() {
    use std::os::unix::fs::symlink;

    let f = Fixture::new(PURE_VES.as_bytes());
    let target = f.root.join("target.ves");
    fs::rename(&f.source, &target).unwrap();
    symlink(&target, &f.source).unwrap();
    let out = run(Command::new(binary())
        .arg("verify")
        .arg(&f.source)
        .args(["--extract", "--json"]));
    reject_json(out, Some("io"));
    assert!(
        fs::symlink_metadata(&f.source)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), PURE_VES.as_bytes());

    let f2 = Fixture::new(PURE_VES.as_bytes());
    let proofs_target = f2.root.join("target.lean");
    fs::write(&proofs_target, "theorem ok : True := trivial\n").unwrap();
    let proofs_link = f2.root.join("link.lean");
    symlink(&proofs_target, &proofs_link).unwrap();
    reject_json(
        f2.verify(&[
            "--extract",
            "--proofs",
            proofs_link.to_str().unwrap(),
            "--json",
        ]),
        Some("io"),
    );
}

#[test]
fn actual_lean_extract_verification_contracts_policy_and_olean_cas_roundtrip() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let lean_str = lean.to_str().unwrap();

    // 1. Bare --extract --json without --proofs kernel-checks a total contracted model.
    let f = Fixture::new(TOTAL_CONTRACTS.as_bytes());
    let bare = f.verify(&[
        "--extract",
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        bare.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&bare.stdout),
        String::from_utf8_lossy(&bare.stderr)
    );
    let report: Value = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(report["schema"], "vesper.verify-extract/1");
    assert_eq!(report["status"], "kernel_checked");
    assert_eq!(report["scope"], "source_lean_extraction");
    assert_eq!(
        report["source_blake3"],
        blake3::hash(TOTAL_CONTRACTS.as_bytes()).to_hex().as_str()
    );
    assert_eq!(report["input_bytes"], TOTAL_CONTRACTS.len());
    assert_eq!(report["totality"]["clamp"], "total");
    let contracts = report["contracts"].as_array().unwrap();
    assert_eq!(contracts, &vec![Value::String("clamp".into())]);
    assert_eq!(report["audit"]["accepted"], true);
    assert_eq!(report["audit"]["kernel_replayed"], true);
    assert_eq!(report["audit"]["forbidden_axioms"], serde_json::json!([]));

    // 2. --emit-lean returns pure extracted Lean (without synthetic VesperVerify wrapper).
    let emitted = f.verify(&[
        "--extract",
        "--emit-lean",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let lean_text = String::from_utf8(emitted.stdout).unwrap();
    assert!(lean_text.contains("clamp$contract"));
    assert!(!lean_text.contains("namespace VesperVerify"));

    // 3. --proofs + --allow-axiom + --olean-store populates CAS on miss and hits CAS on rerun.
    let f_structs = Fixture::new(STRUCTS.as_bytes());
    let proofs_path = f_structs.root.join("structs.proofs.lean");
    fs::write(&proofs_path, STRUCTS_PROOFS).unwrap();
    let store_dir = f_structs.root.join("olean-cas");

    let first = f_structs.verify(&[
        "--extract",
        "--proofs",
        proofs_path.to_str().unwrap(),
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Quot.sound",
        "--allow-axiom",
        "Classical.choice",
        "--olean-store",
        store_dir.to_str().unwrap(),
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        first.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["audit"]["accepted"], true);
    assert_eq!(first_json["audit"]["kernel_replayed"], true);
    let mut audited: Vec<&str> = first_json["audit"]["theorems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    audited.sort_unstable();
    assert_eq!(
        audited,
        vec![
            "VesperProofs.Structs.main_value",
            "VesperProofs.Structs.move_start_frame",
            "VesperProofs.Structs.swap_exchanges",
        ]
    );
    let cert1 = &first_json["olean_certificate"];
    assert_eq!(cert1["cache_hit"], false);
    assert_eq!(cert1["newly_recorded"], true);
    let oid1 = cert1["object_id"].as_str().unwrap();
    assert!(!oid1.is_empty());

    let second = f_structs.verify(&[
        "--extract",
        "--proofs",
        proofs_path.to_str().unwrap(),
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Quot.sound",
        "--allow-axiom",
        "Classical.choice",
        "--olean-store",
        store_dir.to_str().unwrap(),
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(second.status.success());
    let second_json: Value = serde_json::from_slice(&second.stdout).unwrap();
    let cert2 = &second_json["olean_certificate"];
    assert_eq!(cert2["cache_hit"], true);
    assert_eq!(cert2["newly_recorded"], false);
    assert_eq!(cert2["object_id"], oid1);
    assert_eq!(second_json["audit"]["kernel_replayed"], true);

    // 4. Axiom policy rejections & partial-declaration gate with real Lean kernel.
    let f_partial = Fixture::new(PARTIAL_CONTRACTS.as_bytes());
    reject_json(
        f_partial.verify(&[
            "--extract",
            "--json",
            "--lean",
            lean_str,
            "--timeout",
            "300",
        ]),
        Some("kernel"),
    );

    let f_pure = Fixture::new(PURE_VES.as_bytes());
    let sorry_proofs = f_pure.root.join("sorry.lean");
    fs::write(
        &sorry_proofs,
        "namespace Bad\ntheorem uses_sorry : True := by sorry\nend Bad\n",
    )
    .unwrap();
    reject_json(
        f_pure.verify(&[
            "--extract",
            "--proofs",
            sorry_proofs.to_str().unwrap(),
            "--json",
            "--lean",
            lean_str,
            "--timeout",
            "300",
        ]),
        Some("kernel"),
    );

    let axiom_proofs = f_pure.root.join("custom_axiom.lean");
    fs::write(
        &axiom_proofs,
        "namespace Custom\naxiom oracle : True\ntheorem uses_oracle : True := oracle\nend Custom\n",
    )
    .unwrap();
    reject_json(
        f_pure.verify(&[
            "--extract",
            "--proofs",
            axiom_proofs.to_str().unwrap(),
            "--json",
            "--lean",
            lean_str,
            "--timeout",
            "300",
        ]),
        Some("kernel"),
    );
    let allowed = f_pure.verify(&[
        "--extract",
        "--proofs",
        axiom_proofs.to_str().unwrap(),
        "--allow-axiom",
        "Custom.oracle",
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        allowed.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );

    // 5. Attributed theorems with `sorry` must not be skipped by default discovery.
    let mixed_attr_sorry = f_pure.root.join("mixed_attr_sorry.lean");
    fs::write(
        &mixed_attr_sorry,
        "theorem checked : True := trivial\n@[simp] theorem unchecked : False := by sorry\n",
    )
    .unwrap();
    reject_json(
        f_pure.verify(&[
            "--extract",
            "--proofs",
            mixed_attr_sorry.to_str().unwrap(),
            "--json",
            "--lean",
            lean_str,
            "--timeout",
            "300",
        ]),
        Some("kernel"),
    );

    // 6. Valid attributed theorems plus anonymous `section ... end` and named `section ... end` inside `namespace` audit accurately.
    let attr_and_section = f_pure.root.join("attr_section.lean");
    fs::write(
        &attr_and_section,
        "namespace ScopeDemo\nsection\n@[simp] theorem inside_anon_sec : True := trivial\nend\nsection Named\ntheorem inside_named_sec : True := trivial\nend Named\ntheorem still_ns : True := trivial\nend ScopeDemo\ntheorem top_level : True := trivial\n",
    )
    .unwrap();
    let scoped = f_pure.verify(&[
        "--extract",
        "--proofs",
        attr_and_section.to_str().unwrap(),
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        scoped.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&scoped.stdout),
        String::from_utf8_lossy(&scoped.stderr)
    );
    let scoped_json: Value = serde_json::from_slice(&scoped.stdout).unwrap();
    let mut scoped_names: Vec<&str> = scoped_json["audit"]["theorems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    scoped_names.sort_unstable();
    assert_eq!(
        scoped_names,
        vec![
            "ScopeDemo.inside_anon_sec",
            "ScopeDemo.inside_named_sec",
            "ScopeDemo.still_ns",
            "top_level",
        ]
    );

    // 7. Bare --extract --olean-store (without --proofs) populates CAS on miss, hits on rerun,
    //    and misses cleanly when the input source changes.
    let bare_store = f_pure.root.join("bare-olean-cas");
    let bare_miss = f_pure.verify(&[
        "--extract",
        "--olean-store",
        bare_store.to_str().unwrap(),
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        bare_miss.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&bare_miss.stdout),
        String::from_utf8_lossy(&bare_miss.stderr)
    );
    let bare_miss_json: Value = serde_json::from_slice(&bare_miss.stdout).unwrap();
    assert_eq!(bare_miss_json["olean_certificate"]["cache_hit"], false);
    assert_eq!(bare_miss_json["olean_certificate"]["newly_recorded"], true);
    let bare_oid = bare_miss_json["olean_certificate"]["object_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let bare_hit = f_pure.verify(&[
        "--extract",
        "--olean-store",
        bare_store.to_str().unwrap(),
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(bare_hit.status.success());
    let bare_hit_json: Value = serde_json::from_slice(&bare_hit.stdout).unwrap();
    assert_eq!(bare_hit_json["olean_certificate"]["cache_hit"], true);
    assert_eq!(bare_hit_json["olean_certificate"]["newly_recorded"], false);
    assert_eq!(bare_hit_json["olean_certificate"]["object_id"], bare_oid);

    let f_other = Fixture::new(b"fn add2(x: u64) -> u64 { x }\nfn main() -> u64 { add2(2) }\n");
    let bare_changed = f_other.verify(&[
        "--extract",
        "--olean-store",
        bare_store.to_str().unwrap(),
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(bare_changed.status.success());
    let bare_changed_json: Value = serde_json::from_slice(&bare_changed.stdout).unwrap();
    assert_eq!(bare_changed_json["olean_certificate"]["cache_hit"], false);
    assert_eq!(
        bare_changed_json["olean_certificate"]["newly_recorded"],
        true
    );
    assert_ne!(
        bare_changed_json["olean_certificate"]["object_id"],
        bare_oid
    );
}

#[test]
fn extract_rejects_private_theorem_in_default_discovery() {
    let f = Fixture::new(PURE_VES.as_bytes());
    let priv_proofs = f.root.join("private_thm.lean");
    fs::write(
        &priv_proofs,
        "theorem ok : True := trivial\nprivate theorem hidden : False := by sorry\n",
    )
    .unwrap();
    reject_json(
        f.verify(&[
            "--extract",
            "--proofs",
            priv_proofs.to_str().unwrap(),
            "--json",
        ]),
        Some("proofs"),
    );
}
