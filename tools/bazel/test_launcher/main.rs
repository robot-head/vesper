//! Runs a Rust test binary the way `cargo test` does.
//!
//! Cargo starts each test with the package directory as its working
//! directory and exposes the package's binaries through `CARGO_BIN_EXE_*`.
//! Bazel starts tests at the runfiles root instead. This launcher, which
//! `vesper_crate()` puts in front of every test, changes to the package's
//! directory inside the runfiles tree and puts the package's binaries on
//! `PATH`, where the bare `CARGO_BIN_EXE_<name>` values find them.
//!
//! Inputs (set by `cargo_test` in tools/bazel/cargo_test.bzl):
//!   VESPER_TEST_BINARY  runfiles path of the test binary
//!   VESPER_TEST_CWD     runfiles path of the package directory
//!   VESPER_TEST_PATH    runfiles paths of directories to prepend to PATH,
//!                       separated by `|`
//!   VESPER_TEST_COPY    when `1`, copy the runfiles tree into
//!                       `TEST_TMPDIR` as regular files and run there, for
//!                       tests whose code rejects symlinked inputs (runfiles
//!                       are symlinks on Unix)

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        eprintln!("test launcher: {name} is not set");
        std::process::exit(2)
    })
}

fn main() -> ExitCode {
    let srcdir = var("TEST_SRCDIR");
    let workspace = var("TEST_WORKSPACE");
    let root = PathBuf::from(srcdir).join(workspace);
    let root = std::path::absolute(&root).unwrap_or(root);

    let binary = root.join(var("VESPER_TEST_BINARY"));
    let mut cwd = root.join(var("VESPER_TEST_CWD"));
    if env::var("VESPER_TEST_COPY").as_deref() == Ok("1") {
        // Copy the whole main-repository tree so `../` paths to sibling
        // packages keep working.
        let dest = PathBuf::from(var("TEST_TMPDIR")).join("runfiles");
        if let Err(error) = copy_tree(&root, &dest) {
            eprintln!("test launcher: cannot copy {}: {error}", root.display());
            return ExitCode::from(2);
        }
        cwd = dest.join(var("VESPER_TEST_CWD"));
    }

    let mut path: Vec<PathBuf> = env::var("VESPER_TEST_PATH")
        .unwrap_or_default()
        .split('|')
        .filter(|d| !d.is_empty())
        .map(|d| root.join(d))
        .collect();
    if let Some(existing) = env::var_os("PATH") {
        // Bazel's default test PATH contains `.`; a relative entry would
        // resolve against the package directory (the root package has a
        // `lean/` directory, which shadows a missing `lean` tool).
        path.extend(env::split_paths(&existing).filter(|p| p.is_absolute()));
    }
    let path: OsString = env::join_paths(path).expect("PATH entry contains a separator");

    let mut command = Command::new(&binary);
    command
        .args(env::args_os().skip(1))
        .current_dir(&cwd)
        .env("PATH", path)
        .env_remove("VESPER_TEST_BINARY")
        .env_remove("VESPER_TEST_CWD")
        .env_remove("VESPER_TEST_PATH")
        .env_remove("VESPER_TEST_COPY");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("test launcher: cannot run {}: {error}", binary.display());
        ExitCode::from(2)
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => match status.code() {
                Some(0) => ExitCode::SUCCESS,
                Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1).max(1)),
                None => ExitCode::FAILURE,
            },
            Err(error) => {
                eprintln!("test launcher: cannot run {}: {error}", binary.display());
                ExitCode::from(2)
            }
        }
    }
}

/// Copies `from` to `to`, following symlinks, so every file is a regular file.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if std::fs::metadata(&source)?.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}
