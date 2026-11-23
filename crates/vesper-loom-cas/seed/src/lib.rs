//! The frozen Rust seed copy of `vesper-loom-cas` that the compiler and its
//! tools link (seed-copy rule, port wave SH6 lane 16, #1411): the shared API
//! in `../../src/lib.rs` over the frozen Rust CAS engine in `engine/`. Every
//! crate outside the compiler's closure uses `vesper-loom-cas` (the Vesper
//! port). See "Crates the compiler links" in
//! `docs/maintenance/porting-a-crate.md`.

#[path = "../../src/lib.rs"]
mod api;
pub use api::*;

#[allow(unused_imports)]
use api::{derivation, error, fsutil, id, store, tree};

mod engine;
