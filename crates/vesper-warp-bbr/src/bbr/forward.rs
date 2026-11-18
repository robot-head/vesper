//! The Vesper build of the state machine (`vesper-port`): every method with
//! logic forwards to `vesper/src/bbr.ves` through the generated binding.
//!
//! The controller state is the binding's `#[repr(C)]` mirror of the Vesper
//! `Bbrv3` (export ABI v3, #934): every call lends it to Vesper in place,
//! and the field getters read its cells. What stays here is the call
//! protocol only:
//!
//! * Each entry point raises `Fail[BbrFault]` (a `Result` in the binding)
//!   only before its first write, so a rejected input leaves the state
//!   unchanged, and [`BbrFault`] converts to [`BbrError`].
//! * An ACK's packets are not passed as one slice: the packets are Rust
//!   values, not mirrors, and converting them to a slice of mirrors would
//!   need a buffer, which would break the zero-allocation guarantee. The wrapper validates every packet
//!   (`ack_check`, no state change), checks the totals, and then feeds the
//!   packets one by one (`bbrv3_ack_step`).

use super::*;
use crate::BbrFault;
use crate::events::{Delivered, LostPacket, PacketDeliveryState, RttSample, SentPacket};
use crate::port;
use crate::port::repr;

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

/// The Vesper `ack_progress_new()`, computed once: every ACK starts from
/// this constant, and building it in Vesper costs more than the rest of a
/// one-packet ACK's checks.
fn empty_progress() -> repr::AckProgress {
    static EMPTY: std::sync::OnceLock<repr::AckProgress> = std::sync::OnceLock::new();
    *EMPTY.get_or_init(port::ack_progress_new)
}

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
        port::bbr_config_quic(smss).to_value()
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
        let m = port::bbrv3_new(&repr::BbrConfig::from_value(&config), now_ns)
            .get()
            .map_err(|e| e.get())?;
        Ok(Self { m })
    }

    /// `C.cwnd`, saturated to `u32` (contract window type).
    #[must_use]
    pub fn cwnd_bytes(&self) -> u32 {
        port::saturate_u32(self.m.cwnd)
    }

    /// §35.5 `target_cwnd_bytes`: `cwnd` clamped to
    /// `[4 * mss, max(inflight_hi, 4 * mss)]` (see
    /// [`crate::clamp_target_cwnd`]).
    #[must_use]
    pub fn target_cwnd_bytes(&self, mss: u32) -> u64 {
        port::clamp_target_cwnd(self.m.cwnd, mss, self.m.inflight_longterm)
    }

    /// §35.5 `schedule_edt_ns`: returns `max(now, next_edt)` as this
    /// packet's departure time and advances `next_edt` by the pacing delay
    /// of `pkt_bytes` at the current pacing rate. Any `now_ns` is accepted,
    /// in any order; the returned times never decrease.
    pub fn schedule_edt_ns(&mut self, pkt_bytes: u32, now_ns: u64) -> u64 {
        port::schedule_edt_ns(
            &mut self.m.next_edt_ns,
            self.m.pacing_rate,
            pkt_bytes,
            now_ns,
        )
    }

    /// The send guard: `bytes_in_flight < cwnd`.
    ///
    /// This one stays a Rust `const fn` (the same comparison as the port's
    /// `send_allowed`): `vesper-warp` calls it from a `const fn`, and a
    /// foreign call cannot be `const`.
    #[must_use]
    pub const fn can_send(&self) -> bool {
        self.m.inflight < self.m.cwnd
    }

    /// `BBR.inflight_longterm` (called `inflight_hi` in earlier draft revisions and Linux BBRv3);
    /// `None` = Infinity.
    #[must_use]
    pub fn inflight_hi(&self) -> Option<u64> {
        port::finite(self.m.inflight_longterm).get()
    }

    /// `BBR.inflight_shortterm` (formerly `inflight_lo`); `None` = Infinity.
    #[must_use]
    pub fn inflight_lo(&self) -> Option<u64> {
        port::finite(self.m.inflight_shortterm).get()
    }

    /// `BBR.bw_shortterm` (formerly `bw_lo`); `None` = Infinity.
    #[must_use]
    pub fn bw_lo(&self) -> Option<u64> {
        port::finite(self.m.bw_shortterm).get()
    }

    /// Whether the connection is marked application-limited.
    #[must_use]
    pub fn is_app_limited(&self) -> bool {
        port::app_limited_marked(self.m.app_limited)
    }

    /// `OnTransmit()` + `OnPacketSent(P)`, refusing the send when the send
    /// guard is closed. Returns the snapshot to store with the packet.
    pub fn on_packet_sent(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
    ) -> Result<PacketDeliveryState, BbrError> {
        Ok(port::bbrv3_record_send(
            &mut self.m,
            now_ns,
            &repr::SentPacket::from_value(&packet),
            true,
        )?
        .to_value())
    }

    /// As [`Self::on_packet_sent`] but without the send guard, for RFC 9002
    /// probe packets (PTO), which may exceed the congestion window. Sends
    /// made this way are outside the Lean send-guard bound.
    pub fn on_probe_packet_sent(
        &mut self,
        now_ns: u64,
        packet: SentPacket,
    ) -> Result<PacketDeliveryState, BbrError> {
        Ok(port::bbrv3_record_send(
            &mut self.m,
            now_ns,
            &repr::SentPacket::from_value(&packet),
            false,
        )?
        .to_value())
    }

    /// `UpdateRateSample` for each packet, then `GenerateRateSample()` and
    /// `UpdateOnACK()`. `packets` are this ACK's newly acknowledged packets.
    pub fn on_ack(
        &mut self,
        now_ns: u64,
        packets: &[Delivered],
        rtt: Option<RttSample>,
    ) -> Result<(), BbrError> {
        let b = &mut self.m;
        port::check_time(b.last_now_ns, now_ns)?;
        let mut progress = empty_progress();
        for d in packets {
            port::ack_check(
                now_ns,
                b.delivered,
                b.lost,
                b.ce_bytes,
                &mut progress,
                &repr::AckedPacket::from_value(&d.packet),
                &repr::PacketDeliveryState::from_value(&d.state),
                d.previously_lost,
            )?;
        }
        port::ack_check_totals(&progress, b.inflight, b.delivered)?;
        let rtt: vesper_port_abi::OptionMirror<repr::RttSample> =
            rtt.map(|r| repr::RttSample::from_value(&r)).into();
        let Some(last) = packets.len().checked_sub(1) else {
            port::bbrv3_ack_step(
                b,
                now_ns,
                &rtt,
                &mut progress,
                &vesper_port_abi::OptionMirror::none(),
                &repr::PacketDeliveryState::from_value(&NO_SNAPSHOT),
                true,
                true,
            );
            return Ok(());
        };
        for (i, d) in packets.iter().enumerate() {
            port::bbrv3_ack_step(
                b,
                now_ns,
                &rtt,
                &mut progress,
                &vesper_port_abi::OptionMirror::some(repr::AckedPacket::from_value(&d.packet)),
                &repr::PacketDeliveryState::from_value(&d.state),
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
        Ok(port::bbrv3_on_packet_lost(
            &mut self.m,
            now_ns,
            &repr::LostPacket::from_value(&packet),
            &repr::PacketDeliveryState::from_value(&state),
        )?)
    }

    /// `count` newly reported ECN-CE marks. Each mark is accounted as one
    /// SMSS of congested data in the loss-rate test and as one loss event
    /// (draft §ECN: CE MUST be treated as congestion; RFC 8311 allows the
    /// response to equal the loss response).
    pub fn on_ecn_ce(&mut self, now_ns: u64, count: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_ecn_ce(&mut self.m, now_ns, count)?)
    }

    /// RFC 9002 persistent congestion, handled as the draft's `OnEnterRTO()`:
    /// cwnd falls to allow one SMSS beyond the current flight.
    pub fn on_persistent_congestion(&mut self, now_ns: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_persistent_congestion(&mut self.m, now_ns)?)
    }

    /// The application ran out of data (`MarkConnectionAppLimited()`).
    pub fn on_app_limited(&mut self, now_ns: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_app_limited(&mut self.m, now_ns)?)
    }

    /// The transport declared the last loss episode spurious
    /// (`HandleSpuriousLossDetection()`); ends loss recovery.
    pub fn on_spurious_loss_detected(&mut self, now_ns: u64) -> Result<(), BbrError> {
        Ok(port::bbrv3_on_spurious_loss_detected(&mut self.m, now_ns)?)
    }

    // Internal procedures the unit tests call directly.

    #[cfg(test)]
    pub(super) fn start_probe_bw_down(&mut self, now_ns: u64) {
        port::bbrv3_start_probe_bw_down(&mut self.m, now_ns);
    }

    #[cfg(test)]
    pub(super) fn probe_rtt_cwnd_bound(&mut self) -> u64 {
        port::bbrv3_probe_rtt_cwnd_bound(&mut self.m)
    }

    #[cfg(test)]
    pub(super) fn target_inflight(&self) -> u64 {
        port::bbrv3_target_inflight(self.m.bdp, self.m.cwnd)
    }

    #[cfg(test)]
    pub(super) fn inflight_with_gain(&mut self, gain: u64) -> u64 {
        port::bbrv3_inflight_with_gain(&mut self.m, gain)
    }

    #[cfg(test)]
    pub(super) fn check_full_bw_reached(&mut self) {
        port::bbrv3_check_full_bw_reached(&mut self.m);
    }

    #[cfg(test)]
    pub(super) fn check_probe_rtt_done(&mut self, now_ns: u64) {
        port::bbrv3_check_probe_rtt_done(&mut self.m, now_ns);
    }

    #[cfg(test)]
    pub(super) fn is_inflight_too_high(&self, lost: u64, tx_in_flight: u64) -> bool {
        port::bbrv3_is_inflight_too_high(self.m.has_selective_acks.get(), lost, tx_in_flight)
    }

    #[cfg(test)]
    pub(super) fn raise_inflight_longterm_slope(&mut self) {
        port::bbrv3_raise_inflight_longterm_slope(&mut self.m);
    }

    #[cfg(test)]
    pub(super) fn probe_inflight_longterm_upward(&mut self) {
        port::bbrv3_probe_inflight_longterm_upward(&mut self.m);
    }

    #[cfg(test)]
    pub(super) fn is_time_to_go_down(&mut self) -> bool {
        port::bbrv3_is_time_to_go_down(&mut self.m)
    }

    #[cfg(test)]
    pub(super) fn adapt_long_term_model(&mut self, now_ns: u64) -> bool {
        port::bbrv3_adapt_long_term_model(&mut self.m, now_ns)
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

// The field getters read the mirror's cells: full-word fields directly (so
// they stay `const fn`), narrower ones through `Word::get`/`Words::get`.
impl Bbrv3 {
    /// `C.pacing_rate` in bytes per second (always `>= 1`).
    #[must_use]
    pub const fn pacing_rate_bytes_per_sec(&self) -> u64 {
        self.m.pacing_rate
    }

    /// `C.send_quantum` in bytes.
    #[must_use]
    pub const fn send_quantum_bytes(&self) -> u64 {
        self.m.send_quantum
    }

    /// `C.next_send_time`: the earliest departure for the next packet.
    #[must_use]
    pub const fn next_edt_ns(&self) -> u64 {
        self.m.next_edt_ns
    }

    /// `C.inflight`.
    #[must_use]
    pub const fn bytes_in_flight(&self) -> u64 {
        self.m.inflight
    }

    /// `BBR.state`.
    #[must_use]
    pub fn state(&self) -> BbrState {
        self.m.bbr_state.get()
    }

    /// `BBR.max_bw`.
    #[must_use]
    pub const fn max_bw(&self) -> u64 {
        self.m.max_bw
    }

    /// `BBR.bw = min(max_bw, bw_shortterm)`.
    #[must_use]
    pub const fn bw(&self) -> u64 {
        self.m.bw
    }

    /// `BBR.min_rtt` (`None` = Infinity).
    #[must_use]
    pub fn min_rtt_ns(&self) -> Option<u64> {
        self.m.min_rtt_ns.get()
    }

    /// `BBR.bdp` as last computed.
    #[must_use]
    pub const fn bdp_bytes(&self) -> u64 {
        self.m.bdp
    }

    /// `BBR.full_bw_reached`.
    #[must_use]
    pub fn full_bw_reached(&self) -> bool {
        self.m.full_bw_reached.get()
    }

    /// `BBR.round_count`.
    #[must_use]
    pub const fn round_count(&self) -> u64 {
        self.m.round_count
    }

    /// `BBR.cycle_count`.
    #[must_use]
    pub const fn cycle_count(&self) -> u64 {
        self.m.cycle_count
    }

    /// `BBR.pacing_gain`, per mille.
    #[must_use]
    pub const fn pacing_gain(&self) -> u64 {
        self.m.pacing_gain
    }

    /// `BBR.cwnd_gain`, per mille.
    #[must_use]
    pub const fn cwnd_gain(&self) -> u64 {
        self.m.cwnd_gain
    }

    /// `BBR.extra_acked`.
    #[must_use]
    pub const fn extra_acked(&self) -> u64 {
        self.m.extra_acked
    }

    /// `C.delivered`.
    #[must_use]
    pub const fn delivered_bytes(&self) -> u64 {
        self.m.delivered
    }

    /// `C.lost`.
    #[must_use]
    pub const fn lost_bytes(&self) -> u64 {
        self.m.lost
    }

    /// Whether loss recovery is in progress.
    #[must_use]
    pub fn in_recovery(&self) -> bool {
        self.m.in_recovery.get()
    }

    /// The rate sample of the last ACK that delivered data.
    #[must_use]
    pub fn last_rate_sample(&self) -> RateSample {
        self.m.rs.to_value()
    }

    /// The whole state as the Rust value (for `Debug` and the unit tests).
    pub(crate) fn value(&self) -> Bbrv3Value {
        self.m.to_value()
    }
}

/// The same text as the Rust build's derived `Debug`, from the Rust value.
impl std::fmt::Debug for Bbrv3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.value(), f)
    }
}

/// The unit tests read and write the controller's fields by name. In the
/// Vesper build a write goes through this guard, which holds the Rust value
/// and stores it back into the mirror when it is dropped.
#[cfg(test)]
pub(super) struct FieldsMut<'a> {
    bbr: &'a mut Bbrv3,
    value: Bbrv3Value,
}

#[cfg(test)]
impl std::ops::Deref for FieldsMut<'_> {
    type Target = Bbrv3Value;
    fn deref(&self) -> &Bbrv3Value {
        &self.value
    }
}

#[cfg(test)]
impl std::ops::DerefMut for FieldsMut<'_> {
    fn deref_mut(&mut self) -> &mut Bbrv3Value {
        &mut self.value
    }
}

#[cfg(test)]
impl Drop for FieldsMut<'_> {
    fn drop(&mut self) {
        self.bbr.m = repr::Bbrv3::from_value(&self.value);
    }
}

#[cfg(test)]
impl Bbrv3 {
    /// The fields, read by name.
    pub(super) fn fields(&self) -> Bbrv3Value {
        self.value()
    }

    /// The fields, written by name (stored back when the guard drops).
    pub(super) fn fields_mut(&mut self) -> FieldsMut<'_> {
        let value = self.value();
        FieldsMut { bbr: self, value }
    }
}
