//! Replay distinct private kernel names that share one user-facing spelling.
//! Fixed sources and actual artifacts are inspected; metadata is not approval.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const MODULES: [(&str, &str); 3] = [
    (
        "PrivateReplayLeft",
        include_str!("../tools/proof-audit/fixtures/private-replay/PrivateReplayLeft.lean"),
    ),
    (
        "PrivateReplayRight",
        include_str!("../tools/proof-audit/fixtures/private-replay/PrivateReplayRight.lean"),
    ),
    (
        "PrivateReplayBoth",
        include_str!("../tools/proof-audit/fixtures/private-replay/PrivateReplayBoth.lean"),
    ),
];
const METADATA: &str =
    include_str!("../tools/proof-audit/fixtures/private-replay/PrivateReplayMetadata.lean");
const ROOTS: [&str; 2] = [
    "ProofAuditPrivateReplay.left",
    "ProofAuditPrivateReplay.right",
];
const MARKER: &str = "VESPER_PRIVATE_REPLAY_METADATA_V1:";
const CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const FIXTURE_BYTES: usize = 4 * 1024 * 1024;
const TOTAL_SECONDS: u64 = 600;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-private-replay-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl Captured {
    fn success(&self) {
        assert!(
            self.status.success(),
            "real process failed: {}\n{}\n{}",
            self.status,
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        );
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "actual audit JSON: {error}\n{}\n{}",
                String::from_utf8_lossy(&self.stdout),
                String::from_utf8_lossy(&self.stderr)
            )
        })
    }
}

fn bounded_read(path: &Path, cap: usize) -> Vec<u8> {
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_file());
    assert!(fs::metadata(path).unwrap().len() <= cap as u64);
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= cap, "bounded file: {}", path.display());
    bytes
}

fn execute(command: &mut Command, timeout: Duration, started: Instant) -> Captured {
    let remaining = Duration::from_secs(TOTAL_SECONDS)
        .checked_sub(started.elapsed())
        .expect("private replay regression exceeded its whole-test deadline");
    let timeout = timeout.min(remaining);
    let output = Directory::new();
    let stdout = output.0.join("stdout");
    let stderr = output.0.join("stderr");
    let file = |path| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap()
    };
    // Regular files avoid reader-thread joins on inherited pipes. The bounded
    // test driver reaps its direct child; it makes no descendant-tree claim.
    let mut child = command
        .stdout(Stdio::from(file(&stdout)))
        .stderr(Stdio::from(file(&stderr)))
        .spawn()
        .unwrap_or_else(|error| panic!("required real process {command:?}: {error}"));
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let phase = Instant::now();
    let mut refused = false;
    let status = loop {
        let over = [&stdout, &stderr]
            .into_iter()
            .any(|path| fs::metadata(path).unwrap().len() > CAPTURE_BYTES as u64);
        if over || phase.elapsed() > timeout {
            refused = true;
            let _ = child.kill();
            break child.wait().expect("reap refused direct child");
        }
        if let Some(status) = child.try_wait().expect("poll real child") {
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        !refused,
        "real process exceeded time/output bound: {command:?}"
    );
    let captured = Captured {
        status,
        stdout: bounded_read(&stdout, CAPTURE_BYTES),
        stderr: bounded_read(&stderr, CAPTURE_BYTES),
    };
    assert!(
        !String::from_utf8_lossy(&captured.stdout).contains("PANIC")
            && !String::from_utf8_lossy(&captured.stderr).contains("PANIC"),
        "Lean panic cannot approve a replay"
    );
    eprintln!(
        "private replay process {:.2}s: {command:?}",
        phase.elapsed().as_secs_f64()
    );
    captured
}

fn pinned_lean(started: Instant) -> (PathBuf, String) {
    let launcher = std::env::var_os("VESPER_LEAN").unwrap_or_else(|| "lean".into());
    let prefix = execute(
        Command::new(launcher)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("--print-prefix"),
        Duration::from_secs(30),
        started,
    );
    prefix.success();
    let prefix = String::from_utf8(prefix.stdout).unwrap();
    let lean = PathBuf::from(prefix.trim())
        .join("bin")
        .join(format!("lean{}", std::env::consts::EXE_SUFFIX))
        .canonicalize()
        .expect("actual pinned Lean binary");
    let version = execute(
        Command::new(&lean).arg("--version"),
        Duration::from_secs(30),
        started,
    );
    version.success();
    let version = String::from_utf8(version.stdout).unwrap().trim().to_owned();
    assert!(
        version.contains("version 4.24.0,"),
        "requires Lean 4.24.0: {version}"
    );
    (lean, version)
}

fn copy_source(directory: &Path, name: &str, source: &str) {
    assert!(source.len() <= FIXTURE_BYTES);
    let path = directory.join(format!("{name}.lean"));
    fs::write(&path, source).unwrap();
    assert_eq!(bounded_read(&path, FIXTURE_BYTES), source.as_bytes());
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct FileDigest {
    path: String,
    bytes: u64,
    blake3: String,
}

fn artifacts(root: &Path) -> Vec<FileDigest> {
    let mut records = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        let path = entry.path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if [".olean", ".olean.private", ".olean.server", ".ir"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            assert!(
                MODULES
                    .iter()
                    .any(|(module, _)| name.starts_with(&format!("{module}.")))
            );
            let bytes = bounded_read(&path, FIXTURE_BYTES);
            records.push(FileDigest {
                path: name.to_owned(),
                bytes: bytes.len() as u64,
                blake3: blake3::hash(&bytes).to_hex().to_string(),
            });
        }
    }
    records.sort_by(|a, b| a.path.cmp(&b.path));
    for (module, _) in MODULES {
        assert!(
            records
                .iter()
                .any(|record| record.path == format!("{module}.olean"))
        );
    }
    records
}

fn inspect_private_names(metadata: &Value) {
    assert_eq!(metadata["schema_version"], 1);
    assert_eq!(metadata["declaration_count"], 4);
    let modules = metadata["modules"].as_array().unwrap();
    let actual = modules
        .iter()
        .map(|module| {
            (
                module["name"].as_str().unwrap(),
                module["declaration_count"].as_u64().unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual.len(), modules.len());
    assert_eq!(
        actual,
        BTreeMap::from([
            ("PrivateReplayBoth", 0),
            ("PrivateReplayLeft", 2),
            ("PrivateReplayRight", 2)
        ])
    );
    let private = metadata["private_declarations"].as_array().unwrap();
    assert_eq!(private.len(), 2);
    let mut names = BTreeSet::new();
    for declaration in private {
        assert_eq!(declaration["user_name"], "ProofAuditPrivateReplay.shared");
        assert_eq!(declaration["unsafe"], false);
        assert_eq!(declaration["partial"], false);
        assert!(names.insert(declaration["full_name"].as_str().unwrap()));
    }
    let public = metadata["public_roots"].as_array().unwrap();
    assert_eq!(public.len(), ROOTS.len());
    let mut roots = BTreeSet::new();
    let mut dependencies = BTreeSet::new();
    for declaration in public {
        let name = declaration["name"].as_str().unwrap();
        let dependency = declaration["private_dependency"].as_str().unwrap();
        assert!(roots.insert(name));
        assert!(dependencies.insert(dependency));
        assert!(names.contains(dependency));
        let owner = match name {
            "ProofAuditPrivateReplay.left" => "PrivateReplayLeft",
            "ProofAuditPrivateReplay.right" => "PrivateReplayRight",
            _ => panic!("unexpected public root: {name}"),
        };
        assert!(dependency.split('.').any(|part| part == owner));
    }
    assert_eq!(roots, BTreeSet::from(ROOTS));
    assert_eq!(dependencies, names);
}

#[test]
fn distinct_private_kernel_names_survive_real_replay_together() {
    let started = Instant::now();
    let (lean, version) = pinned_lean(started);
    let directory = Directory::new();
    let root = directory.0.join("artifacts");
    let probe = directory.0.join("probe");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&probe).unwrap();
    for (module, source) in MODULES {
        copy_source(&root, module, source);
        let output = execute(
            Command::new(&lean)
                .current_dir(&root)
                .env("LEAN_PATH", &root)
                .env("LEAN_NUM_THREADS", "1")
                .args(["-j1", "-M1024", "-o"])
                .arg(format!("{module}.olean"))
                .arg(format!("{module}.lean")),
            Duration::from_secs(60),
            started,
        );
        output.success();
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "fixed fixture compiled without diagnostics"
        );
    }
    copy_source(&probe, "PrivateReplayMetadata", METADATA);
    let output = execute(
        Command::new(&lean)
            .current_dir(&probe)
            .env("LEAN_PATH", &root)
            .env("LEAN_NUM_THREADS", "1")
            .args(["-j1", "-M1024", "--run", "PrivateReplayMetadata.lean"]),
        Duration::from_secs(60),
        started,
    );
    output.success();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let records = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(MARKER))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "one actual metadata record: {stdout}");
    inspect_private_names(&serde_json::from_str::<Value>(records[0]).unwrap());

    let before = artifacts(&root);
    let output = execute(
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&directory.0)
            .args(["proof-audit", "PrivateReplayBoth", "--root"])
            .arg(&root)
            .args(["--theorem", ROOTS[0], "--theorem", ROOTS[1], "--lean"])
            .arg(&lean)
            .args(["--timeout", "300", "--json"]),
        Duration::from_secs(330),
        started,
    );
    output.success();
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "audited");
    assert_eq!(report["module"], "PrivateReplayBoth");
    assert_eq!(report["accepted"], true);
    assert_eq!(report["kernel_replayed"], true);
    assert_eq!(report["cache_eligible"], false);
    assert_eq!(report["checked_constants"], 4);
    assert_eq!(report["allowed_axioms"], json!([]));
    assert_eq!(report["forbidden_axioms"], json!([]));
    assert_eq!(report["lean_version"], version);
    assert_eq!(
        report["lean_binary_blake3"],
        blake3::hash(&bounded_read(&lean, 64 * 1024 * 1024))
            .to_hex()
            .to_string()
    );
    assert_eq!(
        report["auditor_blake3"],
        blake3::hash(include_bytes!("../tools/proof-audit/Audit.lean"))
            .to_hex()
            .to_string(),
        "the real CLI must embed the current auditor source"
    );
    let theorems = report["theorems"].as_array().unwrap();
    assert_eq!(theorems.len(), ROOTS.len());
    let actual_roots = theorems
        .iter()
        .map(|entry| {
            assert_eq!(entry["axioms"], json!([]));
            entry["name"].as_str().unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_roots, BTreeSet::from(ROOTS));
    assert_eq!(
        report["artifact_files"],
        serde_json::to_value(&before).unwrap()
    );
    assert_eq!(
        report["artifact_inventory_blake3"],
        blake3::hash(&serde_json::to_vec(&before).unwrap())
            .to_hex()
            .to_string()
    );
    let imports = report["imports"].as_array().unwrap();
    let mut names = BTreeSet::new();
    let mut artifact_modules = BTreeSet::new();
    for module in imports {
        assert!(names.insert(module["name"].as_str().unwrap()));
        match module["scope"].as_str().unwrap() {
            "artifact" => {
                let name = module["name"].as_str().unwrap();
                assert!(artifact_modules.insert(name));
                let expected = before
                    .iter()
                    .filter(|file| file.path.starts_with(&format!("{name}.")))
                    .collect::<Vec<_>>();
                let files = module["files"].as_array().unwrap();
                assert_eq!(files.len(), expected.len());
                let mut paths = BTreeSet::new();
                for file in files {
                    let path = file["path"].as_str().unwrap();
                    assert!(paths.insert(path));
                    let expected = expected.iter().find(|entry| entry.path == path).unwrap();
                    assert_eq!(*file, serde_json::to_value(expected).unwrap());
                }
            }
            "toolchain" => {}
            other => panic!("unknown import scope: {other}"),
        }
    }
    assert_eq!(
        artifact_modules,
        BTreeSet::from(MODULES.map(|(module, _)| module))
    );
    assert!(imports.iter().any(|module| module["scope"] == "toolchain"));
    assert_eq!(
        before,
        artifacts(&root),
        "audit must not mutate compiled inputs"
    );
    for (module, source) in MODULES {
        assert_eq!(
            bounded_read(&root.join(format!("{module}.lean")), FIXTURE_BYTES),
            source.as_bytes(),
            "fresh exact source association"
        );
    }
    assert_eq!(
        bounded_read(&probe.join("PrivateReplayMetadata.lean"), FIXTURE_BYTES),
        METADATA.as_bytes()
    );
    eprintln!(
        "private replay regression completed in {:.2}s",
        started.elapsed().as_secs_f64()
    );
}
