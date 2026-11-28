//! Finds the compiler under test and the repository it runs in (#2743).
//!
//! The compiler is, in order:
//! 1. `$VESPER`, a path or a bare name looked up on `PATH`;
//! 2. under Cargo, the `vesper-hybrid` built into the same target directory
//!    as this test (`cargo build -p vesper-hybrid -p vesper-tools`), run
//!    with the gated swap set, which is what the shipped compiler runs;
//! 3. under Bazel, `vesper` on `PATH`, which the test launcher points at
//!    `//:vesper_shipped`.
//!
//! Every command starts in the repository root, where the root package's
//! CLI tests ran, so relative paths such as `examples/fibonacci.ves` keep
//! working. A test that needs another directory sets its own.
#![allow(dead_code)]

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

/// The variable that names the compiler under test.
const COMPILER_VARIABLE: &str = "VESPER";

/// The Cargo binary of the hybrid compiler (crates/vesper-hybrid).
const CARGO_HYBRID: &str = "vesper-hybrid";

/// The name the shipped compiler has on `PATH` under Bazel.
const SHIPPED_NAME: &str = "vesper";

/// Set by Bazel for every test it runs.
const BAZEL_TEST_VARIABLE: &str = "TEST_SRCDIR";

/// The hybrid's swap-set variable, and the set the shipped compiler runs
/// (crates/vesper-hybrid/src/swaps.rs): a hybrid started as `vesper-hybrid`
/// runs every swap unless told otherwise.
const SWAPS_VARIABLE: &str = "VESPER_HYBRID_SWAPS";
const SHIPPED_SWAPS: &str = "gated";

/// The repository root: this crate lives at `crates/vesper-cli-tests`.
pub fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    root.canonicalize()
        .unwrap_or_else(|e| panic!("repository root {}: {e}", root.display()))
}

/// The compiler under test, as an absolute path.
pub fn vesper_path() -> PathBuf {
    if let Some(named) = env::var_os(COMPILER_VARIABLE).filter(|v| !v.is_empty()) {
        return resolve(&named).unwrap_or_else(|| {
            panic!(
                "${COMPILER_VARIABLE}={} is not an executable",
                Path::new(&named).display()
            )
        });
    }
    if let Some(path) = cargo_hybrid() {
        return path;
    }
    if env::var_os(BAZEL_TEST_VARIABLE).is_some()
        && let Some(path) = resolve(&OsString::from(SHIPPED_NAME))
    {
        return path;
    }
    panic!(
        "no compiler to test: set ${COMPILER_VARIABLE}, or build the shipped compiler with \
         `cargo build -p {CARGO_HYBRID} -p vesper-tools` into this target directory"
    )
}

/// A command for the compiler under test, started in the repository root.
pub fn vesper() -> Command {
    command(&vesper_path())
}

/// A command for a compiler at `path`, started in the repository root. A
/// hybrid started under its Cargo name runs the shipped swap set unless the
/// caller's environment names one.
pub fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.current_dir(repo_root());
    if path.file_stem().is_some_and(|stem| stem == CARGO_HYBRID)
        && env::var_os(SWAPS_VARIABLE).is_none()
    {
        command.env(SWAPS_VARIABLE, SHIPPED_SWAPS);
    }
    command
}

/// `<target>/<profile>/vesper-hybrid`: the nearest directory above this
/// test's executable that holds one (the executable is under
/// `<profile>/deps/` or `<profile>/build/<crate>/<hash>/out/`).
fn cargo_hybrid() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let name = format!("{CARGO_HYBRID}{}", env::consts::EXE_SUFFIX);
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&name))
        .find(|path| path.is_file())
}

/// A path as given, or a bare name looked up on `PATH`, made absolute but
/// not resolved: the hybrid picks its swap set from the name it starts as.
fn resolve(name: &OsString) -> Option<PathBuf> {
    let given = Path::new(name);
    let found = if given.components().count() > 1 || given.is_absolute() {
        with_suffix(given)
    } else {
        env::split_paths(&env::var_os("PATH")?).find_map(|dir| with_suffix(&dir.join(given)))
    }?;
    std::path::absolute(found).ok()
}

/// The file itself, or with the platform's executable suffix.
fn with_suffix(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let suffixed = PathBuf::from(format!("{}{}", path.display(), env::consts::EXE_SUFFIX));
    suffixed.is_file().then_some(suffixed)
}
