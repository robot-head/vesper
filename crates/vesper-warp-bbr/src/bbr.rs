//! The BBRv3 state machine, transcribed from draft-ietf-ccwg-bbr-07.
//!
//! Function names follow the draft's pseudocode (snake_case). All volumes are
//! bytes (`u64`), rates bytes per second (`u64`), times nanoseconds (`u64`),
//! gains per mille (`GAIN_UNIT == 1000`). `INFINITY` (`u64::MAX`) is the
//! draft's "Infinity" for volume and rate bounds. Products are computed in
//! `u128` and saturate to `u64::MAX`; differences of counters saturate at 0.

use crate::events::{
    BbrError, CongestionEvent, Delivered, LostPacket, MAX_PACKET_NUMBER, PacketDeliveryState,
    RttSample, SentPacket,
};
use crate::filter::{CycleMax, SplitMix64, WindowedMax};
use crate::pacing::{NANOS_PER_SEC, clamp_target_cwnd, edt_step, pacing_delay_ns, send_allowed};

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
pub const PACING_MARGIN_PERCENT: u64 = 1;
/// Full-pipe growth threshold 1.25.
pub const FULL_BW_GROWTH: u64 = 1250;
/// Rounds without growth before the pipe is full.
pub const FULL_BW_COUNT: u32 = 3;
/// `BBR.StartupFullLossCnt` = 6.
pub const STARTUP_FULL_LOSS_CNT: u64 = 6;
/// `BBR.MinRTTFilterLen` = 10 s.
pub const MIN_RTT_FILTER_LEN_NS: u64 = 10 * NANOS_PER_SEC;
/// `BBR.ProbeRTTDuration` = 200 ms.
pub const PROBE_RTT_DURATION_NS: u64 = 200_000_000;
/// `BBR.ExtraAckedFilterLen` = 10 rounds.
pub const EXTRA_ACKED_FILTER_LEN: u64 = 10;
/// Maximum send quantum, 64 KiB.
pub const MAX_SEND_QUANTUM: u64 = 64 * 1024;
/// Lower bound of the randomized ProbeBW wait, 2 s.
pub const PROBE_WAIT_BASE_NS: u64 = 2 * NANOS_PER_SEC;
/// Width of the randomized ProbeBW wait, 1 s.
pub const PROBE_WAIT_RANGE_NS: u64 = NANOS_PER_SEC;
/// Cap on Reno-coexistence rounds.
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

impl BbrState {
    /// `IsInAProbeBWState()`.
    #[must_use]
    pub const fn is_probe_bw(self) -> bool {
        matches!(
            self,
            Self::ProbeBwDown | Self::ProbeBwCruise | Self::ProbeBwRefill | Self::ProbeBwUp
        )
    }
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

impl BbrConfig {
    /// QUIC defaults: RFC 9002 §7.2 initial window
    /// `min(10 * smss, max(14720, 2 * smss))`, selective ACKs, QUIC offload.
    #[must_use]
    pub const fn quic(smss: u32) -> Self {
        let ten = smss as u64 * 10;
        let two = smss as u64 * 2;
        let floor = if two > 14_720 { two } else { 14_720 };
        let iw = if ten < floor { ten } else { floor };
        Self {
            smss,
            initial_cwnd: if iw > u32::MAX as u64 {
                u32::MAX
            } else {
                iw as u32
            },
            has_selective_acks: true,
            offload: OffloadModel::Quic,
            probe_seed: 0x6262_7233_7761_7270,
        }
    }

    fn validate(&self) -> Result<(), BbrError> {
        if self.smss == 0 || self.smss > 65_535 {
            return Err(BbrError::InvalidConfig("smss must be in 1..=65535"));
        }
        if self.initial_cwnd < self.smss {
            return Err(BbrError::InvalidConfig("initial_cwnd must be >= smss"));
        }
        Ok(())
    }
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

/// Saturating `x * gain / GAIN_UNIT`; `INFINITY` stays `INFINITY`.
fn mul_gain(x: u64, gain: u64) -> u64 {
    if x == INFINITY {
        return INFINITY;
    }
    sat(x as u128 * gain as u128 / GAIN_UNIT as u128)
}

/// Saturating `bw * ns / 10^9`: bytes delivered at `bw` over `ns`.
fn bw_volume(bw: u64, ns: u64) -> u64 {
    sat(bw as u128 * ns as u128 / NANOS_PER_SEC as u128)
}

fn sat(x: u128) -> u64 {
    if x > u64::MAX as u128 {
        u64::MAX
    } else {
        x as u64
    }
}

fn check_pn(pn: u64) -> Result<(), BbrError> {
    if pn > MAX_PACKET_NUMBER {
        Err(BbrError::PacketNumberOutOfRange { pn })
    } else {
        Ok(())
    }
}

/// BBRv3 controller and EDT pacer for one flow (§35.5 `Bbrv3FlowState`).
///
/// Sans-IO: every input carries a caller-supplied virtual nanosecond time,
/// and the controller never reads a clock or allocates. Rejected inputs
/// return [`BbrError`] and leave the state unchanged.
#[derive(Clone, Debug)]
pub struct Bbrv3 {
    // Configuration.
    smss: u64,
    initial_cwnd: u64,
    has_selective_acks: bool,
    offload: OffloadModel,
    rng: SplitMix64,
    // Connection (C) state.
    delivered: u64,
    delivered_ns: u64,
    first_sent_ns: u64,
    app_limited: u64,
    lost: u64,
    ce_bytes: u64,
    inflight: u64,
    cwnd: u64,
    pacing_rate: u64,
    send_quantum: u64,
    next_edt_ns: u64,
    srtt_ns: Option<u64>,
    lifetime_min_rtt_ns: Option<u64>,
    pacing_from_rtt: bool,
    is_cwnd_limited: bool,
    cwnd_limited_this_round: bool,
    in_recovery: bool,
    recovery_start_ns: u64,
    last_now_ns: u64,
    ce_pending: bool,
    rs: RateSample,
    // BBR state.
    state: BbrState,
    pacing_gain: u64,
    cwnd_gain: u64,
    round_count: u64,
    round_start: bool,
    next_round_delivered: u64,
    idle_restart: bool,
    drain_start_round: u64,
    max_bw_filter: CycleMax,
    max_bw: u64,
    bw_shortterm: u64,
    bw: u64,
    cycle_count: u64,
    min_rtt_ns: Option<u64>,
    min_rtt_stamp_ns: u64,
    min_rtt_expired: bool,
    bdp: u64,
    extra_acked_filter: WindowedMax,
    extra_acked: u64,
    extra_acked_interval_start_ns: u64,
    extra_acked_delivered: u64,
    offload_budget: u64,
    max_inflight: u64,
    inflight_longterm: u64,
    inflight_shortterm: u64,
    bw_latest: u64,
    inflight_latest: u64,
    loss_round_delivered: u64,
    loss_round_start: bool,
    is_loss_in_round: bool,
    loss_events_in_round: u64,
    full_bw_reached: bool,
    full_bw_now: bool,
    full_bw: u64,
    full_bw_count: u32,
    is_bw_probe_stopping: bool,
    check_probe_loss: bool,
    bw_probe_up_acked: u64,
    probe_up_acked_per_inc: u64,
    bw_probe_up_rounds: u32,
    rounds_since_probe_up: u64,
    bw_probe_wait_ns: u64,
    cycle_stamp_ns: u64,
    prev_probe_too_high: bool,
    prev_probe_precautionary: bool,
    undo_state: Option<BbrState>,
    undo_bw_shortterm: u64,
    undo_inflight_shortterm: u64,
    undo_inflight_longterm: u64,
    prior_cwnd: u64,
    probe_rtt_done_stamp_ns: Option<u64>,
    probe_rtt_round_done: bool,
    probe_rtt_cwnd: u64,
}

/// The §35.5 name for the controller.
pub type Bbrv3FlowState = Bbrv3;

impl Bbrv3 {
    /// `OnInit()` at time `now_ns`.
    pub fn new(config: BbrConfig, now_ns: u64) -> Result<Self, BbrError> {
        config.validate()?;
        let mut bbr = Self {
            smss: config.smss as u64,
            initial_cwnd: config.initial_cwnd as u64,
            has_selective_acks: config.has_selective_acks,
            offload: config.offload,
            rng: SplitMix64(config.probe_seed),
            delivered: 0,
            delivered_ns: 0,
            first_sent_ns: 0,
            app_limited: 0,
            lost: 0,
            ce_bytes: 0,
            inflight: 0,
            cwnd: config.initial_cwnd as u64,
            pacing_rate: 1,
            send_quantum: 2 * config.smss as u64,
            next_edt_ns: 0,
            srtt_ns: None,
            lifetime_min_rtt_ns: None,
            pacing_from_rtt: false,
            is_cwnd_limited: false,
            cwnd_limited_this_round: false,
            in_recovery: false,
            recovery_start_ns: 0,
            last_now_ns: now_ns,
            ce_pending: false,
            rs: RateSample::default(),
            state: BbrState::Startup,
            pacing_gain: STARTUP_PACING_GAIN,
            cwnd_gain: DEFAULT_CWND_GAIN,
            round_count: 0,
            round_start: false,
            next_round_delivered: 0,
            idle_restart: false,
            drain_start_round: 0,
            max_bw_filter: CycleMax::default(),
            max_bw: 0,
            bw_shortterm: INFINITY,
            bw: 0,
            cycle_count: 0,
            min_rtt_ns: None,
            min_rtt_stamp_ns: now_ns,
            min_rtt_expired: false,
            bdp: 0,
            extra_acked_filter: WindowedMax::new(0, 0),
            extra_acked: 0,
            extra_acked_interval_start_ns: now_ns,
            extra_acked_delivered: 0,
            offload_budget: 0,
            max_inflight: 0,
            inflight_longterm: INFINITY,
            inflight_shortterm: INFINITY,
            bw_latest: 0,
            inflight_latest: 0,
            loss_round_delivered: 0,
            loss_round_start: false,
            is_loss_in_round: false,
            loss_events_in_round: 0,
            full_bw_reached: false,
            full_bw_now: false,
            full_bw: 0,
            full_bw_count: 0,
            is_bw_probe_stopping: false,
            check_probe_loss: false,
            bw_probe_up_acked: 0,
            probe_up_acked_per_inc: INFINITY,
            bw_probe_up_rounds: 0,
            rounds_since_probe_up: 0,
            bw_probe_wait_ns: 0,
            cycle_stamp_ns: 0,
            prev_probe_too_high: false,
            prev_probe_precautionary: false,
            undo_state: None,
            undo_bw_shortterm: 0,
            undo_inflight_shortterm: 0,
            undo_inflight_longterm: 0,
            prior_cwnd: 0,
            probe_rtt_done_stamp_ns: None,
            probe_rtt_round_done: false,
            probe_rtt_cwnd: INFINITY,
        };
        bbr.init_pacing_rate();
        bbr.set_send_quantum();
        bbr.enter_startup();
        Ok(bbr)
    }

    // ------------------------------------------------------------------
    // Outputs.

    /// `C.cwnd`, saturated to `u32` (contract window type).
    #[must_use]
    pub fn cwnd_bytes(&self) -> u32 {
        self.cwnd.min(u32::MAX as u64) as u32
    }

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

    /// §35.5 `target_cwnd_bytes`: `cwnd` clamped to
    /// `[4 * mss, max(inflight_hi, 4 * mss)]` (see
    /// [`crate::clamp_target_cwnd`]).
    #[must_use]
    pub const fn target_cwnd_bytes(&self, mss: u32) -> u64 {
        clamp_target_cwnd(self.cwnd, mss, self.inflight_longterm)
    }

    /// §35.5 `schedule_edt_ns`: returns `max(now, next_edt)` as this
    /// packet's departure time and advances `next_edt` by the pacing delay
    /// of `pkt_bytes` at the current pacing rate. Any `now_ns` is accepted,
    /// in any order; the returned times never decrease.
    pub fn schedule_edt_ns(&mut self, pkt_bytes: u32, now_ns: u64) -> u64 {
        let delay = pacing_delay_ns(pkt_bytes, self.pacing_rate);
        let (launch, next) = edt_step(self.next_edt_ns, now_ns, delay);
        self.next_edt_ns = next;
        launch
    }

    /// `C.next_send_time`: the earliest departure for the next packet.
    #[must_use]
    pub const fn next_edt_ns(&self) -> u64 {
        self.next_edt_ns
    }

    /// The send guard: `bytes_in_flight < cwnd`.
    #[must_use]
    pub const fn can_send(&self) -> bool {
        send_allowed(self.inflight, self.cwnd)
    }

    /// `C.inflight`.
    #[must_use]
    pub const fn bytes_in_flight(&self) -> u64 {
        self.inflight
    }

    /// `BBR.state`.
    #[must_use]
    pub const fn state(&self) -> BbrState {
        self.state
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

    /// `BBR.inflight_longterm` (called `inflight_hi` in earlier draft revisions and Linux BBRv3);
    /// `None` = Infinity.
    #[must_use]
    pub const fn inflight_hi(&self) -> Option<u64> {
        finite(self.inflight_longterm)
    }

    /// `BBR.inflight_shortterm` (formerly `inflight_lo`); `None` = Infinity.
    #[must_use]
    pub const fn inflight_lo(&self) -> Option<u64> {
        finite(self.inflight_shortterm)
    }

    /// `BBR.bw_shortterm` (formerly `bw_lo`); `None` = Infinity.
    #[must_use]
    pub const fn bw_lo(&self) -> Option<u64> {
        finite(self.bw_shortterm)
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

    /// Whether the connection is marked application-limited.
    #[must_use]
    pub const fn is_app_limited(&self) -> bool {
        self.app_limited != 0
    }

    /// The rate sample of the last ACK that delivered data.
    #[must_use]
    pub const fn last_rate_sample(&self) -> RateSample {
        self.rs
    }

    // ------------------------------------------------------------------
    // Inputs.

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

    /// `OnTransmit()` + `OnPacketSent(P)`, refusing the send when the send
    /// guard is closed. Returns the snapshot to store with the packet.
    pub fn on_packet_sent(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
    ) -> Result<PacketDeliveryState, BbrError> {
        self.record_send(now_ns, packet, true)
    }

    /// As [`Self::on_packet_sent`] but without the send guard, for RFC 9002
    /// probe packets (PTO), which may exceed the congestion window. Sends
    /// made this way are outside the Lean send-guard bound.
    pub fn on_probe_packet_sent(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
    ) -> Result<PacketDeliveryState, BbrError> {
        self.record_send(now_ns, packet, false)
    }

    fn record_send(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
        guard: bool,
    ) -> Result<PacketDeliveryState, BbrError> {
        self.check_time(now_ns)?;
        check_pn(packet.pn)?;
        let bytes = packet.bytes as u64;
        if packet.in_flight {
            if bytes == 0 {
                return Err(BbrError::EmptyInFlightPacket { pn: packet.pn });
            }
            if guard && !self.can_send() {
                return Err(BbrError::CwndFull {
                    bytes_in_flight: self.inflight,
                    cwnd: self.cwnd,
                });
            }
            if self.inflight.checked_add(bytes).is_none() {
                return Err(BbrError::CounterOverflow);
            }
        }
        self.last_now_ns = now_ns;
        if packet.in_flight {
            self.handle_restart_from_idle(now_ns);
            if self.inflight == 0 {
                self.first_sent_ns = packet.sent_ns;
                self.delivered_ns = packet.sent_ns;
            }
            self.inflight += bytes;
            if self.inflight >= self.cwnd {
                self.cwnd_limited_this_round = true;
                self.is_cwnd_limited = true;
            }
        }
        Ok(PacketDeliveryState {
            delivered: self.delivered,
            delivered_ns: self.delivered_ns,
            first_sent_ns: self.first_sent_ns,
            is_app_limited: self.app_limited != 0,
            tx_in_flight: self.inflight,
            lost: self.congested(),
            counted: packet.in_flight,
        })
    }

    /// `UpdateRateSample` for each packet, then `GenerateRateSample()` and
    /// `UpdateOnACK()`. `packets` are this ACK's newly acknowledged packets.
    pub fn on_ack(
        &mut self,
        now_ns: u64,
        packets: &[Delivered],
        rtt: Option<RttSample>,
    ) -> Result<(), BbrError> {
        self.check_time(now_ns)?;
        let mut released: u64 = 0;
        for d in packets {
            check_pn(d.packet.pn)?;
            if d.packet.sent_ns > now_ns {
                return Err(BbrError::AckedBeforeSent { pn: d.packet.pn });
            }
            if !d.state.counted {
                continue;
            }
            self.check_snapshot(d.packet.pn, &d.state)?;
            released = released
                .checked_add(d.packet.bytes as u64)
                .ok_or(BbrError::CounterOverflow)?;
        }
        if released > self.inflight {
            return Err(BbrError::InflightUnderflow {
                bytes_in_flight: self.inflight,
                released,
            });
        }
        if self.delivered.checked_add(released).is_none() {
            return Err(BbrError::CounterOverflow);
        }

        self.last_now_ns = now_ns;
        if let Some(sample) = rtt {
            self.note_rtt(sample);
        }
        self.inflight -= released;
        let mut rs = RateSample {
            rtt_ns: rtt.map(|r| r.latest_ns),
            ..RateSample::default()
        };
        let mut newest: Option<&Delivered> = None;
        for d in packets.iter().filter(|d| d.state.counted) {
            // UpdateRateSample(P)
            self.delivered += d.packet.bytes as u64;
            self.delivered_ns = now_ns;
            let is_newest = newest.is_none()
                || d.packet.sent_ns > self.first_sent_ns
                || (d.packet.sent_ns == self.first_sent_ns && d.packet.pn > rs.last_acked_pn);
            if is_newest {
                rs.prior_delivered = d.state.delivered;
                rs.prior_time_ns = Some(d.state.delivered_ns);
                rs.is_app_limited = d.state.is_app_limited;
                rs.send_elapsed_ns = d.packet.sent_ns.saturating_sub(d.state.first_sent_ns);
                rs.ack_elapsed_ns = self.delivered_ns.saturating_sub(d.state.delivered_ns);
                rs.last_acked_pn = d.packet.pn;
                self.first_sent_ns = d.packet.sent_ns;
                newest = Some(d);
            }
        }
        rs.newly_acked = released;
        // GenerateRateSample()
        if self.app_limited != 0 && self.delivered > self.app_limited {
            self.app_limited = 0;
        }
        let Some(newest) = newest else {
            // Nothing counted was delivered: no rate sample, no model update.
            self.ce_pending = false;
            return Ok(());
        };
        rs.interval_ns = rs.send_elapsed_ns.max(rs.ack_elapsed_ns);
        rs.delivered = self.delivered - rs.prior_delivered;
        if rs.interval_ns >= self.lifetime_min_rtt_ns.unwrap_or(0) && rs.interval_ns != 0 {
            rs.delivery_rate =
                sat(rs.delivered as u128 * NANOS_PER_SEC as u128 / rs.interval_ns as u128);
        }
        rs.tx_in_flight = newest.state.tx_in_flight;
        rs.lost = self.congested().saturating_sub(newest.state.lost);
        self.rs = rs;

        // The model sees InLossRecovery() as it was when this ACK arrived,
        // so the ACK that closes a loss round inside recovery can still
        // trigger CheckStartupHighLoss().
        self.update_model_and_state(now_ns);
        // RFC 9002 §7.3.2: recovery ends when a packet sent after it began
        // is acknowledged. RestoreCwnd() on exit; SetCwnd() below then
        // bounds the restored window by the model (draft §"Modulating cwnd
        // in Loss Recovery").
        if self.in_recovery && newest.packet.sent_ns > self.recovery_start_ns {
            self.in_recovery = false;
            self.restore_cwnd();
        }
        self.update_control_parameters();
        self.ce_pending = false;
        Ok(())
    }

    /// Loss detection declared `packet` lost: `OnEnterFastRecovery()` on the
    /// first loss of an episode, then `HandleLostPacket(P)`.
    pub fn on_packet_lost(
        &mut self,
        now_ns: u64,
        packet: LostPacket,
        state: PacketDeliveryState,
    ) -> Result<(), BbrError> {
        self.check_time(now_ns)?;
        check_pn(packet.pn)?;
        if !state.counted {
            self.last_now_ns = now_ns;
            return Ok(());
        }
        self.check_snapshot(packet.pn, &state)?;
        let bytes = packet.bytes as u64;
        if bytes > self.inflight {
            return Err(BbrError::InflightUnderflow {
                bytes_in_flight: self.inflight,
                released: bytes,
            });
        }
        let lost = self
            .lost
            .checked_add(bytes)
            .ok_or(BbrError::CounterOverflow)?;
        self.last_now_ns = now_ns;
        self.inflight -= bytes;
        self.lost = lost;
        if !self.in_recovery {
            self.save_cwnd();
            self.save_state_upon_loss();
            self.in_recovery = true;
            self.recovery_start_ns = now_ns;
        }
        self.handle_lost_packet(now_ns, bytes, &state);
        Ok(())
    }

    /// `count` newly reported ECN-CE marks. Each mark is accounted as one
    /// SMSS of congested data in the loss-rate test and as one loss event
    /// (draft §ECN: CE MUST be treated as congestion; RFC 8311 allows the
    /// response to equal the loss response).
    pub fn on_ecn_ce(&mut self, now_ns: u64, count: u64) -> Result<(), BbrError> {
        self.check_time(now_ns)?;
        self.last_now_ns = now_ns;
        if count == 0 {
            return Ok(());
        }
        // Saturating: 2^64 congested bytes is unreachable; saturation keeps
        // the counter monotone.
        self.ce_bytes = self
            .ce_bytes
            .saturating_add(count.saturating_mul(self.smss));
        self.note_loss();
        self.loss_events_in_round = self.loss_events_in_round.saturating_add(count - 1);
        self.ce_pending = true;
        Ok(())
    }

    /// RFC 9002 persistent congestion, handled as the draft's `OnEnterRTO()`:
    /// cwnd falls to allow one SMSS beyond the current flight.
    pub fn on_persistent_congestion(&mut self, now_ns: u64) -> Result<(), BbrError> {
        self.check_time(now_ns)?;
        self.last_now_ns = now_ns;
        self.save_cwnd();
        self.save_state_upon_loss();
        self.cwnd = self.inflight.saturating_add(self.smss).min(u32::MAX as u64);
        self.in_recovery = true;
        self.recovery_start_ns = now_ns;
        Ok(())
    }

    /// The application ran out of data (`MarkConnectionAppLimited()`).
    pub fn on_app_limited(&mut self, now_ns: u64) -> Result<(), BbrError> {
        self.check_time(now_ns)?;
        self.last_now_ns = now_ns;
        self.mark_connection_app_limited();
        Ok(())
    }

    /// The transport declared the last loss episode spurious
    /// (`HandleSpuriousLossDetection()`); ends loss recovery.
    pub fn on_spurious_loss_detected(&mut self, now_ns: u64) -> Result<(), BbrError> {
        self.check_time(now_ns)?;
        self.last_now_ns = now_ns;
        self.restore_cwnd();
        self.is_loss_in_round = false;
        self.loss_events_in_round = 0;
        self.reset_full_bw();
        self.bw_shortterm = self.bw_shortterm.max(self.undo_bw_shortterm);
        self.inflight_shortterm = self.inflight_shortterm.max(self.undo_inflight_shortterm);
        self.inflight_longterm = self.inflight_longterm.max(self.undo_inflight_longterm);
        match self.undo_state {
            Some(BbrState::Startup) if self.state != BbrState::Startup => {
                self.full_bw_reached = false;
                if self.state != BbrState::ProbeRtt {
                    self.enter_startup();
                }
            }
            Some(BbrState::ProbeBwUp)
                if self.state != BbrState::ProbeBwUp && self.state != BbrState::ProbeRtt =>
            {
                self.start_probe_bw_refill();
            }
            _ => {}
        }
        self.undo_state = None;
        self.in_recovery = false;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Validation helpers.

    fn check_time(&self, now_ns: u64) -> Result<(), BbrError> {
        if now_ns < self.last_now_ns {
            Err(BbrError::TimeWentBackwards {
                last_ns: self.last_now_ns,
                now_ns,
            })
        } else {
            Ok(())
        }
    }

    fn check_snapshot(&self, pn: u64, state: &PacketDeliveryState) -> Result<(), BbrError> {
        if state.delivered > self.delivered || state.lost > self.congested() {
            Err(BbrError::ForeignSnapshot { pn })
        } else {
            Ok(())
        }
    }

    const fn congested(&self) -> u64 {
        self.lost.saturating_add(self.ce_bytes)
    }

    fn min_pipe_cwnd(&self) -> u64 {
        4 * self.smss
    }

    fn note_rtt(&mut self, sample: RttSample) {
        self.srtt_ns = Some(sample.smoothed_ns);
        let lifetime = self
            .lifetime_min_rtt_ns
            .map_or(sample.min_ns, |m| m.min(sample.min_ns));
        self.lifetime_min_rtt_ns = Some(lifetime.min(sample.latest_ns));
        // InitPacingRate() "after the first non-zero RTT sample", while no
        // bandwidth sample exists yet.
        if !self.pacing_from_rtt && self.max_bw == 0 && sample.smoothed_ns > 0 {
            self.pacing_from_rtt = true;
            self.init_pacing_rate();
        }
    }

    // ------------------------------------------------------------------
    // Draft procedures.

    fn init_pacing_rate(&mut self) {
        let srtt = match self.srtt_ns {
            Some(s) if s > 0 => s,
            _ => 1_000_000,
        };
        let nominal = self.initial_cwnd as u128 * NANOS_PER_SEC as u128 / srtt as u128;
        let rate = nominal * STARTUP_PACING_GAIN as u128 / GAIN_UNIT as u128;
        self.pacing_rate = sat(rate).max(1);
    }

    fn enter_startup(&mut self) {
        self.state = BbrState::Startup;
        self.pacing_gain = STARTUP_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    fn enter_drain(&mut self) {
        self.state = BbrState::Drain;
        self.pacing_gain = DRAIN_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
        self.drain_start_round = self.round_count;
    }

    fn handle_restart_from_idle(&mut self, now_ns: u64) {
        if self.inflight == 0 && self.app_limited != 0 {
            self.idle_restart = true;
            self.extra_acked_interval_start_ns = now_ns;
            if self.state.is_probe_bw() {
                self.set_pacing_rate_with_gain(UNIT_PACING_GAIN);
            } else if self.state == BbrState::ProbeRtt {
                self.check_probe_rtt_done(now_ns);
            }
        }
    }

    fn mark_connection_app_limited(&mut self) {
        self.app_limited = self.delivered.saturating_add(self.inflight).max(1);
    }

    fn update_model_and_state(&mut self, now_ns: u64) {
        self.update_latest_delivery_signals();
        self.update_congestion_signals();
        self.update_ack_aggregation(now_ns);
        self.check_full_bw_reached();
        self.check_startup_done();
        self.check_drain_done(now_ns);
        self.update_probe_bw_cycle_phase(now_ns);
        self.update_min_rtt(now_ns);
        self.check_probe_rtt(now_ns);
        self.advance_latest_delivery_signals();
        self.bound_bw_for_model();
    }

    fn update_control_parameters(&mut self) {
        self.set_pacing_rate();
        self.set_send_quantum();
        self.set_cwnd();
    }

    fn update_latest_delivery_signals(&mut self) {
        self.loss_round_start = false;
        self.bw_latest = self.bw_latest.max(self.rs.delivery_rate);
        self.inflight_latest = self.inflight_latest.max(self.rs.delivered);
        if self.rs.prior_delivered >= self.loss_round_delivered {
            self.loss_round_delivered = self.delivered;
            self.loss_round_start = true;
        }
    }

    fn advance_latest_delivery_signals(&mut self) {
        if self.loss_round_start {
            self.bw_latest = self.rs.delivery_rate;
            self.inflight_latest = self.rs.delivered;
            self.loss_events_in_round = 0;
        }
    }

    fn reset_congestion_signals(&mut self) {
        self.is_loss_in_round = false;
        self.loss_events_in_round = 0;
        self.bw_latest = 0;
        self.inflight_latest = 0;
    }

    fn update_congestion_signals(&mut self) {
        self.update_max_bw();
        if !self.loss_round_start {
            return;
        }
        self.adapt_lower_bounds_from_congestion();
        self.is_loss_in_round = false;
    }

    fn adapt_lower_bounds_from_congestion(&mut self) {
        if self.is_probing_bw() {
            return;
        }
        if self.is_loss_in_round {
            self.init_lower_bounds();
            self.loss_lower_bounds();
        }
    }

    fn init_lower_bounds(&mut self) {
        if self.bw_shortterm == INFINITY {
            self.bw_shortterm = self.max_bw;
        }
        if self.inflight_shortterm == INFINITY {
            self.inflight_shortterm = self.cwnd;
        }
    }

    fn loss_lower_bounds(&mut self) {
        self.bw_shortterm = self.bw_latest.max(mul_gain(self.bw_shortterm, BETA));
        self.inflight_shortterm = self
            .inflight_latest
            .max(mul_gain(self.inflight_shortterm, BETA));
    }

    fn reset_short_term_model(&mut self) {
        self.bw_shortterm = INFINITY;
        self.inflight_shortterm = INFINITY;
    }

    fn bound_bw_for_model(&mut self) {
        self.bw = self.max_bw.min(self.bw_shortterm);
    }

    fn update_round(&mut self) {
        if self.rs.prior_delivered >= self.next_round_delivered {
            self.start_round();
            self.round_count = self.round_count.saturating_add(1);
            self.rounds_since_probe_up = self.rounds_since_probe_up.saturating_add(1);
            self.round_start = true;
            // C.is_cwnd_limited: cwnd was fully used in the last round.
            self.is_cwnd_limited = self.cwnd_limited_this_round;
            self.cwnd_limited_this_round = false;
        } else {
            self.round_start = false;
        }
    }

    fn start_round(&mut self) {
        self.next_round_delivered = self.delivered;
    }

    fn update_max_bw(&mut self) {
        self.update_round();
        let rate = self.rs.delivery_rate;
        if rate > 0 && (rate >= self.max_bw || !self.rs.is_app_limited) {
            self.max_bw = self.max_bw_filter.update(rate);
        }
    }

    fn advance_max_bw_filter(&mut self) {
        self.cycle_count = self.cycle_count.saturating_add(1);
        self.max_bw = self.max_bw_filter.advance();
    }

    fn update_ack_aggregation(&mut self, now_ns: u64) {
        let interval = now_ns.saturating_sub(self.extra_acked_interval_start_ns);
        let mut expected = bw_volume(self.bw, interval);
        if self.extra_acked_delivered <= expected {
            self.extra_acked_delivered = 0;
            self.extra_acked_interval_start_ns = now_ns;
            expected = 0;
        }
        self.extra_acked_delivered = self
            .extra_acked_delivered
            .saturating_add(self.rs.newly_acked);
        let extra = (self.extra_acked_delivered - expected).min(self.cwnd);
        let filter_len = if self.full_bw_reached {
            EXTRA_ACKED_FILTER_LEN
        } else {
            1
        };
        self.extra_acked = self
            .extra_acked_filter
            .update(filter_len, self.round_count, extra);
    }

    fn reset_full_bw(&mut self) {
        self.full_bw = 0;
        self.full_bw_count = 0;
        self.full_bw_now = false;
    }

    fn check_full_bw_reached(&mut self) {
        if self.full_bw_now || !self.round_start || self.rs.is_app_limited {
            return;
        }
        let rate = self.rs.delivery_rate as u128;
        if rate * GAIN_UNIT as u128 >= self.full_bw as u128 * FULL_BW_GROWTH as u128 {
            self.reset_full_bw();
            self.full_bw = self.rs.delivery_rate;
            return;
        }
        self.full_bw_count += 1;
        self.full_bw_now = self.full_bw_count >= FULL_BW_COUNT;
        if self.full_bw_now {
            self.full_bw_reached = true;
        }
    }

    fn check_startup_done(&mut self) {
        self.check_startup_high_loss();
        if self.state == BbrState::Startup && self.full_bw_reached {
            self.enter_drain();
        }
    }

    fn check_startup_high_loss(&mut self) {
        if self.full_bw_reached || !self.loss_round_start || !self.in_recovery {
            return;
        }
        if self.loss_events_in_round < STARTUP_FULL_LOSS_CNT && self.has_selective_acks {
            return;
        }
        if !self.is_inflight_too_high(self.rs.lost, self.rs.tx_in_flight) {
            return;
        }
        self.undo_state = Some(BbrState::Startup);
        self.full_bw_reached = true;
        self.inflight_longterm = self.bdp.max(self.inflight_latest);
    }

    fn check_drain_done(&mut self, now_ns: u64) {
        if self.state == BbrState::Drain
            && (self.inflight <= self.inflight_with_gain(UNIT_PACING_GAIN)
                || self.round_count > self.drain_start_round.saturating_add(3))
        {
            self.enter_probe_bw(now_ns);
        }
    }

    fn enter_probe_bw(&mut self, now_ns: u64) {
        self.cwnd_gain = DEFAULT_CWND_GAIN;
        self.start_probe_bw_down(now_ns);
    }

    fn start_probe_bw_down(&mut self, now_ns: u64) {
        self.reset_congestion_signals();
        self.probe_up_acked_per_inc = INFINITY;
        self.pick_probe_wait();
        self.cycle_stamp_ns = now_ns;
        self.is_bw_probe_stopping = true;
        self.start_round();
        self.state = BbrState::ProbeBwDown;
        self.pacing_gain = PROBE_DOWN_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    fn start_probe_bw_cruise(&mut self) {
        self.state = BbrState::ProbeBwCruise;
        self.pacing_gain = UNIT_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    fn start_probe_bw_refill(&mut self) {
        self.reset_short_term_model();
        self.bw_probe_up_rounds = 0;
        self.bw_probe_up_acked = 0;
        self.prev_probe_precautionary = false;
        self.is_bw_probe_stopping = false;
        self.start_round();
        self.state = BbrState::ProbeBwRefill;
        self.pacing_gain = UNIT_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    fn start_probe_bw_up(&mut self) {
        self.start_round();
        self.reset_full_bw();
        self.full_bw = self.rs.delivery_rate;
        self.state = BbrState::ProbeBwUp;
        self.pacing_gain = PROBE_UP_PACING_GAIN;
        self.cwnd_gain = PROBE_UP_CWND_GAIN;
        self.raise_inflight_longterm_slope();
    }

    // The per-state checks have side effects (they may start a new phase),
    // so they stay in the arm bodies rather than in match guards.
    #[allow(clippy::collapsible_match)]
    fn update_probe_bw_cycle_phase(&mut self, now_ns: u64) {
        if !self.full_bw_reached {
            return;
        }
        if self.adapt_long_term_model(now_ns) {
            return;
        }
        match self.state {
            BbrState::ProbeBwDown => {
                if self.is_time_to_probe_bw(now_ns) {
                    return;
                }
                if self.is_time_to_cruise() {
                    self.start_probe_bw_cruise();
                }
            }
            BbrState::ProbeBwCruise => {
                self.is_time_to_probe_bw(now_ns);
            }
            BbrState::ProbeBwRefill => {
                if self.round_start {
                    self.check_probe_loss = true;
                    self.start_probe_bw_up();
                }
            }
            BbrState::ProbeBwUp => {
                if self.is_time_to_go_down() {
                    self.prev_probe_too_high = false;
                    self.start_probe_bw_down(now_ns);
                }
            }
            _ => {}
        }
    }

    fn is_time_to_cruise(&mut self) -> bool {
        if self.inflight > self.inflight_with_headroom() {
            return false;
        }
        self.inflight <= self.inflight_with_gain(UNIT_PACING_GAIN)
    }

    fn is_time_to_go_down(&mut self) -> bool {
        if self.prev_probe_too_high && self.inflight >= self.inflight_longterm {
            self.prev_probe_precautionary = true;
            return true;
        }
        if self.is_cwnd_limited && self.cwnd >= self.inflight_longterm {
            self.reset_full_bw();
            self.full_bw = self.rs.delivery_rate;
        } else if self.full_bw_now {
            return true;
        }
        false
    }

    fn is_probing_bw(&self) -> bool {
        matches!(
            self.state,
            BbrState::Startup | BbrState::ProbeBwRefill | BbrState::ProbeBwUp
        )
    }

    fn is_time_to_probe_bw(&mut self, now_ns: u64) -> bool {
        let elapsed = now_ns > self.cycle_stamp_ns.saturating_add(self.bw_probe_wait_ns);
        if elapsed || self.is_reno_coexistence_probe_time() {
            self.start_probe_bw_refill();
            return true;
        }
        false
    }

    fn pick_probe_wait(&mut self) {
        self.rounds_since_probe_up = self.rng.below(2);
        self.bw_probe_wait_ns = PROBE_WAIT_BASE_NS + self.rng.below(PROBE_WAIT_RANGE_NS + 1);
    }

    /// The draft counts Reno rounds in packets: `TargetInflight()` is
    /// converted from bytes by dividing by SMSS.
    fn is_reno_coexistence_probe_time(&self) -> bool {
        let reno_rounds = self.target_inflight() / self.smss;
        self.rounds_since_probe_up >= reno_rounds.min(RENO_ROUNDS_CAP)
    }

    fn target_inflight(&self) -> u64 {
        self.bdp.min(self.cwnd)
    }

    fn inflight_with_headroom(&self) -> u64 {
        if self.inflight_longterm == INFINITY {
            return INFINITY;
        }
        let headroom = self.smss.max(mul_gain(self.inflight_longterm, HEADROOM));
        self.inflight_longterm
            .saturating_sub(headroom)
            .max(self.min_pipe_cwnd())
    }

    fn raise_inflight_longterm_slope(&mut self) {
        let growth_this_round = 1u64 << self.bw_probe_up_rounds;
        self.bw_probe_up_rounds = (self.bw_probe_up_rounds + 1).min(30);
        self.probe_up_acked_per_inc = (self.cwnd / growth_this_round).max(self.smss);
    }

    fn probe_inflight_longterm_upward(&mut self) {
        if !self.is_cwnd_limited || self.cwnd < self.inflight_longterm {
            return;
        }
        self.bw_probe_up_acked = self.bw_probe_up_acked.saturating_add(self.rs.newly_acked);
        if self.bw_probe_up_acked >= self.probe_up_acked_per_inc {
            let delta = self.bw_probe_up_acked / self.probe_up_acked_per_inc;
            self.bw_probe_up_acked -= delta * self.probe_up_acked_per_inc;
            self.inflight_longterm = self
                .inflight_longterm
                .saturating_add(delta.saturating_mul(self.smss));
        }
        if self.round_start {
            self.raise_inflight_longterm_slope();
        }
    }

    fn adapt_long_term_model(&mut self, now_ns: u64) -> bool {
        if self.is_bw_probe_stopping && self.round_start {
            self.check_probe_loss = false;
            self.is_bw_probe_stopping = false;
            if self.state.is_probe_bw() && !self.rs.is_app_limited {
                self.advance_max_bw_filter();
            }
            if self.state.is_probe_bw()
                && self.prev_probe_precautionary
                && !self.prev_probe_too_high
            {
                self.start_probe_bw_refill();
                return true;
            }
        }
        // ECN extension: CE marks arrive with ACKs, not with loss events, so
        // the probing-phase "inflight too high" response runs here when this
        // ACK's rate sample includes new CE marks.
        if self.check_probe_loss
            && self.ce_pending
            && self.is_inflight_too_high(self.rs.lost, self.rs.tx_in_flight)
        {
            let before = self.state;
            self.handle_inflight_too_high(now_ns, self.rs.is_app_limited, self.rs.tx_in_flight);
            return self.state != before;
        }
        if !self.is_inflight_too_high(self.rs.lost, self.rs.tx_in_flight) {
            if self.inflight_longterm == INFINITY {
                return false;
            }
            if self.rs.tx_in_flight > self.inflight_longterm {
                self.inflight_longterm = self.rs.tx_in_flight;
            }
            if self.state == BbrState::ProbeBwUp {
                self.probe_inflight_longterm_upward();
            }
        }
        false
    }

    fn is_inflight_too_high(&self, lost: u64, tx_in_flight: u64) -> bool {
        (lost as u128 * GAIN_UNIT as u128 > tx_in_flight as u128 * LOSS_THRESH as u128)
            || (lost > 0 && !self.has_selective_acks)
    }

    fn handle_inflight_too_high(&mut self, now_ns: u64, is_app_limited: bool, tx_in_flight: u64) {
        self.prev_probe_too_high = true;
        self.check_probe_loss = false;
        if !is_app_limited {
            self.inflight_longterm = tx_in_flight.max(mul_gain(self.target_inflight(), BETA));
        }
        if self.state == BbrState::ProbeBwUp {
            self.undo_state = Some(BbrState::ProbeBwUp);
            self.start_probe_bw_down(now_ns);
        }
    }

    fn note_loss(&mut self) {
        if !self.is_loss_in_round {
            self.loss_round_delivered = self.delivered;
            self.save_state_upon_loss();
        }
        self.is_loss_in_round = true;
        self.loss_events_in_round = self.loss_events_in_round.saturating_add(1);
    }

    fn handle_lost_packet(&mut self, now_ns: u64, size: u64, p: &PacketDeliveryState) {
        self.note_loss();
        if !self.check_probe_loss {
            return;
        }
        let tx_in_flight = p.tx_in_flight;
        let lost = self.congested().saturating_sub(p.lost);
        if self.is_inflight_too_high(lost, tx_in_flight) {
            let at_loss = inflight_at_loss(size, tx_in_flight, lost);
            self.handle_inflight_too_high(now_ns, p.is_app_limited, at_loss);
        }
    }

    fn save_state_upon_loss(&mut self) {
        self.save_cwnd();
        self.undo_state = None;
        self.undo_bw_shortterm = self.bw_shortterm;
        self.undo_inflight_shortterm = self.inflight_shortterm;
        self.undo_inflight_longterm = self.inflight_longterm;
    }

    fn save_cwnd(&mut self) {
        if !self.in_recovery && self.state != BbrState::ProbeRtt {
            self.prior_cwnd = self.cwnd;
        } else {
            self.prior_cwnd = self.prior_cwnd.max(self.cwnd);
        }
    }

    fn restore_cwnd(&mut self) {
        self.cwnd = self.cwnd.max(self.prior_cwnd);
    }

    fn update_min_rtt(&mut self, now_ns: u64) {
        self.min_rtt_expired = now_ns > self.min_rtt_stamp_ns.saturating_add(MIN_RTT_FILTER_LEN_NS);
        if self.min_rtt_expired {
            self.probe_rtt_cwnd = self.bdp_multiple(PROBE_RTT_CWND_GAIN);
        }
        if let Some(rtt) = self.rs.rtt_ns
            && (self.min_rtt_ns.is_none_or(|m| rtt < m) || self.min_rtt_expired)
        {
            self.min_rtt_ns = Some(rtt);
            self.min_rtt_stamp_ns = now_ns;
        }
    }

    fn check_probe_rtt(&mut self, now_ns: u64) {
        if self.state != BbrState::ProbeRtt && self.min_rtt_expired && !self.idle_restart {
            self.save_cwnd();
            self.enter_probe_rtt();
            self.probe_rtt_done_stamp_ns = None;
            self.is_bw_probe_stopping = true;
            self.start_round();
        }
        if self.state == BbrState::ProbeRtt {
            self.handle_probe_rtt(now_ns);
        }
        if self.rs.delivered > 0 {
            self.idle_restart = false;
        }
    }

    fn enter_probe_rtt(&mut self) {
        self.state = BbrState::ProbeRtt;
        self.pacing_gain = UNIT_PACING_GAIN;
        self.cwnd_gain = PROBE_RTT_CWND_GAIN;
    }

    fn handle_probe_rtt(&mut self, now_ns: u64) {
        self.mark_connection_app_limited();
        if self.probe_rtt_done_stamp_ns.is_none() && self.inflight <= self.probe_rtt_cwnd_bound() {
            self.probe_rtt_done_stamp_ns = Some(now_ns.saturating_add(PROBE_RTT_DURATION_NS));
            self.probe_rtt_round_done = false;
            self.start_round();
        } else if self.probe_rtt_done_stamp_ns.is_some() {
            if self.round_start {
                self.probe_rtt_round_done = true;
            }
            if self.probe_rtt_round_done {
                self.check_probe_rtt_done(now_ns);
            }
        }
    }

    fn check_probe_rtt_done(&mut self, now_ns: u64) {
        if let Some(done) = self.probe_rtt_done_stamp_ns
            && now_ns > done
        {
            self.min_rtt_stamp_ns = now_ns;
            self.restore_cwnd();
            self.exit_probe_rtt(now_ns);
        }
    }

    fn exit_probe_rtt(&mut self, now_ns: u64) {
        self.reset_short_term_model();
        if self.full_bw_reached {
            self.start_probe_bw_down(now_ns);
            self.start_probe_bw_cruise();
        } else {
            self.enter_startup();
        }
    }

    fn set_pacing_rate_with_gain(&mut self, gain: u64) {
        let rate = sat(
            self.bw as u128 * gain as u128 * (100 - PACING_MARGIN_PERCENT) as u128
                / (GAIN_UNIT as u128 * 100),
        );
        if self.full_bw_reached || rate > self.pacing_rate {
            self.pacing_rate = rate.max(1);
        }
    }

    fn set_pacing_rate(&mut self) {
        self.set_pacing_rate_with_gain(self.pacing_gain);
    }

    fn set_send_quantum(&mut self) {
        self.send_quantum = (self.pacing_rate / 1000)
            .min(MAX_SEND_QUANTUM)
            .max(2 * self.smss);
    }

    fn bdp_multiple(&mut self, gain: u64) -> u64 {
        let Some(min_rtt) = self.min_rtt_ns else {
            return self.initial_cwnd;
        };
        self.bdp = bw_volume(self.bw, min_rtt);
        mul_gain(self.bdp, gain)
    }

    fn update_offload_budget(&mut self) {
        self.offload_budget = match self.offload {
            OffloadModel::Quic => self.send_quantum,
            OffloadModel::Tcp => self.send_quantum.saturating_mul(3),
        };
    }

    fn quantization_budget(&mut self, inflight_cap: u64) -> u64 {
        self.update_offload_budget();
        let mut cap = inflight_cap
            .max(self.offload_budget)
            .max(self.min_pipe_cwnd());
        if self.state == BbrState::ProbeBwUp {
            cap = cap.saturating_add(2 * self.smss);
        }
        cap
    }

    fn inflight_with_gain(&mut self, gain: u64) -> u64 {
        let cap = self.bdp_multiple(gain);
        self.quantization_budget(cap)
    }

    fn update_max_inflight(&mut self) {
        let cap = self
            .bdp_multiple(self.cwnd_gain)
            .saturating_add(self.extra_acked);
        self.max_inflight = self.quantization_budget(cap);
    }

    fn probe_rtt_cwnd_bound(&mut self) -> u64 {
        self.bdp_multiple(PROBE_RTT_CWND_GAIN)
            .min(self.probe_rtt_cwnd)
            .max(self.min_pipe_cwnd())
    }

    fn set_cwnd(&mut self) {
        self.update_max_inflight();
        let acked = self.rs.newly_acked;
        if self.full_bw_reached {
            self.cwnd = self.cwnd.saturating_add(acked).min(self.max_inflight);
        } else if self.cwnd < self.max_inflight || self.delivered < self.initial_cwnd {
            self.cwnd = self.cwnd.saturating_add(acked);
        }
        self.cwnd = self.cwnd.max(self.min_pipe_cwnd());
        if self.state == BbrState::ProbeRtt {
            self.cwnd = self.cwnd.min(self.probe_rtt_cwnd_bound());
        }
        self.bound_cwnd_for_model();
        self.cwnd = self.cwnd.min(u32::MAX as u64);
    }

    fn bound_cwnd_for_model(&mut self) {
        let mut cap = INFINITY;
        if self.state.is_probe_bw() && self.state != BbrState::ProbeBwCruise {
            cap = self.inflight_longterm;
        } else if matches!(self.state, BbrState::ProbeRtt | BbrState::ProbeBwCruise) {
            cap = self.inflight_with_headroom();
        }
        cap = cap.min(self.inflight_shortterm).max(self.min_pipe_cwnd());
        self.cwnd = self.cwnd.min(cap);
    }
}

const fn finite(x: u64) -> Option<u64> {
    if x == INFINITY { None } else { Some(x) }
}

/// `InflightAtLoss(P)`: the in-flight volume at which the loss rate crossed
/// `LossThresh`, solving `(lost_prev + p) / (inflight_prev + p) = thresh`.
fn inflight_at_loss(size: u64, tx_in_flight: u64, lost: u64) -> u64 {
    let inflight_prev = tx_in_flight.saturating_sub(size);
    let lost_prev = lost.saturating_sub(size);
    let numerator =
        inflight_prev as i128 * LOSS_THRESH as i128 - lost_prev as i128 * GAIN_UNIT as i128;
    let lost_prefix = if numerator > 0 {
        sat(numerator as u128 / (GAIN_UNIT - LOSS_THRESH) as u128)
    } else {
        0
    };
    inflight_prev.saturating_add(lost_prefix)
}

#[cfg(test)]
mod tests;
