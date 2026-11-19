//! RFC 9002 loss detection and RTT estimation, congestion-control agnostic.
//!
//! [`LossRecovery`] implements RFC 9002 Appendix A (A.5 `OnPacketSent`, A.7
//! `OnAckReceived`, A.8 `SetLossDetectionTimer`/`GetPtoTimeAndSpace`, A.9
//! `OnLossDetectionTimeout`, A.10 `DetectAndRemoveLostPackets`, §6.4 space
//! discard) and §7.6 persistent-congestion detection. It does **not**
//! contain a congestion controller: acknowledged and lost in-flight packets,
//! ECN-CE increases and persistent congestion are reported as wave-four
//! [`CongestionEvent`]s through a caller-supplied sink, so the BBRv3 lane
//! plugs in at integration. Time is caller-supplied `u64` nanoseconds; no
//! clock is read.
//!
//! Sent-packet records live in per-space ring buffers preallocated at
//! construction (`max_tracked_per_space`); the steady state allocates nothing.
//! Only in-flight packets are recorded: ACK-only packets are not, so
//! receive-only traffic cannot fill the bound.
//! Exceeding the bound is [`RecoveryError::TooManyTracked`].
//!
//! Limitations: persistent congestion is evaluated per space and per loss
//! batch (RFC 9002 §7.6.2 allows a wider view across spaces); ECN validation
//! (RFC 9000 §13.4.2) is not performed, only CE increases are reported;
//! [`CongestionEvent::AppLimited`] is part of the shared vocabulary but is
//! the sender's to emit, not recovery's.

use crate::{PnSpace, TransportErrorCode};

pub use crate::imp::recovery::{LossRecovery, RttEstimator, ack_delay_ns};

pub const K_PACKET_THRESHOLD: u64 = 3;
/// kTimeThreshold = 9/8.
pub const K_TIME_THRESHOLD_NUM: u64 = 9;
pub const K_TIME_THRESHOLD_DEN: u64 = 8;
pub const K_GRANULARITY_NS: u64 = 1_000_000;
pub const K_INITIAL_RTT_NS: u64 = 333_000_000;
pub const K_PERSISTENT_CONGESTION_THRESHOLD: u64 = 3;
/// RFC 9000 §18.2 default `max_ack_delay` (25 ms).
pub const DEFAULT_MAX_ACK_DELAY_NS: u64 = 25_000_000;

/// Wave-four shared vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentPacket {
    pub pn: u64,
    pub sent_ns: u64,
    pub bytes: u32,
    pub ack_eliciting: bool,
    pub in_flight: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckedPacket {
    pub pn: u64,
    pub sent_ns: u64,
    pub bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LostPacket {
    pub pn: u64,
    pub sent_ns: u64,
    pub bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RttSample {
    pub latest_ns: u64,
    pub min_ns: u64,
    pub smoothed_ns: u64,
    /// The ack delay actually applied (capped, zero for Initial).
    pub ack_delay_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CongestionEvent {
    /// An in-flight packet was newly acknowledged.
    Acked { packet: AckedPacket },
    /// An in-flight packet was declared lost.
    Lost { packet: LostPacket },
    /// The peer's reported ECN-CE count rose by `count`.
    EcnCe { count: u64 },
    /// RFC 9002 §7.6: the loss batch spans the persistent-congestion period.
    PersistentCongestion,
    /// Emitted by the sender, not by recovery (see module docs).
    AppLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryError {
    PnOutOfRange(u64),
    /// Packet numbers in a space must strictly increase.
    NonIncreasingPn {
        pn: u64,
        last: u64,
    },
    ClockWentBackwards,
    /// The per-space sent-packet bound would be exceeded.
    TooManyTracked,
    /// An ACK acknowledges a packet number never sent (RFC 9000 §13.1).
    AckOfUnsentPacket {
        largest_acked: u64,
    },
    /// The space's keys were discarded.
    SpaceDiscarded(PnSpace),
    BadAckDelayExponent(u8),
}

impl RecoveryError {
    pub const fn transport_code(&self) -> TransportErrorCode {
        TransportErrorCode::ProtocolViolation
    }
}

/// Handshake facts recovery needs; the caller keeps them current with
/// [`LossRecovery::set_handshake_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HandshakeStatus {
    pub confirmed: bool,
    /// RFC 9002 `PeerCompletedAddressValidation()`: always true for a server;
    /// for a client, true once the handshake is confirmed or a Handshake ACK
    /// arrived.
    pub peer_completed_address_validation: bool,
    pub has_handshake_keys: bool,
    /// Server at the anti-amplification limit (RFC 9000 §8.1).
    pub at_amplification_limit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryConfig {
    /// The peer's `max_ack_delay`.
    pub max_ack_delay_ns: u64,
    pub initial_rtt_ns: u64,
    /// Sent-packet records preallocated per space.
    pub max_tracked_per_space: usize,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            max_ack_delay_ns: DEFAULT_MAX_ACK_DELAY_NS,
            initial_rtt_ns: K_INITIAL_RTT_NS,
            max_tracked_per_space: 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    /// Time-threshold loss detection for the space.
    Loss(PnSpace),
    /// Probe timeout for the space.
    Pto(PnSpace),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    pub deadline_ns: u64,
    pub kind: TimerKind,
}

/// What the caller must do after a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutAction {
    /// The timer was not armed or has not expired.
    None,
    /// Time-threshold loss was declared in `space`; events were emitted.
    LossDetected { space: PnSpace, lost: u32 },
    /// Send one or two ack-eliciting probe packets in `space` (RFC 9002
    /// §6.2.4). `anti_deadlock` marks the client's padded Initial or
    /// Handshake probe sent with nothing in flight (§6.2.2.1).
    SendProbe {
        space: PnSpace,
        packets: u8,
        anti_deadlock: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AckOutcome {
    pub newly_acked: u32,
    pub newly_acked_bytes: u64,
    pub lost: u32,
    pub lost_bytes: u64,
    pub rtt_sample: Option<RttSample>,
    pub ecn_ce_increase: u64,
    pub persistent_congestion: bool,
}
