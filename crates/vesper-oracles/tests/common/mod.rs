//! The `vesper` binary the oracle tests drive.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{}", name.display(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
}

/// `VESPER_ORACLE42_COMPILER`, else the `vesper` beside the oracle binary
/// (Cargo's target directory, rebuilt here so the tests never run a stale
/// compiler: this crate does not depend on it), else `vesper` on PATH
/// (Bazel puts `//:bin/vesper` there).
pub fn vesper() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Some(path) = std::env::var_os("VESPER_ORACLE42_COMPILER") {
            return PathBuf::from(path);
        }
        let oracle = PathBuf::from(env!("CARGO_BIN_EXE_arithmetic_oracle42"));
        if !oracle.is_absolute() {
            // Bazel's launcher gives a bare name found on PATH.
            return which(Path::new("vesper")).expect("vesper on PATH");
        }
        let sibling = oracle.with_file_name(format!("vesper{}", std::env::consts::EXE_SUFFIX));
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cargo
            .current_dir(root)
            .args(["build", "-p", "vesper", "--bin", "vesper"]);
        if oracle.parent().and_then(Path::file_name) == Some("release".as_ref()) {
            cargo.arg("--release");
        }
        let status = cargo.status().expect("run cargo build --bin vesper");
        assert!(status.success(), "cargo build --bin vesper failed");
        assert!(sibling.is_file(), "{} is missing", sibling.display());
        sibling
    })
    .clone()
}
