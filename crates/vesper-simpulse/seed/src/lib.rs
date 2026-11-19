//! The frozen Rust seed copy of vesper-simpulse that the compiler and the
//! crates it links use (seed-copy rule, port wave P1 lane 2, #899): the
//! shared API in ../../src/lib.rs over the Rust `Simulator` in reference.rs.
//! Every other crate uses the package vesper-simpulse, whose only build is
//! the Vesper port. See "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md.

#[path = "../../src/lib.rs"]
mod api;
pub use api::*;

mod reference;
pub use reference::Simulator;
