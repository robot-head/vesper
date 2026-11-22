//! The frozen Rust seed copy of vesper-tensor that the compiler links
//! (seed-copy rule, port wave P1 lane 2, #899): the shared source in
//! ../../src/lib.rs, built without the port, over the Rust engine in
//! engine/ for the modules whose only build outside the compiler is the
//! Vesper port. See "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md.

#[path = "../../src/lib.rs"]
mod api;
pub use api::*;

mod engine;
