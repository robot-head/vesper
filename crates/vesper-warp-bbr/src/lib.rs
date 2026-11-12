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
//! Input events use the CC-agnostic vocabulary of
//! `docs/rfcs/wave-four-contracts.md` ([`SentPacket`], [`AckedPacket`],
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

#![forbid(unsafe_code)]
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
