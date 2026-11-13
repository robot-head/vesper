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
        let chi = min ^ max;
        let bits = 64 - chi.leading_zeros();
        if bits > 63 {
            return Tnum::UNKNOWN;
        }
        let delta = (1u64 << bits) - 1;
        Tnum {
            value: min & !delta,
            mask: delta,
        }
    }

    pub fn is_const(self) -> bool {
        self.mask == 0
    }

    pub fn contains(self, v: u64) -> bool {
        v & !self.mask == self.value
    }

    /// `other` ⊆ `self`.
    pub fn includes(self, other: Tnum) -> bool {
        if other.mask & !self.mask != 0 {
            return false;
        }
        other.value & !self.mask == self.value
    }

    pub fn umin(self) -> u64 {
        self.value
    }

    pub fn umax(self) -> u64 {
        self.value | self.mask
    }

    pub fn add(a: Tnum, b: Tnum) -> Tnum {
        let sm = a.mask.wrapping_add(b.mask);
        let sv = a.value.wrapping_add(b.value);
        let sigma = sm.wrapping_add(sv);
        let chi = sigma ^ sv;
        let mu = chi | a.mask | b.mask;
        Tnum {
            value: sv & !mu,
            mask: mu,
        }
    }

    pub fn sub(a: Tnum, b: Tnum) -> Tnum {
        let dv = a.value.wrapping_sub(b.value);
        let alpha = dv.wrapping_add(a.mask);
        let beta = dv.wrapping_sub(b.mask);
        let chi = alpha ^ beta;
        let mu = chi | a.mask | b.mask;
        Tnum {
            value: dv & !mu,
            mask: mu,
        }
    }

    pub fn and(a: Tnum, b: Tnum) -> Tnum {
        let alpha = a.value | a.mask;
        let beta = b.value | b.mask;
        let v = a.value & b.value;
        Tnum {
            value: v,
            mask: alpha & beta & !v,
        }
    }

    pub fn or(a: Tnum, b: Tnum) -> Tnum {
        let v = a.value | b.value;
        let mu = a.mask | b.mask;
        Tnum {
            value: v,
            mask: mu & !v,
        }
    }

    pub fn xor(a: Tnum, b: Tnum) -> Tnum {
        let v = a.value ^ b.value;
        let mu = a.mask | b.mask;
        Tnum {
            value: v & !mu,
            mask: mu,
        }
    }

    pub fn lsh(a: Tnum, s: u32) -> Tnum {
        Tnum {
            value: a.value << s,
            mask: a.mask << s,
        }
    }

    pub fn rsh(a: Tnum, s: u32) -> Tnum {
        Tnum {
            value: a.value >> s,
            mask: a.mask >> s,
        }
    }

    pub fn arsh(a: Tnum, s: u32) -> Tnum {
        let v = ((a.value as i64) >> s) as u64;
        let m = ((a.mask as i64) >> s) as u64;
        Tnum {
            value: v & !m,
            mask: m,
        }
    }

    /// Intersection; `None` when the two disagree on a known bit.
    pub fn intersect(a: Tnum, b: Tnum) -> Option<Tnum> {
        let known_both = !a.mask & !b.mask;
        if (a.value ^ b.value) & known_both != 0 {
            return None;
        }
        let v = a.value | b.value;
        let mu = a.mask & b.mask;
        Some(Tnum {
            value: v & !mu,
            mask: mu,
        })
    }

    /// Keep the low `bits` bits (zero-extend).
    pub fn truncate(a: Tnum, bits: u32) -> Tnum {
        if bits >= 64 {
            return a;
        }
        let m = (1u64 << bits) - 1;
        Tnum {
            value: a.value & m,
            mask: a.mask & m,
        }
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
        Scalar {
            umin: min,
            umax: max,
            ..Scalar::UNKNOWN
        }
        .normalize()
        .unwrap_or(Scalar::UNKNOWN)
    }

    pub fn srange(min: i64, max: i64) -> Scalar {
        Scalar {
            smin: min,
            smax: max,
            ..Scalar::UNKNOWN
        }
        .normalize()
        .unwrap_or(Scalar::UNKNOWN)
    }

    /// All values that fit in `bits` bits, zero-extended.
    pub fn unknown_bits(bits: u32) -> Scalar {
        if bits >= 64 {
            Scalar::UNKNOWN
        } else {
            Scalar::urange(0, (1u64 << bits) - 1)
        }
    }

    pub fn as_const(&self) -> Option<u64> {
        (self.umin == self.umax).then_some(self.umin)
    }

    pub fn contains(&self, v: u64) -> bool {
        self.umin <= v
            && v <= self.umax
            && self.smin <= v as i64
            && v as i64 <= self.smax
            && self.var.contains(v)
    }

    /// `other` ⊆ `self` (sufficient check on the representation).
    pub fn includes(&self, other: &Scalar) -> bool {
        self.umin <= other.umin
            && other.umax <= self.umax
            && self.smin <= other.smin
            && other.smax <= self.smax
            && self.var.includes(other.var)
    }

    /// Reduce the three components against each other. `None` when empty.
    pub fn normalize(self) -> Option<Scalar> {
        let mut s = self;
        for _ in 0..3 {
            // tnum -> intervals
            s.umin = s.umin.max(s.var.umin());
            s.umax = s.umax.min(s.var.umax());
            let tsmin = (s.var.value | (s.var.mask & (1u64 << 63))) as i64;
            let tsmax = (s.var.value | (s.var.mask & !(1u64 << 63))) as i64;
            s.smin = s.smin.max(tsmin);
            s.smax = s.smax.min(tsmax);
            if s.umin > s.umax || s.smin > s.smax {
                return None;
            }
            // signed <-> unsigned
            if s.smin >= 0 || s.smax < 0 {
                // the signed range does not cross the sign boundary, so it is
                // also an unsigned range
                let lo = s.umin.max(s.smin as u64);
                let hi = s.umax.min(s.smax as u64);
                if lo > hi {
                    return None;
                }
                s.umin = lo;
                s.umax = hi;
                s.smin = lo as i64;
                s.smax = hi as i64;
            } else if (s.umax as i64) >= 0 {
                // unsigned range entirely in [0, i64::MAX]
                s.smin = s.umin as i64;
                s.smax = s.smax.min(s.umax as i64);
                s.umax = s.smax as u64;
            } else if (s.umin as i64) < 0 {
                // unsigned range entirely in [2^63, 2^64)
                s.smin = s.smin.max(s.umin as i64);
                s.smax = s.umax as i64;
                s.umin = s.smin as u64;
            }
            if s.umin > s.umax || s.smin > s.smax {
                return None;
            }
            // intervals -> tnum
            s.var = Tnum::intersect(s.var, Tnum::range(s.umin, s.umax))?;
        }
        if s.umin > s.umax || s.smin > s.smax {
            return None;
        }
        Some(s)
    }

    fn norm(self) -> Scalar {
        // Transfer functions build supersets of a nonempty set, so the
        // result is nonempty; fall back to UNKNOWN defensively.
        self.normalize().unwrap_or(Scalar::UNKNOWN)
    }

    fn from_parts(umin: u64, umax: u64, smin: i64, smax: i64, var: Tnum) -> Scalar {
        Scalar {
            umin,
            umax,
            smin,
            smax,
            var,
        }
        .norm()
    }

    pub fn add(a: Scalar, b: Scalar) -> Scalar {
        let var = Tnum::add(a.var, b.var);
        let (umin, umax) = match (a.umin.checked_add(b.umin), a.umax.checked_add(b.umax)) {
            (Some(lo), Some(hi)) => (lo, hi),
            _ => (0, u64::MAX),
        };
        let (smin, smax) = match (a.smin.checked_add(b.smin), a.smax.checked_add(b.smax)) {
            (Some(lo), Some(hi)) => (lo, hi),
            _ => (i64::MIN, i64::MAX),
        };
        Scalar::from_parts(umin, umax, smin, smax, var)
    }

    pub fn sub(a: Scalar, b: Scalar) -> Scalar {
        let var = Tnum::sub(a.var, b.var);
        let (umin, umax) = if a.umin >= b.umax {
            (a.umin - b.umax, a.umax - b.umin)
        } else {
            (0, u64::MAX)
        };
        let (smin, smax) = match (a.smin.checked_sub(b.smax), a.smax.checked_sub(b.smin)) {
            (Some(lo), Some(hi)) => (lo, hi),
            _ => (i64::MIN, i64::MAX),
        };
        Scalar::from_parts(umin, umax, smin, smax, var)
    }

    pub fn mul(a: Scalar, b: Scalar) -> Scalar {
        if let (Some(x), Some(y)) = (a.as_const(), b.as_const()) {
            return Scalar::konst(x.wrapping_mul(y));
        }
        match (a.umax.checked_mul(b.umax), a.umin.checked_mul(b.umin)) {
            (Some(hi), Some(lo)) => Scalar::urange(lo, hi),
            _ => Scalar::UNKNOWN,
        }
    }

    pub fn and(a: Scalar, b: Scalar) -> Scalar {
        let var = Tnum::and(a.var, b.var);
        let umax = a.umax.min(b.umax);
        let (smin, smax) = if a.smin >= 0 || b.smin >= 0 {
            // the result is <= the nonnegative operand and is nonnegative
            let hi = if a.smin >= 0 && b.smin >= 0 {
                a.smax.min(b.smax)
            } else if a.smin >= 0 {
                a.smax
            } else {
                b.smax
            };
            (0, hi)
        } else {
            (i64::MIN, i64::MAX)
        };
        Scalar::from_parts(0, umax, smin, smax, var)
    }

    pub fn or(a: Scalar, b: Scalar) -> Scalar {
        let var = Tnum::or(a.var, b.var);
        Scalar::from_parts(a.umin.max(b.umin), u64::MAX, i64::MIN, i64::MAX, var)
    }

    pub fn xor(a: Scalar, b: Scalar) -> Scalar {
        Scalar::from_parts(0, u64::MAX, i64::MIN, i64::MAX, Tnum::xor(a.var, b.var))
    }

    /// Shift by an amount already masked to `0..64`.
    pub fn lsh(a: Scalar, s: u32) -> Scalar {
        let var = Tnum::lsh(a.var, s);
        if s == 0 {
            return a;
        }
        if a.umax.leading_zeros() >= s {
            Scalar::from_parts(a.umin << s, a.umax << s, i64::MIN, i64::MAX, var)
        } else {
            Scalar::from_parts(0, u64::MAX, i64::MIN, i64::MAX, var)
        }
    }

    pub fn rsh(a: Scalar, s: u32) -> Scalar {
        let var = Tnum::rsh(a.var, s);
        Scalar::from_parts(a.umin >> s, a.umax >> s, i64::MIN, i64::MAX, var)
    }

    pub fn arsh(a: Scalar, s: u32) -> Scalar {
        let var = Tnum::arsh(a.var, s);
        Scalar::from_parts(0, u64::MAX, a.smin >> s, a.smax >> s, var)
    }

    /// Zero-extend from 32 bits (the result of every 32-bit ALU op).
    pub fn zext32(a: Scalar) -> Scalar {
        if a.umax <= u32::MAX as u64 {
            a
        } else {
            Scalar::from_parts(
                0,
                u32::MAX as u64,
                i64::MIN,
                i64::MAX,
                Tnum::truncate(a.var, 32),
            )
        }
    }

    /// Keep the low `bits` bits.
    pub fn truncate(a: Scalar, bits: u32) -> Scalar {
        if bits >= 64 {
            return a;
        }
        let m = (1u64 << bits) - 1;
        if a.umax <= m {
            a
        } else {
            Scalar::from_parts(0, m, i64::MIN, i64::MAX, Tnum::truncate(a.var, bits))
        }
    }

    /// Values sign-extended from the low `bits` bits.
    pub fn sext(a: Scalar, bits: u32) -> Scalar {
        if let Some(c) = a.as_const() {
            return Scalar::konst(sext_const(c, bits));
        }
        let half = 1u64 << (bits - 1);
        let t = Scalar::truncate(a, bits);
        if t.umax < half {
            // sign bit clear: the low bits zero-extended
            t
        } else {
            Scalar::srange(-(half as i64), half as i64 - 1)
        }
    }

    /// Hull of two scalars (used only for diagnostics, never for joins).
    pub fn hull(a: Scalar, b: Scalar) -> Scalar {
        let x = a.var.value ^ b.var.value;
        let mask = a.var.mask | b.var.mask | x;
        Scalar::from_parts(
            a.umin.min(b.umin),
            a.umax.max(b.umax),
            a.smin.min(b.smin),
            a.smax.max(b.smax),
            Tnum {
                value: a.var.value & !mask,
                mask,
            },
        )
    }

    /// Intersection of two scalars; `None` when disjoint.
    pub fn meet(a: Scalar, b: Scalar) -> Option<Scalar> {
        Scalar {
            umin: a.umin.max(b.umin),
            umax: a.umax.min(b.umax),
            smin: a.smin.max(b.smin),
            smax: a.smax.min(b.smax),
            var: Tnum::intersect(a.var, b.var)?,
        }
        .normalize()
    }
}

pub fn sext_const(c: u64, bits: u32) -> u64 {
    match bits {
        8 => c as u8 as i8 as i64 as u64,
        16 => c as u16 as i16 as i64 as u64,
        32 => c as u32 as i32 as i64 as u64,
        _ => c,
    }
}
