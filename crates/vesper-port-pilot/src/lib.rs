//! Pilot crate for the Vesper port harness (port wave P0 lane 6, #878).
//!
//! The public API below has two implementations: Rust (`src/reference.rs`,
//! the default) and Vesper (`vesper/pilot.ves`, under the `vesper-port`
//! feature, reached through the generated `port/binding.rs`). The tests
//! in `tests/` are written against the API only, so they run unchanged
//! against both builds. That is the recipe every crate port follows
//! (`docs/maintenance/porting-a-crate.md`).
pub use vesper_port_abi::{ArithOverflow, RuntimeError};

pub mod kit;

/// A point on the integer grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}

/// A shape whose area is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Empty,
    Square(i64),
    Rect { w: i64, h: i64 },
}

/// The errors of [`ratio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatioError {
    ArithOverflow(ArithOverflow),
    RuntimeError(RuntimeError),
}

#[cfg(not(feature = "vesper-port"))]
mod reference;
#[cfg(not(feature = "vesper-port"))]
pub use reference::*;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[cfg(feature = "vesper-port")]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
#[cfg(feature = "vesper-port")]
pub use port_binding::*;

/// Which implementation this build runs.
pub const IMPLEMENTATION: &str = if cfg!(feature = "vesper-port") {
    "vesper"
} else {
    "rust"
};
