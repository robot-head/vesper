//! Modular 32-bit sequence-number arithmetic (RFC 9293 §3.4).
//!
//! Comparisons are only meaningful for numbers less than 2^31 apart, which the
//! window limits (at most 2^30 with RFC 7323 scaling) guarantee for every
//! comparison this crate performs.

use core::fmt;

/// A TCP sequence or acknowledgment number. Arithmetic wraps modulo 2^32 by
/// definition of the sequence space, so every `+`/`-` here is explicitly
/// wrapping.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Seq(pub u32);

impl Seq {
    /// `self + n` modulo 2^32.
    #[must_use]
    pub const fn add(self, n: u32) -> Seq {
        Seq(self.0.wrapping_add(n))
    }

    /// Signed distance `self - other`, interpreting the difference modulo
    /// 2^32 as a two's-complement `i32`.
    #[must_use]
    pub const fn diff(self, other: Seq) -> i32 {
        self.0.wrapping_sub(other.0) as i32
    }

    /// `self < other` in sequence space.
    #[must_use]
    pub const fn lt(self, other: Seq) -> bool {
        self.diff(other) < 0
    }

    /// `self <= other` in sequence space.
    #[must_use]
    pub const fn le(self, other: Seq) -> bool {
        self.diff(other) <= 0
    }

    /// `self > other` in sequence space.
    #[must_use]
    pub const fn gt(self, other: Seq) -> bool {
        self.diff(other) > 0
    }

    /// `self >= other` in sequence space.
    #[must_use]
    pub const fn ge(self, other: Seq) -> bool {
        self.diff(other) >= 0
    }

    /// `lo <= self < hi`, the half-open window test used by RFC 9293
    /// segment acceptability.
    #[must_use]
    pub const fn in_window(self, lo: Seq, hi: Seq) -> bool {
        lo.le(self) && self.lt(hi)
    }
}

impl fmt::Debug for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Seq({})", self.0)
    }
}

/// Maps unbounded 64-bit stream positions onto the 32-bit sequence space.
///
/// Position 0 is the initial sequence number (the SYN); data byte `k` is at
/// position `k + 1`; the FIN follows the last data byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SeqMap {
    /// The initial sequence number.
    pub isn: Seq,
}

impl SeqMap {
    /// Sequence number of absolute position `abs` (truncation modulo 2^32 is
    /// the definition of the sequence space).
    #[must_use]
    pub const fn seq(self, abs: u64) -> Seq {
        self.isn.add(abs as u32)
    }

    /// Lift a wire sequence number to the absolute position closest to
    /// `reference`. Returns `None` when it would precede the ISN.
    #[must_use]
    pub fn abs(self, reference: u64, seq: Seq) -> Option<u64> {
        let delta = seq.diff(self.seq(reference));
        reference.checked_add_signed(i64::from(delta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraparound_ordering() {
        let a = Seq(u32::MAX - 5);
        let b = a.add(10);
        assert_eq!(b, Seq(4));
        assert!(a.lt(b) && b.gt(a) && a.le(a) && a.ge(a));
        assert_eq!(b.diff(a), 10);
        assert_eq!(a.diff(b), -10);
        assert!(Seq(0).in_window(a, b));
        assert!(!b.in_window(a, b));
    }

    #[test]
    fn lift_absolute_positions() {
        let map = SeqMap {
            isn: Seq(u32::MAX - 1),
        };
        assert_eq!(map.seq(0), Seq(u32::MAX - 1));
        assert_eq!(map.seq(3), Seq(1));
        assert_eq!(map.abs(0, Seq(1)), Some(3));
        let far = 5u64 << 32;
        assert_eq!(map.abs(far, map.seq(far + 7)), Some(far + 7));
        assert_eq!(map.abs(far, map.seq(far - 7)), Some(far - 7));
        assert_eq!(map.abs(0, Seq(u32::MAX - 3)), None);
    }
}
