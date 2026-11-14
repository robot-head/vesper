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
    assert!(
        fs::metadata(path).unwrap().is_file(),
        "regular fixed source: {}",
        path.display()
    );
    let bytes = fs::read(path).unwrap();
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
                            .any(|token| token == "theorem"),
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
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_audit_error(output: Captured) {
    assert!(!output.status.success());
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["kind"], "audit_failed");
    let message = report["error"]["message"].as_str().unwrap();
    assert!(
        !message.is_empty() && message.contains("Acceptance"),
        "{message}"
    );
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
    let artifact = fixture.root.join(module_path(MODULE, "olean"));
    let before = fs::read(&artifact).unwrap();
    let mut actual_roots = BTreeSet::new();
    for partition in roots.chunks(128) {
        let output = fixture.audit(MODULE, partition);
        output.success();
        let report = output.json();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["status"], "audited");
        assert_eq!(report["accepted"], true);
        assert_eq!(report["kernel_replayed"], true);
        assert_eq!(report["cache_eligible"], false);
        assert_eq!(report["module"], MODULE);
        assert_eq!(report["allowed_axioms"], json!(POLICY));
        assert_eq!(report["forbidden_axioms"], json!([]));
        assert!(report["checked_constants"].as_u64().unwrap() > 0);
        assert_eq!(report["lean_binary_blake3"].as_str().unwrap().len(), 64);
        assert_eq!(report["auditor_blake3"].as_str().unwrap().len(), 64);
        let mut partition_roots = BTreeSet::new();
        for theorem in report["theorems"].as_array().unwrap() {
            let root = theorem["name"].as_str().unwrap().to_owned();
            assert!(partition_roots.insert(root.clone()));
            assert!(actual_roots.insert(root));
            for axiom in theorem["axioms"].as_array().unwrap() {
                assert!(POLICY.contains(&axiom.as_str().unwrap()), "{theorem}");
            }
        }
        assert_eq!(partition_roots, partition.iter().cloned().collect());
        assert!(
            report["artifact_files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["path"] == "ResourceModel42/Acceptance.olean")
        );
        assert_eq!(fs::read(&artifact).unwrap(), before);
    }
    assert_eq!(actual_roots, roots.iter().cloned().collect());

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
        imports: vec![ModuleName::new("ResourceModel42").unwrap()],
        declarations: vec![Declaration::Theorem {
            name: lean_name("Vesper.ResourceModel42Rejected.forbidden"),
            binders: vec![],
            proposition: reference("True"),
            proof: Term::app(reference("sorryAx"), [reference("True"), Term::Bool(false)]),
        }],
    };
    fixture
        .compile("ResourceModel42Rejected", &negative.render().unwrap())
        .success();
    let output = fixture.audit(
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

    let missing = fixture.clone_artifacts("Missing");
    fs::remove_file(missing.root.join(module_path(MODULE, "olean"))).unwrap();
    assert_audit_error(missing.audit(MODULE, &roots[..1]));
    let corrupt = fixture.clone_artifacts("Corrupt");
    fs::write(
        corrupt.root.join(module_path(MODULE, "olean")),
        b"not a Lean object\n",
    )
    .unwrap();
    assert_audit_error(corrupt.audit(MODULE, &roots[..1]));
    assert_eq!(fs::read(&artifact).unwrap(), before);
    eprintln!(
        "resource_model42 complete: {} fixed modules, {} explicit theorem roots, {} partitions, {:.2}s target",
        sources.len(),
        roots.len(),
        roots.chunks(128).len(),
        started.elapsed().as_secs_f64()
    );
}
