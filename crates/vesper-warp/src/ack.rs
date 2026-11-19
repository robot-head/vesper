//! Per-packet-number-space received-packet tracking (RFC 9000 §13.2).
//!
//! [`ReceivedPackets`] keeps at most `N` disjoint received ranges in a fixed
//! array (no heap), detects duplicates, and produces ACK frames whose range
//! count is bounded by `N`. When a new range would exceed `N`, the lowest
//! range is forgotten and every packet number at or below it is thereafter
//! reported as [`AckError::TooOld`] rather than risk processing a duplicate
//! (RFC 9000 §13.2.3 permits not acknowledging old ranges; §12.3 requires
//! duplicate suppression, which this conservatively over-approximates).

#[cfg(not(feature = "vesper-port"))]
use crate::PnSpace;
use crate::frame::AckRange;
#[cfg(not(feature = "vesper-port"))]
use crate::frame::EcnCounts;

/// Default ACK range bound.
pub const DEFAULT_MAX_RANGES: usize = 32;
/// RFC 9000 §18.2 default `ack_delay_exponent`.
pub const DEFAULT_ACK_DELAY_EXPONENT: u8 = 3;
/// RFC 9000 §13.2.2: acknowledge at least every second ack-eliciting packet.
pub const ACK_ELICITING_THRESHOLD: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckError {
    /// Already received.
    Duplicate(u64),
    /// At or below the forgotten floor: possibly a duplicate, so dropped.
    TooOld(u64),
    OutOfRange(u64),
    /// Reception time earlier than a previous one.
    ClockWentBackwards,
    /// `ack_delay_exponent` above 20 (RFC 9000 §18.2).
    BadAckDelayExponent(u8),
}

/// The ECN codepoint a packet arrived with (from the IP header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecn {
    NotEct,
    Ect0,
    Ect1,
    Ce,
}

/// What a newly received packet means for acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// The packet is the new largest in the space.
    pub largest: bool,
    /// The packet is below the previous largest or leaves a gap: RFC 9000
    /// §13.2.1 asks for an immediate ACK.
    pub out_of_order: bool,
}

#[cfg(not(feature = "vesper-port"))]
#[derive(Debug, Clone)]
pub struct ReceivedPackets<const N: usize = DEFAULT_MAX_RANGES> {
    pub(crate) space: PnSpace,
    /// Descending, disjoint, non-adjacent; `ranges[..len]` are live.
    pub(crate) ranges: [AckRange; N],
    pub(crate) len: usize,
    /// Packet numbers `<= floor` are no longer tracked.
    pub(crate) floor: Option<u64>,
    /// Largest packet number ever received, kept independently of the
    /// retained ranges so ACK-of-ACK pruning never loses the packet-number
    /// reconstruction context.
    pub(crate) largest: Option<u64>,
    pub(crate) largest_received_ns: u64,
    pub(crate) last_received_ns: u64,
    pub(crate) ecn: EcnCounts,
    pub(crate) unacked_ack_eliciting: u32,
    pub(crate) ack_deadline_ns: Option<u64>,
    pub(crate) max_ack_delay_ns: u64,
}

/// The Vesper build: the range array stays here and is lent to Vesper in
/// place; the rest of the state is the Vesper `RxState` record.
#[cfg(feature = "vesper-port")]
#[derive(Debug, Clone)]
pub struct ReceivedPackets<const N: usize = DEFAULT_MAX_RANGES> {
    pub(crate) state: crate::ported::binding::repr::RxState,
    /// Descending, disjoint, non-adjacent; `ranges[..state.len]` are live.
    pub(crate) ranges: [AckRange; N],
}
