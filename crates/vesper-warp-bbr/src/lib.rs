//! Sans-IO BBRv3 congestion control and earliest-departure-time (EDT)
//! pacing for Vesper Warp (design §35.1 item 3, §35.3, §35.4 theorem 2,
//! §35.5 `Bbrv3FlowState`).
//!
//! The state machine follows **draft-ietf-ccwg-bbr-07** (the BBRv3
//! specification of the IETF CCWG), transcribed into integer arithmetic:
//! volumes in bytes, rates in bytes per second, times in caller-supplied
//! virtual nanoseconds, gains per mille. The draft's renamed bounds
//! `inflight_longterm`, `inflight_shortterm` and `bw_shortterm` are also
//! exposed under their older names `inflight_hi`, `inflight_lo`, `bw_lo`.
//!
//! Input events use the CC-agnostic vocabulary ([`SentPacket`], [`AckedPacket`],
//! [`LostPacket`], [`RttSample`], [`CongestionEvent`]). The per-packet
//! delivery-rate snapshot ([`PacketDeliveryState`]) is returned by
//! [`Bbrv3::on_packet_sent`] and handed back on acknowledgement or loss.
//!
//! What this crate does **not** claim: fairness or interoperability with
//! Linux BBRv3, real-network performance, or use of hardware pacing
//! (`SO_TXTIME`, NIC EDT). The link simulator in the tests checks the
//! modelled behaviour only. The Lean file `lean/WarpBbr.lean` proves
//! statements about its own transcription of [`pacing`]'s functions, not
//! about this Rust code.
//!
//! # Two builds
//!
//! The implementation is being ported to Vesper (port wave P1 lane 10,
//! #907). By default the crate builds its Rust implementation; with the
//! `vesper-port` feature every function with logic forwards to the Vesper
//! source in `vesper/src/bbr.ves`, compiled by `vesper build --lib`, through
//! the generated binding `port/binding.rs`. The public API and the tests are
//! the same for both builds.

// The Rust build has no unsafe code at all; the Vesper build's only unsafe
// code is the generated binding's foreign calls.
#![cfg_attr(not(feature = "vesper-port"), forbid(unsafe_code))]
#![cfg_attr(feature = "vesper-port", deny(unsafe_code))]
#![warn(missing_docs)]

mod bbr;
mod events;
mod filter;
pub mod pacing;

pub use bbr::{
    BETA, BbrConfig, BbrState, Bbrv3, Bbrv3FlowState, DEFAULT_CWND_GAIN, DRAIN_PACING_GAIN,
    GAIN_UNIT, HEADROOM, INFINITY, LOSS_THRESH, MIN_RTT_FILTER_LEN_NS, OffloadModel,
    PROBE_DOWN_PACING_GAIN, PROBE_RTT_CWND_GAIN, PROBE_RTT_DURATION_NS, PROBE_UP_CWND_GAIN,
    PROBE_UP_PACING_GAIN, RateSample, STARTUP_PACING_GAIN, UNIT_PACING_GAIN,
};
pub use events::{
    AckedPacket, BbrError, CongestionEvent, Delivered, LostPacket, MAX_PACKET_NUMBER,
    PacketDeliveryState, RttSample, SentPacket,
};
pub use filter::{CycleMax, SplitMix64, WindowedMax};
pub use pacing::{clamp_target_cwnd, edt_step, pacing_delay_ns, send_allowed};

// Generated and committed, but kept outside src/ so that formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[cfg(feature = "vesper-port")]
#[allow(unsafe_code, missing_docs)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port;

/// The Vesper side's `BbrError`: the configuration message is a code (0:
/// `smss` out of range, 1: `initial_cwnd` below `smss`), because a
/// `&'static str` cannot cross the export ABI.
#[cfg(feature = "vesper-port")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BbrFault {
    InvalidConfig(u64),
    PacketNumberOutOfRange { pn: u64 },
    EmptyInFlightPacket { pn: u64 },
    CwndFull { bytes_in_flight: u64, cwnd: u64 },
    InflightUnderflow { bytes_in_flight: u64, released: u64 },
    ForeignSnapshot { pn: u64 },
    TimeWentBackwards { last_ns: u64, now_ns: u64 },
    AckedBeforeSent { pn: u64 },
    CounterOverflow,
}

/// One ACK's state between the calls of `Bbrv3::on_ack` under
/// `vesper-port` (the Vesper `AckProgress`).
#[cfg(feature = "vesper-port")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct AckProgress {
    rs: RateSample,
    has_newest: bool,
    newest_sent_ns: u64,
    newest_tx_in_flight: u64,
    newest_lost: u64,
    released: u64,
    newly_delivered: u64,
}

/// Which implementation this build runs.
pub const IMPLEMENTATION: &str = if cfg!(feature = "vesper-port") {
    "vesper"
} else {
    "rust"
};
