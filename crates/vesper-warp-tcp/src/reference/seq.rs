//! The Rust reference implementation of [`crate::seq`], compiled without
//! `vesper-port` (the Vesper build is `vesper/seq.ves`).

use crate::seq::{Seq, SeqMap};

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
