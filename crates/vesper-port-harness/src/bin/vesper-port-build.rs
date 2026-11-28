//! `vesper-port-build`: one port's `vesper build --lib`, by the Stage 2
//! port builder first and the Rust compiler where it declines
//! (bootstrap switch slice 3, lane E, #2666). tools/bazel/vesper_port.bzl
//! runs it under `--config=stage2-ports`; build scripts get the same from
//! [`vesper_port_harness::stage2::BUILDER_ENV`].
//!
//! ```text
//! vesper-port-build --builder <wasm> --wasmtime <cli> --vesper <compiler>
//!     --library <name> [--target <triple>] --input <package or .ves>
//!     --object <out> --interface <out> --binding <out>
//!     [--rust-types <path>]
//! ```
//!
//! Without `--target` (a configuration with no Vesper target) the Rust
//! compiler builds the port for the host, as before.
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use vesper_port_harness::stage2;

/// The scratch directory beside the object, for the status file.
const SCRATCH_SUFFIX: &str = "stage2";

#[derive(Default)]
struct Args {
    builder: Option<PathBuf>,
    wasmtime: Option<PathBuf>,
    vesper: Option<PathBuf>,
    library: Option<String>,
    target: Option<String>,
    input: Option<PathBuf>,
    object: Option<PathBuf>,
    interface: Option<PathBuf>,
    binding: Option<PathBuf>,
    rust_types: Option<String>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--builder" => a.builder = Some(value.into()),
            "--wasmtime" => a.wasmtime = Some(value.into()),
            "--vesper" => a.vesper = Some(value.into()),
            "--library" => a.library = Some(value),
            "--target" => a.target = Some(value).filter(|t| !t.is_empty()),
            "--input" => a.input = Some(value.into()),
            "--object" => a.object = Some(value.into()),
            "--interface" => a.interface = Some(value.into()),
            "--binding" => a.binding = Some(value.into()),
            "--rust-types" => a.rust_types = Some(value),
            _ => return Err(format!("unknown option '{flag}'")),
        }
    }
    Ok(a)
}

fn need<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{flag} is required"))
}

fn scratch(object: &Path) -> PathBuf {
    let mut name = object.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(SCRATCH_SUFFIX);
    object.with_file_name(name)
}

fn main_inner() -> Result<(), String> {
    let a = parse()?;
    let vesper = need(a.vesper, "--vesper")?;
    let library = need(a.library, "--library")?;
    let input = need(a.input, "--input")?;
    let object = need(a.object, "--object")?;
    let interface = need(a.interface, "--interface")?;
    let binding = need(a.binding, "--binding")?;
    if let (Some(builder), Some(wasmtime), Some(target)) = (&a.builder, &a.wasmtime, &a.target) {
        let request = stage2::Request {
            builder,
            wasmtime,
            package: &input,
            library: &library,
            target,
            object: &object,
            interface: &interface,
            binding: &binding,
            rust_types: a.rust_types.as_deref(),
            scratch: &scratch(&object),
        };
        match stage2::build(&request) {
            Ok(o) if o.built() => return Ok(()),
            outcome => {
                if std::env::var(stage2::VERBOSE_ENV).as_deref() == Ok("1") {
                    eprintln!(
                        "vesper-port-build: {library} left to the Rust compiler: {outcome:?}"
                    );
                }
            }
        }
    }
    let mut command = Command::new(&vesper);
    command.args(["build", "--lib", "--lib-name", &library]);
    if let Some(target) = &a.target {
        command.args(["--target", target]);
    }
    command
        .arg(&input)
        .arg("-o")
        .arg(&object)
        .arg("--interface")
        .arg(&interface)
        .arg("--rust-binding")
        .arg(&binding);
    if let Some(types) = &a.rust_types {
        command.args(["--rust-types", types]);
    }
    let status = command
        .status()
        .map_err(|e| format!("{}: {e}", vesper.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} failed ({status})", vesper.display()))
    }
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vesper-port-build: {e}");
            ExitCode::FAILURE
        }
    }
}
