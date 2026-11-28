//! The shipped `vesper`, asked over a process boundary (#2670, part of
//! #1397).
//!
//! Tools that need compiler services (checking, interfaces, runs) no longer
//! link the Rust compiler library. They run `vesper tool-facts <kind>`, which
//! reads the source file named by `--source` and writes one JSON document to
//! `--out` (see `src/tool_facts.rs` in the root package). The standard streams
//! stay with the tool, so a program the compiler runs prints where it did.
//!
//! The compiler is the `vesper` that started the tool (`VESPER_COMPILER`, set
//! by `vesper <tool>`), else a `vesper` beside the tool, else one on `PATH`.
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

/// The variable `vesper <tool>` sets to its own path (`COMPILER_ENV` in the
/// root package's `src/tool_commands.rs`).
pub const COMPILER_ENV: &str = "VESPER_COMPILER";

/// The hidden compiler command that answers the tools.
const FACTS_COMMAND: &str = "tool-facts";

/// The schema of the answer (`SCHEMA` in `src/tool_facts.rs`).
const FACTS_SCHEMA: &str = "vesper.tool-facts/1";

/// The field an answer carries when the compiler service itself failed.
const ERROR_FIELD: &str = "error";

/// The `vesper` executable's file name on this host.
fn compiler_name() -> String {
    format!("vesper{}", env::consts::EXE_SUFFIX)
}

/// Where the compiler is: `VESPER_COMPILER`, beside this tool, or on `PATH`.
pub fn locate() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os(COMPILER_ENV).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let name = compiler_name();
    if let Ok(exe) = env::current_exe() {
        let mut dirs = vec![];
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
        if let Some(dir) = exe
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
        for dir in dirs {
            // Cargo puts the tools beside `vesper`. Bazel puts them in
            // `<bin>/crates/vesper-tools/bin/`, and `vesper` in `<bin>/bin/`.
            for candidate in [dir.join(&name), dir.join("../../../bin").join(&name)] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .map(|dir| dir.join(&name))
                .find(|candidate| candidate.is_file())
        })
        .ok_or_else(|| {
            format!(
                "this tool asks the vesper compiler, which was not found: set {COMPILER_ENV}, or \
                 install vesper beside the tool or on PATH"
            )
        })
}

/// A private file for one answer, removed when dropped.
struct Answer(PathBuf);

impl Answer {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Answer(env::temp_dir().join(format!(
            "vesper-tool-facts-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Ask the compiler for `kind` facts about the source file `source`, with
/// extra `options`. An answer whose service failed (`error`) comes back as
/// `Err` with the compiler's message, as the in-process call returned it.
pub fn facts(kind: &str, source: &Path, options: &[&str]) -> Result<Value, String> {
    let compiler = locate()?;
    let answer = Answer::new();
    let status = Command::new(&compiler)
        .arg(FACTS_COMMAND)
        .arg(kind)
        .arg("--source")
        .arg(source)
        .arg("--out")
        .arg(&answer.0)
        .args(options)
        .status()
        .map_err(|e| format!("{}: {e}", compiler.display()))?;
    if !status.success() {
        return Err(format!(
            "{} {FACTS_COMMAND} {kind} failed ({status})",
            compiler.display()
        ));
    }
    let bytes = fs::read(&answer.0).map_err(|e| format!("{}: {e}", answer.0.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{FACTS_COMMAND} {kind}: invalid answer: {e}"))?;
    if value.get("schema").and_then(Value::as_str) != Some(FACTS_SCHEMA) {
        return Err(format!(
            "{FACTS_COMMAND} {kind}: the compiler answered another schema; use a matching vesper"
        ));
    }
    if let Some(error) = value.get(ERROR_FIELD) {
        return Err(error.as_str().unwrap_or_default().to_owned());
    }
    Ok(value)
}

/// The parse or check error count an answer reports, if the source did not
/// parse (`syntax`) or check (`semantic`).
pub enum Rejected {
    Syntax(u64),
    Semantic(u64),
}

/// Whether `facts` says the source failed to parse or check.
pub fn rejected(facts: &Value) -> Option<Rejected> {
    let count = |field: &str| facts.get(field).and_then(Value::as_u64);
    count("syntax_errors")
        .map(Rejected::Syntax)
        .or_else(|| count("semantic_errors").map(Rejected::Semantic))
}
