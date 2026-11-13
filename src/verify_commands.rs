//! Explicit formatter-document model checking, never program verification.
use crate::proof_commands::{self, LeanToolchain, Stage};
use serde::Serialize;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use vesper_fmt::{Limits, ProofLimits, format_for_verification};

const MODEL: &str = include_str!("../lean/StylusAndLintSafety.lean");
const SCHEMA: &str = "vesper.format-proof/1";
const MAX_INPUT: usize = 128 * 1024;
const MAX_JSON: usize = 2 * 1024 * 1024;
const HELP: &str = "Usage: vesper verify SOURCE --format [--json | --emit-lean] [--lean PATH] [--timeout SECONDS]\n\nKernel-check a formatter document-algebra comment-conservation model with Lean 4.24.0.\nThe witness projects the actual formatter Doc and preserves the input file.\nThis scope does not certify program semantics or refinement of the Rust renderer.\nSource must be supported ordinary or fixed Expr macro syntax; recovery proofs are unavailable.\n--emit-lean prints the AST-generated witness only after compilation and empty-axiom kernel replay.\n--timeout bounds each Lean process (1 to 300 seconds; default 30).\nThe selected Lean toolchain and artifact parser remain trusted.\n";

#[derive(Debug)]
struct Failure {
    stage: &'static str,
    message: String,
}
impl Failure {
    fn new(stage: &'static str, message: impl std::fmt::Display) -> Self {
        let message = message.to_string();
        let mut end = message.len().min(4096);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            stage,
            message: message[..end].to_owned(),
        }
    }
}
struct Options {
    source: PathBuf,
    lean: String,
    timeout: u64,
    json: bool,
    emit_lean: bool,
}
fn options(args: Vec<String>) -> Result<Options, Failure> {
    let size = args
        .iter()
        .try_fold(0usize, |n, arg| n.checked_add(arg.len()));
    if args.len() > 20 || size.is_none_or(|n| n > 16 * 1024) {
        return Err(Failure::new(
            "config",
            "verification argument limit exceeded",
        ));
    }
    let mut source = None;
    let mut format = false;
    let mut json = false;
    let mut emit_lean = false;
    let mut lean = None;
    let mut timeout = None;
    let mut positional = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if !positional {
            match arg.as_str() {
                "--format" => {
                    if format {
                        return Err(Failure::new("config", "duplicate --format"));
                    }
                    format = true;
                    continue;
                }
                "--json" => {
                    json = true;
                    continue;
                }
                "--emit-lean" => {
                    emit_lean = true;
                    continue;
                }
                "--lean" => {
                    if lean.is_some() {
                        return Err(Failure::new("config", "duplicate --lean"));
                    }
                    lean = Some(
                        args.next()
                            .ok_or_else(|| Failure::new("config", "--lean requires a path"))?,
                    );
                    continue;
                }
                "--timeout" => {
                    if timeout.is_some() {
                        return Err(Failure::new("config", "duplicate --timeout"));
                    }
                    let value = args
                        .next()
                        .ok_or_else(|| Failure::new("config", "--timeout requires seconds"))?;
                    let seconds = value
                        .parse::<u64>()
                        .ok()
                        .filter(|n| (1..=300).contains(n))
                        .ok_or_else(|| {
                            Failure::new("config", "--timeout must be between 1 and 300 seconds")
                        })?;
                    timeout = Some(seconds);
                    continue;
                }
                "--" => {
                    positional = true;
                    continue;
                }
                _ if arg.starts_with('-') => {
                    return Err(Failure::new("config", "unknown verification option"));
                }
                _ => {}
            }
        }
        if arg.len() > 4096 || source.replace(PathBuf::from(arg)).is_some() {
            return Err(Failure::new(
                "config",
                "verify accepts one bounded source path",
            ));
        }
    }
    if !format || json && emit_lean {
        return Err(Failure::new(
            "config",
            "--format is required; --json and --emit-lean cannot be combined",
        ));
    }
    let lean = lean
        .or_else(|| env::var("VESPER_LEAN").ok())
        .unwrap_or_else(|| "lean".into());
    if lean.is_empty() || lean.len() > 4096 {
        return Err(Failure::new("config", "invalid Lean executable path"));
    }
    Ok(Options {
        source: source.ok_or_else(|| Failure::new("config", "verify requires a source file"))?,
        lean,
        timeout: timeout.unwrap_or(30),
        json,
        emit_lean,
    })
}
fn read_source(path: &Path) -> Result<String, Failure> {
    let meta = fs::symlink_metadata(path).map_err(|e| Failure::new("io", e))?;
    if !meta.is_file() {
        return Err(Failure::new(
            "io",
            "verification requires a regular file without final symlinks",
        ));
    }
    let mut open = OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        open.custom_flags(0x0020_0000);
    }
    let file = open.open(path).map_err(|e| Failure::new("io", e))?;
    let meta = file.metadata().map_err(|e| Failure::new("io", e))?;
    if !meta.is_file() || meta.len() > MAX_INPUT as u64 {
        return Err(Failure::new(
            "resource",
            "verification requires a regular file of at most 128 KiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::new("io", e))?;
    if bytes.len() > MAX_INPUT {
        return Err(Failure::new(
            "resource",
            "verification source exceeds 128 KiB",
        ));
    }
    String::from_utf8(bytes).map_err(|_| Failure::new("parse", "verification source must be UTF-8"))
}
fn compile(
    toolchain: &LeanToolchain,
    stage: &Path,
    name: &str,
    timeout: u64,
) -> Result<(), Failure> {
    let search = env::join_paths([stage.to_path_buf(), toolchain.prefix.join("lib/lean")])
        .map_err(|e| Failure::new("toolchain", e))?;
    let mut command = Command::new(&toolchain.binary);
    command
        .env_clear()
        .env("LEAN_PATH", search)
        .env("LEAN_ABORT_ON_PANIC", "1")
        .current_dir(stage);
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    proof_commands::execute(
        command
            .args(["-j1", "-M1024", "-o"])
            .arg(format!("{name}.olean"))
            .arg(format!("{name}.lean")),
        Duration::from_secs(timeout),
    )
    .map_err(|e| Failure::new("compile", e))?;
    Ok(())
}
#[derive(Serialize)]
struct Success {
    schema: &'static str,
    status: &'static str,
    scope: &'static str,
    source_blake3: String,
    output_blake3: String,
    input_bytes: usize,
    output_bytes: usize,
    document_nodes: usize,
    comment_tokens: usize,
    lean_version: String,
    audit: serde_json::Value,
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_JSON)
        {
            return Err(io::Error::other("verification JSON exceeds 2 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn execute(options: &Options) -> Result<Vec<u8>, Failure> {
    let source = read_source(&options.source)?;
    let verification = format_for_verification(&source, Limits::default(), ProofLimits::default())
        .map_err(|e| Failure::new("format_model", e))?;
    let witness = verification
        .emit_lean(ProofLimits::default())
        .map_err(|e| Failure::new("emit", e))?;
    let toolchain =
        proof_commands::lean_toolchain(&options.lean).map_err(|e| Failure::new("toolchain", e))?;
    let stage = Stage::new().map_err(|e| Failure::new("io", e))?;
    fs::write(stage.0.join("StylusAndLintSafety.lean"), MODEL)
        .map_err(|e| Failure::new("io", e))?;
    fs::write(stage.0.join("FormatWitness.lean"), &witness).map_err(|e| Failure::new("io", e))?;
    compile(&toolchain, &stage.0, "StylusAndLintSafety", options.timeout)?;
    compile(&toolchain, &stage.0, "FormatWitness", options.timeout)?;
    let audit = proof_commands::audit_generated(
        &stage.0,
        "FormatWitness",
        &[
            "Vesper.StylusGenerated.comments_preserved",
            "Vesper.StylusSafety.projection_narrowing_preserves_disjointness",
        ],
        &options.lean,
        options.timeout,
    )
    .map_err(|e| Failure::new("kernel", e))?;
    if options.emit_lean {
        return Ok(witness.into_bytes());
    }
    if !options.json {
        return Ok(b"Formatter document model: kernel checked; no program-semantics or Rust-refinement claim.\n".to_vec());
    }
    let report = Success {
        schema: SCHEMA,
        status: "kernel_checked",
        scope: "formatter_document_model",
        source_blake3: blake3::Hash::from_bytes(verification.source_hash())
            .to_hex()
            .to_string(),
        output_blake3: blake3::Hash::from_bytes(verification.output_hash())
            .to_hex()
            .to_string(),
        input_bytes: source.len(),
        output_bytes: verification.output().len(),
        document_nodes: verification.model().node_count(),
        comment_tokens: verification.model().comments().len(),
        lean_version: audit["lean_version"]
            .as_str()
            .ok_or_else(|| Failure::new("kernel", "audit omitted its actual Lean version"))?
            .to_owned(),
        audit,
    };
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, &report).map_err(|e| Failure::new("resource", e))?;
    output
        .write_all(b"\n")
        .map_err(|e| Failure::new("resource", e))?;
    Ok(output.0)
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| matches!(a.as_str(), "--help" | "-h"))
    {
        print!("{HELP}");
        return Ok(());
    }
    let json = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--json");
    let result = options(args).and_then(|options| execute(&options));
    match result {
        Ok(output) => io::stdout()
            .lock()
            .write_all(&output)
            .map_err(|e| e.to_string()),
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema":SCHEMA,"status":"rejected","stage":error.stage,"message":error.message})
                );
            }
            Err(format!(
                "formatter model verification failed: {}",
                error.message
            ))
        }
    }
}
