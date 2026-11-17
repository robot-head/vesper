//! The BBRv3 state machine, transcribed from draft-ietf-ccwg-bbr-07.
//!
//! Function names follow the draft's pseudocode (snake_case). All volumes are
//! bytes (`u64`), rates bytes per second (`u64`), times nanoseconds (`u64`),
//! gains per mille (`GAIN_UNIT == 1000`). `INFINITY` (`u64::MAX`) is the
//! draft's "Infinity" for volume and rate bounds. Products are computed in
//! `u128` and saturate to `u64::MAX`; differences of counters saturate at 0.

use crate::events::{BbrError, CongestionEvent};
use crate::filter::{CycleMax, WindowedMax};
use crate::pacing::NANOS_PER_SEC;

// The Vesper source spells the constants below as literals; the Vesper
// build does not read every one of them.

/// The draft's "Infinity" for volumes and rates.
pub const INFINITY: u64 = u64::MAX;
/// Fixed-point unit for gains and fractions: 1000 means 1.0.
pub const GAIN_UNIT: u64 = 1000;
/// `BBR.StartupPacingGain` = 4 ln 2 ≈ 2.77.
pub const STARTUP_PACING_GAIN: u64 = 2770;
/// `BBR.DrainPacingGain` = 0.5.
pub const DRAIN_PACING_GAIN: u64 = 500;
/// `BBR.DefaultCwndGain` = 2.
pub const DEFAULT_CWND_GAIN: u64 = 2000;
/// ProbeBW_DOWN pacing gain 0.90.
pub const PROBE_DOWN_PACING_GAIN: u64 = 900;
/// ProbeBW_CRUISE / REFILL / ProbeRTT pacing gain 1.0.
pub const UNIT_PACING_GAIN: u64 = 1000;
/// ProbeBW_UP pacing gain 1.25.
pub const PROBE_UP_PACING_GAIN: u64 = 1250;
/// ProbeBW_UP cwnd gain 2.25.
pub const PROBE_UP_CWND_GAIN: u64 = 2250;
/// `BBR.ProbeRTTCwndGain` = 0.5.
pub const PROBE_RTT_CWND_GAIN: u64 = 500;
/// `BBR.LossThresh` = 2%.
pub const LOSS_THRESH: u64 = 20;
/// `BBR.Beta` = 0.7.
pub const BETA: u64 = 700;
/// `BBR.Headroom` = 0.15.
pub const HEADROOM: u64 = 150;
/// `BBR.PacingMarginPercent` = 1.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const PACING_MARGIN_PERCENT: u64 = 1;
/// Full-pipe growth threshold 1.25.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const FULL_BW_GROWTH: u64 = 1250;
/// Rounds without growth before the pipe is full.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const FULL_BW_COUNT: u32 = 3;
/// `BBR.StartupFullLossCnt` = 6.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const STARTUP_FULL_LOSS_CNT: u64 = 6;
/// `BBR.MinRTTFilterLen` = 10 s.
pub const MIN_RTT_FILTER_LEN_NS: u64 = 10 * NANOS_PER_SEC;
/// `BBR.ProbeRTTDuration` = 200 ms.
pub const PROBE_RTT_DURATION_NS: u64 = 200_000_000;
/// `BBR.ExtraAckedFilterLen` = 10 rounds.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const EXTRA_ACKED_FILTER_LEN: u64 = 10;
/// Maximum send quantum, 64 KiB.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const MAX_SEND_QUANTUM: u64 = 64 * 1024;
/// Lower bound of the randomized ProbeBW wait, 2 s.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const PROBE_WAIT_BASE_NS: u64 = 2 * NANOS_PER_SEC;
/// Width of the randomized ProbeBW wait, 1 s.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const PROBE_WAIT_RANGE_NS: u64 = NANOS_PER_SEC;
/// Cap on Reno-coexistence rounds.
#[cfg_attr(feature = "vesper-port", allow(dead_code))]
pub const RENO_ROUNDS_CAP: u64 = 63;

/// `BBR.state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BbrState {
    /// Exponential bandwidth search.
    Startup,
    /// Drain the Startup queue.
    Drain,
    /// ProbeBW deceleration.
    ProbeBwDown,
    /// ProbeBW cruising.
    ProbeBwCruise,
    /// ProbeBW refill (one round at gain 1).
    ProbeBwRefill,
    /// ProbeBW acceleration.
    ProbeBwUp,
    /// Periodic min-RTT probe.
    ProbeRtt,
}

/// How `BBR.offload_budget` is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffloadModel {
    /// `offload_budget = send_quantum` (draft "QUIC Offload Budget").
    Quic,
    /// `offload_budget = 3 * send_quantum` (draft "TCP Offload Budget").
    Tcp,
}

/// Controller configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BbrConfig {
    /// `C.SMSS` in bytes (QUIC: maximum datagram size), `1..=65535`.
    pub smss: u32,
    /// `C.InitialCwnd` in bytes, at least `smss`.
    pub initial_cwnd: u32,
    /// `C.has_selective_acks` (true for QUIC).
    pub has_selective_acks: bool,
    /// Offload budget model.
    pub offload: OffloadModel,
    /// Seed for the randomized ProbeBW wait.
    pub probe_seed: u64,
}

/// The per-ACK rate sample `RS` (draft §"Rate Sample (rs) Output" and
/// §"Per-ACK Rate Sample State").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RateSample {
    /// `RS.delivery_rate`, bytes per second (0 when no reliable sample).
    pub delivery_rate: u64,
    /// `RS.is_app_limited`.
    pub is_app_limited: bool,
    /// `RS.interval`.
    pub interval_ns: u64,
    /// `RS.delivered`.
    pub delivered: u64,
    /// `RS.prior_delivered`.
    pub prior_delivered: u64,
    /// `RS.prior_time` (`None` before any delivery on this ACK).
    pub prior_time_ns: Option<u64>,
    /// `RS.send_elapsed`.
    pub send_elapsed_ns: u64,
    /// `RS.ack_elapsed`.
    pub ack_elapsed_ns: u64,
    /// `RS.last_acked_packet_id`.
    pub last_acked_pn: u64,
    /// `RS.rtt` (`None` is the draft's -1).
    pub rtt_ns: Option<u64>,
    /// `RS.newly_acked`.
    pub newly_acked: u64,
    /// `RS.tx_in_flight`.
    pub tx_in_flight: u64,
    /// `RS.lost` (lost plus CE-marked bytes since the newest acked packet
    /// was sent).
    pub lost: u64,
}

/// BBRv3 controller and EDT pacer for one flow (§35.5 `Bbrv3FlowState`).
///
/// Sans-IO: every input carries a caller-supplied virtual nanosecond time,
/// and the controller never reads a clock or allocates. Rejected inputs
/// return [`BbrError`] and leave the state unchanged.
//
// The fields are `pub(crate)` so that the generated binding (`port/binding.rs`)
// can move the state across the export ABI under `vesper-port`; their names
// and types match the Vesper `Bbrv3` in `vesper/src/bbr.ves`.
#[derive(Clone, Debug)]
pub struct Bbrv3 {
    // Configuration.
    pub(crate) smss: u64,
    pub(crate) initial_cwnd: u64,
    pub(crate) has_selective_acks: bool,
    pub(crate) offload: OffloadModel,
    // SplitMix64 state of the randomized probe wait.
    pub(crate) rng: u64,
    // Connection (C) state.
    pub(crate) delivered: u64,
    pub(crate) delivered_ns: u64,
    pub(crate) first_sent_ns: u64,
    pub(crate) app_limited: u64,
    pub(crate) lost: u64,
    pub(crate) ce_bytes: u64,
    pub(crate) inflight: u64,
    pub(crate) cwnd: u64,
    pub(crate) pacing_rate: u64,
    pub(crate) send_quantum: u64,
    pub(crate) next_edt_ns: u64,
    pub(crate) srtt_ns: Option<u64>,
    pub(crate) lifetime_min_rtt_ns: Option<u64>,
    pub(crate) pacing_from_rtt: bool,
    pub(crate) is_cwnd_limited: bool,
    pub(crate) cwnd_limited_this_round: bool,
    pub(crate) in_recovery: bool,
    pub(crate) recovery_start_ns: u64,
    pub(crate) last_now_ns: u64,
    pub(crate) ce_pending: bool,
    pub(crate) ce_in_loss_round: bool,
    pub(crate) rs: RateSample,
    // BBR state.
    pub(crate) bbr_state: BbrState,
    pub(crate) pacing_gain: u64,
    pub(crate) cwnd_gain: u64,
    pub(crate) round_count: u64,
    pub(crate) round_start: bool,
    pub(crate) next_round_delivered: u64,
    pub(crate) idle_restart: bool,
    pub(crate) drain_start_round: u64,
    pub(crate) max_bw_filter: CycleMax,
    pub(crate) max_bw: u64,
    pub(crate) bw_shortterm: u64,
    pub(crate) bw: u64,
    pub(crate) cycle_count: u64,
    pub(crate) min_rtt_ns: Option<u64>,
    pub(crate) min_rtt_stamp_ns: u64,
    pub(crate) min_rtt_expired: bool,
    pub(crate) bdp: u64,
    pub(crate) extra_acked_filter: WindowedMax,
    pub(crate) extra_acked: u64,
    pub(crate) extra_acked_interval_start_ns: u64,
    pub(crate) extra_acked_delivered: u64,
    pub(crate) offload_budget: u64,
    pub(crate) max_inflight: u64,
    pub(crate) inflight_longterm: u64,
    pub(crate) inflight_shortterm: u64,
    pub(crate) bw_latest: u64,
    pub(crate) inflight_latest: u64,
    pub(crate) loss_round_delivered: u64,
    pub(crate) loss_round_start: bool,
    pub(crate) is_loss_in_round: bool,
    pub(crate) loss_events_in_round: u64,
    pub(crate) full_bw_reached: bool,
    pub(crate) full_bw_now: bool,
    pub(crate) full_bw: u64,
    pub(crate) full_bw_count: u32,
    pub(crate) is_bw_probe_stopping: bool,
    pub(crate) check_probe_loss: bool,
    pub(crate) bw_probe_up_acked: u64,
    pub(crate) probe_up_acked_per_inc: u64,
    pub(crate) bw_probe_up_rounds: u32,
    pub(crate) rounds_since_probe_up: u64,
    pub(crate) bw_probe_wait_ns: u64,
    pub(crate) cycle_stamp_ns: u64,
    pub(crate) prev_probe_too_high: bool,
    pub(crate) prev_probe_precautionary: bool,
    pub(crate) undo_state: Option<BbrState>,
    pub(crate) undo_bw_shortterm: u64,
    pub(crate) undo_inflight_shortterm: u64,
    pub(crate) undo_inflight_longterm: u64,
    pub(crate) prior_cwnd: u64,
    pub(crate) probe_rtt_done_stamp_ns: Option<u64>,
    pub(crate) probe_rtt_round_done: bool,
    pub(crate) probe_rtt_cwnd: u64,
}

/// The §35.5 name for the controller.
pub type Bbrv3FlowState = Bbrv3;

impl Bbrv3 {
    /// `C.pacing_rate` in bytes per second (always `>= 1`).
    #[must_use]
    pub const fn pacing_rate_bytes_per_sec(&self) -> u64 {
        self.pacing_rate
    }

    /// `C.send_quantum` in bytes.
    #[must_use]
    pub const fn send_quantum_bytes(&self) -> u64 {
        self.send_quantum
    }

    /// `C.next_send_time`: the earliest departure for the next packet.
    #[must_use]
    pub const fn next_edt_ns(&self) -> u64 {
        self.next_edt_ns
    }

    /// `C.inflight`.
    #[must_use]
    pub const fn bytes_in_flight(&self) -> u64 {
        self.inflight
    }

    /// `BBR.state`.
    #[must_use]
    pub const fn state(&self) -> BbrState {
        self.bbr_state
    }

    /// `BBR.max_bw`.
    #[must_use]
    pub const fn max_bw(&self) -> u64 {
        self.max_bw
    }

    /// `BBR.bw = min(max_bw, bw_shortterm)`.
    #[must_use]
    pub const fn bw(&self) -> u64 {
        self.bw
    }

    /// `BBR.min_rtt` (`None` = Infinity).
    #[must_use]
    pub const fn min_rtt_ns(&self) -> Option<u64> {
        self.min_rtt_ns
    }

    /// `BBR.bdp` as last computed.
    #[must_use]
    pub const fn bdp_bytes(&self) -> u64 {
        self.bdp
    }

    /// `BBR.full_bw_reached`.
    #[must_use]
    pub const fn full_bw_reached(&self) -> bool {
        self.full_bw_reached
    }

    /// `BBR.round_count`.
    #[must_use]
    pub const fn round_count(&self) -> u64 {
        self.round_count
    }

    /// `BBR.cycle_count`.
    #[must_use]
    pub const fn cycle_count(&self) -> u64 {
        self.cycle_count
    }

    /// `BBR.pacing_gain`, per mille.
    #[must_use]
    pub const fn pacing_gain(&self) -> u64 {
        self.pacing_gain
    }

    /// `BBR.cwnd_gain`, per mille.
    #[must_use]
    pub const fn cwnd_gain(&self) -> u64 {
        self.cwnd_gain
    }

    /// `BBR.extra_acked`.
    #[must_use]
    pub const fn extra_acked(&self) -> u64 {
        self.extra_acked
    }

    /// `C.delivered`.
    #[must_use]
    pub const fn delivered_bytes(&self) -> u64 {
        self.delivered
    }

    /// `C.lost`.
    #[must_use]
    pub const fn lost_bytes(&self) -> u64 {
        self.lost
    }

    /// Whether loss recovery is in progress.
    #[must_use]
    pub const fn in_recovery(&self) -> bool {
        self.in_recovery
    }

    /// The rate sample of the last ACK that delivered data.
    #[must_use]
    pub const fn last_rate_sample(&self) -> RateSample {
        self.rs
    }

    /// Dispatch a contract [`CongestionEvent`] at `now_ns`.
    pub fn on_event(&mut self, now_ns: u64, event: CongestionEvent<'_>) -> Result<(), BbrError> {
        match event {
            CongestionEvent::Acked { packets, rtt } => self.on_ack(now_ns, packets, rtt),
            CongestionEvent::Lost { packet, state } => self.on_packet_lost(now_ns, packet, state),
            CongestionEvent::EcnCe { count } => self.on_ecn_ce(now_ns, count),
            CongestionEvent::PersistentCongestion => self.on_persistent_congestion(now_ns),
            CongestionEvent::AppLimited => self.on_app_limited(now_ns),
        }
    }
}

// The implementation: Rust (`bbr/reference.rs`) by default, the Vesper port
// (`vesper/src/bbr.ves`, through `bbr/forward.rs`) under `vesper-port`.
#[cfg(not(feature = "vesper-port"))]
mod reference;

#[cfg(feature = "vesper-port")]
mod forward;

// Helpers the unit tests call directly.
#[cfg(all(test, feature = "vesper-port"))]
use forward::{inflight_at_loss, mul_gain};
#[cfg(all(test, not(feature = "vesper-port")))]
use reference::{inflight_at_loss, mul_gain};

#[cfg(test)]
mod tests;
