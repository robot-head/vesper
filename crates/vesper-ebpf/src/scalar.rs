//! The scalar abstract domain: tristate numbers (`tnum`, after the kernel's
//! `kernel/bpf/tnum.c`) reduced with unsigned and signed 64-bit intervals.
//!
//! Every transfer function here over-approximates the concrete BPF operation
//! (`interp::alu_concrete`); `tests/scalar_soundness.rs` checks that on random
//! members of random abstract values. A `None` from [`Scalar::normalize`]
//! means the constraints are contradictory, i.e. no concrete value exists.
// `Tnum::add(a, b)` and friends mirror the kernel's `tnum_add` naming.
#![allow(clippy::should_implement_trait)]

/// A tristate number: bit `i` is unknown when `mask` bit `i` is set,
/// otherwise it equals `value` bit `i`. Invariant: `value & mask == 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tnum {
    pub value: u64,
    pub mask: u64,
}

impl Tnum {
    pub const UNKNOWN: Tnum = Tnum {
        value: 0,
        mask: u64::MAX,
    };

    pub const fn konst(v: u64) -> Tnum {
        Tnum { value: v, mask: 0 }
    }

    /// The smallest tnum containing every value of `[min, max]`.
    pub fn range(min: u64, max: u64) -> Tnum {
        crate::port::ffi::tnum_range_of(min, max)
    }

    pub fn is_const(self) -> bool {
        self.mask == 0
    }

    pub fn contains(self, v: u64) -> bool {
        crate::port::ffi::tnum_contains_value(self, v)
    }

    /// `other` ⊆ `self`.
    pub fn includes(self, other: Tnum) -> bool {
        crate::port::ffi::tnum_includes_tnum(self, other)
    }

    pub fn umin(self) -> u64 {
        self.value
    }

    pub fn umax(self) -> u64 {
        self.value | self.mask
    }

    pub fn add(a: Tnum, b: Tnum) -> Tnum {
        crate::port::ffi::tnum_add_of(a, b)
    }

    pub fn sub(a: Tnum, b: Tnum) -> Tnum {
        crate::port::ffi::tnum_sub_of(a, b)
    }

    pub fn and(a: Tnum, b: Tnum) -> Tnum {
        crate::port::ffi::tnum_and_of(a, b)
    }

    pub fn or(a: Tnum, b: Tnum) -> Tnum {
        crate::port::ffi::tnum_or_of(a, b)
    }

    pub fn xor(a: Tnum, b: Tnum) -> Tnum {
        crate::port::ffi::tnum_xor_of(a, b)
    }

    pub fn lsh(a: Tnum, s: u32) -> Tnum {
        crate::port::ffi::tnum_lsh_of(a, s)
    }

    pub fn rsh(a: Tnum, s: u32) -> Tnum {
        crate::port::ffi::tnum_rsh_of(a, s)
    }

    pub fn arsh(a: Tnum, s: u32) -> Tnum {
        crate::port::ffi::tnum_arsh_of(a, s)
    }

    /// Intersection; `None` when the two disagree on a known bit.
    pub fn intersect(a: Tnum, b: Tnum) -> Option<Tnum> {
        crate::port::ffi::tnum_intersect_of(a, b)
    }

    /// Keep the low `bits` bits (zero-extend).
    pub fn truncate(a: Tnum, bits: u32) -> Tnum {
        crate::port::ffi::tnum_truncate_of(a, bits)
    }
}

/// A set of 64-bit values: the intersection of a tnum, an unsigned interval
/// and a signed interval. Always kept normalized and nonempty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scalar {
    pub umin: u64,
    pub umax: u64,
    pub smin: i64,
    pub smax: i64,
    pub var: Tnum,
}

impl Scalar {
    pub const UNKNOWN: Scalar = Scalar {
        umin: 0,
        umax: u64::MAX,
        smin: i64::MIN,
        smax: i64::MAX,
        var: Tnum::UNKNOWN,
    };

    pub fn konst(v: u64) -> Scalar {
        Scalar {
            umin: v,
            umax: v,
            smin: v as i64,
            smax: v as i64,
            var: Tnum::konst(v),
        }
    }

    pub fn urange(min: u64, max: u64) -> Scalar {
        crate::port::ffi::scalar_urange_of(min, max)
    }

    pub fn srange(min: i64, max: i64) -> Scalar {
        crate::port::ffi::scalar_srange_of(min, max)
    }

    /// All values that fit in `bits` bits, zero-extended.
    pub fn unknown_bits(bits: u32) -> Scalar {
        crate::port::ffi::scalar_unknown_bits_of(bits)
    }

    pub fn as_const(&self) -> Option<u64> {
        (self.umin == self.umax).then_some(self.umin)
    }

    pub fn contains(&self, v: u64) -> bool {
        crate::port::ffi::scalar_contains_value(*self, v)
    }

    /// `other` ⊆ `self` (sufficient check on the representation).
    pub fn includes(&self, other: &Scalar) -> bool {
        crate::port::ffi::scalar_includes_scalar(*self, *other)
    }

    /// Reduce the three components against each other. `None` when empty.
    pub fn normalize(self) -> Option<Scalar> {
        crate::port::ffi::scalar_normalize_of(self)
    }

    pub fn add(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_add_of(a, b)
    }

    pub fn sub(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_sub_of(a, b)
    }

    pub fn mul(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_mul_of(a, b)
    }

    pub fn and(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_and_of(a, b)
    }

    pub fn or(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_or_of(a, b)
    }

    pub fn xor(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_xor_of(a, b)
    }

    /// Shift by an amount already masked to `0..64`.
    pub fn lsh(a: Scalar, s: u32) -> Scalar {
        crate::port::ffi::scalar_lsh_of(a, s)
    }

    pub fn rsh(a: Scalar, s: u32) -> Scalar {
        crate::port::ffi::scalar_rsh_of(a, s)
    }

    pub fn arsh(a: Scalar, s: u32) -> Scalar {
        crate::port::ffi::scalar_arsh_of(a, s)
    }

    /// Zero-extend from 32 bits (the result of every 32-bit ALU op).
    pub fn zext32(a: Scalar) -> Scalar {
        crate::port::ffi::scalar_zext32_of(a)
    }

    /// Keep the low `bits` bits.
    pub fn truncate(a: Scalar, bits: u32) -> Scalar {
        crate::port::ffi::scalar_truncate_of(a, bits)
    }

    /// Values sign-extended from the low `bits` bits.
    pub fn sext(a: Scalar, bits: u32) -> Scalar {
        crate::port::ffi::scalar_sext_of(a, bits)
    }

    /// Hull of two scalars (used only for diagnostics, never for joins).
    pub fn hull(a: Scalar, b: Scalar) -> Scalar {
        crate::port::ffi::scalar_hull_of(a, b)
    }

    /// Intersection of two scalars; `None` when disjoint.
    pub fn meet(a: Scalar, b: Scalar) -> Option<Scalar> {
        crate::port::ffi::scalar_meet_of(a, b)
    }
}

pub fn sext_const(c: u64, bits: u32) -> u64 {
    crate::port::ffi::scalar_sext_const(c, bits)
}
