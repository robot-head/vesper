//! The frozen Rust seed copy of vesper-loom-audit that the compiler's tools
//! link (seed-copy rule, port wave P1 lane 2, #899): the shared API in
//! ../../src/lib.rs over the Rust audit in engine/. Every other crate uses
//! the package vesper-loom-audit, whose audit runs in the Vesper port. See
//! "Crates the compiler links" in docs/maintenance/porting-a-crate.md.

#[path = "../../src/lib.rs"]
mod api;
pub use api::*;

mod engine;
