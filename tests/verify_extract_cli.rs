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
        "--contract-proof",
        "--contract-witness",
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
        &["--format", "--contract-proof", "clamp=Proof.ok", "--json"],
        &[
            "--format",
            "--contract-witness",
            "clamp=Proof.input",
            "--json",
        ],
        &["--extract", "--contract-proof", "clamp", "--json"],
        &[
            "--extract",
            "--contract-proof",
            "clamp=bad;command",
            "--json",
        ],
        &[
            "--extract",
            "--contract-proof",
            "clamp=Proof.ok",
            "--contract-proof",
            "clamp=Proof.other",
            "--json",
        ],
        &["--extract", "--allow-axiom", "sorryAx", "--json"],
        &["--extract", "--allow-axiom", "Lean.ofReduceBool", "--json"],
        &["--extract", "--allow-axiom", "Lean.trustCompiler", "--json"],
        &[
            "--extract",
            "--allow-axiom",
            "Example.native._native.native_decide.ax_1_1",
            "--json",
        ],
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

    // 4. Recursive models use the total projection, with explicit scope.
    let f_partial = Fixture::new(PARTIAL_CONTRACTS.as_bytes());
    let projected = f_partial.verify(&[
        "--extract",
        "--json",
        "--lean",
        lean_str,
        "--timeout",
        "300",
    ]);
    assert!(
        projected.status.success(),
        "{}",
        String::from_utf8_lossy(&projected.stdout)
    );
    let projected: Value = serde_json::from_slice(&projected.stdout).unwrap();
    assert_eq!(projected["scope"], "source_lean_fuel_projection");
    assert!(
        projected["omitted_executable_definitions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("Vesper.countdown"))
    );
    assert_eq!(projected["checked_contracts"], serde_json::json!({}));

    // Axiom policy and proof-supplied unsafe declarations still fail closed.
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

const CONTRACT_ANSWER: &str = "fn answer() -> u64 { 7 }\nfn ensures_answer(r: u64) -> bool { r == 7 }\nfn main() -> u64 { answer() }\n";

#[test]
fn contract_proof_bindings_require_complete_and_known_contracts_before_lean() {
    let f = Fixture::new(CONTRACT_ANSWER.as_bytes());
    reject_json(
        f.verify(&[
            "--extract",
            "--contract-proof",
            "missing=Proof.ok",
            "--lean",
            "missing-lean",
            "--json",
        ]),
        Some("contracts"),
    );
    let two = format!(
        "{CONTRACT_ANSWER}fn second() -> u64 {{ 3 }}\nfn ensures_second(r: u64) -> bool {{ r == 3 }}\n"
    );
    let f = Fixture::new(two.as_bytes());
    let output = f.verify(&[
        "--extract",
        "--contract-proof",
        "answer=Proof.ok",
        "--lean",
        "missing-lean",
        "--json",
    ]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["message"]
            .as_str()
            .unwrap()
            .contains("missing proof binding for contract 'second'")
    );
    reject_json(output, Some("contracts"));
}

#[test]
fn contract_proof_binding_rejects_unrelated_theorem_and_replays_exact_contract() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let f = Fixture::new(CONTRACT_ANSWER.as_bytes());
    let proofs = f.root.join("answer.lean");
    fs::write(&proofs, "theorem answer_ok : True := trivial\ntheorem answer_input : Vesper.«answer$admissible» := trivial\n").unwrap();
    let args = [
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "answer=answer_ok",
        "--contract-witness",
        "answer=answer_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    // Ordinary audit of this theorem succeeds; it is not the requested contract.
    let denied = f.verify(&args);
    reject_json(denied, Some("compile"));
    fs::write(
        &proofs,
        "theorem answer_ok : Vesper.«answer$contract» := by rfl\ntheorem answer_input : Vesper.«answer$admissible» := trivial\n",
    )
    .unwrap();
    let accepted = f.verify(&args);
    assert!(
        accepted.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&accepted.stdout),
        String::from_utf8_lossy(&accepted.stderr)
    );
    let report: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(
        report["checked_contracts"],
        serde_json::json!({"answer": "answer_ok"})
    );
    let binding = report["audit"]["theorems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "VesperVerify.contract_0")
        .unwrap();
    let axioms = binding["axioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        axioms,
        std::collections::BTreeSet::from(["Classical.choice", "Quot.sound", "propext"])
    );
    assert_eq!(binding["type_repr"], "Vesper.answer$contract");
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"answer": "answer_input"})
    );
    assert!(
        report["audit"]["theorems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "VesperVerify.precondition_0"
                && t["type_repr"] == "Vesper.answer$admissible")
    );

    // Lean can implicitly abstract a section instance into the generated
    // binding. Merely elaborating that binding would admit a hidden premise.
    fs::write(&proofs, "class HiddenAssumption : Prop where bad : False\nvariable [HiddenAssumption]\ntheorem answer_ok [HiddenAssumption] : Vesper.«answer$contract» := False.elim HiddenAssumption.bad\ntheorem answer_input : Vesper.«answer$admissible» := trivial\n").unwrap();
    let conditional = f.verify(&args);
    let report: Value = serde_json::from_slice(&conditional.stdout).unwrap();
    assert!(
        report["message"]
            .as_str()
            .unwrap()
            .contains("extra hypotheses"),
        "{report}"
    );
    reject_json(conditional, Some("kernel"));

    fs::write(
        &proofs,
        "theorem answer_ok : Vesper.«answer$contract» := by rfl\ntheorem answer_input : Vesper.«answer$admissible» := trivial\n",
    )
    .unwrap();
    // Changing the extracted implementation invalidates its old proof.
    let wrong = Fixture::new(CONTRACT_ANSWER.replace("{ 7 }", "{ 8 }").as_bytes());
    reject_json(wrong.verify(&args), Some("compile"));
}

#[test]
fn explicit_contracts_require_precondition_witnesses() {
    let f = Fixture::new(CONTRACT_ANSWER.as_bytes());
    for args in [
        vec![
            "--extract",
            "--contract-proof",
            "answer=Proof.ok",
            "--lean",
            "missing-lean",
            "--json",
        ],
        vec![
            "--extract",
            "--contract-witness",
            "answer=Proof.input",
            "--lean",
            "missing-lean",
            "--json",
        ],
        vec![
            "--extract",
            "--contract-proof",
            "answer=Proof.ok",
            "--contract-witness",
            "missing=Proof.input",
            "--lean",
            "missing-lean",
            "--json",
        ],
    ] {
        reject_json(f.verify(&args), Some("contracts"));
    }
    for args in [
        vec![
            "--extract",
            "--contract-proof",
            "answer=Proof.ok",
            "--contract-witness",
            "answer=Proof.input",
            "--allow-axiom",
            "Bad",
            "--json",
        ],
        vec!["--extract", "--contract-witness", "answer", "--json"],
        vec![
            "--extract",
            "--contract-witness",
            "answer=Proof.input",
            "--contract-witness",
            "answer=Proof.other",
            "--json",
        ],
    ] {
        reject_json(f.verify(&args), Some("config"));
    }
}

#[test]
fn precondition_witness_rejects_vacuity_and_hidden_hypotheses() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let source = "fn echo(x: u64, y: u64) -> u64 { x }\nfn requires_echo(x: u64, y: u64) -> bool { x == 7 && y == 9 }\nfn ensures_echo(x: u64, y: u64, r: u64) -> bool { r == x }\nfn main() -> u64 { 0 }\n";
    let f = Fixture::new(source.as_bytes());
    let proofs = f.root.join("witness.lean");
    let proof = "theorem echo_ok : Vesper.«echo$contract» := by intro x y _; simp [Vesper.ensures_echo, Vesper.echo]\n";
    let witness = "theorem echo_input : Vesper.«echo$admissible» := by exact ⟨7, 9, rfl⟩\n";
    fs::write(&proofs, format!("{proof}{witness}")).unwrap();
    let args = [
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "echo=echo_ok",
        "--contract-witness",
        "echo=echo_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let accepted = f.verify(&args);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stdout)
    );
    let report: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"echo": "echo_input"})
    );

    // The old witness fails after the precondition becomes contradictory,
    // even though the same partial-correctness proof still holds.
    let impossible = Fixture::new(source.replace("x == 7 && y == 9", "false").as_bytes());
    reject_json(impossible.verify(&args), Some("compile"));

    // A satisfiability proof cannot borrow an unfulfilled section assumption.
    fs::write(&proofs, format!("{proof}class HiddenInput : Prop where witness : Vesper.«echo$admissible»\nvariable [HiddenInput]\ntheorem echo_input [HiddenInput] : Vesper.«echo$admissible» := HiddenInput.witness\n")).unwrap();
    let denied = f.verify(&args);
    let report: Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert!(
        report["message"]
            .as_str()
            .unwrap()
            .contains("extra hypotheses"),
        "{report}"
    );
    reject_json(denied, Some("kernel"));

    // Unrelated True has no authority to establish admissible inputs.
    fs::write(
        &proofs,
        format!("{proof}theorem echo_input : True := trivial\n"),
    )
    .unwrap();
    reject_json(f.verify(&args), Some("compile"));
}

#[test]
fn vacuous_false_postcondition_needs_an_impossible_input_witness() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let f = Fixture::new(b"fn vacuous() -> u64 { 0 }\nfn requires_vacuous() -> bool { false }\nfn ensures_vacuous(r: u64) -> bool { false }\nfn main() -> u64 { 0 }\n");
    let proofs = f.root.join("vacuous.lean");
    fs::write(
        &proofs,
        "theorem vacuous_ok : Vesper.«vacuous$contract» := by intro impossible; cases impossible\n",
    )
    .unwrap();
    let mut args = vec![
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let partial = f.verify(&args);
    assert!(
        partial.status.success(),
        "{}",
        String::from_utf8_lossy(&partial.stdout)
    );
    let report: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(report["checked_contracts"], serde_json::json!({}));
    args.extend(["--contract-proof", "vacuous=vacuous_ok"]);
    reject_json(f.verify(&args), Some("contracts"));
    fs::write(&proofs, "theorem vacuous_ok : Vesper.«vacuous$contract» := by intro impossible; cases impossible\ntheorem vacuous_input : True := trivial\n").unwrap();
    args.extend(["--contract-witness", "vacuous=vacuous_input"]);
    reject_json(f.verify(&args), Some("compile"));
}

#[test]
fn generic_contract_admission_requires_a_universal_proof_and_type_witness() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let source = "fn choose[T](x: T, flag: bool) -> bool { flag }\nfn ensures_choose[U](x: U, flag: bool, result: bool) -> bool { result == flag }\nfn main() -> u64 { 0 }\n";
    let f = Fixture::new(source.as_bytes());
    let proofs = f.root.join("generic.lean");
    let witness =
        "theorem choose_input : Vesper.«choose$admissible» := ⟨UInt64, 5, true, trivial⟩\n";
    fs::write(&proofs, format!("theorem choose_ok : Vesper.«choose$contract» := by intro T x flag; simp [Vesper.ensures_choose, Vesper.choose]\n{witness}")).unwrap();
    let args = [
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "choose=choose_ok",
        "--contract-witness",
        "choose=choose_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let accepted = f.verify(&args);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stdout)
    );
    let report: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(
        report["checked_contracts"],
        serde_json::json!({"choose": "choose_ok"})
    );
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"choose": "choose_input"})
    );

    let wrong = Fixture::new(source.replace("{ flag }", "{ false }").as_bytes());
    reject_json(wrong.verify(&args), Some("compile"));

    // A valid theorem for UInt64 does not prove the contract for every type.
    fs::write(&proofs, format!("theorem choose_ok : ∀ (x : UInt64) (flag : Bool), Vesper.ensures_choose x flag (Vesper.choose x flag) = true := by intro x flag; simp [Vesper.ensures_choose, Vesper.choose]\n{witness}")).unwrap();
    reject_json(f.verify(&args), Some("compile"));
}

#[test]
fn labeled_fail_contract_admission_checks_successful_results() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let source = "fn choose(n: u64, reject: bool) -> u64 eff [stop: Fail[u64]] { if reject { stop.raise(n) } else { n } }\nfn ensures_choose(n: u64, reject: bool, result: u64) -> bool { result == n }\nfn main() -> u64 { 0 }\n";
    let f = Fixture::new(source.as_bytes());
    let proofs = f.root.join("labeled.lean");
    fs::write(&proofs, "theorem choose_ok : Vesper.«choose$contract» := by\n  intro n reject result run\n  cases reject with\n  | false =>\n    simp [Vesper.choose] at run\n    cases run\n    simp [Vesper.ensures_choose]\n  | true =>\n    change Except.error n = Except.ok result at run\n    contradiction\ntheorem choose_input : Vesper.«choose$admissible» := ⟨42, false, trivial⟩\n").unwrap();
    let args = [
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "choose=choose_ok",
        "--contract-witness",
        "choose=choose_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let accepted = f.verify(&args);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stdout)
    );
    let report: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(
        report["checked_contracts"],
        serde_json::json!({"choose": "choose_ok"})
    );
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"choose": "choose_input"})
    );
    let wrong = Fixture::new(source.replace("else { n }", "else { 0 }").as_bytes());
    reject_json(wrong.verify(&args), Some("compile"));
}

#[test]
fn inout_contract_admission_binds_post_state_and_rejects_changed_implementation() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let source = "fn replace(inout x: u64, value: u64) -> u64 { let old = x; x = value; old }\nfn ensures_replace(before: u64, value: u64, result: u64, after: u64) -> bool { result == before && after == value }\nfn main() -> u64 { 0 }\n";
    let f = Fixture::new(source.as_bytes());
    let proofs = f.root.join("inout.lean");
    fs::write(&proofs, "theorem replace_ok : Vesper.«replace$contract» := by intro before value; simp [Vesper.ensures_replace, Vesper.replace]\ntheorem replace_input : Vesper.«replace$admissible» := ⟨5, 42, trivial⟩\n").unwrap();
    let args = [
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "replace=replace_ok",
        "--contract-witness",
        "replace=replace_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let accepted = f.verify(&args);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stdout)
    );
    let report: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(
        report["checked_contracts"],
        serde_json::json!({"replace": "replace_ok"})
    );
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"replace": "replace_input"})
    );
    // Returning the old value still works, but silently failing to update x
    // invalidates the same closed contract proof.
    let wrong = Fixture::new(source.replace("x = value;", "x = 0;").as_bytes());
    reject_json(wrong.verify(&args), Some("compile"));
}

#[test]
fn multi_fail_contract_admission_checks_tagged_model_results() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let source = "fn choose(n: u64, reject: bool) -> u64 eff [left: Fail[u64], right: Fail[bool]] { if reject { right.raise(false) } else { n } }\nfn ensures_choose(n: u64, reject: bool, result: u64) -> bool { result == n }\nfn main() -> u64 { 0 }\n";
    let f = Fixture::new(source.as_bytes());
    let proofs = f.root.join("multi.lean");
    fs::write(&proofs, "theorem choose_ok : Vesper.«choose$contract» := by\n  intro n reject result run\n  cases reject with\n  | false =>\n    simp [Vesper.choose] at run\n    cases run\n    simp [Vesper.ensures_choose]\n  | true =>\n    change Except.error (Sum.inr false) = Except.ok result at run\n    contradiction\ntheorem choose_input : Vesper.«choose$admissible» := ⟨42, false, trivial⟩\n").unwrap();
    let args = [
        "--extract",
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "choose=choose_ok",
        "--contract-witness",
        "choose=choose_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let accepted = f.verify(&args);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stdout)
    );
    let report: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(
        report["checked_contracts"],
        serde_json::json!({"choose": "choose_ok"})
    );
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"choose": "choose_input"})
    );
    let wrong = Fixture::new(source.replace("else { n }", "else { 0 }").as_bytes());
    reject_json(wrong.verify(&args), Some("compile"));
}

#[test]
fn effectful_recursive_callback_contract_admission_uses_total_proof_projection() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lean) = lean() else { return };
    let source = include_str!("fixtures/lean_extract/recursive_values.ves");
    let f = Fixture::new(source.as_bytes());
    let proofs = f.root.join("recursive-values.lean");
    let store = f.root.join("proof-store");
    fs::write(
        &proofs,
        include_str!("fixtures/lean_extract/recursive_values.proofs.lean"),
    )
    .unwrap();
    let args = [
        "--extract",
        "--olean-store",
        store.to_str().unwrap(),
        "--proofs",
        proofs.to_str().unwrap(),
        "--contract-proof",
        "down=VesperProofs.RecursiveValues.down_contract",
        "--contract-witness",
        "down=VesperProofs.RecursiveValues.down_input",
        "--allow-axiom",
        "propext",
        "--allow-axiom",
        "Classical.choice",
        "--allow-axiom",
        "Quot.sound",
        "--lean",
        lean.to_str().unwrap(),
        "--timeout",
        "300",
        "--json",
    ];
    let audited = f.verify(&args);
    assert!(
        audited.status.success(),
        "{}",
        String::from_utf8_lossy(&audited.stdout)
    );
    let report: Value = serde_json::from_slice(&audited.stdout).unwrap();
    assert_eq!(
        report["checked_contracts"],
        serde_json::json!({"down": "VesperProofs.RecursiveValues.down_contract"})
    );
    assert_eq!(
        report["checked_preconditions"],
        serde_json::json!({"down": "VesperProofs.RecursiveValues.down_input"})
    );
    assert_eq!(report["scope"], "source_lean_fuel_projection");
    assert_eq!(
        report["omitted_executable_definitions"],
        serde_json::json!(["Vesper.down", "Vesper.main", "Vesper.spin", "Vesper.stored"])
    );
    assert_eq!(report["olean_certificate"]["cache_hit"], false);
    let cached = f.verify(&args);
    assert!(
        cached.status.success(),
        "{}",
        String::from_utf8_lossy(&cached.stdout)
    );
    let cached: Value = serde_json::from_slice(&cached.stdout).unwrap();
    assert_eq!(cached["olean_certificate"]["cache_hit"], true);
    assert_eq!(cached["lean_model_blake3"], report["lean_model_blake3"]);
    // The first 42 is the base-case result; leave the postcondition unchanged.
    let wrong = Fixture::new(source.replacen("42", "41", 1).as_bytes());
    reject_json(wrong.verify(&args), Some("compile"));
    // The projection does not allow unsafe declarations supplied by a proof.
    fs::write(
        &proofs,
        format!(
            "{}\nunsafe def forbidden : Nat := 0\n",
            include_str!("fixtures/lean_extract/recursive_values.proofs.lean")
        ),
    )
    .unwrap();
    reject_json(f.verify(&args), Some("kernel"));
}
