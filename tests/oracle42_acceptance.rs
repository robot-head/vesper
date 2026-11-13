//! Independent acceptance of real Lean data answers and all Rust backend modes.
//! Fault peers exercise transport only; they never supply arithmetic authority.
#[path = "../src/bin/arithmetic_oracle42/mod.rs"]
pub mod oracle;

use oracle::{
    Callee, Case, DiagnosticClass, DiagnosticPhase, FailureKind, HarnessLimits, Input, Intent,
    Kind, Node, Op, OracleAnswer, Outcome, Program, RunConfig, RunReport, RunStatus, RustAnswer,
    Scope, Suite,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CAPTURE: usize = 65536;
const READY: &str = "4.24.0";
const SEED: u64 = 0;
static SERIAL: Mutex<()> = Mutex::new(());

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-oracle42-acceptance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("fresh regular-file acceptance directory");
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn actual_binary() -> PathBuf {
    let given = PathBuf::from(env!("CARGO_BIN_EXE_arithmetic_oracle42"));
    let path = if given.components().count() > 1 || given.is_absolute() {
        given
    } else {
        std::env::split_paths(&std::env::var_os("PATH").expect("binary discovery PATH"))
            .flat_map(|directory| {
                let direct = directory.join(&given);
                let mut candidates = vec![direct];
                if cfg!(windows) && given.extension().is_none() {
                    candidates.push(directory.join(&given).with_extension("exe"));
                }
                candidates
            })
            .find(|path| path.is_file())
            .expect("the genuine Cargo binary on PATH")
    };
    path.canonicalize()
        .expect("canonical actual Cargo oracle binary")
}

fn hash_file(path: &Path) -> [u8; 32] {
    let mut file = File::open(path).unwrap();
    let mut buffer = [0; 65536];
    let mut hash = blake3::Hasher::new();
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    *hash.finalize().as_bytes()
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl Captured {
    fn diagnostic(&self) -> String {
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
    fn success(&self) {
        assert!(self.status.success(), "{}", self.diagnostic());
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.stdout).unwrap_or_else(|error| {
            panic!("invalid bounded CLI JSON: {error}\n{}", self.diagnostic())
        })
    }
}
fn drain(mut pipe: impl Read, overflow: &AtomicBool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = pipe
            .read(&mut buffer)
            .expect("read captured subprocess pipe");
        if count == 0 {
            return bytes;
        }
        let room = CAPTURE.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(room)]);
        if count > room {
            overflow.store(true, Ordering::Relaxed);
        }
    }
}
fn execute(command: &mut Command, seconds: u64) -> Captured {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("required real subprocess {command:?}: {error}"));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let overflow = AtomicBool::new(false);
    let (status, stdout, stderr, refused) = thread::scope(|scope| {
        let out = scope.spawn(|| drain(stdout, &overflow));
        let err = scope.spawn(|| drain(stderr, &overflow));
        let mut refused = false;
        let status = loop {
            if overflow.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(seconds)
            {
                refused = true;
                let _ = child.kill();
                break child.wait().unwrap();
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            thread::sleep(Duration::from_millis(10));
        };
        (status, out.join().unwrap(), err.join().unwrap(), refused)
    });
    let captured = Captured {
        status,
        stdout,
        stderr,
    };
    assert!(
        !refused && !overflow.load(Ordering::Relaxed),
        "subprocess exceeded {seconds}s or 64KiB per stream: {command:?}\n{}",
        captured.diagnostic()
    );
    assert!(
        !captured.diagnostic().contains("PANIC"),
        "{}",
        captured.diagnostic()
    );
    eprintln!(
        "oracle42 acceptance subprocess {:.2}s: {command:?}",
        started.elapsed().as_secs_f64()
    );
    captured
}

struct Toolchain {
    lean: PathBuf,
    prefix: PathBuf,
}
impl Toolchain {
    fn resolve() -> Self {
        let selected = std::env::var_os("VESPER_LEAN").unwrap_or_else(|| "lean".into());
        let output = execute(
            Command::new(selected)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .arg("--print-prefix"),
            30,
        );
        output.success();
        let prefix = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
            .canonicalize()
            .expect("actual pinned Lean prefix");
        let lean = prefix
            .join("bin")
            .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
        let version = execute(Command::new(&lean).arg("--version"), 10);
        version.success();
        assert!(
            String::from_utf8_lossy(&version.stdout).contains("version 4.24.0,"),
            "actual Lean4.24.0 required; missing/wrong default tools never skip"
        );
        Self { lean, prefix }
    }
    fn command(&self, root: &Path) -> Command {
        let mut command = Command::new(&self.lean);
        command
            .env_clear()
            .current_dir(root)
            .env(
                "LEAN_PATH",
                std::env::join_paths([root.to_path_buf(), self.prefix.join("lib/lean")]).unwrap(),
            )
            .env("LEAN_ABORT_ON_PANIC", "1");
        #[cfg(windows)]
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
    }
}

fn portable_faults(binary: &Path) {
    use oracle::process::testing::{ProbeKind as P, ProbeLimits, probe};
    let modes = [
        ("malformed", P::Malformed, Some(FailureKind::Protocol)),
        ("oversize", P::Oversize, Some(FailureKind::Protocol)),
        (
            "truncated_header",
            P::TruncatedHeader,
            Some(FailureKind::Protocol),
        ),
        (
            "truncated_payload",
            P::TruncatedPayload,
            Some(FailureKind::Protocol),
        ),
        ("stall", P::Stall, Some(FailureKind::Timeout)),
        ("stderr_flood", P::StderrFlood, Some(FailureKind::Protocol)),
        ("reordered", P::Reordered, Some(FailureKind::Protocol)),
        ("bad_bye", P::BadBye, Some(FailureKind::CounterMismatch)),
        ("short_writes", P::ShortWrites, None),
        ("crash", P::Crash, Some(FailureKind::Crash)),
    ];
    for (mode, kind, expected) in modes {
        let mut command = Command::new(binary);
        command.args(["--fault-peer", mode]);
        let report = probe(
            command,
            kind,
            ProbeLimits {
                frame_bytes: 512,
                stderr_bytes: 1024,
                timeout_ms: if mode == "stall" { 100 } else { 5000 },
                shutdown_timeout_ms: 1000,
            },
        )
        .expect("real production transport probe");
        assert_eq!(report.kind, kind);
        assert_eq!(
            report.observed, expected,
            "actual classification for {mode}"
        );
        assert!(
            report.child_reaped
                && report.reader_joined
                && report.writer_joined
                && report.stderr_joined,
            "actual wait/reap and all pipe joins for {mode}: {report:?}"
        );
        assert!(report.max_frame_bytes <= 512 && report.stderr_bytes <= 1024);
        if mode == "oversize" {
            assert_eq!(report.max_frame_bytes, 0, "cap before payload allocation");
        }
        if mode == "stderr_flood" {
            assert_eq!(report.stderr_bytes, 1024);
        }
    }
    let invalid = probe(
        Command::new(binary),
        P::Stall,
        ProbeLimits {
            frame_bytes: 0,
            stderr_bytes: 1,
            timeout_ms: 1,
            shutdown_timeout_ms: 1,
        },
    )
    .expect_err("invalid probe caps before spawn");
    assert_eq!(invalid.kind, FailureKind::InvalidConfig);
}

fn config(binary: &Path, tool: &Toolchain, jobs: u8) -> RunConfig {
    RunConfig {
        suite: Suite::Ci128,
        seed: SEED,
        jobs,
        lean: tool.lean.clone(),
        worker_exe: binary.to_path_buf(),
        artifacts: None,
        limits: HarnessLimits::default(),
    }
}

fn report_counts(report: &RunReport, jobs: u8, binary: &Path) {
    assert_eq!(report.schema, 1);
    assert_eq!(report.status, RunStatus::Complete);
    assert_eq!(report.suite, Suite::Ci128);
    assert_eq!(report.seed, SEED);
    assert_eq!(
        (
            report.requested_valid,
            report.generated_valid,
            report.unique_valid,
            report.committed_valid
        ),
        (128, 128, 128, 128)
    );
    assert_eq!(
        (report.generated_negative, report.matched_negative),
        (15, 15)
    );
    assert_eq!(
        (
            report.parser_calls,
            report.checker_calls,
            report.ast_optimizer_calls
        ),
        (143, 269, 128)
    );
    assert_eq!(report.evaluator_calls, [128; 4]);
    assert_eq!(report.jit_calls, [128; 4]);
    assert_eq!(
        (
            report.lean_requests,
            report.lean_responses,
            report.lean_spawns
        ),
        (143, 143, 1)
    );
    assert_eq!(report.jobs, jobs);
    assert!(report.max_worker_cases > 0 && report.max_worker_cases <= 32);
    assert!(report.max_in_flight > 0 && report.max_in_flight <= u16::from(jobs) * 4);
    if jobs == 1 {
        assert_eq!(
            (report.rust_worker_spawns, report.rust_worker_recycles),
            (5, 4)
        );
    }
    for count in [
        report.timeouts,
        report.crashes,
        report.protocol_errors,
        report.outcome_mismatches,
        report.diagnostic_mismatches,
        report.cancelled_cases,
        report.shrink_attempts,
        report.interrupted_worker_cases,
    ] {
        assert_eq!(count, 0);
    }
    assert_eq!(report.per_kind_valid, [32; 4]);
    assert!(report.per_op_nodes.iter().all(|count| *count > 0));
    assert_eq!(report.lean_values + report.lean_overflows, 128);
    assert!(report.lean_values > 0 && report.lean_overflows > 0);
    assert_eq!(report.diagnostic_counts, [3, 2, 5, 2, 1, 1, 1]);
    assert_eq!(report.first_failed_case, None);
    assert!(report.elapsed_ms > 0);
    assert!(report.tools.lean_version.contains("4.24.0"));
    assert_eq!(report.tools.os, std::env::consts::OS);
    assert_eq!(report.tools.arch, std::env::consts::ARCH);
    assert_eq!(report.worker_blake3, hash_file(binary));
    assert_eq!(
        report.model_blake3,
        *blake3::hash(include_bytes!("../lean/RewriteWitness.lean")).as_bytes()
    );
    assert_eq!(
        report.driver_blake3,
        *blake3::hash(include_bytes!("../lean/ArithmeticOracle.lean")).as_bytes()
    );
    assert_eq!(report.shrink_calls.lean_requests, 0);
    assert_eq!(report.shrink_calls.lean_responses, 0);
    assert_eq!(report.shrink_calls.parser_calls, 0);
    assert_eq!(report.shrink_calls.checker_calls, 0);
    assert_eq!(report.shrink_calls.ast_optimizer_calls, 0);
    assert_eq!(report.shrink_calls.evaluator_calls, [0; 4]);
    assert_eq!(report.shrink_calls.jit_calls, [0; 4]);
}

fn kind_tag(kind: Kind) -> u8 {
    match kind {
        Kind::U8 => 0,
        Kind::U32 => 1,
        Kind::U64 => 2,
        Kind::I64 => 3,
    }
}
fn op_tag(op: Op) -> u8 {
    match op {
        Op::Add => 0,
        Op::Sub => 1,
        Op::Mul => 2,
    }
}
fn independent_fingerprint(case: &Case) -> [u8; 32] {
    fn code(bytes: &mut Vec<u8>, code: &[Node]) {
        bytes.extend((code.len() as u16).to_le_bytes());
        for node in code {
            match node {
                Node::Int(value) => {
                    bytes.push(0);
                    bytes.extend(value.to_le_bytes());
                }
                Node::Variable(scope, slot) => {
                    bytes.push(if *scope == Scope::Input { 1 } else { 2 });
                    bytes.extend(slot.to_le_bytes());
                }
                Node::Checked(op) => {
                    bytes.push(3);
                    bytes.push(op_tag(*op));
                }
                Node::Call(callee, arity) => {
                    bytes.push(4);
                    let (target, selector) = match callee {
                        Callee::Intrinsic(op) => (0, u16::from(op_tag(*op))),
                        Callee::Variable(Scope::Input, slot) => (1, *slot),
                        Callee::Variable(Scope::Local, slot) => (2, *slot),
                    };
                    bytes.push(target);
                    bytes.extend(selector.to_le_bytes());
                    bytes.push(*arity);
                }
                Node::Bool(value) => {
                    bytes.push(5);
                    bytes.push(u8::from(*value));
                }
            }
        }
    }
    let program = &case.program;
    let mut bytes = b"vesper.oracle42.program\0".to_vec();
    bytes.extend(1u16.to_le_bytes());
    bytes.push(kind_tag(program.kind));
    bytes.push(program.inputs.len() as u8);
    for input in &program.inputs {
        bytes.push(kind_tag(input.kind));
        bytes.extend(input.value.to_le_bytes());
    }
    bytes.push(u8::from(program.allow_fail));
    bytes.push(program.locals.len() as u8);
    for local in &program.locals {
        code(&mut bytes, local);
    }
    code(&mut bytes, &program.result);
    *blake3::hash(&bytes).as_bytes()
}
fn corpus_hash() -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"vesper.oracle42.corpus\0");
    hash.update(&1u16.to_le_bytes());
    hash.update(&128u64.to_le_bytes());
    hash.update(&SEED.to_le_bytes());
    let negatives = oracle::corpus::negative_cases(128).unwrap();
    assert_eq!(negatives.len(), 15);
    let mut unique = BTreeSet::new();
    for id in 0..143 {
        let case = if id < 128 {
            oracle::corpus::case_at(Suite::Ci128, SEED, id).unwrap()
        } else {
            negatives[(id - 128) as usize].clone()
        };
        assert_eq!(case.id, id);
        assert_eq!(
            case.intent,
            if id < 128 {
                Intent::Valid
            } else {
                Intent::StaticNegative
            }
        );
        let fingerprint = independent_fingerprint(&case);
        assert_eq!(
            oracle::corpus::semantic_fingerprint(&case).unwrap(),
            fingerprint
        );
        if id < 128 {
            assert!(unique.insert(fingerprint));
        }
        let mut renumbered = case.clone();
        renumbered.id += 1_000_000;
        assert_eq!(
            oracle::corpus::semantic_fingerprint(&renumbered).unwrap(),
            fingerprint,
            "case numbering is not program uniqueness"
        );
        hash.update(&id.to_le_bytes());
        hash.update(&[u8::from(id >= 128)]);
        hash.update(&fingerprint);
        let source = oracle::corpus::render_case(&case).unwrap();
        assert!(source.len() <= 16384);
        let parsed = vesper::parser::parse(&source);
        if id == 133 || id == 142 {
            assert_eq!(parsed.diagnostics.len(), 1);
            assert_eq!(parsed.diagnostics[0].code, "E0002");
        } else {
            assert!(
                parsed.diagnostics.is_empty(),
                "{source}\n{:?}",
                parsed.diagnostics
            );
            oracle::corpus::validate_rendered(&case, &source, &parsed.module).unwrap();
        }
    }
    assert_eq!(unique.len(), 128);
    // Independent source changes must not normalize to the intended DTO.
    let case = oracle::corpus::case_at(Suite::Ci128, SEED, 0).unwrap();
    let original = oracle::corpus::render_case(&case).unwrap();
    for changed in [
        original.replacen("42", "41", 1),
        format!("{original}\nfn unrelated()->u8{{0}}\n"),
    ] {
        assert_ne!(changed, original);
        let parsed = vesper::parser::parse(&changed);
        assert!(parsed.diagnostics.is_empty());
        assert!(oracle::corpus::validate_rendered(&case, &changed, &parsed.module).is_err());
    }
    assert!(oracle::corpus::case_at(Suite::Ci128, SEED, 128).is_err());
    assert!(oracle::corpus::case_at(Suite::Full100000, SEED, 100000).is_err());
    *hash.finalize().as_bytes()
}

fn inspect_artifacts(parent: &Path, report: &RunReport, allowance: usize) {
    assert_eq!(
        fs::read(parent.join("unrelated.txt")).unwrap(),
        b"keep original bytes"
    );
    let roots: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(roots.len(), 1, "one unique per-run artifact directory");
    let root = roots[0].canonicalize().unwrap();
    let mut pending = vec![root.clone()];
    let mut total = 0usize;
    let mut files = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            assert!(!entry.file_type().unwrap().is_symlink());
            if entry.path().is_dir() {
                pending.push(entry.path());
            } else {
                total += entry.metadata().unwrap().len() as usize;
                files += 1;
            }
        }
    }
    assert!(total <= allowance && files <= 18);
    assert!(root.join("report.json").is_file());
    let saved: RunReport =
        serde_json::from_slice(&fs::read(root.join("report.json")).unwrap()).unwrap();
    assert_eq!(saved.status, RunStatus::Complete);
    assert_eq!(saved.committed_valid, 128);
    assert!(report.artifacts.len() <= 16);
    for artifact in &report.artifacts {
        assert!(artifact.relative_path.is_ascii() && artifact.relative_path.len() <= 256);
        let relative = Path::new(&artifact.relative_path);
        assert!(
            relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        );
        assert!(
            !matches!(
                relative.file_name().and_then(|name| name.to_str()),
                Some("report.json" | "failure.json")
            ),
            "no self-referential JSON inventory hash"
        );
        let candidates = [root.join(relative), parent.join(relative)];
        let actual: Vec<_> = candidates
            .into_iter()
            .filter(|path| path.is_file())
            .collect();
        assert_eq!(actual.len(), 1, "unambiguous retained artifact path");
        let path = actual[0].canonicalize().unwrap();
        assert!(path.starts_with(&root));
        assert_eq!(fs::metadata(&path).unwrap().len(), artifact.bytes);
        assert_eq!(hash_file(&path), artifact.blake3);
    }
}

fn real_ci(binary: &Path, tool: &Toolchain) {
    let artifacts = Directory::new();
    fs::write(artifacts.0.join("unrelated.txt"), b"keep original bytes").unwrap();
    let actual = execute(
        Command::new(binary)
            .arg("--lean")
            .arg(&tool.lean)
            .arg("--artifacts")
            .arg(&artifacts.0),
        840,
    );
    actual.success();
    let first: RunReport = serde_json::from_slice(&actual.stdout)
        .unwrap_or_else(|error| panic!("{error}\n{}", actual.diagnostic()));
    report_counts(&first, 1, binary);
    assert_eq!(first.corpus_blake3, corpus_hash());
    inspect_artifacts(
        &artifacts.0,
        &first,
        HarnessLimits::default().artifact_bytes,
    );

    let second =
        oracle::run(&config(binary, tool, 2)).expect("real CI128 with two bounded workers");
    report_counts(&second, 2, binary);
    assert_eq!(first.corpus_blake3, second.corpus_blake3);
    assert_eq!(
        first.requests_blake3, second.requests_blake3,
        "actual Lean request wire hash is independent of Rust worker scheduling"
    );
    assert_eq!(
        first.responses_blake3, second.responses_blake3,
        "actual Lean response wire hash is independent of Rust worker scheduling"
    );
}

fn preflight_failures(binary: &Path, tool: &Toolchain) {
    let missing = Directory::new();
    for setting in 0..8 {
        let mut invalid = config(binary, tool, 1);
        invalid.lean = missing.0.join("no-lean");
        invalid.worker_exe = missing.0.join("no-worker");
        match setting {
            0 => invalid.jobs = 0,
            1 => invalid.limits.request_bytes = 65537,
            2 => invalid.limits.source_bytes = 16385,
            3 => invalid.limits.worker_cases = 129,
            4 => invalid.limits.lean_fuel = 0,
            5 => invalid.limits.stderr_bytes = 0,
            6 => invalid.limits.shrink_attempts = 65,
            7 => invalid.limits.startup_timeout_ms = 300001,
            _ => unreachable!(),
        }
        let failure = oracle::run(&invalid).expect_err("invalid limits before any tool spawn");
        assert_eq!(failure.error.kind, FailureKind::InvalidConfig);
        assert_eq!(failure.case_id, None);
        assert_eq!(failure.partial.committed_valid, 0);
        assert_eq!(failure.partial.lean_spawns, 0);
        assert_eq!(failure.partial.rust_worker_spawns, 0);
    }
    for arguments in [
        vec!["--unknown"],
        vec!["--suite", "129"],
        vec!["--suite", "128", "--suite", "128"],
        vec!["--jobs", "0"],
        vec!["--jobs", "5"],
        vec!["--seed", "invalid"],
        vec!["--shrink-attempts", "65"],
        vec!["--lean"],
    ] {
        let output = execute(Command::new(binary).args(arguments), 10);
        assert!(!output.status.success());
        let failure = output.json();
        let fields: BTreeSet<_> = failure
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            ["case_id", "error", "partial"].into_iter().collect()
        );
        assert_eq!(failure["error"]["kind"], "invalid_config");
        assert_eq!(failure["partial"]["status"], "failed");
        assert_eq!(failure["partial"]["committed_valid"], 0);
        assert_eq!(failure["partial"]["lean_spawns"], 0);
        assert_eq!(failure["partial"]["rust_worker_spawns"], 0);
    }
    for lean in [missing.0.join("no-lean"), binary.to_path_buf()] {
        let mut invalid = config(binary, tool, 1);
        invalid.lean = lean;
        let failure =
            oracle::run(&invalid).expect_err("actual missing/wrong Lean is a hard failure");
        assert!(
            matches!(
                failure.error.kind,
                FailureKind::Io | FailureKind::ToolVersion | FailureKind::Crash
            ),
            "{failure:?}"
        );
        assert_eq!(failure.partial.committed_valid, 0);
        assert_ne!(failure.partial.status, RunStatus::Complete);
        assert!(!failure.error.detail.is_empty());
    }
    let mut wrong_worker = config(binary, tool, 1);
    wrong_worker.worker_exe = missing.0.join("no-worker");
    let failure =
        oracle::run(&wrong_worker).expect_err("missing actual worker never skips backend work");
    assert_eq!(failure.partial.committed_valid, 0);
    assert_ne!(failure.partial.status, RunStatus::Complete);
    assert!(matches!(
        failure.error.kind,
        FailureKind::Io | FailureKind::ToolVersion
    ));

    // A real executable which cannot speak the worker protocol must not become
    // a skipped backend case. Its failed artifact write must also stay failed.
    let artifacts = Directory::new();
    fs::write(artifacts.0.join("unrelated.txt"), b"keep original bytes").unwrap();
    let mut wrong_protocol = config(binary, tool, 1);
    wrong_protocol.worker_exe = PathBuf::from(env!("CARGO_BIN_EXE_vesper"));
    wrong_protocol.artifacts = Some(artifacts.0.clone());
    wrong_protocol.limits.artifact_bytes = 1;
    let failure =
        oracle::run(&wrong_protocol).expect_err("foreign worker and refused artifact cannot pass");
    assert_eq!(failure.error.kind, FailureKind::Artifact);
    assert_eq!(failure.partial.committed_valid, 0);
    assert_ne!(failure.partial.status, RunStatus::Complete);
    assert_eq!(
        fs::read(artifacts.0.join("unrelated.txt")).unwrap(),
        b"keep original bytes"
    );
}

fn independent_wire(program: &Program, id: u64, fuel: u16) -> Vec<u8> {
    fn code(nodes: &[Node]) -> Value {
        Value::Array(
            nodes
                .iter()
                .map(|node| match node {
                    Node::Int(value) => json!([0, value.to_string()]),
                    Node::Bool(value) => json!([5, u8::from(*value)]),
                    Node::Variable(scope, slot) => {
                        json!([if *scope == Scope::Input { 1 } else { 2 }, slot])
                    }
                    Node::Checked(op) => json!([3, op_tag(*op)]),
                    Node::Call(callee, arity) => {
                        let (target, selector) = match callee {
                            Callee::Intrinsic(op) => (0, u16::from(op_tag(*op))),
                            Callee::Variable(Scope::Input, slot) => (1, *slot),
                            Callee::Variable(Scope::Local, slot) => (2, *slot),
                        };
                        json!([4, target, selector, arity])
                    }
                })
                .collect(),
        )
    }
    serde_json::to_vec(&json!([
        1,
        id,
        kind_tag(program.kind),
        fuel,
        u8::from(program.allow_fail),
        program
            .inputs
            .iter()
            .map(|input| json!([kind_tag(input.kind), input.value.to_string()]))
            .collect::<Vec<_>>(),
        program
            .locals
            .iter()
            .map(|local| code(local))
            .collect::<Vec<_>>(),
        code(&program.result)
    ]))
    .unwrap()
}

enum FrameEvent {
    Frame(Vec<u8>),
    Eof,
    Error(String),
}
enum InputEvent {
    Bytes(Vec<u8>, bool),
}
fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0; 4];
    loop {
        match input.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    input.read_exact(&mut header[1..])?;
    let length = u32::from_le_bytes(header) as usize;
    if length > 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "actual Lean response exceeded512bytes",
        ));
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

struct StaticDriver {
    directory: Directory,
}
impl StaticDriver {
    fn compile(tool: &Toolchain) -> Self {
        let directory = Directory::new();
        fs::write(
            directory.0.join("RewriteWitness.lean"),
            include_bytes!("../lean/RewriteWitness.lean"),
        )
        .unwrap();
        fs::write(
            directory.0.join("ArithmeticOracle.lean"),
            include_bytes!("../lean/ArithmeticOracle.lean"),
        )
        .unwrap();
        for module in ["RewriteWitness", "ArithmeticOracle"] {
            execute(
                tool.command(&directory.0)
                    .args(["-j1", "-M4096", "-o"])
                    .arg(format!("{module}.olean"))
                    .arg(format!("{module}.lean")),
                300,
            )
            .success();
        }
        Self { directory }
    }
    fn session(&self, tool: &Toolchain) -> LeanSession {
        let mut command = tool.command(&self.directory.0);
        command.args(["-j1", "-M4096", "--run", "ArithmeticOracle.lean"]);
        let mut session = LeanSession::spawn(command);
        assert_eq!(
            session.recv_json(300),
            json!([
                1,
                "ready",
                1,
                READY,
                "vesper.arith.checked/1",
                63,
                8,
                3,
                3,
                4096
            ])
        );
        session
    }
}

#[derive(Default)]
struct Counters {
    seen: u64,
    checked: u64,
    diagnostics: u64,
    invalid: u64,
    values: u64,
    overflows: u64,
    fuel: u64,
}
impl Counters {
    fn bye(&self) -> Value {
        json!([
            1,
            "bye",
            self.seen,
            self.checked,
            self.diagnostics,
            self.invalid,
            self.values,
            self.overflows,
            self.fuel
        ])
    }
    fn observe(&mut self, response: &Value) {
        self.seen += 1;
        match response[1].as_str().unwrap() {
            "value" => {
                self.checked += 1;
                self.values += 1;
            }
            "overflow" => {
                self.checked += 1;
                self.overflows += 1;
            }
            "fuel_exhausted" => {
                self.checked += 1;
                self.fuel += 1;
            }
            "diagnostic" => self.diagnostics += 1,
            "invalid_request" => self.invalid += 1,
            _ => panic!("unexpected actual response {response}"),
        }
    }
}

/// Independent bounded client used only for direct real static-driver probes.
/// Production cleanup/failure handling is separately exercised by portable_faults.
struct LeanSession {
    child: Child,
    incoming: Receiver<FrameEvent>,
    outgoing: Option<SyncSender<InputEvent>>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
    fault: Arc<AtomicBool>,
    stderr_bytes: Arc<Mutex<Vec<u8>>>,
    next: u64,
    counts: Counters,
}
impl LeanSession {
    fn spawn(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("actual Lean driver {command:?}: {error}"));
        let mut output = child.stdout.take().unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let fault = Arc::new(AtomicBool::new(false));
        let (frames, incoming) = mpsc::sync_channel(2);
        let reader_fault = fault.clone();
        let reader = thread::spawn(move || {
            loop {
                let (event, stop) = match read_frame(&mut output) {
                    Ok(Some(bytes)) => (FrameEvent::Frame(bytes), false),
                    Ok(None) => (FrameEvent::Eof, true),
                    Err(error) => (FrameEvent::Error(error.to_string()), true),
                };
                if frames.try_send(event).is_err() {
                    reader_fault.store(true, Ordering::Relaxed);
                    break;
                }
                if stop {
                    break;
                }
            }
        });
        let (outgoing, writes) = mpsc::sync_channel(1);
        let writer_fault = fault.clone();
        let writer = thread::spawn(move || {
            while let Ok(InputEvent::Bytes(bytes, fragmented)) = writes.recv() {
                let result = if fragmented {
                    bytes
                        .iter()
                        .try_for_each(|byte| input.write_all(&[*byte]).and_then(|()| input.flush()))
                } else {
                    input.write_all(&bytes).and_then(|()| input.flush())
                };
                if result.is_err() {
                    writer_fault.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });
        let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
        let captured = stderr_bytes.clone();
        let stderr_fault = fault.clone();
        let stderr = thread::spawn(move || {
            let mut buffer = [0; 4096];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut bytes = captured.lock().unwrap();
                        let room = CAPTURE.saturating_sub(bytes.len());
                        bytes.extend_from_slice(&buffer[..count.min(room)]);
                        if count > room {
                            stderr_fault.store(true, Ordering::Relaxed);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        stderr_fault.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        });
        Self {
            child,
            incoming,
            outgoing: Some(outgoing),
            reader: Some(reader),
            writer: Some(writer),
            stderr: Some(stderr),
            fault,
            stderr_bytes,
            next: 0,
            counts: Counters::default(),
        }
    }
    fn recv(&self, seconds: u64) -> FrameEvent {
        assert!(
            !self.fault.load(Ordering::Relaxed),
            "actual driver IO/queue cap fault"
        );
        self.incoming
            .recv_timeout(Duration::from_secs(seconds))
            .unwrap_or_else(|error| {
                panic!(
                    "actual driver response/EOF deadline: {error}; stderr={}",
                    String::from_utf8_lossy(&self.stderr_bytes.lock().unwrap())
                )
            })
    }
    fn recv_json(&mut self, seconds: u64) -> Value {
        match self.recv(seconds) {
            FrameEvent::Frame(bytes) => {
                assert!(bytes.is_ascii());
                serde_json::from_slice(&bytes).expect("bounded actual driver JSON")
            }
            FrameEvent::Eof => panic!("unexpected actual driver EOF"),
            FrameEvent::Error(error) => panic!("actual driver frame error: {error}"),
        }
    }
    fn raw(&mut self, bytes: Vec<u8>, fragmented: bool) {
        assert!(bytes.len() <= 65540);
        self.outgoing
            .as_ref()
            .unwrap()
            .try_send(InputEvent::Bytes(bytes, fragmented))
            .unwrap_or_else(|_| panic!("bounded writer has unexpected queued request"));
    }
    fn payload(&mut self, payload: Vec<u8>, fragmented: bool) -> Value {
        assert!(payload.len() <= 65536);
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend(payload);
        self.raw(frame, fragmented);
        let response = self.recv_json(5);
        assert_eq!(response[0], 1);
        assert_eq!(response[2], self.next);
        self.next += 1;
        self.counts.observe(&response);
        response
    }
    fn program(&mut self, program: &Program, fuel: u16, fragmented: bool) -> Value {
        let independent = independent_wire(program, self.next, fuel);
        let encoded = oracle::protocol::encode_request(self.next, program, fuel, 16384).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded).unwrap(),
            serde_json::from_slice::<Value>(&independent).unwrap()
        );
        self.payload(independent, fragmented)
    }
    fn close_input(&mut self) {
        // Disconnection drains the one queued frame before the writer closes
        // stdin, including intentionally fragmented/truncated fatal probes.
        self.outgoing.take();
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "actual driver shutdown deadline");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn join(&mut self) {
        for thread in [&mut self.reader, &mut self.writer, &mut self.stderr] {
            thread
                .take()
                .unwrap()
                .join()
                .expect("actual driver pipe thread joined");
        }
        assert!(
            !self.fault.load(Ordering::Relaxed),
            "actual driver pipe/queue/capture fault"
        );
    }
    fn finish(mut self) {
        self.close_input();
        let expected = self.counts.bye();
        assert_eq!(self.recv_json(5), expected, "actual terminal counters");
        assert!(
            matches!(self.recv(5), FrameEvent::Eof),
            "no extra frame after BYE"
        );
        assert!(self.wait().success());
        self.join();
    }
    fn fatal(mut self, category: &str) {
        self.close_input();
        assert_eq!(self.recv_json(5), json!([1, "fatal", category]));
        assert!(
            matches!(self.recv(5), FrameEvent::Eof),
            "fatal response must not be followed by normal BYE"
        );
        assert!(
            !self.wait().success(),
            "actual fatal protocol exit is nonzero"
        );
        self.join();
    }
}
impl Drop for LeanSession {
    fn drop(&mut self) {
        self.outgoing.take();
        let _ = self.child.kill();
        let _ = self.child.wait(); // Always kill/reap before joining blocked pipe threads.
        for thread in [&mut self.reader, &mut self.writer, &mut self.stderr] {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
}

fn tiny(kind: Kind, value: i128) -> Program {
    Program {
        kind,
        inputs: vec![],
        locals: vec![],
        result: vec![Node::Int(value)],
        allow_fail: true,
    }
}
fn diagnostic_code(class: DiagnosticClass) -> &'static str {
    match class {
        DiagnosticClass::UnknownVariable => "E0201",
        DiagnosticClass::TypeMismatch => "E0200",
        DiagnosticClass::LiteralRange => "E0209",
        DiagnosticClass::LiteralParseRange => "E0002",
        DiagnosticClass::MissingFail => "E0401",
        DiagnosticClass::CallArity => "E0207",
        DiagnosticClass::NotCallable => "E0206",
    }
}
fn compare_real_backends(case: &Case, response: &Value) {
    // The expected outcome is read from ACTUAL Lean, never Rust arithmetic.
    let bytes = serde_json::to_vec(response).unwrap();
    let answer = oracle::protocol::decode_answer(response[2].as_u64().unwrap(), &bytes).unwrap();
    let rust =
        oracle::worker::execute_case(case, 100000).expect("real compiler/evaluator/JIT execution");
    match (answer, rust) {
        (OracleAnswer::Outcome { outcome, .. }, RustAnswer::Outcomes { evaluator, jit }) => {
            assert_eq!(*evaluator, [outcome; 4]);
            assert_eq!(*jit, [outcome; 4]);
        }
        (OracleAnswer::Diagnostic { class, .. }, RustAnswer::Diagnostic { phase, codes }) => {
            assert_eq!(
                phase,
                if class == DiagnosticClass::LiteralParseRange {
                    DiagnosticPhase::Parse
                } else {
                    DiagnosticPhase::Check
                }
            );
            assert_eq!(codes, [diagnostic_code(class)]);
        }
        other => panic!("actual Rust/Lean classification mismatch: {other:?}"),
    }
}

fn direct_semantics(driver: &StaticDriver, tool: &Toolchain) {
    let mut session = driver.session(tool);
    // A valid tiny literal permits checking the transport without a Rust oracle.
    let literal = tiny(Kind::U8, 42);
    assert_eq!(
        session.program(&literal, 128, true),
        json!([1, "value", 0, "42", 1])
    );
    let classes = [
        DiagnosticClass::UnknownVariable,
        DiagnosticClass::TypeMismatch,
        DiagnosticClass::LiteralRange,
        DiagnosticClass::LiteralRange,
        DiagnosticClass::LiteralRange,
        DiagnosticClass::LiteralParseRange,
        DiagnosticClass::LiteralRange,
        DiagnosticClass::LiteralRange,
        DiagnosticClass::MissingFail,
        DiagnosticClass::CallArity,
        DiagnosticClass::TypeMismatch,
        DiagnosticClass::NotCallable,
        DiagnosticClass::UnknownVariable,
        DiagnosticClass::UnknownVariable,
        DiagnosticClass::LiteralParseRange,
    ];
    for (case, class) in oracle::corpus::negative_cases(700)
        .unwrap()
        .iter()
        .zip(classes)
    {
        let wire_id = session.next;
        assert_ne!(
            wire_id, case.id,
            "wire sequence is separate from original case identity"
        );
        let response = session.program(&case.program, 128, false);
        assert_eq!(response[1], "diagnostic");
        assert_eq!(response[3], diagnostic_code(class));
        let decoded =
            oracle::protocol::decode_answer(wire_id, &serde_json::to_vec(&response).unwrap())
                .unwrap();
        let OracleAnswer::Diagnostic {
            class: actual,
            blame,
        } = decoded
        else {
            panic!("static verdict")
        };
        assert_eq!(actual, class);
        assert!(blame.scope <= 2);
        compare_real_backends(case, &response);
    }
    // Strict unused local overflow survives all eight execution settings.
    let case = oracle::corpus::case_at(Suite::Ci128, SEED, 14 * 4).unwrap();
    let response = session.program(&case.program, 128, false);
    assert_eq!(response[1], "overflow");
    compare_real_backends(&case, &response);

    // Static checking sees the result error despite an earlier strict overflow.
    let mut late_error = case.clone();
    late_error.id = 900;
    late_error.intent = Intent::StaticNegative;
    late_error.program.result = vec![Node::Variable(Scope::Local, 9)];
    let response = session.program(&late_error.program, 128, false);
    assert_eq!(response[1], "diagnostic");
    assert_eq!(response[3], "E0201");
    compare_real_backends(&late_error, &response);

    // A differently typed unused input is admitted; it is never a constant fact.
    let mut unused = tiny(Kind::U8, 42);
    unused.inputs.push(Input {
        kind: Kind::U32,
        value: 4294967295,
    });
    let case = Case {
        id: 901,
        program: unused,
        intent: Intent::Valid,
    };
    let response = session.program(&case.program, 128, false);
    assert_eq!(response[1], "value");
    compare_real_backends(&case, &response);

    let mut unknown_value = tiny(Kind::U8, 0);
    unknown_value.inputs.push(Input {
        kind: Kind::U8,
        value: 0,
    });
    unknown_value.result = vec![
        Node::Variable(Scope::Input, 0),
        Node::Int(0),
        Node::Checked(Op::Add),
    ];
    unknown_value.allow_fail = false;
    let case = Case {
        id: 902,
        program: unknown_value,
        intent: Intent::StaticNegative,
    };
    let response = session.program(&case.program, 128, false);
    assert_eq!(response[1], "diagnostic");
    assert_eq!(response[3], "E0401");
    compare_real_backends(&case, &response);

    // Resolution order is a direct reference-profile probe; unknown callees
    // are intentionally outside the compiler's isolated single-code matrix.
    let mut unknown_callee = tiny(Kind::U8, 0);
    unknown_callee.result = vec![
        Node::Int(1),
        Node::Int(2),
        Node::Call(Callee::Variable(Scope::Input, 9), 2),
    ];
    let response = session.program(&unknown_callee, 128, false);
    assert_eq!(response[1], "diagnostic");
    assert_eq!(response[3], "E0201");

    let copies = Program {
        kind: Kind::U8,
        inputs: vec![],
        locals: vec![vec![Node::Int(1)], vec![Node::Variable(Scope::Local, 0)]],
        result: vec![Node::Variable(Scope::Local, 1)],
        allow_fail: true,
    };
    let id = session.next;
    assert_eq!(
        session.program(&copies, 0, false),
        json!([1, "fuel_exhausted", id, 0])
    );
    let id = session.next;
    assert_eq!(
        session.program(&copies, 2, false),
        json!([1, "fuel_exhausted", id, 2])
    );
    let id = session.next;
    assert_eq!(
        session.program(&copies, 3, false),
        json!([1, "value", id, "1", 3]),
        "local reads copy values rather than re-evaluating initializers"
    );
    session.finish();
}

fn invalid_payloads(driver: &StaticDriver, tool: &Toolchain) {
    let mut session = driver.session(tool);
    for which in 0..15 {
        let mut request: Value =
            serde_json::from_slice(&independent_wire(&tiny(Kind::U8, 42), session.next, 128))
                .unwrap();
        let (bytes, categories): (Vec<u8>, &[&str]) = match which {
            0 => {
                request[0] = json!(2);
                (serde_json::to_vec(&request).unwrap(), &["schema"])
            }
            1 => {
                request[2] = json!(4);
                (serde_json::to_vec(&request).unwrap(), &["kind_tag"])
            }
            2 => {
                request[3] = json!(4097);
                (serde_json::to_vec(&request).unwrap(), &["profile_limit"])
            }
            3 => {
                request[5] = json!([[0, "256"]]);
                (serde_json::to_vec(&request).unwrap(), &["input_range"])
            }
            4 => {
                request[7] = json!([[0, "1"], [0, "2"], [3, 3]]);
                (serde_json::to_vec(&request).unwrap(), &["op_tag"])
            }
            5 => {
                request[7] = json!([]);
                (serde_json::to_vec(&request).unwrap(), &["profile_limit"])
            }
            6 => {
                request[7] = json!([[0, "-0"]]);
                (serde_json::to_vec(&request).unwrap(), &["decimal"])
            }
            7 => {
                request[7] = json!([[0, "170141183460469231731687303715884105728"]]);
                (serde_json::to_vec(&request).unwrap(), &["decimal"])
            }
            8 => (b"{}".to_vec(), &["lexical"]),
            9 => ("[\"é\"]".as_bytes().to_vec(), &["lexical"]),
            10 => (br#"["\u0030"]"#.to_vec(), &["lexical"]),
            11 => {
                request[3] = json!(1.5);
                (serde_json::to_vec(&request).unwrap(), &["lexical"])
            }
            12 => (b"[[[[[]]]]]".to_vec(), &["lexical"]),
            13 => (
                format!("[{}]", vec!["0"; 1025].join(",")).into_bytes(),
                &["lexical"],
            ),
            // A lexical prepass may reject an unbalanced array before Json.parse;
            // either route must report invalid input, consume its ID, and stay live.
            14 => (b"[".to_vec(), &["lexical", "json"]),
            _ => unreachable!(),
        };
        let id = session.next;
        let response = session.payload(bytes, false);
        assert_eq!(response[1], "invalid_request", "{response}");
        assert_eq!(response[2], id);
        assert!(
            categories.contains(&response[3].as_str().unwrap()),
            "{which}: {response}"
        );
    }
    let id = session.next;
    assert_eq!(
        session.program(&tiny(Kind::U8, 42), 128, false),
        json!([1, "value", id, "42", 1]),
        "complete invalid frames consume sequence IDs without poisoning a healthy oracle"
    );
    session.finish();
}

fn fatal_frames(driver: &StaticDriver, tool: &Toolchain) {
    driver.session(tool).finish(); // Only EOF at a header boundary produces BYE.
    for (bytes, category) in [
        (65537u32.to_le_bytes().to_vec(), "frame_limit"),
        (vec![5, 0], "truncated_header"),
        (
            {
                let mut bytes = 5u32.to_le_bytes().to_vec();
                bytes.push(b'[');
                bytes
            },
            "truncated_payload",
        ),
    ] {
        let mut session = driver.session(tool);
        session.raw(bytes, true);
        session.fatal(category);
    }
    let mut wrong_id = driver.session(tool);
    let payload = independent_wire(&tiny(Kind::U8, 42), 9, 128);
    let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
    bytes.extend(payload);
    wrong_id.raw(bytes, false);
    wrong_id.fatal("sequence");
    let mut duplicate = driver.session(tool);
    duplicate.program(&tiny(Kind::U8, 42), 128, false);
    let payload = independent_wire(&tiny(Kind::U8, 42), 0, 128);
    let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
    bytes.extend(payload);
    duplicate.raw(bytes, false);
    duplicate.fatal("sequence");
}

fn codec_refusals() {
    let good = br#"[1,"value",0,"0",1]"#;
    assert!(matches!(
        oracle::protocol::decode_answer(0, good).unwrap(),
        OracleAnswer::Outcome {
            outcome: Outcome::Value(0),
            steps: 1
        }
    ));
    for bytes in [
        &br#"[1,"value",1,"0",1]"#[..],
        &br#"[2,"value",0,"0",1]"#[..],
        &br#"[1,"value",0,"00",1]"#[..],
        &br#"[1,"value",0,"-0",1]"#[..],
        &br#"[1,"value",0,"0",65536]"#[..],
        &br#"[1,"value",0,"0",1,0]"#[..],
        &br#"[1,"diagnostic",0,"E0209","unknown_variable",[2,0,0]]"#[..],
        &br#"[1,"bye",0,0,0,0,0,0,0]"#[..],
        &b"["[..],
    ] {
        assert!(oracle::protocol::decode_answer(0, bytes).is_err());
    }
    assert!(oracle::protocol::decode_answer(0, &vec![b' '; 513]).is_err());
    let program = tiny(Kind::U8, 42);
    assert!(oracle::protocol::encode_request(0, &program, 128, 1).is_err());

    // Exact i128 strings survive private worker serde, including parser-negative
    // literals beyond u64. This is transport testing, not an arithmetic oracle.
    for value in [
        i128::MIN,
        -18446744073709551616,
        0,
        18446744073709551616,
        i128::MAX,
    ] {
        let node = Node::Int(value);
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json, json!({"int":value.to_string()}));
        assert_eq!(serde_json::from_value::<Node>(json).unwrap(), node);
        let input = Input {
            kind: Kind::U64,
            value,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["value"], value.to_string());
        assert_eq!(serde_json::from_value::<Input>(json).unwrap(), input);
    }
    for bad in [
        json!(0),
        json!("00"),
        json!("-0"),
        json!("1e3"),
        json!("1.0"),
        json!("170141183460469231731687303715884105728"),
    ] {
        assert!(serde_json::from_value::<Node>(json!({"int":bad})).is_err());
    }
}

#[test]
fn real_lean_oracle_all_modes_and_bounded_hostile_processes() {
    let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let started = Instant::now();
    let binary = actual_binary();
    let tool = Toolchain::resolve(); // Mandatory, including when VESPER_LEAN is unset.
    codec_refusals();
    portable_faults(&binary);
    preflight_failures(&binary, &tool);
    real_ci(&binary, &tool);
    let driver = StaticDriver::compile(&tool);
    direct_semantics(&driver, &tool);
    invalid_payloads(&driver, &tool);
    fatal_frames(&driver, &tool);
    eprintln!(
        "oracle42 complete serial acceptance wall time {:.2}s",
        started.elapsed().as_secs_f64()
    );
}
