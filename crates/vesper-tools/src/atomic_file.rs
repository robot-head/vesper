//! Atomic file output for the tools (#2670, part of #1397).
//!
//! A copy of the compiler's `src/atomic_file.rs`, so the tools that write
//! receipts, lockfiles and snapshots no longer link the Rust compiler library
//! for it. The compiler's copy goes away with `src/`.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Write `bytes` to `path` without truncating an existing file on failure.
/// Temporary files are created exclusively beside the target.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    path.file_name().ok_or("output path must name a file")?;
    let mut temp = PathBuf::from(parent);
    // Do not append a suffix to the destination leaf: a valid NAME_MAX-sized
    // output name must still leave room for the private staging file.
    let temporary_name = format!(
        ".vesper-tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    temp.push(temporary_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    let written = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    let result = written.and_then(|_| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| format!("{}: {e}", path.display()))
}
