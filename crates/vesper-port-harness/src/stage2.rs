//! The Stage 2 port builder (bootstrap switch slice 3, lane E, #2666).
//!
//! `vesper-port-builder.wasm` (`//:vesper-port-builder.wasm`) is
//! packages/vesper-port-builder built by Stage 2 and composed with the
//! Cranelift component, so the pinned wasmtime CLI runs it with nothing
//! else. It writes `vesper build --lib`'s three outputs for a port package,
//! or declines some of them; a declined port is built by the Rust compiler
//! instead, so the outputs never mix the two compilers.
//!
//! [`BUILDER_ENV`] turns it on for [`crate::Port::build`];
//! `vesper-port-build` (src/bin) does the same for
//! tools/bazel/vesper_port.bzl under `--config=stage2-ports`.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path of `vesper-port-builder.wasm`: try it before the Rust compiler.
pub const BUILDER_ENV: &str = "VESPER_PORT_BUILDER";
/// The wasmtime CLI that runs the builder ([`WASMTIME_FALLBACK_ENV`], then
/// `wasmtime` on `PATH`, when unset).
pub const WASMTIME_ENV: &str = "VESPER_PORT_WASMTIME";
/// The variable the Stage 2 tasks and tests already name the CLI by.
pub const WASMTIME_FALLBACK_ENV: &str = "WASMTIME_BIN";
/// Set to `1` to print, as a Cargo warning, why the builder declined.
pub const VERBOSE_ENV: &str = "VESPER_PORT_BUILDER_VERBOSE";

/// The guest directory the builder sees the shared ancestor of its inputs
/// and outputs at.
const GUEST_ROOT: &str = "/w";
/// The status file's tags (packages/vesper-port-builder/src/lib.ves).
const TAG_EXPORT: &str = "export";
const TAG_OBJECT: &str = "object";
/// The object reason of a run that trapped in native-lib
/// (`OBJECT_TRAPPED` of @vesper/port-builder).
pub const OBJECT_TRAPPED: u64 = 4;
/// A decline reason of 0: the files were written.
const BUILT: u64 = 0;

/// What the builder did with one port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// `@vesper/export-build`'s decline reason (`DECLINE_*`), 0 when the
    /// interface and binding were written.
    pub export: u64,
    /// The object's (`OBJECT_*` of @vesper/port-builder), 0 when written.
    pub object: u64,
}

impl Outcome {
    /// Every output was written.
    pub fn built(&self) -> bool {
        self.export == BUILT && self.object == BUILT
    }
}

/// One `vesper build --lib` the builder runs.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub builder: &'a Path,
    pub wasmtime: &'a Path,
    /// A Loom package directory (a `.ves` file is wrapped in one first).
    pub package: &'a Path,
    pub library: &'a str,
    pub target: &'a str,
    pub object: &'a Path,
    pub interface: &'a Path,
    pub binding: &'a Path,
    pub rust_types: Option<&'a str>,
    /// Scratch space for the status file and a wrapped `.ves` file.
    pub scratch: &'a Path,
}

/// The wasmtime CLI: [`WASMTIME_ENV`], [`WASMTIME_FALLBACK_ENV`], or
/// `wasmtime` on `PATH`.
pub fn wasmtime() -> PathBuf {
    std::env::var_os(WASMTIME_ENV)
        .or_else(|| std::env::var_os(WASMTIME_FALLBACK_ENV))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wasmtime"))
}

/// Run the builder on one port. `Ok` holds what it built or declined; an
/// `Err` is a failure to run it at all (missing CLI, trap, unreadable
/// status), which callers also answer with the Rust compiler.
pub fn build(r: &Request<'_>) -> Result<Outcome, String> {
    std::fs::create_dir_all(r.scratch).map_err(|e| format!("{}: {e}", r.scratch.display()))?;
    // Regular files only: the builder reads its input through a WASI
    // preopen, which refuses absolute symlinks (Bazel's sandbox inputs).
    let package = if crate::is_package(r.package) {
        let copy = crate::copy_package(r.package, &r.scratch.join("input"))?;
        std::fs::canonicalize(&copy).map_err(|e| format!("{}: {e}", copy.display()))?
    } else {
        wrap_file(r.package, r.library, &r.scratch.join("wrapped"))?
    };
    let status = r.scratch.join(format!("vesper_{}.status", r.library));
    let _ = std::fs::remove_file(&status);
    for out in [r.object, r.interface, r.binding] {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let outputs = [r.object, r.interface, r.binding, status.as_path()]
        .into_iter()
        .map(absolute)
        .collect::<Result<Vec<_>, _>>()?;
    let mut dirs = crate::package_closure(&package)?;
    dirs.extend(
        outputs
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf)),
    );
    let base = crate::common_ancestor(&dirs);
    let guest = |p: &Path| guest_path(&base, p);
    let mut command = Command::new(r.wasmtime);
    command
        .arg("run")
        .arg("--dir")
        .arg(format!("{}::{GUEST_ROOT}", base.display()))
        .args(["--argv0", "port-builder"])
        .arg(r.builder)
        .arg(guest(&package)?)
        .arg(r.library)
        .arg(r.target);
    for out in &outputs {
        command.arg(guest(out)?);
    }
    if let Some(types) = r.rust_types {
        command.args(["--rust-types", types]);
    }
    let ran = crate::run(&mut command, r.wasmtime);
    let text = std::fs::read_to_string(&status);
    match (ran, text) {
        (Ok(()), Ok(text)) => parse_status(&text)
            .ok_or_else(|| format!("{}: malformed status {text:?}", status.display())),
        (Ok(()), Err(e)) => Err(format!("{}: {e}", status.display())),
        // A run that trapped while building the object (Stage 2's memory
        // ran out in native-lib) leaves a status naming that: the exports
        // stand, and the object is declined.
        (Err(e), Ok(text)) => parse_status(&text)
            .filter(|o| o.object == OBJECT_TRAPPED)
            .ok_or(e),
        (Err(e), Err(_)) => Err(e),
    }
}

/// The status file's two reasons.
pub fn parse_status(text: &str) -> Option<Outcome> {
    let mut export = None;
    let mut object = None;
    for line in text.lines() {
        let (tag, reason) = line.split_once(' ')?;
        let reason = reason.parse().ok()?;
        match tag {
            TAG_EXPORT => export = Some(reason),
            TAG_OBJECT => object = Some(reason),
            _ => return None,
        }
    }
    Some(Outcome {
        export: export?,
        object: object?,
    })
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent = std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    Ok(parent.join(path.file_name().ok_or("an output path has no file name")?))
}

/// `path` under [`GUEST_ROOT`], with `/` separators.
fn guest_path(base: &Path, path: &Path) -> Result<String, String> {
    let rel = path
        .strip_prefix(base)
        .map_err(|_| format!("{} is not under {}", path.display(), base.display()))?;
    let mut out = String::from(GUEST_ROOT);
    for part in rel.components() {
        out.push('/');
        out.push_str(
            part.as_os_str()
                .to_str()
                .ok_or_else(|| format!("{}: not UTF-8", path.display()))?,
        );
    }
    Ok(out)
}

/// A single-file port as a package whose `src/lib.ves` is the file, as
/// tests/self_host_codegen_export.rs builds it for @vesper/export-build.
fn wrap_file(file: &Path, library: &str, dir: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let manifest = format!(
        "[package]\nname = \"@port/{}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        library.replace('_', "-")
    );
    crate::write(&dir.join("Loom.toml"), &manifest)?;
    crate::write(&dir.join("src").join("lib.ves"), &text)?;
    std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_parse() {
        assert_eq!(
            parse_status("export 0\nobject 1\n"),
            Some(Outcome {
                export: 0,
                object: 1
            })
        );
        assert!(parse_status("export 0\nobject 0\n").unwrap().built());
        assert_eq!(parse_status("export 0\n"), None);
        assert_eq!(parse_status("export x\nobject 0\n"), None);
        assert_eq!(parse_status("other 0\nobject 0\n"), None);
    }

    #[test]
    fn guest_paths_use_forward_slashes() {
        let base = Path::new("/a/b");
        assert_eq!(
            guest_path(base, &base.join("c").join("d.o")).unwrap(),
            "/w/c/d.o"
        );
        assert!(guest_path(base, Path::new("/x")).is_err());
    }
}
