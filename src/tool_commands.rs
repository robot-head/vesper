//! Cargo-style tool dispatch (self-hosting SH0 lane 2, #1124).
//!
//! Tool subcommands that the compiler itself does not need live in separate
//! executables (crates/vesper-tools), so their libraries stay out of the
//! compiler's closure. `vesper <tool> ARGS...` runs `vesper-<tool> ARGS...`
//! with the same arguments, standard streams and exit status. The executable
//! is looked up next to the running `vesper` first (Bazel: in
//! `crates/vesper-tools/bin/` beside `bin/vesper`), then on `PATH`.
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

/// Subcommands served by `vesper-<name>` tool binaries. `vesper --help` still
/// lists each of them.
pub const TOOLS: &[&str] = &[
    "attest",
    "audit",
    "bench",
    "cas",
    "fuzz",
    "grammar",
    "highlight",
    "json",
    "migrate",
    "publish",
    "replay",
    "resolve",
    "sbom",
    "semver",
    "shot",
    "simd",
    "top",
    "traceparent",
];

/// The tool binary `vesper build --tee` runs to measure an executable.
pub const TEE_TOOL: &str = "tee";

/// The tool binary `vesper verify --refute` runs (it links Crucible).
pub const REFUTE_TOOL: &str = "refute";

/// The file name of the executable for `tool` on this host.
pub fn binary_name(tool: &str) -> String {
    format!("vesper-{tool}{}", env::consts::EXE_SUFFIX)
}

/// Where `vesper-<tool>` is: beside the running executable, else on `PATH`.
pub fn locate(tool: &str) -> Option<PathBuf> {
    let name = binary_name(tool);
    if let Ok(exe) = env::current_exe() {
        let mut dirs = vec![];
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
        // A symlinked `vesper` (an install prefix, a Bazel runfiles tree)
        // also finds tools beside its target.
        if let Some(dir) = exe
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
        for dir in dirs {
            // Cargo puts the tools beside `vesper`. Bazel builds them as
            // data of `//:bin/vesper` at `<bin>/crates/vesper-tools/bin/`,
            // next to `<bin>/bin/vesper` (outputs and runfiles alike).
            for candidate in [
                dir.join(&name),
                dir.join("../crates/vesper-tools/bin").join(&name),
            ] {
                if executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(&name))
        .find(|candidate| executable(candidate))
}

/// A regular file the host would run: on Unix, one with an execute bit, so
/// a stray non-executable `vesper-<tool>` cannot shadow a later PATH entry.
fn executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// The error for a known tool whose binary is missing.
pub fn missing(tool: &str) -> String {
    format!(
        "`vesper {tool}` is provided by the `vesper-{tool}` tool, which was not found next to \
         the vesper executable or on PATH; build it with `cargo build -p vesper-tools` (or \
         `bazel build //crates/vesper-tools:all`) and install it beside vesper"
    )
}

/// Run `vesper-<tool>` with `args`, inheriting the standard streams.
pub fn run(tool: &str, args: &[OsString]) -> Result<ExitStatus, String> {
    let binary = locate(tool).ok_or_else(|| missing(tool))?;
    Command::new(&binary)
        .args(args)
        .status()
        .map_err(|e| format!("{}: {e}", binary.display()))
}

/// The exit code that reproduces `status` for the caller.
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

/// Dispatch `vesper <command> ...` before the compiler CLI starts. A command
/// listed in [`TOOLS`] always goes to its tool (or fails, naming the missing
/// binary). Any other command the compiler does not know runs
/// `vesper-<command>` when one exists, as Cargo does for external
/// subcommands. Returns the exit code, or `None` to continue in the compiler.
pub fn dispatch(known_by_compiler: impl Fn(&str) -> bool) -> Option<i32> {
    let mut args = env::args_os().skip(1);
    let command = args.next()?;
    let command = command.to_str()?.to_owned();
    if command.starts_with('-') || known_by_compiler(&command) {
        return None;
    }
    let rest: Vec<OsString> = args.collect();
    if !TOOLS.contains(&command.as_str()) {
        // Unknown commands keep the compiler's own error unless a tool exists.
        if command.is_empty()
            || !command
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        locate(&command)?;
    }
    Some(match run(&command, &rest) {
        Ok(status) => exit_code(status),
        Err(e) => {
            eprintln!("vesper: {e}");
            1
        }
    })
}
