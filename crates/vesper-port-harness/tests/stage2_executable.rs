//! Stage 2's native executables against `vesper build` (Stage 2 retirement
//! slice 4e, #2752): each fixture package under tests/stage2_executable is
//! built into an object by `vesper-exe-builder.wasm` (packages/vesper-exe-builder
//! built by Stage 2 and composed with the Cranelift component) under the
//! pinned wasmtime CLI, and linked as `vesper build` links one
//! (`cc <object> -o <path> -lm`). The Rust compiler builds the same package
//! with `vesper build` and `vesper emit --emit object`. The objects must be
//! byte-identical, and the two executables must exit with the same status
//! and print the same output.
//!
//! Inputs: [`stage2::EXE_BUILDER_ENV`] (the composed component, from
//! `//:vesper-exe-builder.wasm`), the wasmtime CLI ([`stage2::wasmtime`])
//! and [`vesper_port_harness::COMPILER_ENV`] (the Rust compiler). Bazel
//! passes all three. Without the builder or the compiler, the test says so
//! and passes, unless it runs under Bazel.
use std::path::{Path, PathBuf};
use std::process::Command;
use vesper_port_harness::stage2;

/// The fixtures, by directory under tests/stage2_executable, and whether
/// their exit status is defined (a Unit `main` leaves the return register
/// as it is, so only its object is compared).
const FIXTURES: &[(&str, bool)] = &[
    ("hello", true),
    ("main_module", true),
    ("fib", true),
    ("branch", true),
    ("structs", true),
    ("unit", false),
    ("unreached", true),
];
/// The target both compilers build for (an explicit triple with no extra
/// CPU features, so objects do not depend on the machine).
const TRIPLE: &str = "x86_64-unknown-linux-gnu";
/// `vesper build`'s linker on this target.
const LINKER: &str = "cc";
/// Why @vesper/exe-builder declines the object (`OBJECT_*`).
const OBJECT_DECLINES: &[&str] = &[
    "",
    "Stage 2's front end made no calls input",
    "native-lib answered with an error",
    "(no exe-builder reason)",
    "native-lib ran out of Stage 2's memory",
];

fn fixtures() -> PathBuf {
    // Cargo runs tests in the crate directory; Bazel in the package's
    // runfiles directory.
    for base in [
        Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf(),
        PathBuf::from("."),
    ] {
        let dir = base.join("tests/stage2_executable");
        if dir.is_dir() {
            return std::fs::canonicalize(dir).unwrap();
        }
    }
    panic!("no tests/stage2_executable beside the test")
}

fn bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

/// A path from the environment, absolute (Bazel gives runfiles paths).
fn env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

fn run(command: &mut Command) -> Result<std::process::Output, String> {
    command.output().map_err(|e| format!("{command:?}: {e}"))
}

fn checked(command: &mut Command) -> Result<(), String> {
    let out = run(command)?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{command:?} failed ({}):\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// The exit status and standard output of `exe`.
fn outcome(exe: &Path) -> Result<(Option<i32>, Vec<u8>), String> {
    let out = run(&mut Command::new(exe))?;
    Ok((out.status.code(), out.stdout))
}

/// Builds one fixture with both compilers; `Err` names what differed.
fn compare(
    builder: &Path,
    wasmtime: &Path,
    compiler: &Path,
    name: &str,
    defined_status: bool,
    scratch: &Path,
) -> Result<(), String> {
    let package = fixtures().join(name);
    let dir = scratch.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let object = dir.join("stage2.o");
    let built = stage2::build_executable(&stage2::ExeRequest {
        builder,
        wasmtime,
        package: &package,
        target: TRIPLE,
        object: &object,
        scratch: &dir.join("scratch"),
    })?;
    if built.object != 0 {
        let why = OBJECT_DECLINES
            .get(built.object as usize)
            .copied()
            .unwrap_or("an unknown reason");
        return Err(format!("declined ({why}): {}", built.error.trim_end()));
    }
    let stage2_exe = dir.join("stage2");
    checked(
        Command::new(LINKER)
            .arg(&object)
            .arg("-o")
            .arg(&stage2_exe)
            .arg("-lm"),
    )?;
    let rust_object = dir.join("rust.o");
    checked(
        Command::new(compiler)
            .args(["emit", "--emit", "object", "--target", TRIPLE])
            .arg(&package)
            .arg("-o")
            .arg(&rust_object),
    )?;
    let rust_exe = dir.join("rust");
    checked(
        Command::new(compiler)
            .args(["build", "--target", TRIPLE])
            .arg(&package)
            .arg("-o")
            .arg(&rust_exe),
    )?;
    let ours = std::fs::read(&object).map_err(|e| format!("{}: {e}", object.display()))?;
    let theirs =
        std::fs::read(&rust_object).map_err(|e| format!("{}: {e}", rust_object.display()))?;
    if ours != theirs {
        return Err(format!(
            "objects differ ({} bytes from Stage 2, {} from Rust): {} {}",
            ours.len(),
            theirs.len(),
            object.display(),
            rust_object.display()
        ));
    }
    let (ours, theirs) = (outcome(&stage2_exe)?, outcome(&rust_exe)?);
    if (defined_status && ours.0 != theirs.0) || ours.1 != theirs.1 {
        return Err(format!(
            "executables differ: Stage 2 exits {:?} printing {:?}, Rust exits {:?} printing {:?}",
            ours.0,
            String::from_utf8_lossy(&ours.1),
            theirs.0,
            String::from_utf8_lossy(&theirs.1)
        ));
    }
    Ok(())
}

#[test]
fn stage2_executables_match_rust() {
    let builder = env_path(stage2::EXE_BUILDER_ENV);
    let compiler = env_path(vesper_port_harness::COMPILER_ENV);
    let (Some(builder), Some(compiler)) = (builder, compiler) else {
        assert!(
            !bazel(),
            "{} and {} must be set under Bazel",
            stage2::EXE_BUILDER_ENV,
            vesper_port_harness::COMPILER_ENV
        );
        eprintln!(
            "skipped: set {} (//:vesper-exe-builder.wasm) and {} (the Rust compiler)",
            stage2::EXE_BUILDER_ENV,
            vesper_port_harness::COMPILER_ENV
        );
        return;
    };
    let wasmtime = stage2::wasmtime();
    let scratch = std::env::temp_dir().join(format!("vesper-stage2-exe-{}", std::process::id()));
    let mut failures = Vec::new();
    for &(name, defined_status) in FIXTURES {
        match compare(
            &builder,
            &wasmtime,
            &compiler,
            name,
            defined_status,
            &scratch,
        ) {
            Ok(()) => eprintln!("{name}: identical"),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} of {} fixtures differ:\n{}",
        failures.len(),
        FIXTURES.len(),
        failures.join("\n")
    );
}
