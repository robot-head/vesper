//! The port-backed package's link to the Vesper port in `vesper/` (port wave
//! P3b lane 8, #1070): the generated binding, the crate types it converts
//! (`--rust-types crate::port`), and the `.vsys` library the port imports.
//! Compiled only with cfg(vesper_port); the seed package never sees it.

#[allow(clippy::all, dead_code, unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;

pub(crate) use binding::*;

pub use crate::coloring::ColoringError;
pub use crate::lbvh::LbvhError;
pub use crate::morton::MortonError;
pub use crate::sap::SapError;

/// The `.vsys` library `Crmath` that `vesper/src/lib.ves` imports: the
/// correctly rounded `sin` and `cos` of [`crate::crmath`], which the fast
/// path calls past its reduction range until crmath.rs is ported.
pub(crate) mod shim {
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_kinetic_crmath_sin(bits: u64) -> u64 {
        crate::crmath::sin(f64::from_bits(bits)).to_bits()
    }

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_kinetic_crmath_cos(bits: u64) -> u64 {
        crate::crmath::cos(f64::from_bits(bits)).to_bits()
    }
}
