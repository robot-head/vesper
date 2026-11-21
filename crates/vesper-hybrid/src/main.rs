//! `vesper-hybrid`: the hybrid Stage 2 compiler (self-hosting SH0 lane 1,
//! #1123). It is the default `vesper` CLI, whose sources are included below
//! unchanged, with the swappable compiler modules replaced by Vesper objects
//! (see `hybrid`). It is never the default build: the default compiler is
//! the root package's `vesper` binary, which installs nothing and runs the
//! Rust implementation of every module.
//!
//! Its entry is the Vesper driver (SH5 lane 12, #1335, `driver`): the
//! included CLI's `fn main` is an ordinary function here, which the driver
//! calls for every command it does not run itself.
#![no_main]

// The default CLI: the root package's src/main.rs and, through its `mod`
// items (resolved next to that file), every command module.
include!("../../../src/main.rs");

#[path = "hybrid.rs"]
mod vesper_hybrid;

#[path = "driver.rs"]
mod driver;

/// The C entry point: the Vesper driver.
#[unsafe(export_name = "main")]
extern "C" fn hybrid_main(_argc: std::ffi::c_int, _argv: *const *const std::ffi::c_char) -> std::ffi::c_int {
    driver::main()
}
