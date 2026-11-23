//! The frozen Rust implementation the Vesper port replaced. The seed copy
//! (`crates/vesper-loom-trust/seed`, which the compiler's tools link) runs
//! it as the engine, and the port-backed build compiles it only for tests,
//! as the reference of `src/differential.rs`. Bug fixes only.
pub mod slopsquat;
pub mod verify;
