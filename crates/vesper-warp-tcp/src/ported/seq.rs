//! [`crate::seq`] forwarded to the Vesper build.

use super::binding as b;
use crate::seq::{Seq, SeqMap};

impl Seq {
    /// `self + n` modulo 2^32.
    #[must_use]
    // The public name of the Rust build's `const fn add`.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, n: u32) -> Seq {
        Seq(b::sn_add(self.0, n))
    }

    /// Signed distance `self - other`, interpreting the difference modulo
    /// 2^32 as a two's-complement `i32`.
    #[must_use]
    pub fn diff(self, other: Seq) -> i32 {
        b::sn_diff(self.0, other.0)
    }

    /// `self < other` in sequence space.
    #[must_use]
    pub fn lt(self, other: Seq) -> bool {
        b::sn_lt(self.0, other.0)
    }

    /// `self <= other` in sequence space.
    #[must_use]
    pub fn le(self, other: Seq) -> bool {
        b::sn_le(self.0, other.0)
    }

    /// `self > other` in sequence space.
    #[must_use]
    pub fn gt(self, other: Seq) -> bool {
        b::sn_gt(self.0, other.0)
    }

    /// `self >= other` in sequence space.
    #[must_use]
    pub fn ge(self, other: Seq) -> bool {
        b::sn_ge(self.0, other.0)
    }

    /// `lo <= self < hi`, the half-open window test used by RFC 9293
    /// segment acceptability.
    #[must_use]
    pub fn in_window(self, lo: Seq, hi: Seq) -> bool {
        b::sn_in_window(self.0, lo.0, hi.0)
    }
}

impl SeqMap {
    /// Sequence number of absolute position `abs` (truncation modulo 2^32 is
    /// the definition of the sequence space).
    #[must_use]
    pub fn seq(self, abs: u64) -> Seq {
        Seq(b::seqmap_seq(self.isn.0, abs))
    }

    /// Lift a wire sequence number to the absolute position closest to
    /// `reference`. Returns `None` when it would precede the ISN.
    #[must_use]
    pub fn abs(self, reference: u64, seq: Seq) -> Option<u64> {
        b::seqmap_abs(self.isn.0, reference, seq.0)
    }
}
