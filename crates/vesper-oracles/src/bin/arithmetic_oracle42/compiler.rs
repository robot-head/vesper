//! The system under test as a process: the `vesper` command line.
//!
//! The worker used to link the Rust compiler crate and call its parser,
//! checker, optimizer, evaluator and JIT in process. The oracle now drives
//! whatever `vesper` binary it is given, so it tests the Vesper compiler the
//! same way after the Rust one retires (#1397, #1929). Every invocation is a
//! fresh process with bounded output and a deadline; nothing here computes
//! an arithmetic answer.
#![forbid(unsafe_code)]

use super::{FailureKind, HarnessError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Bytes kept from each stream of one invocation. A pretty-printed HIR of
/// a 16 KiB source is well under this.
pub const OUTPUT_CAP: usize = 4 << 20;
/// Deadline of one invocation.
pub const INVOCATION_TIMEOUT: Duration = Duration::from_secs(60);

/// The optimizer setting of one execution (`--no-opt`, `--no-ast-opt`,
/// `--no-ir-opt` or neither).
pub fn optimizer_flags(ast_optimized: bool, ir_optimized: bool) -> &'static [&'static str] {
    match (ast_optimized, ir_optimized) {
        (false, false) => &["--no-opt"],
        (false, true) => &["--no-ast-opt"],
        (true, false) => &["--no-ir-opt"],
        (true, true) => &[],
    }
}

/// What one invocation printed.
#[derive(Debug)]
pub struct Invocation {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Invocation {
    /// The CLI's last stderr line (`vesper: <error>`), if any.
    pub fn error_line(&self) -> &str {
        std::str::from_utf8(&self.stderr)
            .unwrap_or("")
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
    }
}

fn invariant(detail: impl Into<String>) -> HarnessError {
    HarnessError::new(FailureKind::Invariant, detail)
}

fn drain(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut truncated = false;
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = OUTPUT_CAP.saturating_sub(kept.len());
                    truncated |= n > room;
                    kept.extend_from_slice(&buffer[..n.min(room)]);
                }
            }
        }
        (kept, truncated)
    })
}

struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// A `vesper` binary.
#[derive(Debug, Clone)]
pub struct Compiler {
    pub path: PathBuf,
}

impl Compiler {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Run `vesper <args>` with no stdin, in `dir`.
    pub fn invoke(&self, dir: &Path, args: &[&str]) -> Result<Invocation, HarnessError> {
        let mut child = Command::new(&self.path);
        child
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The hybrid reports statistics on stderr when this is set.
            .env_remove("VESPER_HYBRID_STATS");
        let mut child = Reaped(child.spawn().map_err(|e| {
            HarnessError::new(
                FailureKind::Io,
                format!("cannot start compiler {}: {e}", self.path.display()),
            )
        })?);
        let stdout = drain(child.0.stdout.take().expect("piped stdout"));
        let stderr = drain(child.0.stderr.take().expect("piped stderr"));
        let deadline = Instant::now() + INVOCATION_TIMEOUT;
        let status = loop {
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                drop(child);
                return Err(HarnessError::new(
                    FailureKind::Timeout,
                    format!("compiler invocation `{}` timed out", args.join(" ")),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let (stdout, out_cut) = stdout.join().map_err(|_| invariant("stdout reader"))?;
        let (stderr, err_cut) = stderr.join().map_err(|_| invariant("stderr reader"))?;
        if out_cut || err_cut {
            return Err(invariant("compiler output exceeds the worker profile"));
        }
        // A signal (no exit code) or a panic (101) is a crash, never an answer.
        match status.code() {
            Some(0) | Some(1) => {}
            code => {
                return Err(HarnessError::new(
                    FailureKind::Crash,
                    format!(
                        "compiler `{}` exited with {code:?}: {}",
                        args.join(" "),
                        String::from_utf8_lossy(&stderr)
                            .chars()
                            .take(1024)
                            .collect::<String>()
                    ),
                ));
            }
        }
        Ok(Invocation {
            success: status.success(),
            stdout,
            stderr,
        })
    }
}

/// What `vesper emit --emit hir --no-opt --json` says about a source.
#[derive(Debug)]
pub enum Front {
    /// The source parsed and checked; the parsed module as JSON.
    Module(serde_json::Value),
    /// The parser (`Parse`) or checker rejected it with these codes.
    Rejected { parse: bool, codes: Vec<String> },
}

/// Parse and check `source`, written to `dir/file`.
pub fn front(
    compiler: &Compiler,
    dir: &Path,
    file: &str,
    source: &str,
) -> Result<Front, HarnessError> {
    std::fs::write(dir.join(file), source)?;
    let out = compiler.invoke(dir, &["emit", "--emit", "hir", "--no-opt", "--json", file])?;
    if out.success {
        let module = serde_json::from_slice(&out.stdout)
            .map_err(|_| invariant("compiler HIR is not JSON"))?;
        return Ok(Front::Module(module));
    }
    let line = out.error_line();
    let parse = match line.strip_prefix("vesper: ") {
        Some(rest) if rest.ends_with(" syntax error(s)") => true,
        Some(rest) if rest.ends_with(" semantic error(s)") => false,
        _ => {
            return Err(invariant(format!(
                "compiler rejected the source without diagnostics: {line}"
            )));
        }
    };
    // `--json` prints the diagnostics as one JSON array line on stdout.
    let diagnostics: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| invariant("compiler diagnostics are not JSON"))?;
    let codes = diagnostics
        .as_array()
        .ok_or_else(|| invariant("compiler diagnostics are not a list"))?
        .iter()
        .map(|d| {
            d.get("code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| invariant("compiler diagnostic has no code"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Front::Rejected { parse, codes })
}

/// One execution's printed result, or the runtime error the CLI reported.
#[derive(Debug, PartialEq, Eq)]
pub enum Executed {
    Printed(String),
    FuelExhausted,
    Failed(String),
}

/// `vesper run|jit [optimizer flags] [--fuel N] file`.
pub fn execute(
    compiler: &Compiler,
    dir: &Path,
    file: &str,
    jit: bool,
    ast_optimized: bool,
    ir_optimized: bool,
    fuel: u64,
) -> Result<Executed, HarnessError> {
    let fuel = fuel.to_string();
    let mut args = vec![if jit { "jit" } else { "run" }];
    args.extend_from_slice(optimizer_flags(ast_optimized, ir_optimized));
    if !jit {
        args.extend(["--fuel", fuel.as_str()]);
    }
    args.push(file);
    let out = compiler.invoke(dir, &args)?;
    if !out.success {
        let line = out.error_line();
        return Ok(if line == "vesper: execution fuel exhausted" {
            Executed::FuelExhausted
        } else {
            Executed::Failed(line.chars().take(1024).collect())
        });
    }
    let text =
        std::str::from_utf8(&out.stdout).map_err(|_| invariant("compiler result is not UTF-8"))?;
    let Some(line) = text.strip_suffix('\n').filter(|l| !l.contains('\n')) else {
        return Err(invariant("compiler result is not exactly one line"));
    };
    Ok(Executed::Printed(line.to_owned()))
}

/// A private scratch directory, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Result<Self, HarnessError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-oracle42-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
