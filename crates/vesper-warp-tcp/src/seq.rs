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
