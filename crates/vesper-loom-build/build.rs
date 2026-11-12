//! Computes the execution fingerprint compiled into the runner.
//!
//! Every value is emitted as a `VLB_*` compile-time environment variable.
//! A value that cannot be determined is emitted as the literal `unknown`,
//! and the runner then refuses to read or publish cached results.
//!
//! * `VLB_BUILDER_SRC`  BLAKE3 over this crate's `Cargo.toml`, `build.rs`, `src/`.
//! * `VLB_SANDBOX_SRC`  BLAKE3 over `vesper-loom-sandbox`'s `Cargo.toml`, `src/`, `wit/`.
//! * `VLB_CAS_SRC`      BLAKE3 over `vesper-loom-cas`'s `Cargo.toml`, `src/`.
//! * `VLB_ENGINE_DEPS`  BLAKE3 over the locked name/version/source/checksum of
//!   every Wasmtime/Cranelift/WASI/cap-std package in the nearest `Cargo.lock`.
//! * `VLB_RUSTC`        `rustc -vV` of the compiler building this crate.
//! * `VLB_HOST_TARGET`  the target triple this runner is compiled for.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const UNKNOWN: &str = "unknown";

/// Packages whose code determines how a component is compiled and executed.
const ENGINE_PREFIXES: &[&str] = &[
    "wasmtime",
    "cranelift",
    "pulley",
    "regalloc2",
    "winch",
    "wasmparser",
    "wasm-encoder",
    "wasm-metadata",
    "wasi",
    "wit-",
    "cap-",
    "io-extras",
    "fs-set-times",
    "rustix",
    "system-interface",
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let crates = manifest.parent().map(Path::to_path_buf);

    let builder = tree_digest(&manifest, &["Cargo.toml", "build.rs", "src"], "builder-src");
    let (sandbox, cas) = match &crates {
        Some(c) => (
            tree_digest(
                &c.join("vesper-loom-sandbox"),
                &["Cargo.toml", "src", "wit"],
                "sandbox-src",
            ),
            tree_digest(
                &c.join("vesper-loom-cas"),
                &["Cargo.toml", "src"],
                "cas-src",
            ),
        ),
        None => (None, None),
    };
    let engine = engine_deps(&manifest);
    let rustc = rustc_version();
    let target = env::var("TARGET").ok();

    emit("VLB_BUILDER_SRC", builder);
    emit("VLB_SANDBOX_SRC", sandbox);
    emit("VLB_CAS_SRC", cas);
    emit("VLB_ENGINE_DEPS", engine);
    emit("VLB_RUSTC", rustc);
    emit("VLB_HOST_TARGET", target);
    println!("cargo:rerun-if-env-changed=RUSTC");
}

fn emit(name: &str, value: Option<String>) {
    let value = value.unwrap_or_else(|| UNKNOWN.to_owned());
    // Values are single-line by construction (hex digests, joined lines).
    println!("cargo:rustc-env={name}={value}");
}

/// Digest of the named files/directories under `root`, or `None` if any is
/// missing. Paths are hashed relative to `root` in sorted order.
fn tree_digest(root: &Path, parts: &[&str], context: &str) -> Option<String> {
    let mut files = Vec::new();
    for part in parts {
        let path = root.join(part);
        println!("cargo:rerun-if-changed={}", path.display());
        collect(&path, &mut files).ok()?;
    }
    files.sort();
    let mut hasher =
        blake3::Hasher::new_derive_key(&format!("vesper-loom-build 2026-11 {context}"));
    for file in files {
        let rel = file.strip_prefix(root).ok()?.to_str()?.replace('\\', "/");
        let bytes = fs::read(&file).ok()?;
        hasher.update(&(rel.len() as u64).to_le_bytes());
        hasher.update(rel.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            collect(&entry?.path(), out)?;
        }
    } else if meta.is_file() {
        out.push(path.to_owned());
    } else {
        return Err(std::io::Error::other(
            "unsupported file type in fingerprint",
        ));
    }
    Ok(())
}

/// The nearest `Cargo.lock` at or above the manifest directory.
fn find_lock(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let lock = d.join("Cargo.lock");
        if lock.is_file() {
            return Some(lock);
        }
        dir = d.parent();
    }
    None
}

fn engine_deps(manifest: &Path) -> Option<String> {
    let lock = find_lock(manifest)?;
    println!("cargo:rerun-if-changed={}", lock.display());
    let text = fs::read_to_string(&lock).ok()?;
    let mut entries = Vec::new();
    for block in text.split("[[package]]").skip(1) {
        let field = |key: &str| {
            block.lines().find_map(|l| {
                l.strip_prefix(key)
                    .and_then(|r| r.trim().strip_prefix('='))
                    .map(|v| v.trim().trim_matches('"').to_owned())
            })
        };
        let Some(name) = field("name ") else { continue };
        if !ENGINE_PREFIXES.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        entries.push(format!(
            "{name} {} {} {}",
            field("version ").unwrap_or_default(),
            field("source ").unwrap_or_default(),
            field("checksum ").unwrap_or_default()
        ));
    }
    // Without the engine itself the fingerprint would be meaningless.
    if !entries.iter().any(|e| e.starts_with("wasmtime ")) {
        return None;
    }
    entries.sort();
    entries.dedup();
    let mut hasher = blake3::Hasher::new_derive_key("vesper-loom-build 2026-11 engine-deps");
    for e in &entries {
        hasher.update(&(e.len() as u64).to_le_bytes());
        hasher.update(e.as_bytes());
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn rustc_version() -> Option<String> {
    let rustc = env::var("RUSTC").ok()?;
    let out = Command::new(rustc).arg("-vV").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let joined: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Some(joined.join("; "))
}
