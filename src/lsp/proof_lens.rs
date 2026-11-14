//! Proof code lenses (§21.1.5, §21.3 Layer 5): Lean obligations for source
//! functions, run on demand or as a debounced background job.
//!
//! Two obligations exist per function.
//!
//! *Extraction* (`lean_extraction_axiom_free`): the module extracted from the
//! checked source compiles under Lean 4.24.0, and the theorem
//! `VesperLens.obligation_<n> : @f = @f` replays through the kernel with an
//! empty transitive axiom closure. That establishes that Lean accepts the
//! extracted definition and that its kernel-visible closure uses no axioms.
//! For `partial def` extractions the kernel sees only the opaque constant.
//!
//! *Semantic* (`lean_model_arith_fault_free`): the per-function theorem of
//! [`super::proof_model`] that the function's scalar model raises no
//! arithmetic fault for any input. It is proved by `omega` and replayed by
//! the kernel with at most Lean's standard axioms (`propext`, `Quot.sound`,
//! `Classical.choice`). When `omega` fails, `bv_decide` searches for a
//! concrete counterexample.
//!
//! Neither is a theorem about the Rust compiler, the extraction's faithfulness
//! or native code.
//!
//! Compiled `.olean` files are reused across runs from a private cache
//! directory keyed by the exact module text and the Lean version. A reused
//! `.olean` is still replayed through the kernel by `vesper proof-audit`; the
//! cache only skips elaboration. Failed models (counterexamples) are cached
//! as outcomes, since nothing about them is replayed.
use super::proof_model::{Model, hex};
use crate::{lean, tooling::frontend::CheckedSource};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use vesper_lsp::lens::{CancelToken, ProofStatus};

pub(crate) const EXTRACTION: &str = "VesperLensObligation";
const RUN_MODULE: &str = "VesperLensRun";
const MAX_FUNCTIONS: usize = 256;
const MAX_THEOREMS: usize = 128;
const MAX_CAPTURE: u64 = 8 * 1024 * 1024;
/// Axioms a semantic theorem may use: Lean's standard axioms, never
/// `sorryAx` or native evaluation (`Lean.ofReduceBool`).
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

pub(crate) const EXTRACTION_SCOPE: &str = "lean_extraction_axiom_free";
pub(crate) const SEMANTIC_SCOPE: &str = "lean_model_arith_fault_free";

/// Functions whose definitions appear in the Lean extraction, in source
/// order, or the reason extraction is unavailable for this source.
pub(crate) fn candidates(
    snapshot: &CheckedSource,
) -> Result<(String, Vec<(String, crate::ast::Span)>), String> {
    let text = lean::extract(snapshot.world())?;
    let mut functions: Vec<_> = snapshot
        .world()
        .functions
        .values()
        .filter(|f| f.body.is_some() && defined(&text, &f.name))
        .map(|f| (f.name.clone(), f.span.clone()))
        .collect();
    functions.sort_by_key(|(_, span)| span.start);
    functions.truncate(MAX_FUNCTIONS);
    Ok((text, functions))
}

fn defined(text: &str, name: &str) -> bool {
    let needle = format!("def «{name}» ");
    text.contains(&needle)
}

/// Where proof work runs and what it may reuse. Server-side only.
#[derive(Clone, Debug)]
pub(crate) struct Toolchain {
    pub launcher: String,
    pub auditor: Option<PathBuf>,
    pub timeout: u64,
    pub cache: Option<PathBuf>,
}

/// A self-contained audit request; it owns everything a worker thread needs.
#[derive(Debug)]
pub(crate) struct Job {
    pub source: [u8; 32],
    /// Extraction module text and the functions whose obligation runs.
    extraction: Option<(String, Vec<String>)>,
    /// Semantic models to check.
    pub models: Vec<Model>,
    toolchain: Toolchain,
    /// The function and obligation (`true`: semantic) a command asked for.
    pub requested: Option<(String, bool)>,
}
impl Job {
    pub(crate) fn is_empty(&self) -> bool {
        self.extraction.is_none() && self.models.is_empty()
    }
}

/// Every outcome a job produced.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub extraction: Vec<(String, ProofStatus)>,
    /// (function, model key, status)
    pub semantic: Vec<(String, [u8; 32], ProofStatus)>,
    /// `.olean` files reused from the cache, and compiled fresh.
    pub reused: usize,
    pub compiled: usize,
    pub elapsed: Duration,
}

/// Prepare a job. `extraction` names functions for the extraction obligation;
/// `models` are semantic models to check.
pub(crate) fn prepare(
    snapshot: &CheckedSource,
    extraction: &[String],
    models: Vec<Model>,
    toolchain: &Toolchain,
) -> Result<Job, String> {
    let extraction = if extraction.is_empty() {
        None
    } else {
        let (text, functions) = candidates(snapshot)?;
        for function in extraction {
            if !functions.iter().any(|(name, _)| name == function) {
                return Err(format!("function '{function}' has no Lean extraction"));
            }
            if function.contains('»') {
                return Err("function name cannot be quoted for Lean".into());
            }
        }
        let mut module = format!("{text}\n\nnamespace VesperLens\n");
        for (index, function) in extraction.iter().enumerate() {
            let constant = format!("@«Vesper».«{function}»");
            module.push_str(&format!(
                "theorem obligation_{index} : {constant} = {constant} := rfl\n"
            ));
        }
        module.push_str("end VesperLens\n");
        Some((module, extraction.to_vec()))
    };
    if extraction.as_ref().map_or(0, |(_, f)| f.len()) + models.len() > MAX_THEOREMS {
        return Err("too many proof obligations for one run".into());
    }
    let mut toolchain = toolchain.clone();
    toolchain.timeout = toolchain.timeout.clamp(1, 300);
    Ok(Job {
        source: snapshot.source_hash(),
        extraction,
        models,
        toolchain,
        requested: None,
    })
}

/// Run the job, polling `cancel` while Lean or the auditor runs. A cancelled
/// job kills its child processes and reports nothing.
pub(crate) fn run(job: &Job, cancel: &CancelToken) -> Option<Outcome> {
    let started = Instant::now();
    let mut outcome = Outcome::default();
    let fail_all = |outcome: &mut Outcome, stage: &str, reason: &str| {
        let status = ProofStatus::Failed {
            stage: stage.into(),
            reason: bounded(reason.to_owned()),
        };
        if let Some((_, functions)) = &job.extraction {
            for f in functions {
                outcome.extraction.push((f.clone(), status.clone()));
            }
        }
        for model in &job.models {
            outcome
                .semantic
                .push((model.function.clone(), model.key, status.clone()));
        }
    };
    match run_stages(job, cancel, &mut outcome) {
        Ok(true) => {}
        Ok(false) => return None,
        Err((stage, reason)) => {
            outcome.extraction.clear();
            outcome.semantic.clear();
            fail_all(&mut outcome, stage, &reason);
        }
    }
    outcome.elapsed = started.elapsed();
    Some(outcome)
}

fn bounded(mut text: String) -> String {
    let mut end = text.len().min(2048);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

type StageResult<T> = Result<T, (&'static str, String)>;

struct Lean {
    prefix: PathBuf,
    binary: PathBuf,
    version: String,
}

/// `Ok(false)` means cancelled.
fn run_stages(job: &Job, cancel: &CancelToken, outcome: &mut Outcome) -> StageResult<bool> {
    let timeout = Duration::from_secs(job.toolchain.timeout);
    let auditor = job.toolchain.auditor.as_ref().ok_or((
        "toolchain",
        "the vesper proof auditor is unavailable".to_owned(),
    ))?;
    let Some(prefix) = child(
        Command::new(&job.toolchain.launcher).arg("--print-prefix"),
        Duration::from_secs(30),
        cancel,
    )
    .map_err(|e| ("toolchain", e))?
    else {
        return Ok(false);
    };
    let prefix = PathBuf::from(String::from_utf8_lossy(&prefix).trim());
    let binary = prefix
        .join("bin")
        .join(if cfg!(windows) { "lean.exe" } else { "lean" });
    let Some(version) = child(Command::new(&binary).arg("--version"), timeout, cancel)
        .map_err(|e| ("toolchain", e))?
    else {
        return Ok(false);
    };
    let version = String::from_utf8_lossy(&version).trim().to_owned();
    if !version.contains("version 4.24.0,") {
        return Err(("toolchain", "proof lenses require Lean 4.24.0".into()));
    }
    let lean = Lean {
        prefix,
        binary,
        version,
    };
    let cache = job
        .toolchain
        .cache
        .as_deref()
        .and_then(|dir| Cache::open(dir).ok());
    let stage = Stage::new().map_err(|e| ("io", e))?;
    // (module, theorem names) available for the single kernel audit.
    let mut audited: Vec<(String, Vec<String>)> = Vec::new();

    if let Some((text, functions)) = &job.extraction {
        match ensure_olean(
            &lean,
            cache.as_ref(),
            &stage,
            EXTRACTION,
            text,
            timeout,
            cancel,
            outcome,
        ) {
            Compiled::Ready => audited.push((
                EXTRACTION.into(),
                (0..functions.len())
                    .map(|i| format!("VesperLens.obligation_{i}"))
                    .collect(),
            )),
            Compiled::Cancelled => return Ok(false),
            Compiled::Failed(reason) => {
                for f in functions {
                    outcome.extraction.push((
                        f.clone(),
                        ProofStatus::Failed {
                            stage: "compile".into(),
                            reason: bounded(reason.clone()),
                        },
                    ));
                }
            }
        }
    }

    for model in &job.models {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let key = hex(&model.key);
        if let Some(status) = cache.as_ref().and_then(|c| c.outcome(&key, &lean.version)) {
            outcome
                .semantic
                .push((model.function.clone(), model.key, status));
            continue;
        }
        match ensure_olean(
            &lean,
            cache.as_ref(),
            &stage,
            &model.module_name(),
            &model.exact_module(),
            timeout,
            cancel,
            outcome,
        ) {
            Compiled::Ready => {
                audited.push((model.module_name(), vec![model.theorem()]));
            }
            Compiled::Cancelled => return Ok(false),
            Compiled::Failed(omega) => {
                // omega could not prove it: look for a concrete counterexample.
                let Some(status) = bitvec_check(&lean, &stage, model, &omega, timeout, cancel)
                else {
                    return Ok(false);
                };
                if let Some(cache) = &cache {
                    cache.record_outcome(&key, &lean.version, &status);
                }
                outcome
                    .semantic
                    .push((model.function.clone(), model.key, status));
            }
        }
    }
    if audited.is_empty() {
        return Ok(true);
    }

    // One aggregate module and one kernel replay for every theorem.
    let imports: String = audited
        .iter()
        .map(|(module, _)| format!("import {module}\n"))
        .collect();
    fs::write(stage.0.join(format!("{RUN_MODULE}.lean")), imports)
        .map_err(|e| ("io", e.to_string()))?;
    match compile(&lean, &stage.0, RUN_MODULE, timeout, cancel) {
        Ok(Some(())) => {}
        Ok(None) => return Ok(false),
        Err(reason) => return Err(("compile", reason)),
    }
    let mut command = Command::new(auditor);
    command
        .arg("proof-audit")
        .arg(RUN_MODULE)
        .arg("--root")
        .arg(&stage.0);
    for (_, theorems) in &audited {
        for theorem in theorems {
            command.args(["--theorem", theorem]);
        }
    }
    for axiom in STANDARD_AXIOMS {
        command.args(["--allow-axiom", axiom]);
    }
    command
        .args(["--json", "--timeout"])
        .arg(job.toolchain.timeout.to_string())
        .arg("--lean")
        .arg(&lean.binary);
    let Some(report) =
        child(&mut command, timeout + Duration::from_secs(5), cancel).map_err(|e| ("kernel", e))?
    else {
        return Ok(false);
    };
    let report: serde_json::Value =
        serde_json::from_slice(&report).map_err(|e| ("kernel", format!("audit report: {e}")))?;
    if report["kernel_replayed"] != true {
        return Err(("kernel", "the audit did not replay the kernel".into()));
    }
    let checked = report["checked_constants"].as_u64().unwrap_or(0);
    let axioms_of = |theorem: &str| -> Option<Vec<String>> {
        report["theorems"].as_array()?.iter().find_map(|t| {
            (t["name"] == theorem).then(|| {
                t["axioms"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            })
        })
    };
    if let Some((_, functions)) = &job.extraction
        && audited.iter().any(|(m, _)| m == EXTRACTION)
    {
        for (i, function) in functions.iter().enumerate() {
            let status = match axioms_of(&format!("VesperLens.obligation_{i}")) {
                Some(axioms) if axioms.is_empty() => ProofStatus::Proved {
                    detail: format!(
                        "kernel replayed {checked} constants with an empty axiom policy"
                    ),
                },
                Some(_) | None => ProofStatus::Failed {
                    stage: "kernel".into(),
                    reason: "the obligation failed the empty-axiom kernel audit".into(),
                },
            };
            outcome.extraction.push((function.clone(), status));
        }
    }
    for model in &job.models {
        if !audited.iter().any(|(m, _)| *m == model.module_name()) {
            continue;
        }
        let status = match axioms_of(&model.theorem()) {
            Some(axioms) if axioms.iter().all(|a| STANDARD_AXIOMS.contains(&a.as_str())) => {
                ProofStatus::Proved {
                    detail: format!(
                        "omega proof replayed by the kernel; axioms: {}",
                        if axioms.is_empty() {
                            "none".to_owned()
                        } else {
                            axioms.join(", ")
                        }
                    ),
                }
            }
            _ => ProofStatus::Failed {
                stage: "kernel".into(),
                reason: "the theorem uses a forbidden axiom or was not replayed".into(),
            },
        };
        outcome
            .semantic
            .push((model.function.clone(), model.key, status));
    }
    Ok(true)
}

enum Compiled {
    Ready,
    Cancelled,
    Failed(String),
}

/// Put `<module>.olean` for `text` into the stage, from the cache when an
/// identical module was compiled before, else by compiling it.
#[allow(clippy::too_many_arguments)]
fn ensure_olean(
    lean: &Lean,
    cache: Option<&Cache>,
    stage: &Stage,
    module: &str,
    text: &str,
    timeout: Duration,
    cancel: &CancelToken,
    outcome: &mut Outcome,
) -> Compiled {
    let key = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(lean.version.as_bytes());
        hasher.update(b"\0");
        hasher.update(module.as_bytes());
        hasher.update(b"\0");
        hasher.update(text.as_bytes());
        hex(hasher.finalize().as_bytes())
    };
    let olean = stage.0.join(format!("{module}.olean"));
    if let Some(cache) = cache
        && cache.fetch(&key, module, text, &olean)
    {
        outcome.reused += 1;
        return Compiled::Ready;
    }
    if let Err(e) = fs::write(stage.0.join(format!("{module}.lean")), text) {
        return Compiled::Failed(e.to_string());
    }
    match compile(lean, &stage.0, module, timeout, cancel) {
        Ok(Some(())) => {
            outcome.compiled += 1;
            if let Some(cache) = cache {
                cache.store(&key, module, text, &olean);
            }
            Compiled::Ready
        }
        Ok(None) => Compiled::Cancelled,
        Err(reason) => Compiled::Failed(reason),
    }
}

fn lean_command(lean: &Lean, dir: &Path) -> Result<Command, String> {
    let search = env::join_paths([dir.to_path_buf(), lean.prefix.join("lib/lean")])
        .map_err(|e| e.to_string())?;
    let mut command = Command::new(&lean.binary);
    command
        .env_clear()
        .env("LEAN_PATH", search)
        .env("LEAN_ABORT_ON_PANIC", "1")
        .current_dir(dir);
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    Ok(command)
}

fn compile(
    lean: &Lean,
    dir: &Path,
    module: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Option<()>, String> {
    let mut command = lean_command(lean, dir)?;
    command
        .args(["-j1", "-M1024", "-o"])
        .arg(format!("{module}.olean"))
        .arg(format!("{module}.lean"));
    Ok(child(&mut command, timeout, cancel)?.map(|_| ()))
}

/// The `bv_decide` rendering of a model that `omega` did not prove. `None`
/// means cancelled.
fn bitvec_check(
    lean: &Lean,
    stage: &Stage,
    model: &Model,
    omega: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Option<ProofStatus> {
    let module = format!("{}_bv", model.module_name());
    let path = stage.0.join(format!("{module}.lean"));
    if let Err(e) = fs::write(&path, model.bitvec_module()) {
        return Some(ProofStatus::Failed {
            stage: "io".into(),
            reason: e.to_string(),
        });
    }
    let mut command = match lean_command(lean, &stage.0) {
        Ok(command) => command,
        Err(reason) => {
            return Some(ProofStatus::Failed {
                stage: "io".into(),
                reason,
            });
        }
    };
    command
        .args(["-j1", "-M1024"])
        .arg(format!("{module}.lean"));
    match child_output(&mut command, timeout, cancel) {
        Ok(None) => None,
        Ok(Some((true, _))) => Some(ProofStatus::Checked {
            detail: "bv_decide found no counterexample (native LRAT check through \
                     Lean.ofReduceBool; not replayed by the kernel auditor); omega \
                     did not prove it"
                .into(),
        }),
        Ok(Some((false, text))) => Some(match model.counterexample(&text) {
            Some(assignment) => ProofStatus::Refuted {
                counterexample: assignment,
            },
            None => ProofStatus::Failed {
                stage: "prove".into(),
                reason: bounded(format!(
                    "omega: {}; bv_decide: {}",
                    first_error(omega),
                    first_error(&text)
                )),
            },
        }),
        Err(reason) => Some(ProofStatus::Failed {
            stage: "prove".into(),
            reason: bounded(reason),
        }),
    }
}

fn first_error(text: &str) -> String {
    text.lines()
        .find(|line| line.contains("error"))
        .or_else(|| text.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or("")
        .chars()
        .take(400)
        .collect()
}

/// Run a child to completion. `Ok(None)` means cancelled (the child is
/// killed); a timeout or non-zero exit is an error with its first output line.
fn child(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, String> {
    match child_output(command, timeout, cancel)? {
        None => Ok(None),
        Some((true, out)) => Ok(Some(out.into_bytes())),
        Some((false, text)) => Err(first_error(&text)),
    }
}

/// Run a child; `(success, stdout or the combined output on failure)`.
fn child_output(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Option<(bool, String)>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so cancellation and timeouts also stop the Lean
    // process that `vesper proof-audit` launches.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    let mut process = command.spawn().map_err(|e| format!("start process: {e}"))?;
    let capture = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.by_ref().take(MAX_CAPTURE).read_to_end(&mut text);
                // Keep draining so a verbose child never sees a broken pipe.
                let _ = std::io::copy(&mut pipe, &mut std::io::sink());
            }
            text
        })
    };
    let stdout = capture(
        process
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = capture(
        process
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        if cancel.is_cancelled() || Instant::now() >= deadline {
            kill_tree(&mut process);
            let _ = (stdout.join(), stderr.join());
            if cancel.is_cancelled() {
                return Ok(None);
            }
            return Err(format!("process exceeded {} seconds", timeout.as_secs()));
        }
        match process.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(5)),
        }
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if status.success() {
        return Ok(Some((true, String::from_utf8_lossy(&out).into_owned())));
    }
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if !err.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&err));
    }
    if text.trim().is_empty() {
        text = status.to_string();
    }
    Ok(Some((false, text)))
}

/// Kill a child and every process in its tree, then reap the child.
fn kill_tree(process: &mut std::process::Child) {
    let pid = process.id().to_string();
    #[cfg(unix)]
    let tree = ["/bin/kill", "/usr/bin/kill"].iter().any(|kill| {
        Command::new(kill)
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    #[cfg(windows)]
    let tree = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    #[cfg(not(any(unix, windows)))]
    let tree = false;
    if !tree {
        let _ = process.kill();
    }
    let _ = process.wait();
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

/// The private `.olean` cache. Entries are content-addressed: the key covers
/// the Lean version, the module name and the exact module text, and the
/// stored source must equal the requested text byte for byte before the
/// `.olean` is reused.
struct Cache(PathBuf);
impl Cache {
    fn open(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root)
                .map_err(|e| e.to_string())?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                // Never reuse artifacts from a directory others can write.
                fs::set_permissions(root, fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("cache directory is not private: {e}"))?;
            }
        }
        Ok(Self(root.to_path_buf()))
    }
    fn entry(&self, key: &str) -> PathBuf {
        self.0.join(key)
    }
    fn fetch(&self, key: &str, module: &str, text: &str, into: &Path) -> bool {
        let dir = self.entry(key);
        let source = fs::read(dir.join(format!("{module}.lean")));
        if source.ok().as_deref() != Some(text.as_bytes()) {
            return false;
        }
        fs::copy(dir.join(format!("{module}.olean")), into).is_ok()
    }
    fn store(&self, key: &str, module: &str, text: &str, olean: &Path) {
        let dir = self.entry(key);
        let temp = self.0.join(format!(
            ".tmp-{key}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let written = private_dir(&temp).is_ok()
            && fs::write(temp.join(format!("{module}.lean")), text).is_ok()
            && fs::copy(olean, temp.join(format!("{module}.olean"))).is_ok();
        // Publish atomically; a concurrent writer of the same key wins.
        if !written || fs::rename(&temp, &dir).is_err() {
            let _ = fs::remove_dir_all(&temp);
        }
    }
    fn outcome(&self, key: &str, version: &str) -> Option<ProofStatus> {
        let text = fs::read_to_string(self.entry(key).join("outcome.json")).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        if value["lean"] != version {
            return None;
        }
        let detail = |field: &str| value[field].as_str().unwrap_or("").to_owned();
        match value["status"].as_str()? {
            "refuted" => Some(ProofStatus::Refuted {
                counterexample: detail("counterexample"),
            }),
            "checked_native" => Some(ProofStatus::Checked {
                detail: detail("detail"),
            }),
            _ => None,
        }
    }
    fn record_outcome(&self, key: &str, version: &str, status: &ProofStatus) {
        let value = match status {
            ProofStatus::Refuted { counterexample } => {
                serde_json::json!({"status":"refuted","counterexample":counterexample,"lean":version})
            }
            ProofStatus::Checked { detail } => {
                serde_json::json!({"status":"checked_native","detail":detail,"lean":version})
            }
            // Timeouts and toolchain failures are not cached.
            _ => return,
        };
        let dir = self.entry(key);
        if !dir.is_dir() && private_dir(&dir).is_err() {
            return;
        }
        let temp = dir.join(format!(".outcome-{}", std::process::id()));
        if fs::write(&temp, value.to_string()).is_ok() {
            let _ = fs::rename(&temp, dir.join("outcome.json"));
        }
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A private staging directory removed on drop.
struct Stage(PathBuf);
impl Stage {
    fn new() -> Result<Self, String> {
        for _ in 0..64 {
            let path = env::temp_dir().join(format!(
                "vesper-lens-proof-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match private_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("private proof staging directory: {e}")),
            }
        }
        Err("cannot allocate a private proof staging directory".into())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
