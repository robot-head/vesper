//! Proof code lenses (§21.1.5, §21.3 Layer 5): run the existing Lean extraction
//! and the existing empty-axiom kernel audit for one function on demand.
//!
//! The obligation audited for function `f` is exactly: the module extracted
//! from the checked source compiles under Lean 4.24.0, and the theorem
//! `VesperLens.obligation : @f = @f` replays through the kernel with an empty
//! transitive axiom closure. That establishes that Lean accepts the extracted
//! definition and that its kernel-visible closure uses no axioms (no `sorry`,
//! no classical choice). It is not a functional-correctness theorem, not a
//! theorem about the Rust compiler, and for `partial def` extractions the
//! kernel sees only the opaque constant, not its body.
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

pub(crate) const MODULE: &str = "VesperLensObligation";
const THEOREM: &str = "VesperLens.obligation";
const MAX_FUNCTIONS: usize = 256;
const MAX_CAPTURE: u64 = 8 * 1024 * 1024;

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

/// A self-contained audit request; it owns everything a worker thread needs.
pub(crate) struct Job {
    pub source: [u8; 32],
    pub function: String,
    module: String,
    launcher: String,
    auditor: Option<PathBuf>,
    timeout: u64,
}

pub(crate) fn prepare(
    snapshot: &CheckedSource,
    function: &str,
    launcher: &str,
    auditor: Option<&Path>,
    timeout: u64,
) -> Result<Job, String> {
    let (text, functions) = candidates(snapshot)?;
    if !functions.iter().any(|(name, _)| name == function) {
        return Err(format!("function '{function}' has no Lean extraction"));
    }
    if function.contains('»') {
        return Err("function name cannot be quoted for Lean".into());
    }
    let constant = format!("@«Vesper».«{function}»");
    let module = format!(
        "{text}\n\nnamespace VesperLens\ntheorem obligation : {constant} = {constant} := rfl\nend VesperLens\n"
    );
    Ok(Job {
        source: snapshot.source_hash(),
        function: function.to_owned(),
        module,
        launcher: launcher.to_owned(),
        auditor: auditor.map(Path::to_path_buf),
        timeout: timeout.clamp(1, 300),
    })
}

/// Run the job, polling `cancel` while Lean or the auditor runs. A cancelled
/// job kills its child process and reports nothing.
pub(crate) fn run(job: &Job, cancel: &CancelToken) -> Option<ProofStatus> {
    match run_stages(job, cancel) {
        Ok(Some(detail)) => Some(ProofStatus::Proved { detail }),
        Ok(None) => None,
        Err((stage, reason)) => Some(ProofStatus::Failed {
            stage: stage.into(),
            reason: bounded(reason),
        }),
    }
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

fn run_stages(job: &Job, cancel: &CancelToken) -> StageResult<Option<String>> {
    let timeout = Duration::from_secs(job.timeout);
    let auditor = job.auditor.as_ref().ok_or((
        "toolchain",
        "the vesper proof auditor is unavailable".to_owned(),
    ))?;
    let Some(prefix) = child(
        Command::new(&job.launcher).arg("--print-prefix"),
        Duration::from_secs(30),
        cancel,
    )
    .map_err(|e| ("toolchain", e))?
    else {
        return Ok(None);
    };
    let prefix = PathBuf::from(String::from_utf8_lossy(&prefix).trim());
    let binary = prefix
        .join("bin")
        .join(if cfg!(windows) { "lean.exe" } else { "lean" });
    let Some(version) = child(Command::new(&binary).arg("--version"), timeout, cancel)
        .map_err(|e| ("toolchain", e))?
    else {
        return Ok(None);
    };
    if !String::from_utf8_lossy(&version).contains("version 4.24.0,") {
        return Err(("toolchain", "proof lenses require Lean 4.24.0".into()));
    }
    let stage = Stage::new().map_err(|e| ("io", e))?;
    fs::write(stage.0.join(format!("{MODULE}.lean")), &job.module)
        .map_err(|e| ("io", e.to_string()))?;
    let search = env::join_paths([stage.0.clone(), prefix.join("lib/lean")])
        .map_err(|e| ("io", e.to_string()))?;
    let mut compile = Command::new(&binary);
    compile
        .env_clear()
        .env("LEAN_PATH", search)
        .env("LEAN_ABORT_ON_PANIC", "1")
        .current_dir(&stage.0)
        .args(["-j1", "-M1024", "-o"])
        .arg(format!("{MODULE}.olean"))
        .arg(format!("{MODULE}.lean"));
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = env::var_os(key) {
            compile.env(key, value);
        }
    }
    if child(&mut compile, timeout, cancel)
        .map_err(|e| ("compile", e))?
        .is_none()
    {
        return Ok(None);
    }
    // The existing `vesper proof-audit` kernel replay with an empty policy.
    let Some(report) = child(
        Command::new(auditor)
            .arg("proof-audit")
            .arg(MODULE)
            .arg("--root")
            .arg(&stage.0)
            .args(["--theorem", THEOREM, "--json", "--timeout"])
            .arg(job.timeout.to_string())
            .arg("--lean")
            .arg(&binary),
        timeout + Duration::from_secs(5),
        cancel,
    )
    .map_err(|e| ("kernel", e))?
    else {
        return Ok(None);
    };
    let report: serde_json::Value =
        serde_json::from_slice(&report).map_err(|e| ("kernel", format!("audit report: {e}")))?;
    let accepted = report["accepted"] == true
        && report["kernel_replayed"] == true
        && report["forbidden_axioms"]
            .as_array()
            .is_some_and(|axioms| axioms.is_empty());
    if !accepted {
        return Err((
            "kernel",
            "the obligation failed the empty-axiom kernel audit".into(),
        ));
    }
    Ok(Some(format!(
        "kernel replayed {} constants with an empty axiom policy",
        report["checked_constants"].as_u64().unwrap_or(0)
    )))
}

/// Run a child to completion. `Ok(None)` means cancelled (the child is
/// killed); a timeout or non-zero exit is an error with its first output line.
fn child(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
            let _ = process.kill();
            let _ = process.wait();
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
        return Ok(Some(out));
    }
    let text = String::from_utf8_lossy(if err.is_empty() { &out } else { &err }).into_owned();
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    Err(format!("{status}: {first}"))
}

/// A private staging directory removed on drop.
struct Stage(PathBuf);
impl Stage {
    fn new() -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let path = env::temp_dir().join(format!(
                "vesper-lens-proof-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let created = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    fs::DirBuilder::new().mode(0o700).create(&path)
                }
                #[cfg(not(unix))]
                {
                    fs::create_dir(&path)
                }
            };
            match created {
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
