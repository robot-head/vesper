//! The Rust implementation of the state machine (the build without
//! `vesper-port`). Frozen: bug fixes only, until the Vesper port replaces it
//! (#907).

use super::*;
use crate::events::{
    Delivered, LostPacket, MAX_PACKET_NUMBER, PacketDeliveryState, RttSample, SentPacket,
};
use crate::filter::SplitMix64;
use crate::pacing::{clamp_target_cwnd, edt_step, pacing_delay_ns, send_allowed};

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

    pub(super) fn validate(&self) -> Result<(), BbrError> {
        if self.smss == 0 || self.smss > 65_535 {
            return Err(BbrError::InvalidConfig("smss must be in 1..=65535"));
        }
        if self.initial_cwnd < self.smss {
            return Err(BbrError::InvalidConfig("initial_cwnd must be >= smss"));
        }
        Ok(())
    }
}

/// Saturating `x * gain / GAIN_UNIT`; `INFINITY` stays `INFINITY`.
pub(super) fn mul_gain(x: u64, gain: u64) -> u64 {
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

impl Bbrv3 {
    /// `OnInit()` at time `now_ns`.
    pub fn new(config: BbrConfig, now_ns: u64) -> Result<Self, BbrError> {
        config.validate()?;
        let mut bbr = Self {
            smss: config.smss as u64,
            initial_cwnd: config.initial_cwnd as u64,
            has_selective_acks: config.has_selective_acks,
            offload: config.offload,
            rng: config.probe_seed,
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
            ce_in_loss_round: false,
            rs: RateSample::default(),
            bbr_state: BbrState::Startup,
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

    /// The send guard: `bytes_in_flight < cwnd`.
    #[must_use]
    pub const fn can_send(&self) -> bool {
        send_allowed(self.inflight, self.cwnd)
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

    /// Whether the connection is marked application-limited.
    #[must_use]
    pub const fn is_app_limited(&self) -> bool {
        self.app_limited != 0
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

    pub(super) fn record_send(
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
        // `released` leaves the flight; `newly_delivered` (which also covers
        // packets already declared lost) is added to C.delivered.
        let mut released: u64 = 0;
        let mut newly_delivered: u64 = 0;
        for d in packets {
            check_pn(d.packet.pn)?;
            if d.packet.sent_ns > now_ns {
                return Err(BbrError::AckedBeforeSent { pn: d.packet.pn });
            }
            if !d.state.counted {
                continue;
            }
            self.check_snapshot(d.packet.pn, &d.state)?;
            newly_delivered = newly_delivered
                .checked_add(d.packet.bytes as u64)
                .ok_or(BbrError::CounterOverflow)?;
            if !d.previously_lost {
                released += d.packet.bytes as u64;
            }
        }
        if released > self.inflight {
            return Err(BbrError::InflightUnderflow {
                bytes_in_flight: self.inflight,
                released,
            });
        }
        if self.delivered.checked_add(newly_delivered).is_none() {
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
        rs.newly_acked = newly_delivered;
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
        self.ce_in_loss_round = true;
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
            Some(BbrState::Startup) if self.bbr_state != BbrState::Startup => {
                self.full_bw_reached = false;
                if self.bbr_state != BbrState::ProbeRtt {
                    self.enter_startup();
                }
            }
            Some(BbrState::ProbeBwUp)
                if self.bbr_state != BbrState::ProbeBwUp
                    && self.bbr_state != BbrState::ProbeRtt =>
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

    pub(super) fn check_time(&self, now_ns: u64) -> Result<(), BbrError> {
        if now_ns < self.last_now_ns {
            Err(BbrError::TimeWentBackwards {
                last_ns: self.last_now_ns,
                now_ns,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn check_snapshot(
        &self,
        pn: u64,
        state: &PacketDeliveryState,
    ) -> Result<(), BbrError> {
        if state.delivered > self.delivered || state.lost > self.congested() {
            Err(BbrError::ForeignSnapshot { pn })
        } else {
            Ok(())
        }
    }

    pub(super) const fn congested(&self) -> u64 {
        self.lost.saturating_add(self.ce_bytes)
    }

    pub(super) fn min_pipe_cwnd(&self) -> u64 {
        4 * self.smss
    }

    pub(super) fn note_rtt(&mut self, sample: RttSample) {
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

    pub(super) fn init_pacing_rate(&mut self) {
        let srtt = match self.srtt_ns {
            Some(s) if s > 0 => s,
            _ => 1_000_000,
        };
        let nominal = self.initial_cwnd as u128 * NANOS_PER_SEC as u128 / srtt as u128;
        let rate = nominal * STARTUP_PACING_GAIN as u128 / GAIN_UNIT as u128;
        self.pacing_rate = sat(rate).max(1);
    }

    pub(super) fn enter_startup(&mut self) {
        self.bbr_state = BbrState::Startup;
        self.pacing_gain = STARTUP_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    pub(super) fn enter_drain(&mut self) {
        self.bbr_state = BbrState::Drain;
        self.pacing_gain = DRAIN_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
        self.drain_start_round = self.round_count;
    }

    pub(super) fn handle_restart_from_idle(&mut self, now_ns: u64) {
        if self.inflight == 0 && self.app_limited != 0 {
            self.idle_restart = true;
            self.extra_acked_interval_start_ns = now_ns;
            if self.bbr_state.is_probe_bw() {
                self.set_pacing_rate_with_gain(UNIT_PACING_GAIN);
            } else if self.bbr_state == BbrState::ProbeRtt {
                self.check_probe_rtt_done(now_ns);
            }
        }
    }

    pub(super) fn mark_connection_app_limited(&mut self) {
        self.app_limited = self.delivered.saturating_add(self.inflight).max(1);
    }

    pub(super) fn update_model_and_state(&mut self, now_ns: u64) {
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

    pub(super) fn update_control_parameters(&mut self) {
        self.set_pacing_rate();
        self.set_send_quantum();
        self.set_cwnd();
    }

    pub(super) fn update_latest_delivery_signals(&mut self) {
        self.loss_round_start = false;
        self.bw_latest = self.bw_latest.max(self.rs.delivery_rate);
        self.inflight_latest = self.inflight_latest.max(self.rs.delivered);
        if self.rs.prior_delivered >= self.loss_round_delivered {
            self.loss_round_delivered = self.delivered;
            self.loss_round_start = true;
        }
    }

    pub(super) fn advance_latest_delivery_signals(&mut self) {
        if self.loss_round_start {
            self.bw_latest = self.rs.delivery_rate;
            self.inflight_latest = self.rs.delivered;
            self.loss_events_in_round = 0;
            self.ce_in_loss_round = false;
        }
    }

    pub(super) fn reset_congestion_signals(&mut self) {
        self.is_loss_in_round = false;
        self.loss_events_in_round = 0;
        self.ce_in_loss_round = false;
        self.bw_latest = 0;
        self.inflight_latest = 0;
    }

    pub(super) fn update_congestion_signals(&mut self) {
        self.update_max_bw();
        if !self.loss_round_start {
            return;
        }
        self.adapt_lower_bounds_from_congestion();
        self.is_loss_in_round = false;
    }

    pub(super) fn adapt_lower_bounds_from_congestion(&mut self) {
        if self.is_probing_bw() {
            return;
        }
        if self.is_loss_in_round {
            self.init_lower_bounds();
            self.loss_lower_bounds();
        }
    }

    pub(super) fn init_lower_bounds(&mut self) {
        if self.bw_shortterm == INFINITY {
            self.bw_shortterm = self.max_bw;
        }
        if self.inflight_shortterm == INFINITY {
            self.inflight_shortterm = self.cwnd;
        }
    }

    pub(super) fn loss_lower_bounds(&mut self) {
        self.bw_shortterm = self.bw_latest.max(mul_gain(self.bw_shortterm, BETA));
        self.inflight_shortterm = self
            .inflight_latest
            .max(mul_gain(self.inflight_shortterm, BETA));
    }

    pub(super) fn reset_short_term_model(&mut self) {
        self.bw_shortterm = INFINITY;
        self.inflight_shortterm = INFINITY;
    }

    pub(super) fn bound_bw_for_model(&mut self) {
        self.bw = self.max_bw.min(self.bw_shortterm);
    }

    pub(super) fn update_round(&mut self) {
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

    pub(super) fn start_round(&mut self) {
        self.next_round_delivered = self.delivered;
    }

    pub(super) fn update_max_bw(&mut self) {
        self.update_round();
        let rate = self.rs.delivery_rate;
        if rate > 0 && (rate >= self.max_bw || !self.rs.is_app_limited) {
            self.max_bw = self.max_bw_filter.update(rate);
        }
    }

    pub(super) fn advance_max_bw_filter(&mut self) {
        self.cycle_count = self.cycle_count.saturating_add(1);
        self.max_bw = self.max_bw_filter.advance();
    }

    pub(super) fn update_ack_aggregation(&mut self, now_ns: u64) {
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

    pub(super) fn reset_full_bw(&mut self) {
        self.full_bw = 0;
        self.full_bw_count = 0;
        self.full_bw_now = false;
    }

    pub(super) fn check_full_bw_reached(&mut self) {
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

    pub(super) fn check_startup_done(&mut self) {
        self.check_startup_high_loss();
        if self.bbr_state == BbrState::Startup && self.full_bw_reached {
            self.enter_drain();
        }
    }

    pub(super) fn check_startup_high_loss(&mut self) {
        // CE marks stand in for fast recovery: without them an ECN path
        // that never drops could not take the loss-based Startup exit.
        if self.full_bw_reached
            || !self.loss_round_start
            || !(self.in_recovery || self.ce_in_loss_round)
        {
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

    pub(super) fn check_drain_done(&mut self, now_ns: u64) {
        if self.bbr_state == BbrState::Drain
            && (self.inflight <= self.inflight_with_gain(UNIT_PACING_GAIN)
                || self.round_count > self.drain_start_round.saturating_add(3))
        {
            self.enter_probe_bw(now_ns);
        }
    }

    pub(super) fn enter_probe_bw(&mut self, now_ns: u64) {
        self.cwnd_gain = DEFAULT_CWND_GAIN;
        self.start_probe_bw_down(now_ns);
    }

    pub(super) fn start_probe_bw_down(&mut self, now_ns: u64) {
        self.reset_congestion_signals();
        self.probe_up_acked_per_inc = INFINITY;
        self.pick_probe_wait();
        self.cycle_stamp_ns = now_ns;
        self.is_bw_probe_stopping = true;
        self.start_round();
        self.bbr_state = BbrState::ProbeBwDown;
        self.pacing_gain = PROBE_DOWN_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    pub(super) fn start_probe_bw_cruise(&mut self) {
        self.bbr_state = BbrState::ProbeBwCruise;
        self.pacing_gain = UNIT_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    pub(super) fn start_probe_bw_refill(&mut self) {
        self.reset_short_term_model();
        self.bw_probe_up_rounds = 0;
        self.bw_probe_up_acked = 0;
        self.prev_probe_precautionary = false;
        self.is_bw_probe_stopping = false;
        self.start_round();
        self.bbr_state = BbrState::ProbeBwRefill;
        self.pacing_gain = UNIT_PACING_GAIN;
        self.cwnd_gain = DEFAULT_CWND_GAIN;
    }

    pub(super) fn start_probe_bw_up(&mut self) {
        self.start_round();
        self.reset_full_bw();
        self.full_bw = self.rs.delivery_rate;
        self.bbr_state = BbrState::ProbeBwUp;
        self.pacing_gain = PROBE_UP_PACING_GAIN;
        self.cwnd_gain = PROBE_UP_CWND_GAIN;
        self.raise_inflight_longterm_slope();
    }

    // The per-state checks have side effects (they may start a new phase),
    // so they stay in the arm bodies rather than in match guards.
    #[allow(clippy::collapsible_match)]
    pub(super) fn update_probe_bw_cycle_phase(&mut self, now_ns: u64) {
        if !self.full_bw_reached {
            return;
        }
        if self.adapt_long_term_model(now_ns) {
            return;
        }
        match self.bbr_state {
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

    pub(super) fn is_time_to_cruise(&mut self) -> bool {
        if self.inflight > self.inflight_with_headroom() {
            return false;
        }
        self.inflight <= self.inflight_with_gain(UNIT_PACING_GAIN)
    }

    pub(super) fn is_time_to_go_down(&mut self) -> bool {
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

    pub(super) fn is_probing_bw(&self) -> bool {
        matches!(
            self.bbr_state,
            BbrState::Startup | BbrState::ProbeBwRefill | BbrState::ProbeBwUp
        )
    }

    pub(super) fn is_time_to_probe_bw(&mut self, now_ns: u64) -> bool {
        let elapsed = now_ns > self.cycle_stamp_ns.saturating_add(self.bw_probe_wait_ns);
        if elapsed || self.is_reno_coexistence_probe_time() {
            self.start_probe_bw_refill();
            return true;
        }
        false
    }

    pub(super) fn pick_probe_wait(&mut self) {
        let mut rng = SplitMix64(self.rng);
        self.rounds_since_probe_up = rng.below(2);
        self.bw_probe_wait_ns = PROBE_WAIT_BASE_NS + rng.below(PROBE_WAIT_RANGE_NS + 1);
        self.rng = rng.0;
    }

    /// The draft counts Reno rounds in packets: `TargetInflight()` is
    /// converted from bytes by dividing by SMSS.
    pub(super) fn is_reno_coexistence_probe_time(&self) -> bool {
        let reno_rounds = self.target_inflight() / self.smss;
        self.rounds_since_probe_up >= reno_rounds.min(RENO_ROUNDS_CAP)
    }

    pub(super) fn target_inflight(&self) -> u64 {
        self.bdp.min(self.cwnd)
    }

    pub(super) fn inflight_with_headroom(&self) -> u64 {
        if self.inflight_longterm == INFINITY {
            return INFINITY;
        }
        let headroom = self.smss.max(mul_gain(self.inflight_longterm, HEADROOM));
        self.inflight_longterm
            .saturating_sub(headroom)
            .max(self.min_pipe_cwnd())
    }

    pub(super) fn raise_inflight_longterm_slope(&mut self) {
        let growth_this_round = 1u64 << self.bw_probe_up_rounds;
        self.bw_probe_up_rounds = (self.bw_probe_up_rounds + 1).min(30);
        self.probe_up_acked_per_inc = (self.cwnd / growth_this_round).max(self.smss);
    }

    pub(super) fn probe_inflight_longterm_upward(&mut self) {
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

    pub(super) fn adapt_long_term_model(&mut self, now_ns: u64) -> bool {
        if self.is_bw_probe_stopping && self.round_start {
            self.check_probe_loss = false;
            self.is_bw_probe_stopping = false;
            if self.bbr_state.is_probe_bw() && !self.rs.is_app_limited {
                self.advance_max_bw_filter();
            }
            if self.bbr_state.is_probe_bw()
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
            let before = self.bbr_state;
            self.handle_inflight_too_high(now_ns, self.rs.is_app_limited, self.rs.tx_in_flight);
            return self.bbr_state != before;
        }
        if !self.is_inflight_too_high(self.rs.lost, self.rs.tx_in_flight) {
            if self.inflight_longterm == INFINITY {
                return false;
            }
            if self.rs.tx_in_flight > self.inflight_longterm {
                self.inflight_longterm = self.rs.tx_in_flight;
            }
            if self.bbr_state == BbrState::ProbeBwUp {
                self.probe_inflight_longterm_upward();
            }
        }
        false
    }

    pub(super) fn is_inflight_too_high(&self, lost: u64, tx_in_flight: u64) -> bool {
        (lost as u128 * GAIN_UNIT as u128 > tx_in_flight as u128 * LOSS_THRESH as u128)
            || (lost > 0 && !self.has_selective_acks)
    }

    pub(super) fn handle_inflight_too_high(
        &mut self,
        now_ns: u64,
        is_app_limited: bool,
        tx_in_flight: u64,
    ) {
        self.prev_probe_too_high = true;
        self.check_probe_loss = false;
        if !is_app_limited {
            self.inflight_longterm = tx_in_flight.max(mul_gain(self.target_inflight(), BETA));
        }
        if self.bbr_state == BbrState::ProbeBwUp {
            self.undo_state = Some(BbrState::ProbeBwUp);
            self.start_probe_bw_down(now_ns);
        }
    }

    pub(super) fn note_loss(&mut self) {
        if !self.is_loss_in_round {
            self.loss_round_delivered = self.delivered;
            self.save_state_upon_loss();
        }
        self.is_loss_in_round = true;
        self.loss_events_in_round = self.loss_events_in_round.saturating_add(1);
    }

    pub(super) fn handle_lost_packet(&mut self, now_ns: u64, size: u64, p: &PacketDeliveryState) {
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

    pub(super) fn save_state_upon_loss(&mut self) {
        self.save_cwnd();
        self.undo_state = None;
        self.undo_bw_shortterm = self.bw_shortterm;
        self.undo_inflight_shortterm = self.inflight_shortterm;
        self.undo_inflight_longterm = self.inflight_longterm;
    }

    pub(super) fn save_cwnd(&mut self) {
        if !self.in_recovery && self.bbr_state != BbrState::ProbeRtt {
            self.prior_cwnd = self.cwnd;
        } else {
            self.prior_cwnd = self.prior_cwnd.max(self.cwnd);
        }
    }

    pub(super) fn restore_cwnd(&mut self) {
        self.cwnd = self.cwnd.max(self.prior_cwnd);
    }

    pub(super) fn update_min_rtt(&mut self, now_ns: u64) {
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

    pub(super) fn check_probe_rtt(&mut self, now_ns: u64) {
        if self.bbr_state != BbrState::ProbeRtt && self.min_rtt_expired && !self.idle_restart {
            self.save_cwnd();
            self.enter_probe_rtt();
            self.probe_rtt_done_stamp_ns = None;
            self.is_bw_probe_stopping = true;
            self.start_round();
        }
        if self.bbr_state == BbrState::ProbeRtt {
            self.handle_probe_rtt(now_ns);
        }
        if self.rs.delivered > 0 {
            self.idle_restart = false;
        }
    }

    pub(super) fn enter_probe_rtt(&mut self) {
        self.bbr_state = BbrState::ProbeRtt;
        self.pacing_gain = UNIT_PACING_GAIN;
        self.cwnd_gain = PROBE_RTT_CWND_GAIN;
    }

    pub(super) fn handle_probe_rtt(&mut self, now_ns: u64) {
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

    pub(super) fn check_probe_rtt_done(&mut self, now_ns: u64) {
        if let Some(done) = self.probe_rtt_done_stamp_ns
            && now_ns > done
        {
            self.min_rtt_stamp_ns = now_ns;
            self.restore_cwnd();
            self.exit_probe_rtt(now_ns);
        }
    }

    pub(super) fn exit_probe_rtt(&mut self, now_ns: u64) {
        self.reset_short_term_model();
        if self.full_bw_reached {
            self.start_probe_bw_down(now_ns);
            self.start_probe_bw_cruise();
        } else {
            self.enter_startup();
        }
    }

    pub(super) fn set_pacing_rate_with_gain(&mut self, gain: u64) {
        let rate = sat(
            self.bw as u128 * gain as u128 * (100 - PACING_MARGIN_PERCENT) as u128
                / (GAIN_UNIT as u128 * 100),
        );
        if self.full_bw_reached || rate > self.pacing_rate {
            self.pacing_rate = rate.max(1);
        }
    }

    pub(super) fn set_pacing_rate(&mut self) {
        self.set_pacing_rate_with_gain(self.pacing_gain);
    }

    pub(super) fn set_send_quantum(&mut self) {
        self.send_quantum = (self.pacing_rate / 1000)
            .min(MAX_SEND_QUANTUM)
            .max(2 * self.smss);
    }

    pub(super) fn bdp_multiple(&mut self, gain: u64) -> u64 {
        let Some(min_rtt) = self.min_rtt_ns else {
            return self.initial_cwnd;
        };
        self.bdp = bw_volume(self.bw, min_rtt);
        mul_gain(self.bdp, gain)
    }

    pub(super) fn update_offload_budget(&mut self) {
        self.offload_budget = match self.offload {
            OffloadModel::Quic => self.send_quantum,
            OffloadModel::Tcp => self.send_quantum.saturating_mul(3),
        };
    }

    pub(super) fn quantization_budget(&mut self, inflight_cap: u64) -> u64 {
        self.update_offload_budget();
        let mut cap = inflight_cap
            .max(self.offload_budget)
            .max(self.min_pipe_cwnd());
        if self.bbr_state == BbrState::ProbeBwUp {
            cap = cap.saturating_add(2 * self.smss);
        }
        cap
    }

    pub(super) fn inflight_with_gain(&mut self, gain: u64) -> u64 {
        let cap = self.bdp_multiple(gain);
        self.quantization_budget(cap)
    }

    pub(super) fn update_max_inflight(&mut self) {
        let cap = self
            .bdp_multiple(self.cwnd_gain)
            .saturating_add(self.extra_acked);
        self.max_inflight = self.quantization_budget(cap);
    }

    pub(super) fn probe_rtt_cwnd_bound(&mut self) -> u64 {
        self.bdp_multiple(PROBE_RTT_CWND_GAIN)
            .min(self.probe_rtt_cwnd)
            .max(self.min_pipe_cwnd())
    }

    pub(super) fn set_cwnd(&mut self) {
        self.update_max_inflight();
        let acked = self.rs.newly_acked;
        if self.full_bw_reached {
            self.cwnd = self.cwnd.saturating_add(acked).min(self.max_inflight);
        } else if self.cwnd < self.max_inflight || self.delivered < self.initial_cwnd {
            self.cwnd = self.cwnd.saturating_add(acked);
        }
        self.cwnd = self.cwnd.max(self.min_pipe_cwnd());
        if self.bbr_state == BbrState::ProbeRtt {
            self.cwnd = self.cwnd.min(self.probe_rtt_cwnd_bound());
        }
        self.bound_cwnd_for_model();
        self.cwnd = self.cwnd.min(u32::MAX as u64);
    }

    pub(super) fn bound_cwnd_for_model(&mut self) {
        let mut cap = INFINITY;
        if self.bbr_state.is_probe_bw() && self.bbr_state != BbrState::ProbeBwCruise {
            cap = self.inflight_longterm;
        } else if matches!(self.bbr_state, BbrState::ProbeRtt | BbrState::ProbeBwCruise) {
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
pub(super) fn inflight_at_loss(size: u64, tx_in_flight: u64, lost: u64) -> u64 {
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
