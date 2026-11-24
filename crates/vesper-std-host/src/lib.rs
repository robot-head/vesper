//! Retired host shim for `@vesper-std/fs` and `@vesper-std/process` (#1408).
//!
//! All three native platforms now call `libc` / `kernel32` directly; this crate
//! retains only the zeroed [`live_handles`] counter and empty [`symbols`] list
//! for existing test harnesses until callers drop the dependency.
#![allow(clippy::missing_safety_doc)]

/// Handle kinds, for the live counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    File = 0,
    Dir = 1,
    Command = 2,
    Child = 3,
}

/// Handles of `kind` created and not yet finalized, process-wide.
pub fn live(_kind: HandleKind) -> i64 {
    0
}

/// Live handles of every kind: `[file, dir, command, child]`.
pub fn live_handles() -> [i64; 4] {
    [0, 0, 0, 0]
}

/// Every host function, by its `.vsys` symbol (now empty; all native packages
/// import `libc` / `kernel32` directly).
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    Vec::new()
}
