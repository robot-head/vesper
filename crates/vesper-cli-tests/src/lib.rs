//! The `vesper` command-line tests (#1397 retirement step R5, #2743).
//! Everything is in `tests/`: each test runs the compiler named by
//! `$VESPER` as a process, by default the shipped compiler (Bazel's
//! `//:vesper_shipped`, or the Cargo-built `vesper-hybrid` with the gated
//! swap set). The crate has no dependency on the Rust compiler, so these
//! tests outlive it. `tests/support/mod.rs` finds the compiler.
