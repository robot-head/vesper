//! The Vesper build of the lane kernels (`vesper/src/kernels.ves`). Every
//! ISA path but [`IsaPath::Scalar`] runs the 128-bit source-vector form;
//! `Scalar` runs the scalar loops, which are the reference semantics.

use crate::port::binding as b;
use crate::simd::{IsaPath, Kernels, LANES};

const MAX: u64 = 0;
const ADD: u64 = 1;
const SUB: u64 = 2;

fn vector(k: Kernels) -> bool {
    k.path() != IsaPath::Scalar
}

impl Kernels {
    /// Bit `i` set iff `lanes[i] == key`.
    pub fn eq_mask_u32(self, lanes: &[u32; LANES], key: u32) -> u16 {
        // A 16-lane mask.
        b::kernel_eq_mask_u32(lanes, key, vector(self)) as u16
    }

    /// Bit `i` set iff `a[i] > b[i]` (unsigned).
    pub fn gt_mask_u64(self, a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        b::kernel_gt_mask_u64(a, b, vector(self)) as u16
    }

    /// Bit `i` set iff `lanes[i] <= bound` (unsigned).
    pub fn le_mask_u64(self, lanes: &[u64; LANES], bound: u64) -> u16 {
        !self.gt_mask_u64(lanes, &[bound; LANES])
    }

    /// `acc[i] = max(acc[i], other[i])` (unsigned).
    pub fn max_u64(self, acc: &mut [u64; LANES], other: &[u64; LANES]) {
        b::kernel_lanes_op(acc, other, MAX, vector(self));
    }

    /// `acc[i] = acc[i].wrapping_add(other[i])`.
    pub fn add_u64(self, acc: &mut [u64; LANES], other: &[u64; LANES]) {
        b::kernel_lanes_op(acc, other, ADD, vector(self));
    }

    /// `acc[i] = acc[i].wrapping_sub(other[i])`.
    pub fn sub_u64(self, acc: &mut [u64; LANES], other: &[u64; LANES]) {
        b::kernel_lanes_op(acc, other, SUB, vector(self));
    }
}

/// Scalar reference kernels.
pub mod scalar {
    use crate::simd::{Kernels, LANES};

    /// Bit `i` set iff `lanes[i] == key`.
    pub fn eq_mask_u32(lanes: &[u32; LANES], key: u32) -> u16 {
        Kernels::scalar().eq_mask_u32(lanes, key)
    }

    /// Bit `i` set iff `a[i] > b[i]` (unsigned).
    pub fn gt_mask_u64(a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        Kernels::scalar().gt_mask_u64(a, b)
    }

    /// `acc[i] = max(acc[i], other[i])`.
    pub fn max_u64(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        Kernels::scalar().max_u64(acc, other)
    }

    /// `acc[i] = acc[i].wrapping_add(other[i])`.
    pub fn add_u64(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        Kernels::scalar().add_u64(acc, other)
    }

    /// `acc[i] = acc[i].wrapping_sub(other[i])`.
    pub fn sub_u64(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        Kernels::scalar().sub_u64(acc, other)
    }
}
