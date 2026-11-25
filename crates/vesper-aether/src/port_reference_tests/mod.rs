//! Differential and ABBA tests of the flipped aether modules (SH6 lane 14,
//! #1409) against their Rust references, which exist only under
//! `#[cfg(test)]`: hot sets (`hotset.ves`), the packed-ring capture
//! (`packed.ves`) and the seccomp evaluator (`seccomp.ves`). The ABBA
//! timings are ignored; run them with
//!
//! ```text
//! RUSTFLAGS="-C target-cpu=native" cargo test --release -p vesper-aether \
//!     --lib port_reference_tests -- --ignored --nocapture
//! ```

mod abba_hotset;
mod abba_packed;
mod abba_seccomp;
mod hotset;
mod packed;
mod seccomp;
