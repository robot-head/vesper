//! Records the compiler and target that built the runner. Executable bytes
//! are hashed at run time (see `src/identity.rs`); these values are bound in
//! addition, and `unknown` marks the runner's identity incomplete.

use std::env;
use std::process::Command;

fn main() {
    let rustc = env::var("RUSTC")
        .ok()
        .and_then(|r| Command::new(r).arg("-vV").output().ok())
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("; ")
        });
    let target = env::var("TARGET").ok();
    println!(
        "cargo:rustc-env=VFR_RUSTC={}",
        rustc.unwrap_or_else(|| "unknown".into())
    );
    println!(
        "cargo:rustc-env=VFR_HOST_TARGET={}",
        target.unwrap_or_else(|| "unknown".into())
    );
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-changed=build.rs");
}
