//! The frozen Rust seed copy of vesper-loom-calculus that the compiler links
//! (seed-copy rule, port wave P1 lane 2, #899): the shared API in
//! ../../src/lib.rs over the Rust calculus in engine/. Every crate outside
//! the compiler's closure uses the package vesper-loom-calculus, whose
//! calculus runs in the Vesper port. See "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md.

#[path = "../../src/lib.rs"]
mod api;
pub use api::*;

mod engine;
