//! The port-backed package's link to the Vesper port in `vesper/` (port wave
//! P3b lane 8, #1070): the generated binding, the crate types it converts
//! (`--rust-types crate::port`).
//! Compiled only with cfg(vesper_port); the seed package never sees it.

#[allow(clippy::all, dead_code, unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;

pub(crate) use binding::*;

pub use crate::coloring::ColoringError;
pub use crate::gjk::GjkError;
pub use crate::lbvh::LbvhError;
pub use crate::morton::MortonError;
pub use crate::sap::SapError;
