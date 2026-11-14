//! Fresh fixed-source Lean compilation and actual kernel/axiom replay.
//! This finite logical model is not a source adapter or a backend observation.
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
use vesper_lean_syntax::{Declaration, Ident, Module, ModuleName, Name, Term};

const POLICY: [&str; 3] = ["Classical.choice", "Quot.sound", "propext"];
const MODULE: &str = "ResourceModel42.Acceptance";
const CAPTURE: usize = 8 * 1024 * 1024;
const SOURCE_CAP: usize = 4 * 1024 * 1024;
const SOURCE_COUNT: usize = 24;
const TOTAL_SECONDS: u64 = 900;
const SNAPSHOT_MARKER: &str = "VESPER_RESOURCE_KERNEL_SNAPSHOT_V1:";
const PROJECTED_MODULE: &str = "ResourceModel42Kernel";

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
            panic!("invalid actual audit JSON: {error}\n{}", self.diagnostic())
        })
    }
}

struct CaptureFiles {
    root: PathBuf,
}
impl CaptureFiles {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-resource-model42-output-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }
    fn file(&self, name: &str) -> fs::File {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join(name))
            .unwrap()
    }
    fn overflow(&self) -> bool {
        ["stdout", "stderr"]
            .into_iter()
            .any(|name| fs::metadata(self.root.join(name)).unwrap().len() > CAPTURE as u64)
    }
    fn read(&self, name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        fs::File::open(self.root.join(name))
            .unwrap()
            .take((CAPTURE + 1) as u64)
            .read_to_end(&mut output)
            .unwrap();
        output
    }
}
impl Drop for CaptureFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn windows_environment(command: &mut Command) {
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn execute(command: &mut Command, seconds: u64, target_start: Instant) -> Captured {
    let start = Instant::now();
    let remaining = Duration::from_secs(TOTAL_SECONDS)
        .checked_sub(target_start.elapsed())
        .expect("finite target deadline");
    let deadline = Duration::from_secs(seconds).min(remaining);
    // Regular capture files prevent a descendant holding an inherited pipe
    // from causing an unbounded reader-thread join after the direct child dies.
    // They live outside the artifact root, so the auditor never snapshots its
    // own changing stdout. This does not claim descendant-tree cancellation.
    let capture = CaptureFiles::new();
    let mut child = command
        .stdout(Stdio::from(capture.file("stdout")))
        .stderr(Stdio::from(capture.file("stderr")))
        .spawn()
        .unwrap_or_else(|error| panic!("required real process {command:?}: {error}"));
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut refused = false;
    let status = loop {
        if capture.overflow() || start.elapsed() > deadline {
            refused = true;
            let _ = child.kill();
            break child.wait().expect("reap refused real process");
        }
        if let Some(status) = child.try_wait().expect("poll real process") {
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = capture.read("stdout");
    let stderr = capture.read("stderr");
    let overflow = stdout.len() > CAPTURE || stderr.len() > CAPTURE;
    let captured = Captured {
        status,
        stdout,
        stderr,
    };
    assert!(
        !captured.diagnostic().contains("PANIC"),
        "actual Lean panic is never success: {}",
        captured.diagnostic()
    );
    assert!(
        !refused && !overflow,
        "real process exceeded finite time/output budget: {command:?}\n{}",
        captured.diagnostic()
    );
    eprintln!(
        "resource_model42 process {:.2}s: {command:?}",
        start.elapsed().as_secs_f64()
    );
    captured
}

fn module_path(module: &str, suffix: &str) -> PathBuf {
    assert!(module.split('.').all(valid_name));
    let mut path = PathBuf::new();
    let mut components = module.split('.').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            path.push(component);
        } else {
            path.push(format!("{component}.{suffix}"));
        }
    }
    path
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || byte == b'_'
                || (index > 0 && (byte.is_ascii_digit() || byte == b'\''))
        })
}

struct FixedSource {
    text: String,
    imports: Vec<String>,
}

fn read_source(path: &Path) -> FixedSource {
    let metadata = fs::metadata(path).unwrap();
    assert!(
        metadata.is_file(),
        "regular fixed source: {}",
        path.display()
    );
    assert!(metadata.len() <= SOURCE_CAP as u64, "bounded fixed source");
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take((SOURCE_CAP + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= SOURCE_CAP, "bounded fixed source");
    let text = String::from_utf8(bytes).unwrap();
    let imports = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("import "))
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect();
    FixedSource { text, imports }
}

fn fixed_sources() -> BTreeMap<String, FixedSource> {
    let lean = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean");
    let mut sources = BTreeMap::new();
    sources.insert(
        "MetatheoryFull".into(),
        read_source(&lean.join("MetatheoryFull.lean")),
    );
    sources.insert(
        "ResourceModel42".into(),
        read_source(&lean.join("ResourceModel42.lean")),
    );
    for entry in fs::read_dir(lean.join("ResourceModel42")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "lean")
        {
            let stem = path.file_stem().unwrap().to_str().unwrap();
            assert!(valid_name(stem));
            assert!(
                sources
                    .insert(format!("ResourceModel42.{stem}"), read_source(&path))
                    .is_none()
            );
        }
    }
    assert!(sources.len() <= SOURCE_COUNT);
    for required in ["Syntax", "Checker", "Evaluator", "Proofs", "Acceptance"] {
        assert!(
            sources.contains_key(&format!("ResourceModel42.{required}")),
            "missing fixed module {required}"
        );
    }
    sources
}

fn compile_order(sources: &BTreeMap<String, FixedSource>) -> Vec<String> {
    let mut imported = BTreeSet::new();
    let mut pending = vec![MODULE.to_owned()];
    while let Some(module) = pending.pop() {
        if imported.insert(module.clone()) {
            for dependency in &sources[&module].imports {
                if sources.contains_key(dependency) {
                    pending.push(dependency.clone());
                }
            }
        }
    }
    assert_eq!(
        imported,
        sources.keys().cloned().collect(),
        "every fixed model definition must enter the replayed artifact closure"
    );
    let mut remaining: BTreeSet<_> = sources.keys().cloned().collect();
    let mut done = BTreeSet::new();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .find(|module| {
                sources[*module].imports.iter().all(|dependency| {
                    if dependency == "Std"
                        || dependency == "Init"
                        || dependency.starts_with("Std.")
                        || dependency.starts_with("Init.")
                    {
                        true
                    } else {
                        assert!(
                            sources.contains_key(dependency),
                            "fixed dependency absent: {dependency}"
                        );
                        done.contains(dependency)
                    }
                })
            })
            .cloned()
            .expect("acyclic complete fixed module imports");
        remaining.remove(&ready);
        done.insert(ready.clone());
        order.push(ready);
    }
    order
}

// Strip nested block/line comments and strings before inventorying the fixed
// source's explicit commands. Unsupported layouts fail rather than omit roots.
fn commands(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let (mut index, mut depth, mut string) = (0, 0usize, false);
    while index < bytes.len() {
        if depth > 0 {
            if bytes[index..].starts_with(b"/-") {
                depth += 1;
                output.extend_from_slice(b"  ");
                index += 2;
            } else if bytes[index..].starts_with(b"-/") {
                depth -= 1;
                output.extend_from_slice(b"  ");
                index += 2;
            } else {
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
        } else if string {
            if bytes[index] == b'\\' && index + 1 < bytes.len() {
                output.extend_from_slice(b"  ");
                index += 2;
            } else {
                if bytes[index] == b'"' {
                    string = false;
                }
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/-") {
            depth = 1;
            output.extend_from_slice(b"  ");
            index += 2;
        } else if bytes[index..].starts_with(b"--") {
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(b' ');
                index += 1;
            }
        } else if bytes[index] == b'"' {
            string = true;
            output.push(b' ');
            index += 1;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    assert_eq!(depth, 0, "closed fixed-source comment");
    assert!(!string, "closed fixed-source string");
    String::from_utf8(output).unwrap()
}

fn theorem_catalog(sources: &BTreeMap<String, FixedSource>) -> Vec<String> {
    let mut roots = BTreeSet::new();
    for (module, source) in sources
        .iter()
        .filter(|(module, _)| module.starts_with("ResourceModel42"))
    {
        let mut scopes: Vec<Option<String>> = Vec::new();
        for line in commands(&source.text).lines() {
            let mut line = line.trim();
            while let Some(attribute) = line.strip_prefix("@[") {
                line = attribute
                    .split_once(']')
                    .expect("fixed complete inline attribute")
                    .1
                    .trim();
            }
            if let Some(namespace) = line.strip_prefix("namespace ") {
                let name = namespace.split_whitespace().next().unwrap();
                assert!(name.split('.').all(valid_name));
                scopes.push(Some(name.to_owned()));
            } else if line == "section"
                || line.starts_with("section ")
                || line == "noncomputable section"
                || line == "mutual"
            {
                scopes.push(None);
            } else if line == "end" || line.starts_with("end ") {
                scopes
                    .pop()
                    .expect("balanced fixed-source namespace/section");
            } else {
                let declaration = line.strip_prefix("protected ").unwrap_or(line);
                assert!(
                    !declaration.starts_with("private theorem ")
                        && !declaration.starts_with("local theorem "),
                    "all new named theorems public: {module}"
                );
                if let Some(theorem) = declaration.strip_prefix("theorem ") {
                    let name = theorem
                        .split(|c: char| c.is_whitespace() || matches!(c, '(' | '{' | ':' | '['))
                        .next()
                        .unwrap();
                    assert!(
                        name.split('.').all(valid_name),
                        "auditable ASCII theorem {name}"
                    );
                    let mut parts = scopes
                        .iter()
                        .filter_map(Option::as_deref)
                        .collect::<Vec<_>>();
                    parts.push(name);
                    let root = parts.join(".");
                    assert!(
                        root.starts_with("Vesper.ResourceModel42."),
                        "actual namespace: {root}"
                    );
                    assert!(roots.insert(root), "duplicate explicit theorem in {module}");
                } else {
                    assert!(
                        !declaration
                            .split_whitespace()
                            .any(|token| matches!(token, "theorem" | "lemma")),
                        "unsupported theorem command layout in {module}: {line}"
                    );
                }
            }
        }
        assert!(scopes.is_empty(), "closed scopes in {module}");
    }
    for required in [
        "TypeInfo.pair_ty",
        "TypeInfo.ofTy_ty",
        "CheckedProgram.check_ok",
        "CheckerProofs.checkedProgram_declared_sound",
        "EvaluatorProofs.step_sound",
        "EvaluatorProofs.runMachine_steps",
        "EvaluatorProofs.step_refusal_witness",
        "EvaluatorProofs.runMachine_refusal_witness",
        "EvaluatorProofs.step_event_site",
        "Proofs.invocation_initial_WT",
        "Proofs.evalSteps_prefix",
        "Proofs.evalSteps_completed_value",
        "Proofs.evalSteps_noInvariant",
        "Proofs.evalSteps_Full_progress",
        "Proofs.evalSteps_completed_copy_residual",
        "Proofs.evalSteps_completed_copy_exact_once",
        "Proofs.evalSteps_public_trace_exact_once",
        "MetricsProofs.preflightSubst_error",
        "MetricsProofs.preflightUnpack_error",
        "MetricsProofs.preflightPlug_error",
        "MetricsProofs.preflightInputs_error",
        "MetricsProofs.measureExpr_complete",
        "MetricsProofs.preflightSubst_complete",
        "MetricsProofs.preflightUnpack_complete",
        "MetricsProofs.preflightPlug_complete",
        "MetricsProofs.preflightInputs_complete",
        "Acceptance.mint_consume_takes_three_full_steps",
        "Acceptance.dynamic_growth_checks_before_substitution_or_ledger_commit",
    ] {
        assert!(
            roots.contains(&format!("Vesper.ResourceModel42.{required}")),
            "missing authoritative computation root {required}"
        );
    }
    roots.into_iter().collect()
}

struct Fixture {
    root: PathBuf,
    lean: PathBuf,
    vesper: PathBuf,
    prefix: PathBuf,
    started: Instant,
}

type ArtifactHashes = BTreeMap<String, (u64, String)>;

fn artifact_hashes(root: &Path) -> ArtifactHashes {
    fn visit(root: &Path, directory: &Path, hashes: &mut ArtifactHashes, total: &mut u64) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), hashes, total);
                continue;
            }
            assert!(
                entry.file_type().unwrap().is_file(),
                "private artifact must be regular"
            );
            assert!(hashes.len() < 4096);
            let size = entry.metadata().unwrap().len();
            *total = total.checked_add(size).unwrap();
            assert!(
                *total <= 128 * 1024 * 1024,
                "bounded actual artifact inventory"
            );
            let mut hasher = blake3::Hasher::new();
            let mut file = fs::File::open(entry.path()).unwrap();
            let mut buffer = [0; 8192];
            let mut read = 0u64;
            loop {
                let count = file.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                read += count as u64;
                assert!(read <= size, "artifact changed while hashing");
                hasher.update(&buffer[..count]);
            }
            assert_eq!(read, size);
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            assert!(
                hashes
                    .insert(relative, (size, hasher.finalize().to_hex().to_string()))
                    .is_none()
            );
        }
    }
    let mut hashes = BTreeMap::new();
    visit(root, root, &mut hashes, &mut 0);
    hashes
}

struct Projection {
    fixture: Fixture,
    module: String,
    report: Value,
    original_hashes: ArtifactHashes,
    projected_hashes: ArtifactHashes,
    tool_blake3: String,
}
impl Fixture {
    fn new(started: Instant) -> Self {
        let launcher = std::env::var_os("VESPER_LEAN").unwrap_or_else(|| "lean".into());
        let prefix = execute(
            Command::new(launcher)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .arg("--print-prefix"),
            30,
            started,
        );
        prefix.success();
        let prefix = PathBuf::from(String::from_utf8(prefix.stdout).unwrap().trim())
            .canonicalize()
            .unwrap();
        let lean = prefix
            .join("bin")
            .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
        let version = execute(Command::new(&lean).arg("--version"), 10, started);
        version.success();
        assert!(
            String::from_utf8_lossy(&version.stdout).contains("version 4.24.0,"),
            "required real Lean4.24: {}",
            version.diagnostic()
        );
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-resource-model42-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            lean,
            vesper: resolve_vesper(),
            prefix,
            started,
        }
    }
    fn compile(&self, module: &str, source: &str) -> Captured {
        assert!(source.len() <= SOURCE_CAP);
        let path = self.root.join(module_path(module, "lean"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, source).unwrap();
        let mut command = Command::new(&self.lean);
        command
            .env_clear()
            .current_dir(&self.root)
            .env(
                "LEAN_PATH",
                std::env::join_paths([self.root.clone(), self.prefix.join("lib/lean")]).unwrap(),
            )
            .env("LEAN_NUM_THREADS", "1")
            .env("LEAN_ABORT_ON_PANIC", "1");
        windows_environment(&mut command);
        execute(
            command
                .args(["-j1", "-M4096", "-o"])
                .arg(module_path(module, "olean"))
                .arg(module_path(module, "lean")),
            300,
            self.started,
        )
    }
    fn sibling(&self, label: &str, create: bool) -> Self {
        assert!(valid_name(label));
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-resource-model42-case-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!root.exists());
        if create {
            fs::create_dir(&root).unwrap();
        }
        Self {
            root,
            lean: self.lean.clone(),
            vesper: self.vesper.clone(),
            prefix: self.prefix.clone(),
            started: self.started,
        }
    }
    fn snapshot_attempt(
        &self,
        module: &str,
        output_module: &str,
    ) -> (Captured, Self, ArtifactHashes, String) {
        let output = self.sibling("Projection", false);
        let request_directory = CaptureFiles::new();
        let request_path = request_directory.root.join("request.json");
        let request = json!({"schema_version":1,"root":self.root.canonicalize().unwrap(),
            "toolchain_prefix":self.prefix,"module":module,
            "output_root":output.root,"output_module":output_module});
        fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
        let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tools/resource-model42/KernelSnapshot.lean")
            .canonicalize()
            .unwrap();
        let tool_source = read_source(&tool).text;
        let tool_blake3 = blake3::hash(tool_source.as_bytes()).to_hex().to_string();
        let before = artifact_hashes(&self.root);
        assert!(
            before.keys().all(|path| !path.ends_with(".ir")
                && !path.ends_with(".olean.private")
                && !path.ends_with(".olean.server")),
            "snapshot acceptance profile uses fresh legacy objects without package sidecars"
        );
        let mut command = Command::new(&self.lean);
        command
            .env_clear()
            .current_dir(&request_directory.root)
            .env("LEAN_PATH", self.prefix.join("lib/lean"))
            .env("LEAN_NUM_THREADS", "1")
            .env("LEAN_ABORT_ON_PANIC", "1");
        windows_environment(&mut command);
        let captured = execute(
            command
                .args(["-j1", "-M4096", "--run"])
                .arg(&tool)
                .arg(&request_path),
            300,
            self.started,
        );
        assert_eq!(
            before,
            artifact_hashes(&self.root),
            "snapshot must not change original input bytes"
        );
        assert_eq!(
            tool_blake3,
            blake3::hash(read_source(&tool).text.as_bytes())
                .to_hex()
                .to_string()
        );
        (captured, output, before, tool_blake3)
    }
    fn project(
        &self,
        module: &str,
        output_module: &str,
        expected_modules: &BTreeSet<String>,
    ) -> Projection {
        let (captured, fixture, original_hashes, tool_blake3) =
            self.snapshot_attempt(module, output_module);
        captured.success();
        assert!(captured.stderr.is_empty(), "{}", captured.diagnostic());
        let stdout = String::from_utf8(captured.stdout).unwrap();
        let lines = stdout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "exactly one real snapshot success record");
        let report: Value = serde_json::from_str(
            lines[0]
                .strip_prefix(SNAPSHOT_MARKER)
                .expect("actual snapshot marker"),
        )
        .unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["scope"], "kernel_logical_projection");
        assert_eq!(report["original_module"], module);
        assert_eq!(report["projection_module"], output_module);
        let artifact = fixture
            .root
            .join(module_path(output_module, "olean"))
            .canonicalize()
            .unwrap();
        assert_eq!(
            PathBuf::from(report["projection_path"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            artifact
        );
        for flag in [
            "exact_inventory",
            "constant_info_equal",
            "kernel_replayed",
            "reimport_verified",
            "extensions_empty",
            "ir_empty",
        ] {
            assert_eq!(report[flag], true, "real snapshot flag {flag}");
        }
        let original = report["original_constants"].as_u64().unwrap();
        let retained = report["retained_constants"].as_u64().unwrap();
        let omitted = report["omitted_constants"].as_u64().unwrap();
        let occurrences = report["original_constant_occurrences"].as_u64().unwrap();
        let duplicate_count = report["duplicate_constants"].as_u64().unwrap();
        assert!(retained > 0 && original == retained + omitted);
        assert_eq!(occurrences, original.checked_add(duplicate_count).unwrap());
        assert_eq!(report["alias_scope"], "synthetic_theorem_proof_variants");
        let alias_count = report["proof_alias_count"].as_u64().unwrap();
        assert_eq!(
            report["output_constants"].as_u64().unwrap(),
            retained.checked_add(alias_count).unwrap()
        );
        assert!(alias_count <= duplicate_count);
        assert!(report["logical_expression_nodes"].as_u64().unwrap() > 0);
        let mut actual_modules = BTreeSet::new();
        for record in report["original_modules"].as_array().unwrap() {
            let name = record["name"].as_str().unwrap();
            assert!(actual_modules.insert(name.to_owned()));
            let path = PathBuf::from(record["path"].as_str().unwrap())
                .canonicalize()
                .unwrap();
            assert_eq!(
                path,
                self.root
                    .join(module_path(name, "olean"))
                    .canonicalize()
                    .unwrap()
            );
            let relative = path
                .strip_prefix(self.root.canonicalize().unwrap())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            assert!(original_hashes.contains_key(&relative));
        }
        assert_eq!(&actual_modules, expected_modules);
        let duplicates = report["duplicates"].as_array().unwrap();
        assert_eq!(duplicates.len() as u64, duplicate_count);
        for record in duplicates {
            assert!(!record["name"].as_str().unwrap().is_empty());
            assert!(matches!(
                record["reason"].as_str().unwrap(),
                "identical_safe_theorem_realization" | "safe_theorem_proof_variant"
            ));
            for field in ["first_module", "repeated_module"] {
                assert!(actual_modules.contains(record[field].as_str().unwrap()));
            }
        }
        let trusted = report["trusted_modules"].as_array().unwrap();
        assert!(!trusted.is_empty());
        let mut trusted_names = BTreeSet::new();
        let library = self.prefix.join("lib/lean").canonicalize().unwrap();
        for record in trusted {
            assert!(trusted_names.insert(record["name"].as_str().unwrap()));
            assert!(
                PathBuf::from(record["path"].as_str().unwrap())
                    .canonicalize()
                    .unwrap()
                    .starts_with(&library)
            );
        }
        let omissions = report["omitted"].as_array().unwrap();
        assert_eq!(omissions.len() as u64, omitted);
        let mut names = BTreeSet::new();
        for record in omissions {
            let name = record["name"].as_str().unwrap();
            let parent = record["parent"].as_str().unwrap();
            assert!(names.insert(name));
            assert_eq!(name, format!("{parent}._unsafe_rec"));
            assert_eq!(record["reason"], "matched_partial_unsafe_rec");
            assert!(actual_modules.contains(record["module"].as_str().unwrap()));
        }
        let aliases = report["proof_aliases"].as_array().unwrap();
        assert_eq!(aliases.len() as u64, alias_count);
        let mut alias_names = BTreeSet::new();
        for alias in aliases {
            let name = alias["name"].as_str().unwrap();
            let ordinal = name
                .strip_prefix("ResourceKernelProofAliases.realization_")
                .expect("isolated synthetic theorem namespace");
            assert!(!ordinal.is_empty() && ordinal.bytes().all(|byte| byte.is_ascii_digit()));
            assert!(alias_names.insert(name));
            assert_eq!(alias["reason"], "checked_safe_theorem_variant");
            assert!(!alias["original_name"].as_str().unwrap().is_empty());
            for field in ["source_module", "canonical_module"] {
                assert!(actual_modules.contains(alias[field].as_str().unwrap()));
            }
            let _ = alias["source_occurrence"].as_u64().unwrap();
            let group = alias["original_group"]
                .as_array()
                .unwrap()
                .iter()
                .map(|member| member.as_str().unwrap())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                group.len(),
                alias["original_group"].as_array().unwrap().len()
            );
            assert!(group.contains(alias["original_name"].as_str().unwrap()));
        }
        let records = report["occurrences"].as_array().unwrap();
        assert_eq!(records.len() as u64, occurrences);
        let mut positions = BTreeSet::new();
        let mut original_names = BTreeSet::new();
        let mut aliased_occurrences = BTreeSet::new();
        let mut omitted_occurrences = BTreeSet::new();
        for record in records {
            let name = record["name"].as_str().unwrap();
            let module = record["module"].as_str().unwrap();
            let index = record["index"].as_u64().unwrap();
            assert!(actual_modules.contains(module));
            assert!(positions.insert((module, index)));
            original_names.insert(name);
            match record["disposition"].as_str().unwrap() {
                "retained_canonical_info" => assert!(record["alias"].is_null()),
                "omitted_auxiliary" => {
                    assert!(record["alias"].is_null());
                    assert!(names.contains(name));
                    assert!(omitted_occurrences.insert(name));
                }
                "proof_alias" => {
                    let alias_name = record["alias"].as_str().unwrap();
                    assert!(aliased_occurrences.insert(alias_name));
                    let alias = aliases
                        .iter()
                        .find(|alias| alias["name"] == alias_name)
                        .expect("every alternate occurrence maps to one actual alias");
                    assert_eq!(alias["original_name"], name);
                    assert_eq!(alias["source_module"], module);
                    assert_eq!(alias["source_occurrence"], index);
                }
                other => panic!("unexpected original occurrence disposition {other}"),
            }
        }
        assert_eq!(original_names.len() as u64, original);
        assert_eq!(omitted_occurrences, names);
        assert_eq!(aliased_occurrences, alias_names);
        let projected_hashes = artifact_hashes(&fixture.root);
        assert_eq!(
            projected_hashes.len(),
            1,
            "standalone projection has no IR/extensions/side artifacts"
        );
        assert!(
            projected_hashes.contains_key(
                &module_path(output_module, "olean")
                    .to_string_lossy()
                    .replace('\\', "/")
            )
        );
        Projection {
            fixture,
            module: output_module.to_owned(),
            report,
            original_hashes,
            projected_hashes,
            tool_blake3,
        }
    }
    fn audit(&self, module: &str, roots: &[String]) -> Captured {
        assert!(!roots.is_empty() && roots.len() <= 128);
        let mut command = Command::new(&self.vesper);
        command
            .env_clear()
            .current_dir(&self.root)
            .args(["proof-audit", module, "--root"])
            .arg(&self.root)
            .arg("--lean")
            .arg(&self.lean)
            .args(["--timeout", "300", "--json"]);
        windows_environment(&mut command);
        for root in roots {
            command.args(["--theorem", root]);
        }
        for axiom in POLICY {
            command.args(["--allow-axiom", axiom]);
        }
        execute(&mut command, 360, self.started)
    }
    fn clone_artifacts(&self, name: &str) -> Self {
        assert!(valid_name(name));
        let root = self.root.join(name);
        fs::create_dir(&root).unwrap();
        fn copy(source: &Path, target: &Path) {
            for entry in fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let destination = target.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    if entry.file_name() == "ResourceModel42" {
                        fs::create_dir(&destination).unwrap();
                        copy(&path, &destination);
                    }
                } else {
                    assert!(entry.file_type().unwrap().is_file());
                    fs::copy(&path, destination).unwrap();
                }
            }
        }
        copy(&self.root, &root);
        Self {
            root,
            lean: self.lean.clone(),
            vesper: self.vesper.clone(),
            prefix: self.prefix.clone(),
            started: self.started,
        }
    }
}

fn resolve_vesper() -> PathBuf {
    let named = PathBuf::from(env!("CARGO_BIN_EXE_vesper"));
    if named.is_absolute() || named.components().count() > 1 {
        return named
            .canonicalize()
            .expect("actual Cargo/Bazel vesper binary");
    }
    // Bazel deliberately supplies a bare name and its launcher installs the
    // real binary directory on PATH. Resolve it before isolating child envs.
    for directory in std::env::split_paths(&std::env::var_os("PATH").expect("tool PATH")) {
        let candidate = directory.join(&named);
        if candidate.is_file() {
            return candidate.canonicalize().unwrap();
        }
        if cfg!(windows) && named.extension().is_none() {
            let candidate = directory.join(format!("{}.exe", named.to_str().unwrap()));
            if candidate.is_file() {
                return candidate.canonicalize().unwrap();
            }
        }
    }
    panic!(
        "required actual vesper executable {} absent from PATH",
        named.display()
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() && self.root.exists() {
            eprintln!(
                "resource_model42 retained failed artifact root: {}",
                self.root.display()
            );
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_audit_error(output: Captured, module: &str) {
    assert!(!output.status.success());
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["kind"], "audit_failed");
    let message = report["error"]["message"].as_str().unwrap();
    assert!(!message.is_empty() && message.contains(module), "{message}");
    assert!(
        !message.contains("timed out") && !message.contains("output limit"),
        "actual artifact refusal: {message}"
    );
    assert_ne!(report["accepted"], true);
}

fn reference(name: &str) -> Term {
    Term::Ref(lean_name(name))
}

fn lean_name(name: &str) -> Name {
    Name::qualified(
        name.split('.')
            .map(|component| Ident::new(component).unwrap()),
    )
    .unwrap()
}

fn all_projection_roots(projected: &Projection, manual: &[String]) -> Vec<String> {
    let mut roots = manual.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(roots.len(), manual.len());
    for alias in projected.report["proof_aliases"].as_array().unwrap() {
        assert!(roots.insert(alias["name"].as_str().unwrap().to_owned()));
    }
    assert_eq!(
        roots.len(),
        manual.len() + projected.report["proof_alias_count"].as_u64().unwrap() as usize
    );
    roots.into_iter().collect()
}

fn positive_audit(fixture: &Fixture, module: &str, roots: &[String]) -> Value {
    let before = artifact_hashes(&fixture.root);
    let output = fixture.audit(module, roots);
    output.success();
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "audited");
    assert_eq!(report["accepted"], true);
    assert_eq!(report["kernel_replayed"], true);
    assert_eq!(report["cache_eligible"], false);
    assert_eq!(report["module"], module);
    assert_eq!(report["allowed_axioms"], json!(POLICY));
    assert_eq!(report["forbidden_axioms"], json!([]));
    assert!(report["checked_constants"].as_u64().unwrap() > 0);
    assert_eq!(report["lean_binary_blake3"].as_str().unwrap().len(), 64);
    assert_eq!(report["auditor_blake3"].as_str().unwrap().len(), 64);
    let artifact_modules = report["imports"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["scope"] == "artifact")
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(artifact_modules, BTreeSet::from([module.to_owned()]));
    let mut actual = BTreeSet::new();
    for theorem in report["theorems"].as_array().unwrap() {
        assert!(actual.insert(theorem["name"].as_str().unwrap().to_owned()));
        for axiom in theorem["axioms"].as_array().unwrap() {
            assert!(POLICY.contains(&axiom.as_str().unwrap()), "{theorem}");
        }
    }
    assert_eq!(actual, roots.iter().cloned().collect());
    let path = module_path(module, "olean")
        .to_string_lossy()
        .replace('\\', "/");
    let file = report["artifact_files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == path)
        .expect("actual audited output object");
    assert_eq!(file["bytes"].as_u64().unwrap(), before[&path].0);
    assert_eq!(file["blake3"].as_str().unwrap(), before[&path].1);
    assert_eq!(
        before,
        artifact_hashes(&fixture.root),
        "strict audit is read-only"
    );
    report
}

fn control_fixtures(base: &Fixture) {
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/resource-model42/fixtures");
    for (module, roots, omissions) in [
        (
            "ResourceSnapshotTotal",
            vec![
                "ResourceSnapshotTotal.total_correct".to_owned(),
                "ResourceSnapshotTotal.tree_correct".to_owned(),
            ],
            2,
        ),
        (
            "ResourceSnapshotFakeHelper",
            vec!["ResourceSnapshotFakeHelper.safe_suffix_correct".to_owned()],
            0,
        ),
    ] {
        let original = base.sibling(module, true);
        original
            .compile(
                module,
                &read_source(&directory.join(format!("{module}.lean"))).text,
            )
            .success();
        if module == "ResourceSnapshotTotal" {
            let raw = original.audit(module, &roots);
            assert!(!raw.status.success());
            let report = raw.json();
            assert_eq!(report["status"], "error");
            assert!(
                report["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("unsupported unsafe or partial artifact declaration")
            );
        }
        let projected = original.project(
            module,
            &format!("{module}Kernel"),
            &BTreeSet::from([module.to_owned()]),
        );
        assert_eq!(projected.report["omitted_constants"], omissions);
        let all_roots = all_projection_roots(&projected, &roots);
        let audited = positive_audit(&projected.fixture, &projected.module, &all_roots);
        assert_eq!(
            audited["checked_constants"],
            projected.report["output_constants"]
        );
    }
    for (module, cause) in [
        ("ResourceSnapshotUnsafe", "explicit unsafe declaration"),
        ("ResourceSnapshotPartial", "package opaque declaration"),
        ("ResourceSnapshotFakeUnsafe", "explicit unsafe declaration"),
    ] {
        let original = base.sibling(module, true);
        original
            .compile(
                module,
                &read_source(&directory.join(format!("{module}.lean"))).text,
            )
            .success();
        let (output, projected, _, _) =
            original.snapshot_attempt(module, &format!("{module}Kernel"));
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SNAPSHOT_MARKER));
        let diagnostic = output.diagnostic();
        assert!(
            diagnostic.contains("kernel snapshot failed:") && diagnostic.contains(cause),
            "{diagnostic}"
        );
        assert!(
            !projected
                .root
                .join(module_path(&format!("{module}Kernel"), "olean"))
                .exists()
        );
    }
    let module = "ResourceSnapshotAxiom";
    let original = base.sibling(module, true);
    original
        .compile(
            module,
            &read_source(&directory.join(format!("{module}.lean"))).text,
        )
        .success();
    let projected = original.project(
        module,
        "ResourceSnapshotAxiomKernel",
        &BTreeSet::from([module.to_owned()]),
    );
    let output = projected.fixture.audit(
        &projected.module,
        &["ResourceSnapshotAxiom.exposed".to_owned()],
    );
    assert!(!output.status.success());
    let report = output.json();
    assert_eq!(report["status"], "audited");
    assert_eq!(report["accepted"], false);
    assert_eq!(report["kernel_replayed"], true);
    assert_eq!(report["cache_eligible"], false);
    assert_eq!(
        report["forbidden_axioms"],
        json!(["ResourceSnapshotAxiom.projectFalse"])
    );
}

#[test]
fn actual_fixed_model_semantics_kernel_replay_and_complete_axiom_policy() {
    let started = Instant::now();
    let sources = fixed_sources();
    let order = compile_order(&sources);
    let roots = theorem_catalog(&sources);
    let fixture = Fixture::new(started);
    for module in &order {
        fixture.compile(module, &sources[module].text).success();
    }
    let expected_files = sources
        .keys()
        .flat_map(|module| {
            ["lean", "olean"].map(|suffix| {
                module_path(module, suffix)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        artifact_hashes(&fixture.root)
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected_files,
        "production input is exactly the freshly compiled legacy source/object pairs"
    );
    let artifact = fixture.root.join(module_path(MODULE, "olean"));
    let before = fs::read(&artifact).unwrap();
    let projected = fixture.project(MODULE, PROJECTED_MODULE, &sources.keys().cloned().collect());
    assert!(
        projected.report["omitted_constants"].as_u64().unwrap() > 0,
        "production snapshot must exercise actual generated-helper exclusions"
    );
    let audited_roots = all_projection_roots(&projected, &roots);
    let mut actual_roots = BTreeSet::new();
    for partition in audited_roots.chunks(128) {
        let report = positive_audit(&projected.fixture, &projected.module, partition);
        assert_eq!(
            report["checked_constants"],
            projected.report["output_constants"]
        );
        for theorem in report["theorems"].as_array().unwrap() {
            let root = theorem["name"].as_str().unwrap().to_owned();
            assert!(actual_roots.insert(root));
        }
        assert_eq!(fs::read(&artifact).unwrap(), before);
    }
    assert_eq!(actual_roots, audited_roots.iter().cloned().collect());

    for (label, corrupt) in [("OriginalMissing", false), ("OriginalCorrupt", true)] {
        let changed = fixture.clone_artifacts(label);
        let path = changed.root.join(module_path(MODULE, "olean"));
        if corrupt {
            fs::write(path, b"not a Lean object\n").unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        let (output, attempted, _, _) = changed.snapshot_attempt(MODULE, PROJECTED_MODULE);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SNAPSHOT_MARKER));
        let message = output.diagnostic();
        assert!(
            message.contains("kernel snapshot failed:") && message.contains("Acceptance"),
            "{message}"
        );
        assert!(
            !message.contains("timed out") && !message.contains("limit"),
            "actual original artifact failure: {message}"
        );
        assert!(
            !attempted
                .root
                .join(module_path(PROJECTED_MODULE, "olean"))
                .exists()
        );
    }

    for (index, constructor) in [
        "TypeInfo.mk",
        "ValidatedProgram.mk",
        "CheckedProgram.mk",
        "Invocation.mk",
    ]
    .into_iter()
    .enumerate()
    {
        let module = Module {
            imports: vec![ModuleName::new("ResourceModel42").unwrap()],
            declarations: vec![Declaration::Check(reference(&format!(
                "Vesper.ResourceModel42.{constructor}"
            )))],
        };
        let output = fixture.compile(
            &format!("ResourceModel42Privacy{index}"),
            &module.render().unwrap(),
        );
        assert!(
            !output.status.success(),
            "private constructor exposed: {constructor}"
        );
        assert!(
            output.diagnostic().contains(constructor),
            "{}",
            output.diagnostic()
        );
    }

    let negative = Module {
        imports: vec![ModuleName::new(PROJECTED_MODULE).unwrap()],
        declarations: vec![Declaration::Theorem {
            name: lean_name("Vesper.ResourceModel42Rejected.forbidden"),
            binders: vec![],
            proposition: reference("True"),
            proof: Term::app(reference("sorryAx"), [reference("True"), Term::Bool(false)]),
        }],
    };
    projected
        .fixture
        .compile("ResourceModel42Rejected", &negative.render().unwrap())
        .success();
    let output = projected.fixture.audit(
        "ResourceModel42Rejected",
        &["Vesper.ResourceModel42Rejected.forbidden".to_owned()],
    );
    assert!(!output.status.success());
    let denied = output.json();
    assert_eq!(denied["status"], "audited");
    assert_eq!(denied["accepted"], false);
    assert_eq!(denied["kernel_replayed"], true);
    assert_eq!(denied["cache_eligible"], false);
    assert_eq!(denied["forbidden_axioms"], json!(["sorryAx"]));

    let missing = projected.fixture.clone_artifacts("Missing");
    fs::remove_file(missing.root.join(module_path(PROJECTED_MODULE, "olean"))).unwrap();
    assert_audit_error(
        missing.audit(PROJECTED_MODULE, &roots[..1]),
        PROJECTED_MODULE,
    );
    let corrupt = projected.fixture.clone_artifacts("Corrupt");
    fs::write(
        corrupt.root.join(module_path(PROJECTED_MODULE, "olean")),
        b"not a Lean object\n",
    )
    .unwrap();
    assert_audit_error(
        corrupt.audit(PROJECTED_MODULE, &roots[..1]),
        PROJECTED_MODULE,
    );
    assert_eq!(fs::read(&artifact).unwrap(), before);
    control_fixtures(&fixture);
    let binding = json!({"scope":"kernel_logical_projection_not_native_equivalence",
        "snapshot_report":projected.report,"original_artifact_hashes":projected.original_hashes,
        "projected_artifact_hashes":projected.projected_hashes,"tool_source_blake3":projected.tool_blake3,
        "manual_roots":roots.len(),"strict_audited_roots":audited_roots.len(),
        "strict_partitions":audited_roots.chunks(128).len()});
    let binding = serde_json::to_string(&binding).unwrap();
    assert!(binding.len() <= CAPTURE);
    eprintln!("RESOURCE_MODEL42_KERNEL_BINDING {binding}");
    eprintln!(
        "resource_model42 complete: {} fixed modules, {} manual roots plus {} aliases, {} partitions, {:.2}s target",
        sources.len(),
        roots.len(),
        audited_roots.len() - roots.len(),
        audited_roots.chunks(128).len(),
        started.elapsed().as_secs_f64()
    );
}
