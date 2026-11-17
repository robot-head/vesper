//! Fixture for the Vesper port harness (port wave P1 lane 2, #899): the
//! port is a multi-file Loom package (`vesper/app`, modules `lib` and
//! `shapes`) with a path dependency (`vesper/math`, modules `lib` and
//! `scale`). build.rs compiles it with `vesper build --lib <package dir>`
//! in Cargo; Bazel builds it with `vesper_port_object(package = ...)`.
//! tests/package.rs checks the results through the generated binding.
#![deny(unsafe_code)]
pub use vesper_port_abi::ArithOverflow;

// Generated and committed, kept outside src/ (see the porting recipe).
#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
pub use port_binding::*;
