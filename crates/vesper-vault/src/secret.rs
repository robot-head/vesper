//! §23.2 opaque secret scalars.
//!
//! `SecU8`, `SecU32`, `SecU64` and `SecVec` deliberately omit `PartialEq`,
//! `Ord`, `Hash`, `Div`, `Rem`, `Display` and a contents-revealing `Debug`.
//! Comparisons return a [`SecMask64`] (all zeros or all ones) usable only in
//! branchless selects; reading a secret back as a public value requires the
//! explicit [`SecU64::declassify`] or [`SecMask64::declassify_bool`] gate.
//!
//! The implementations are straight-line integer arithmetic, with
//! `core::hint::black_box` on mask intermediates so that LLVM is discouraged
//! from rebuilding branches. That is best effort: Rust gives no guarantee
//! about the machine code emitted for this file, and nothing here is claimed
//! to be constant time beyond what the Cranelift verifier in
//! [`crate::verifier`] checks for code Vesper itself emits.

use core::fmt;
use core::hint::black_box;
use core::ops::{BitAnd, BitOr, BitXor, Not};

/// An opaque all-zeros (false) or all-ones (true) secret mask.
#[derive(Clone, Copy)]
pub struct SecMask64(u64);

impl fmt::Debug for SecMask64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecMask64(<secret>)")
    }
}

/// Turn a public-free bit (0 or 1) into a mask without branching.
#[inline]
fn mask_from_bit(bit: u64) -> SecMask64 {
    SecMask64(black_box(bit & 1).wrapping_neg())
}

impl SecMask64 {
    /// The all-ones mask.
    pub const TRUE: SecMask64 = SecMask64(u64::MAX);
    /// The all-zeros mask.
    pub const FALSE: SecMask64 = SecMask64(0);

    /// Lift a public boolean into a mask.
    #[inline]
    pub fn from_public_bool(b: bool) -> Self {
        mask_from_bit(b as u64)
    }

    /// The explicit, auditable declassification gate for a mask.
    #[inline]
    #[must_use]
    pub fn declassify_bool(self) -> bool {
        black_box(self.0) & 1 == 1
    }

    /// Lane value for selects (all zeros or all ones).
    #[inline]
    fn bits(self) -> u64 {
        self.0
    }
}

impl BitAnd for SecMask64 {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        SecMask64(self.0 & rhs.0)
    }
}

impl BitOr for SecMask64 {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        SecMask64(self.0 | rhs.0)
    }
}

impl Not for SecMask64 {
    type Output = Self;
    #[inline]
    fn not(self) -> Self {
        SecMask64(!self.0)
    }
}

macro_rules! sec_scalar {
    ($name:ident, $t:ty, $bits:expr) => {
        #[doc = concat!("An opaque secret `", stringify!($t), "`.")]
        #[derive(Clone, Copy)]
        pub struct $name($t);

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<secret>)"))
            }
        }

        impl $name {
            /// Classify a value as secret.
            #[inline]
            pub const fn new(v: $t) -> Self {
                Self(v)
            }

            /// The explicit, auditable declassification gate.
            #[inline]
            #[must_use]
            pub fn declassify(self) -> $t {
                self.0
            }

            /// Bitwise exclusive or.
            #[inline]
            pub fn bit_xor(self, o: Self) -> Self {
                Self(self.0 ^ o.0)
            }

            /// Bitwise or.
            #[inline]
            pub fn bit_or(self, o: Self) -> Self {
                Self(self.0 | o.0)
            }

            /// Bitwise and.
            #[inline]
            pub fn bit_and(self, o: Self) -> Self {
                Self(self.0 & o.0)
            }

            /// Wrapping addition (no overflow branch).
            #[inline]
            pub fn wrapping_add(self, o: Self) -> Self {
                Self(self.0.wrapping_add(o.0))
            }

            /// Wrapping subtraction (no overflow branch).
            #[inline]
            pub fn wrapping_sub(self, o: Self) -> Self {
                Self(self.0.wrapping_sub(o.0))
            }

            /// Wrapping multiplication.
            #[inline]
            pub fn wrapping_mul(self, o: Self) -> Self {
                Self(self.0.wrapping_mul(o.0))
            }

            /// Left shift by a public amount (taken modulo the width).
            #[inline]
            pub fn shift_left(self, by: u32) -> Self {
                Self(self.0.wrapping_shl(by))
            }

            /// Logical right shift by a public amount (taken modulo the width).
            #[inline]
            pub fn shift_right(self, by: u32) -> Self {
                Self(self.0.wrapping_shr(by))
            }

            /// Rotate left by a public amount.
            #[inline]
            pub fn rotate_left(self, by: u32) -> Self {
                Self(self.0.rotate_left(by))
            }

            /// All-ones mask when the value is zero.
            #[inline]
            pub fn ct_is_zero(self) -> SecMask64 {
                let x = self.0 as u64;
                let nonzero = (x | x.wrapping_neg()) >> 63;
                mask_from_bit(nonzero ^ 1)
            }

            /// All-ones mask when the values are equal.
            #[inline]
            pub fn ct_eq(self, o: Self) -> SecMask64 {
                self.bit_xor(o).ct_is_zero()
            }

            /// All-ones mask when `self < o` (unsigned).
            #[inline]
            pub fn ct_lt(self, o: Self) -> SecMask64 {
                let (a, b) = (self.0 as u64, o.0 as u64);
                // Borrow out of a - b, computed without a comparison.
                let borrow = ((!a & b) | (!(a ^ b) & a.wrapping_sub(b))) >> 63;
                mask_from_bit(borrow)
            }

            /// `mask ? a : b` without branching.
            #[inline]
            pub fn ct_select(mask: SecMask64, a: Self, b: Self) -> Self {
                let m = mask.bits() as $t;
                Self((a.0 & m) | (b.0 & !m))
            }

            /// Width in bits.
            pub const BITS: u32 = $bits;
        }

        impl BitXor for $name {
            type Output = Self;
            #[inline]
            fn bitxor(self, o: Self) -> Self {
                self.bit_xor(o)
            }
        }

        impl BitAnd for $name {
            type Output = Self;
            #[inline]
            fn bitand(self, o: Self) -> Self {
                self.bit_and(o)
            }
        }

        impl BitOr for $name {
            type Output = Self;
            #[inline]
            fn bitor(self, o: Self) -> Self {
                self.bit_or(o)
            }
        }

        impl Not for $name {
            type Output = Self;
            #[inline]
            fn not(self) -> Self {
                Self(!self.0)
            }
        }

        impl SecLane for $name {
            #[inline]
            fn lane_xor(self, o: Self) -> Self {
                self.bit_xor(o)
            }
            #[inline]
            fn lane_and(self, o: Self) -> Self {
                self.bit_and(o)
            }
            #[inline]
            fn lane_or(self, o: Self) -> Self {
                self.bit_or(o)
            }
            #[inline]
            fn lane_is_zero(self) -> SecMask64 {
                self.ct_is_zero()
            }
            #[inline]
            fn lane_select(mask: SecMask64, a: Self, b: Self) -> Self {
                Self::ct_select(mask, a, b)
            }
            #[inline]
            fn zero() -> Self {
                Self(0)
            }
        }

        impl private::Sealed for $name {}
    };
}

sec_scalar!(SecU8, u8, 8);
sec_scalar!(SecU32, u32, 32);
sec_scalar!(SecU64, u64, 64);

impl SecU64 {
    /// Load a little-endian secret `u64` from `bytes[offset..offset + 8]`.
    /// The offset is public; a short buffer is a typed error.
    pub fn load_le(bytes: &[u8], offset: usize) -> Result<Self, OutOfBounds> {
        let end = offset.checked_add(8).ok_or(OutOfBounds)?;
        let chunk: [u8; 8] = bytes
            .get(offset..end)
            .ok_or(OutOfBounds)?
            .try_into()
            .map_err(|_| OutOfBounds)?;
        Ok(Self(u64::from_le_bytes(chunk)))
    }
}

/// A secret load addressed past the end of its public-length buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfBounds;

impl fmt::Display for OutOfBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("secret load out of bounds")
    }
}

impl std::error::Error for OutOfBounds {}

mod private {
    pub trait Sealed {}
}

/// Lane operations shared by the secret scalars (sealed).
pub trait SecLane: Copy + private::Sealed {
    /// Lane xor.
    fn lane_xor(self, o: Self) -> Self;
    /// Lane and.
    fn lane_and(self, o: Self) -> Self;
    /// Lane or.
    fn lane_or(self, o: Self) -> Self;
    /// Lane zero test.
    fn lane_is_zero(self) -> SecMask64;
    /// Lane select.
    fn lane_select(mask: SecMask64, a: Self, b: Self) -> Self;
    /// The zero lane.
    fn zero() -> Self;
}

/// A fixed-width vector of secret lanes (the `SecVec[P, T]` of §23.1, with
/// the lane count as a const parameter and no heap storage).
#[derive(Clone, Copy)]
pub struct SecVec<T: SecLane, const N: usize>([T; N]);

impl<T: SecLane, const N: usize> fmt::Debug for SecVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecVec<{N}>(<secret>)")
    }
}

impl<T: SecLane, const N: usize> SecVec<T, N> {
    /// Wrap secret lanes.
    pub fn new(lanes: [T; N]) -> Self {
        Self(lanes)
    }

    /// All-zero vector.
    pub fn zeroed() -> Self {
        Self([T::zero(); N])
    }

    /// Lane-wise xor.
    pub fn bit_xor(self, o: Self) -> Self {
        let mut out = self.0;
        for (a, b) in out.iter_mut().zip(o.0) {
            *a = a.lane_xor(b);
        }
        Self(out)
    }

    /// Lane-wise and.
    pub fn bit_and(self, o: Self) -> Self {
        let mut out = self.0;
        for (a, b) in out.iter_mut().zip(o.0) {
            *a = a.lane_and(b);
        }
        Self(out)
    }

    /// Lane-wise or.
    pub fn bit_or(self, o: Self) -> Self {
        let mut out = self.0;
        for (a, b) in out.iter_mut().zip(o.0) {
            *a = a.lane_or(b);
        }
        Self(out)
    }

    /// All-ones mask when every lane is equal. Visits every lane regardless
    /// of where the first difference is.
    pub fn ct_eq(self, o: Self) -> SecMask64 {
        let diff = self.bit_xor(o);
        let mut acc = SecMask64::TRUE;
        for lane in diff.0 {
            acc = acc & lane.lane_is_zero();
        }
        acc
    }

    /// Lane-wise `mask ? a : b`.
    pub fn ct_select(mask: SecMask64, a: Self, b: Self) -> Self {
        let mut out = a.0;
        for (x, y) in out.iter_mut().zip(b.0) {
            *x = T::lane_select(mask, *x, y);
        }
        Self(out)
    }

    /// Explicit declassification of every lane.
    #[must_use]
    pub fn declassify(self) -> [T; N] {
        self.0
    }
}

/// §23.2 `verify_tag_16_ct`: branchless 16-byte tag comparison with a single
/// declassification at the verdict. Mirrors `VaultSpec.verifyTag16Ct` in
/// `lean/VaultConstantTime.lean`, where `verify_tag_16_ct_sound` proves the
/// modelled computation equals equality on all 2^256 inputs.
pub fn verify_tag_16_ct(expected: &[u8; 16], computed: &[u8; 16]) -> bool {
    let e_lo = SecU64::new(u64::from_le_bytes(
        expected[0..8].try_into().unwrap_or([0; 8]),
    ));
    let e_hi = SecU64::new(u64::from_le_bytes(
        expected[8..16].try_into().unwrap_or([0; 8]),
    ));
    let c_lo = SecU64::new(u64::from_le_bytes(
        computed[0..8].try_into().unwrap_or([0; 8]),
    ));
    let c_hi = SecU64::new(u64::from_le_bytes(
        computed[8..16].try_into().unwrap_or([0; 8]),
    ));
    let combined = e_lo.bit_xor(c_lo).bit_or(e_hi.bit_xor(c_hi));
    combined.ct_is_zero().declassify_bool()
}
