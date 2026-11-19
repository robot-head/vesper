//! The sample driver of self-hosting SH0 lane 3 (#1125): a Loom package in
//! vesper/ over @vesper-std/fs, process and env, compiled by `vesper build
//! --lib` and linked here under the `vesper-port` feature. It walks a
//! package's `src/**`, reads every `.ves` file, spawns `cc --version` and
//! writes its report atomically (tests/driver.rs).

// The binding names the host functions as `crate::fs` and friends.
pub use vesper_std_host::{env, fs, process};

/// `driver::run()` and `driver::live_allocations()`.
// Generated: rustfmt must never reflow it (the build checks it byte for
// byte).
#[cfg(vesper_port)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
pub mod driver;
