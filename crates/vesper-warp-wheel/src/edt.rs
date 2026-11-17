//! Earliest-departure-time launch queue.
//!
//! Packets are submitted with an EDT (the pacing departure time computed by
//! the congestion controller, §35.4 `bbrv3_edt_launch_monotonic`) and released
//! once `edt_ns <= now_ns`, in non-decreasing EDT order (FIFO among equal
//! EDTs). Submissions further than `horizon_ns` past the queue's clock are
//! rejected, modelling the `SO_TXTIME`/fq horizon that drops packets scheduled
//! too far ahead. This is a software scheduling model only: it does not use or
//! claim kernel timers, NIC launch-time offload or interrupt-free pacing.

use std::fmt;

/// Why a submission was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitErrorKind {
    /// `edt_ns > now_ns + horizon_ns`.
    BeyondHorizon,
    /// `edt_ns` is earlier than a departure already released; accepting it
    /// would break non-decreasing release order.
    BehindReleased,
    /// `now_ns` is earlier than the queue's clock.
    ClockBackwards,
    /// Time outside the supported range.
    OutOfRange,
    /// All slots are in use.
    Full,
}

/// A refused submission; the packet is handed back.
#[must_use = "the refused packet is returned and must be handled"]
pub struct SubmitError<P> {
    /// The reason.
    pub kind: SubmitErrorKind,
    /// The packet that was not queued.
    pub packet: P,
}

impl<P> SubmitError<P> {
    /// Recover the packet.
    pub fn into_packet(self) -> P {
        self.packet
    }
}

impl<P> fmt::Debug for SubmitError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<P> fmt::Display for SubmitError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            SubmitErrorKind::BeyondHorizon => "departure time is beyond the launch horizon",
            SubmitErrorKind::BehindReleased => "departure time precedes an already released one",
            SubmitErrorKind::ClockBackwards => "clock moved backwards",
            SubmitErrorKind::OutOfRange => "time exceeds MAX_TIME_NS",
            SubmitErrorKind::Full => "launch queue is at capacity",
        };
        f.write_str(what)
    }
}

impl<P> std::error::Error for SubmitError<P> {}

/// A packet whose departure time has been reached.
#[must_use = "a released packet carries its owner"]
#[derive(Debug, PartialEq, Eq)]
pub struct Launch<P> {
    /// The departure time (`<= now`).
    pub edt_ns: u64,
    /// The owned packet.
    pub packet: P,
}

// The Rust build (the frozen reference) or the Vesper build (a thin wrapper
// over the Vesper port in vesper/src/).
#[cfg(not(feature = "vesper-port"))]
#[path = "reference/edt.rs"]
mod imp;
#[cfg(feature = "vesper-port")]
#[path = "port/edt.rs"]
mod imp;

pub use imp::EdtLaunchQueue;
