//! [`crate::recovery`] forwarded to `vesper/src/recovery.ves`. The wrapper
//! owns the buffers Vesper works in, all sized at construction: one
//! `RecSpace` per packet-number space, each space's ring of sent-packet
//! records, and the events of one call, which it replays to the caller's
//! sink in the order Vesper wrote them.

use super::binding::{self as b, repr};
use crate::PnSpace;
use crate::frame::{AckFrame, Ranges};
use crate::recovery::{
    AckOutcome, AckedPacket, CongestionEvent, HandshakeStatus, LostPacket, RecoveryConfig,
    RecoveryError, RttSample, SentPacket, TimeoutAction, Timer, TimerKind,
};
use vesper_port_abi::Word;

fn space_of(i: u8) -> PnSpace {
    match i {
        0 => PnSpace::Initial,
        1 => PnSpace::Handshake,
        _ => PnSpace::ApplicationData,
    }
}

fn recovery_error(e: b::RecErr) -> RecoveryError {
    match e {
        b::RecErr::PnOutOfRange { pn } => RecoveryError::PnOutOfRange(pn),
        b::RecErr::NonIncreasingPn { pn, last } => RecoveryError::NonIncreasingPn { pn, last },
        b::RecErr::ClockWentBackwards => RecoveryError::ClockWentBackwards,
        b::RecErr::TooManyTracked => RecoveryError::TooManyTracked,
        b::RecErr::AckOfUnsentPacket { largest_acked } => {
            RecoveryError::AckOfUnsentPacket { largest_acked }
        }
        b::RecErr::SpaceDiscarded { space } => RecoveryError::SpaceDiscarded(space_of(space)),
        b::RecErr::BadAckDelayExponent { exponent } => RecoveryError::BadAckDelayExponent(exponent),
    }
}

/// Converts an encoded ACK Delay to nanoseconds (RFC 9000 §19.3),
/// saturating: the value is peer-controlled.
pub fn ack_delay_ns(encoded: u64, ack_delay_exponent: u8) -> Result<u64, RecoveryError> {
    b::recovery_ack_delay_ns(encoded, ack_delay_exponent).map_err(recovery_error)
}

/// RFC 9002 §5 RTT estimator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RttEstimator(repr::Rtt);

impl Eq for RttEstimator {}

impl RttEstimator {
    pub fn new(initial_rtt_ns: u64) -> Self {
        Self(b::recovery_rtt_new(initial_rtt_ns))
    }
    pub fn latest_ns(&self) -> u64 {
        self.0.latest
    }
    pub fn min_ns(&self) -> u64 {
        self.0.min
    }
    pub fn smoothed_ns(&self) -> u64 {
        self.0.smoothed
    }
    pub fn rttvar_ns(&self) -> u64 {
        self.0.rttvar
    }
    pub fn has_sample(&self) -> bool {
        self.0.has_sample.get()
    }

    /// §5.3. `ack_delay_ns` must already be zero where it is ignored and
    /// capped to `max_ack_delay` once the handshake is confirmed.
    pub fn update(&mut self, latest_ns: u64, ack_delay_ns: u64) {
        b::recovery_rtt_update(&mut self.0, latest_ns, ack_delay_ns);
    }

    /// `smoothed + max(4·rttvar, kGranularity)` (no max_ack_delay, no backoff).
    pub fn pto_base_ns(&self) -> u64 {
        b::recovery_rtt_pto_base_ns(&self.0)
    }

    /// `max(kTimeThreshold · max(smoothed, latest), kGranularity)`.
    pub fn loss_delay_ns(&self) -> u64 {
        b::recovery_rtt_loss_delay_ns(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct LossRecovery {
    state: repr::RecState,
    rtt: RttEstimator,
    spaces: [repr::RecSpace; 3],
    /// Space `i`'s ring is `entries[i * cap..(i + 1) * cap]`.
    entries: Vec<repr::RecEntry>,
    /// One call's events: at most one per record of a space, plus ECN-CE
    /// and persistent congestion.
    events: Vec<repr::RecEvent>,
    timer: Option<Timer>,
}

fn replay(events: &[repr::RecEvent], sink: &mut impl FnMut(CongestionEvent)) {
    for e in events {
        sink(match e.kind.get() {
            0 => CongestionEvent::Acked {
                packet: AckedPacket {
                    pn: e.pn,
                    sent_ns: e.sent_ns,
                    bytes: e.bytes.get(),
                },
            },
            1 => CongestionEvent::Lost {
                packet: LostPacket {
                    pn: e.pn,
                    sent_ns: e.sent_ns,
                    bytes: e.bytes.get(),
                },
            },
            2 => CongestionEvent::EcnCe { count: e.count },
            _ => CongestionEvent::PersistentCongestion,
        });
    }
}

impl LossRecovery {
    pub fn new(config: RecoveryConfig, handshake: HandshakeStatus) -> Self {
        let cap = config.max_tracked_per_space;
        let state = b::recovery_new(
            config.max_ack_delay_ns,
            config.initial_rtt_ns,
            cap as u64,
            handshake.confirmed,
            handshake.peer_completed_address_validation,
            handshake.has_handshake_keys,
            handshake.at_amplification_limit,
        );
        let space = b::recovery_space_new();
        let entry = repr::RecEntry {
            pn: 0,
            sent_ns: 0,
            bytes: Word::new(0),
            ack_eliciting: Word::new(false),
            in_flight: Word::new(false),
            status: Word::new(0),
        };
        let event = repr::RecEvent {
            kind: Word::new(0),
            pn: 0,
            sent_ns: 0,
            bytes: Word::new(0),
            count: 0,
        };
        Self {
            state,
            rtt: RttEstimator::new(config.initial_rtt_ns),
            spaces: [space; 3],
            entries: vec![entry; cap.saturating_mul(3)],
            events: vec![event; cap.saturating_add(2)],
            timer: None,
        }
    }

    pub const fn rtt(&self) -> &RttEstimator {
        &self.rtt
    }
    pub fn pto_count(&self) -> u32 {
        self.state.pto_count.get()
    }
    pub const fn bytes_in_flight(&self) -> u64 {
        self.state.bytes_in_flight
    }
    /// The armed loss-detection timer.
    pub const fn timer(&self) -> Option<Timer> {
        self.timer
    }
    pub fn largest_acked(&self, space: PnSpace) -> Option<u64> {
        let s = &self.spaces[space.index()];
        s.has_largest_acked.get().then_some(s.largest_acked)
    }
    pub fn ack_eliciting_in_flight(&self, space: PnSpace) -> u32 {
        self.spaces[space.index()].ack_eliciting_in_flight.get()
    }
    /// Records still held for `space` (in flight plus unpopped tombstones).
    pub fn tracked(&self, space: PnSpace) -> usize {
        self.spaces[space.index()].len as usize
    }

    /// The timer Vesper armed, as the public type.
    fn sync_timer(&mut self) {
        let st = &self.state;
        self.timer = st.has_timer.get().then(|| {
            let space = space_of(st.timer_space.get());
            Timer {
                deadline_ns: st.timer_deadline_ns,
                kind: if st.timer_pto.get() {
                    TimerKind::Pto(space)
                } else {
                    TimerKind::Loss(space)
                },
            }
        });
    }

    /// The peer's `max_ack_delay` from its transport parameters (wave six).
    pub fn set_max_ack_delay_ns(&mut self, max_ack_delay_ns: u64, now_ns: u64) {
        self.state.max_ack_delay_ns = max_ack_delay_ns;
        b::recovery_set_timer(&mut self.state, &self.rtt.0, &self.spaces, now_ns);
        self.sync_timer();
    }

    /// The `max_ack_delay` in force.
    pub const fn max_ack_delay_ns(&self) -> u64 {
        self.state.max_ack_delay_ns
    }

    fn clear_space(&mut self, space: PnSpace, discard: bool, now_ns: u64) -> u64 {
        let removed = b::recovery_clear_space(
            &mut self.state,
            &self.rtt.0,
            &mut self.spaces,
            &self.entries,
            space.index() as u64,
            discard,
            now_ns,
        );
        self.sync_timer();
        removed
    }

    /// Forgets every packet of `space` without congestion events, keeping
    /// the space usable: a client after Retry (RFC 9002 §6.3) or after its
    /// 0-RTT packets were rejected. Returns the bytes removed from flight.
    pub fn reset_space(&mut self, space: PnSpace, now_ns: u64) -> u64 {
        self.clear_space(space, false, now_ns)
    }

    pub fn set_handshake_status(&mut self, status: HandshakeStatus, now_ns: u64) {
        self.state.confirmed = Word::new(status.confirmed);
        self.state.pcav = Word::new(status.peer_completed_address_validation);
        self.state.has_handshake_keys = Word::new(status.has_handshake_keys);
        self.state.at_amplification_limit = Word::new(status.at_amplification_limit);
        b::recovery_set_timer(&mut self.state, &self.rtt.0, &self.spaces, now_ns);
        self.sync_timer();
    }

    /// A.5 `OnPacketSent`.
    pub fn on_packet_sent(&mut self, space: PnSpace, pkt: SentPacket) -> Result<(), RecoveryError> {
        let entry = repr::RecEntry {
            pn: pkt.pn,
            sent_ns: pkt.sent_ns,
            bytes: Word::new(pkt.bytes),
            ack_eliciting: Word::new(pkt.ack_eliciting),
            in_flight: Word::new(pkt.in_flight),
            status: Word::new(0),
        };
        let r = b::recovery_on_packet_sent(
            &mut self.state,
            &self.rtt.0,
            &mut self.spaces,
            &mut self.entries,
            space.index() as u64,
            &entry,
        )
        .map_err(recovery_error);
        self.sync_timer();
        r
    }

    /// A.7 `OnAckReceived`. `ack_delay_ns` is the peer's decoded ACK Delay
    /// (see [`ack_delay_ns`]). Events go to `sink` in RFC order: lost
    /// packets first, then acknowledged packets.
    pub fn on_ack_received(
        &mut self,
        space: PnSpace,
        ack: &AckFrame<'_>,
        ack_delay_ns: u64,
        now_ns: u64,
        sink: &mut impl FnMut(CongestionEvent),
    ) -> Result<AckOutcome, RecoveryError> {
        let (decoded, first_range, count, pairs, ranges): (bool, u64, u64, &[u8], &[_]) =
            match ack.ranges {
                Ranges::Wire {
                    first_range,
                    count,
                    pairs,
                } => (false, first_range, count, pairs, &[]),
                Ranges::Decoded(r) => (true, 0, 0, &[], r),
            };
        let (has_ecn, ce) = ack.ecn.map_or((false, 0), |e| (true, e.ce));
        let out = b::recovery_on_ack_received(
            &mut self.state,
            &mut self.rtt.0,
            &mut self.spaces,
            &mut self.entries,
            &mut self.events,
            space.index() as u64,
            ack.largest,
            decoded,
            first_range,
            count,
            pairs,
            ranges,
            has_ecn,
            ce,
            ack_delay_ns,
            now_ns,
        )
        .map_err(recovery_error)?;
        self.sync_timer();
        replay(&self.events[..self.state.n_events as usize], sink);
        Ok(AckOutcome {
            newly_acked: out.newly_acked.get(),
            newly_acked_bytes: out.newly_acked_bytes,
            lost: out.lost.get(),
            lost_bytes: out.lost_bytes,
            rtt_sample: out.has_rtt.get().then_some(RttSample {
                latest_ns: out.rtt_latest_ns,
                min_ns: out.rtt_min_ns,
                smoothed_ns: out.rtt_smoothed_ns,
                ack_delay_ns: out.rtt_ack_delay_ns,
            }),
            ecn_ce_increase: out.ecn_ce_increase,
            persistent_congestion: out.persistent_congestion.get(),
        })
    }

    /// §7.6.1: `(smoothed + max(4·rttvar, kGranularity) + max_ack_delay) × 3`.
    pub fn persistent_congestion_duration_ns(&self) -> u64 {
        b::recovery_pc_duration_ns(&self.state, &self.rtt.0)
    }

    /// A.8 `GetPtoTimeAndSpace`.
    pub fn pto_time_and_space(&self, now_ns: u64) -> Option<(u64, PnSpace)> {
        let p = b::recovery_pto_time_and_space(&self.state, &self.rtt.0, &self.spaces, now_ns);
        p.has.get().then(|| (p.t, space_of(p.space.get())))
    }

    /// A.9 `OnLossDetectionTimeout`.
    pub fn on_timeout(
        &mut self,
        now_ns: u64,
        sink: &mut impl FnMut(CongestionEvent),
    ) -> TimeoutAction {
        let out = b::recovery_on_timeout(
            &mut self.state,
            &self.rtt.0,
            &mut self.spaces,
            &mut self.entries,
            &mut self.events,
            now_ns,
        );
        self.sync_timer();
        replay(&self.events[..self.state.n_events as usize], sink);
        let space = space_of(out.space.get());
        match out.kind.get() {
            0 => TimeoutAction::None,
            1 => TimeoutAction::LossDetected {
                space,
                lost: out.lost.get(),
            },
            _ => TimeoutAction::SendProbe {
                space,
                packets: out.packets.get(),
                anti_deadlock: out.anti_deadlock.get(),
            },
        }
    }

    /// RFC 9002 §6.4 / A.11: keys for `space` were discarded. In-flight
    /// bytes are removed without congestion events; returns them.
    pub fn discard_space(&mut self, space: PnSpace, now_ns: u64) -> u64 {
        self.clear_space(space, true, now_ns)
    }
}
