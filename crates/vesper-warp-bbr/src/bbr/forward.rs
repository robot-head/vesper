//! The Vesper build of the state machine (`vesper-port`): every method with
//! logic forwards to `vesper/src/bbr.ves` through the generated binding.
//!
//! The controller state is the same Rust struct in both builds; the binding
//! moves it across the export ABI as a `copy` value. What stays here is
//! the call protocol only:
//!
//! * Vesper's `Fail` cannot cross an `inout` parameter, so each entry point
//!   returns `Result[.., BbrFault]` as a value after checking everything
//!   first, and [`BbrFault`] converts to [`BbrError`].
//! * An ACK's packets are not passed as one slice: the export ABI copies a
//!   slice of non-scalar elements into a heap buffer, which would break the
//!   zero-allocation guarantee. The wrapper validates every packet
//!   (`ack_check`, no state change), checks the totals, and then feeds the
//!   packets one by one (`bbrv3_ack_step`).

use super::*;
use crate::BbrFault;
use crate::events::{Delivered, LostPacket, PacketDeliveryState, RttSample, SentPacket};
use crate::port;

/// The snapshot passed with the single step of an ACK without packets; the
/// port ignores it.
const NO_SNAPSHOT: PacketDeliveryState = PacketDeliveryState {
    delivered: 0,
    delivered_ns: 0,
    first_sent_ns: 0,
    is_app_limited: false,
    tx_in_flight: 0,
    lost: 0,
    counted: false,
};

impl BbrState {
    /// `IsInAProbeBWState()`.
    #[must_use]
    pub fn is_probe_bw(self) -> bool {
        port::bbr_state_is_probe_bw(self)
    }
}

impl BbrConfig {
    /// QUIC defaults: RFC 9002 §7.2 initial window
    /// `min(10 * smss, max(14720, 2 * smss))`, selective ACKs, QUIC offload.
    #[must_use]
    pub fn quic(smss: u32) -> Self {
        port::bbr_config_quic(smss)
    }
}

impl From<BbrFault> for BbrError {
    fn from(fault: BbrFault) -> Self {
        match fault {
            BbrFault::InvalidConfig(0) => Self::InvalidConfig("smss must be in 1..=65535"),
            BbrFault::InvalidConfig(_) => Self::InvalidConfig("initial_cwnd must be >= smss"),
            BbrFault::PacketNumberOutOfRange { pn } => Self::PacketNumberOutOfRange { pn },
            BbrFault::EmptyInFlightPacket { pn } => Self::EmptyInFlightPacket { pn },
            BbrFault::CwndFull {
                bytes_in_flight,
                cwnd,
            } => Self::CwndFull {
                bytes_in_flight,
                cwnd,
            },
            BbrFault::InflightUnderflow {
                bytes_in_flight,
                released,
            } => Self::InflightUnderflow {
                bytes_in_flight,
                released,
            },
            BbrFault::ForeignSnapshot { pn } => Self::ForeignSnapshot { pn },
            BbrFault::TimeWentBackwards { last_ns, now_ns } => {
                Self::TimeWentBackwards { last_ns, now_ns }
            }
            BbrFault::AckedBeforeSent { pn } => Self::AckedBeforeSent { pn },
            BbrFault::CounterOverflow => Self::CounterOverflow,
        }
    }
}

impl Bbrv3 {
    /// `OnInit()` at time `now_ns`.
    pub fn new(config: BbrConfig, now_ns: u64) -> Result<Self, BbrError> {
        Ok(port::bbrv3_new(config, now_ns)?)
    }

    /// `C.cwnd`, saturated to `u32` (contract window type).
    #[must_use]
    pub fn cwnd_bytes(&self) -> u32 {
        port::saturate_u32(self.cwnd)
    }

    /// §35.5 `target_cwnd_bytes`: `cwnd` clamped to
    /// `[4 * mss, max(inflight_hi, 4 * mss)]` (see
    /// [`crate::clamp_target_cwnd`]).
    #[must_use]
    pub fn target_cwnd_bytes(&self, mss: u32) -> u64 {
        port::clamp_target_cwnd(self.cwnd, mss, self.inflight_longterm)
    }

    /// §35.5 `schedule_edt_ns`: returns `max(now, next_edt)` as this
    /// packet's departure time and advances `next_edt` by the pacing delay
    /// of `pkt_bytes` at the current pacing rate. Any `now_ns` is accepted,
    /// in any order; the returned times never decrease.
    pub fn schedule_edt_ns(&mut self, pkt_bytes: u32, now_ns: u64) -> u64 {
        port::schedule_edt_ns(&mut self.next_edt_ns, self.pacing_rate, pkt_bytes, now_ns)
    }

    /// The send guard: `bytes_in_flight < cwnd`.
    ///
    /// This one stays a Rust `const fn` (the same comparison as the port's
    /// `send_allowed`): `vesper-warp` calls it from a `const fn`, and a
    /// foreign call cannot be `const`.
    #[must_use]
    pub const fn can_send(&self) -> bool {
        self.inflight < self.cwnd
    }

    /// `BBR.inflight_longterm` (called `inflight_hi` in earlier draft revisions and Linux BBRv3);
    /// `None` = Infinity.
    #[must_use]
    pub fn inflight_hi(&self) -> Option<u64> {
        port::finite(self.inflight_longterm)
    }

    /// `BBR.inflight_shortterm` (formerly `inflight_lo`); `None` = Infinity.
    #[must_use]
    pub fn inflight_lo(&self) -> Option<u64> {
        port::finite(self.inflight_shortterm)
    }

    /// `BBR.bw_shortterm` (formerly `bw_lo`); `None` = Infinity.
    #[must_use]
    pub fn bw_lo(&self) -> Option<u64> {
        port::finite(self.bw_shortterm)
    }

    /// Whether the connection is marked application-limited.
    #[must_use]
    pub fn is_app_limited(&self) -> bool {
        port::app_limited_marked(self.app_limited)
    }

    /// `OnTransmit()` + `OnPacketSent(P)`, refusing the send when the send
    /// guard is closed. Returns the snapshot to store with the packet.
    pub fn on_packet_sent(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
    ) -> Result<PacketDeliveryState, BbrError> {
        Ok(port::bbrv3_record_send(self, now_ns, packet, true)?)
    }

    /// As [`Self::on_packet_sent`] but without the send guard, for RFC 9002
    /// probe packets (PTO), which may exceed the congestion window. Sends
    /// made this way are outside the Lean send-guard bound.
    pub fn on_probe_packet_sent(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
    ) -> Result<PacketDeliveryState, BbrError> {
        Ok(port::bbrv3_record_send(self, now_ns, packet, false)?)
    }

    /// `UpdateRateSample` for each packet, then `GenerateRateSample()` and
    /// `UpdateOnACK()`. `packets` are this ACK's newly acknowledged packets.
    pub fn on_ack(
        &mut self,
        now_ns: u64,
        packets: &[Delivered],
        rtt: Option<RttSample>,
    ) -> Result<(), BbrError> {
        port::check_time(self.last_now_ns, now_ns)?;
        let mut progress = port::ack_progress_new();
        for d in packets {
            port::ack_check(
                now_ns,
                self.delivered,
                self.lost,
                self.ce_bytes,
                &mut progress,
                d.packet,
                d.state,
                d.previously_lost,
            )?;
        }
        port::ack_check_totals(progress, self.inflight, self.delivered)?;
        let Some(last) = packets.len().checked_sub(1) else {
            port::bbrv3_ack_step(
                self,
                now_ns,
                rtt,
                &mut progress,
                None,
                NO_SNAPSHOT,
                true,
                true,
            );
            return Ok(());
        };
        for (i, d) in packets.iter().enumerate() {
            port::bbrv3_ack_step(
                self,
                now_ns,
                rtt,
                &mut progress,
                Some(d.packet),
                d.state,
                i == 0,
                i == last,
            );
        }
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
        Ok(port::bbrv3_on_packet_lost(self, now_ns, packet, state)?)
    }

    /// `count` newly reported ECN-CE marks. Each mark is accounted as one
    /// SMSS of congested data in the loss-rate test and as one loss event
    /// (draft §ECN: CE MUST be treated as congestion; RFC 8311 allows the
    /// response to equal the loss response).
    pub fn on_ecn_ce(&mut self, now_ns: u64, count: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_ecn_ce(self, now_ns, count)?)
    }

    /// RFC 9002 persistent congestion, handled as the draft's `OnEnterRTO()`:
    /// cwnd falls to allow one SMSS beyond the current flight.
    pub fn on_persistent_congestion(&mut self, now_ns: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_persistent_congestion(self, now_ns)?)
    }

    /// The application ran out of data (`MarkConnectionAppLimited()`).
    pub fn on_app_limited(&mut self, now_ns: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_app_limited(self, now_ns)?)
    }

    /// The transport declared the last loss episode spurious
    /// (`HandleSpuriousLossDetection()`); ends loss recovery.
    pub fn on_spurious_loss_detected(&mut self, now_ns: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_spurious_loss_detected(self, now_ns)?)
    }

    // Internal procedures the unit tests call directly.

    #[cfg(test)]
    pub(super) fn start_probe_bw_down(&mut self, now_ns: u64) {
        port::bbrv3_start_probe_bw_down(self, now_ns);
    }

    #[cfg(test)]
    pub(super) fn probe_rtt_cwnd_bound(&mut self) -> u64 {
        port::bbrv3_probe_rtt_cwnd_bound(self)
    }

    #[cfg(test)]
    pub(super) fn target_inflight(&self) -> u64 {
        port::bbrv3_target_inflight(self.bdp, self.cwnd)
    }

    #[cfg(test)]
    pub(super) fn inflight_with_gain(&mut self, gain: u64) -> u64 {
        port::bbrv3_inflight_with_gain(self, gain)
    }

    #[cfg(test)]
    pub(super) fn check_full_bw_reached(&mut self) {
        port::bbrv3_check_full_bw_reached(self);
    }

    #[cfg(test)]
    pub(super) fn check_probe_rtt_done(&mut self, now_ns: u64) {
        port::bbrv3_check_probe_rtt_done(self, now_ns);
    }

    #[cfg(test)]
    pub(super) fn is_inflight_too_high(&self, lost: u64, tx_in_flight: u64) -> bool {
        port::bbrv3_is_inflight_too_high(self.has_selective_acks, lost, tx_in_flight)
    }

    #[cfg(test)]
    pub(super) fn raise_inflight_longterm_slope(&mut self) {
        port::bbrv3_raise_inflight_longterm_slope(self);
    }

    #[cfg(test)]
    pub(super) fn probe_inflight_longterm_upward(&mut self) {
        port::bbrv3_probe_inflight_longterm_upward(self);
    }

    #[cfg(test)]
    pub(super) fn is_time_to_go_down(&mut self) -> bool {
        port::bbrv3_is_time_to_go_down(self)
    }

    #[cfg(test)]
    pub(super) fn adapt_long_term_model(&mut self, now_ns: u64) -> bool {
        port::bbrv3_adapt_long_term_model(self, now_ns)
    }
}

/// Saturating `x * gain / GAIN_UNIT`; `INFINITY` stays `INFINITY`.
#[cfg(test)]
pub(super) fn mul_gain(x: u64, gain: u64) -> u64 {
    port::mul_gain(x, gain)
}

/// `InflightAtLoss(P)`.
#[cfg(test)]
pub(super) fn inflight_at_loss(size: u64, tx_in_flight: u64, lost: u64) -> u64 {
    port::inflight_at_loss(size, tx_in_flight, lost)
}
