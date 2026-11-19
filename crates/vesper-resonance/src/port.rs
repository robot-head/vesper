//! The port build's side of the boundary (`cfg(vesper_port)`): helpers that
//! lend the crate's `f32` state to the Vesper port and map its errors.
//!
//! The export ABI carries no `f32`, so an `f32` crosses as its bit pattern:
//! `&[f32]` is lent as `&[u32]` with `bytemuck` (in place, no copy), and an
//! `f32` scalar as `f32::to_bits`. The crate's state stays in its own Rust
//! arrays, which the binding lends to Vesper in place, so a call allocates
//! nothing.

pub use crate::port_binding::*;

/// `&[f32]` as its bit patterns, in place.
pub fn bits(s: &[f32]) -> &[u32] {
    bytemuck::cast_slice(s)
}

/// `&mut [f32]` as its bit patterns, in place.
pub fn bits_mut(s: &mut [f32]) -> &mut [u32] {
    bytemuck::cast_slice_mut(s)
}

/// The value of a call whose only failures are Vesper's builtin errors
/// (`RuntimeError` for an index out of range, `ArithOverflow`). The Rust
/// build panics on the same out-of-range inputs, and valid calls raise
/// neither, so the port panics too.
#[track_caller]
pub fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("the Vesper port of vesper-resonance failed: {e:?}"),
    }
}
