//! The Rust reference implementation of [`crate::recovery`], compiled
//! without `vesper-port` (the Vesper build is `vesper/src/recovery.ves`).
//! Frozen: bug fixes only (#1066).

use crate::frame::AckFrame;
use crate::recovery::{
    AckOutcome, AckedPacket, CongestionEvent, HandshakeStatus, K_GRANULARITY_NS,
    K_PACKET_THRESHOLD, K_PERSISTENT_CONGESTION_THRESHOLD, K_TIME_THRESHOLD_DEN,
    K_TIME_THRESHOLD_NUM, LostPacket, RecoveryConfig, RecoveryError, RttSample, SentPacket,
    TimeoutAction, Timer, TimerKind,
};
use crate::{PN_LIMIT, PnSpace};
use std::collections::VecDeque;

/// Converts an encoded ACK Delay to nanoseconds (RFC 9000 §19.3),
/// saturating: the value is peer-controlled.
pub fn ack_delay_ns(encoded: u64, ack_delay_exponent: u8) -> Result<u64, RecoveryError> {
    if ack_delay_exponent > 20 {
        return Err(RecoveryError::BadAckDelayExponent(ack_delay_exponent));
    }
    let us = encoded
        .checked_shl(u32::from(ack_delay_exponent))
        .filter(|v| v >> ack_delay_exponent == encoded)
        .unwrap_or(u64::MAX);
    Ok(us.saturating_mul(1_000))
}

/// RFC 9002 §5 RTT estimator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RttEstimator {
    latest: u64,
    min: u64,
    smoothed: u64,
    rttvar: u64,
    has_sample: bool,
}

impl RttEstimator {
    pub const fn new(initial_rtt_ns: u64) -> Self {
        Self {
            latest: 0,
            min: 0,
            smoothed: initial_rtt_ns,
            rttvar: initial_rtt_ns / 2,
            has_sample: false,
        }
    }
    pub const fn latest_ns(&self) -> u64 {
        self.latest
    }
    pub const fn min_ns(&self) -> u64 {
        self.min
    }
    pub const fn smoothed_ns(&self) -> u64 {
        self.smoothed
    }
    pub const fn rttvar_ns(&self) -> u64 {
        self.rttvar
    }
    pub const fn has_sample(&self) -> bool {
        self.has_sample
    }

    /// §5.3. `ack_delay_ns` must already be zero where it is ignored and
    /// capped to `max_ack_delay` once the handshake is confirmed.
    pub fn update(&mut self, latest_ns: u64, ack_delay_ns: u64) {
        self.latest = latest_ns;
        if !self.has_sample {
            self.has_sample = true;
            self.min = latest_ns;
            self.smoothed = latest_ns;
            self.rttvar = latest_ns / 2;
            return;
        }
        self.min = self.min.min(latest_ns);
        let adjusted = if latest_ns >= self.min.saturating_add(ack_delay_ns) {
            latest_ns - ack_delay_ns
        } else {
            latest_ns
        };
        let diff = self.smoothed.abs_diff(adjusted);
        // Exact integer forms of 3/4·rttvar + 1/4·diff and 7/8·srtt + 1/8·adj
        // in u128, so peer-influenced large samples cannot overflow.
        self.rttvar = ((3 * u128::from(self.rttvar) + u128::from(diff)) / 4) as u64;
        self.smoothed = ((7 * u128::from(self.smoothed) + u128::from(adjusted)) / 8) as u64;
    }

    /// `smoothed + max(4·rttvar, kGranularity)` (no max_ack_delay, no backoff).
    pub fn pto_base_ns(&self) -> u64 {
        self.smoothed
            .saturating_add(self.rttvar.saturating_mul(4).max(K_GRANULARITY_NS))
    }

    /// `max(kTimeThreshold · max(smoothed, latest), kGranularity)`.
    pub fn loss_delay_ns(&self) -> u64 {
        let base = u128::from(self.smoothed.max(self.latest));
        let delay = base * u128::from(K_TIME_THRESHOLD_NUM) / u128::from(K_TIME_THRESHOLD_DEN);
        u64::try_from(delay)
            .unwrap_or(u64::MAX)
            .max(K_GRANULARITY_NS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    InFlight,
    /// Acknowledged by the ACK being processed; events not yet emitted.
    NewlyAcked,
    Acked,
    Lost,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    pkt: SentPacket,
    state: State,
}

#[derive(Debug, Clone)]
struct Space {
    /// Ascending packet numbers; acked/lost entries are tombstones until they
    /// reach the front.
    sent: VecDeque<Entry>,
    largest_acked: Option<u64>,
    last_sent_pn: Option<u64>,
    time_of_last_ack_eliciting: Option<u64>,
    loss_time: Option<u64>,
    ack_eliciting_in_flight: u32,
    ecn_ce: u64,
    discarded: bool,
}

impl Space {
    fn new(capacity: usize) -> Self {
        Self {
            sent: VecDeque::with_capacity(capacity),
            largest_acked: None,
            last_sent_pn: None,
            time_of_last_ack_eliciting: None,
            loss_time: None,
            ack_eliciting_in_flight: 0,
            ecn_ce: 0,
            discarded: false,
        }
    }
    fn pop_tombstones(&mut self) {
        while self
            .sent
            .front()
            .is_some_and(|e| matches!(e.state, State::Acked | State::Lost))
        {
            self.sent.pop_front();
        }
    }
}

#[derive(Debug, Clone)]
pub struct LossRecovery {
    config: RecoveryConfig,
    rtt: RttEstimator,
    spaces: [Space; 3],
    pto_count: u32,
    bytes_in_flight: u64,
    handshake: HandshakeStatus,
    first_rtt_sample_ns: Option<u64>,
    last_sent_ns: u64,
    timer: Option<Timer>,
}

#[derive(Debug, Clone, Copy, Default)]
struct LossBatch {
    lost: u32,
    lost_bytes: u64,
    persistent_congestion: bool,
}

impl LossRecovery {
    pub fn new(config: RecoveryConfig, handshake: HandshakeStatus) -> Self {
        let cap = config.max_tracked_per_space;
        Self {
            config,
            rtt: RttEstimator::new(config.initial_rtt_ns),
            spaces: [Space::new(cap), Space::new(cap), Space::new(cap)],
            pto_count: 0,
            bytes_in_flight: 0,
            handshake,
            first_rtt_sample_ns: None,
            last_sent_ns: 0,
            timer: None,
        }
    }

    pub const fn rtt(&self) -> &RttEstimator {
        &self.rtt
    }
    pub const fn pto_count(&self) -> u32 {
        self.pto_count
    }
    pub const fn bytes_in_flight(&self) -> u64 {
        self.bytes_in_flight
    }
    /// The armed loss-detection timer.
    pub const fn timer(&self) -> Option<Timer> {
        self.timer
    }
    pub fn largest_acked(&self, space: PnSpace) -> Option<u64> {
        self.spaces[space.index()].largest_acked
    }
    pub fn ack_eliciting_in_flight(&self, space: PnSpace) -> u32 {
        self.spaces[space.index()].ack_eliciting_in_flight
    }
    /// Records still held for `space` (in flight plus unpopped tombstones).
    pub fn tracked(&self, space: PnSpace) -> usize {
        self.spaces[space.index()].sent.len()
    }

    /// The peer's `max_ack_delay` from its transport parameters (wave six).
    pub fn set_max_ack_delay_ns(&mut self, max_ack_delay_ns: u64, now_ns: u64) {
        self.config.max_ack_delay_ns = max_ack_delay_ns;
        self.set_timer(now_ns);
    }

    /// The `max_ack_delay` in force.
    pub const fn max_ack_delay_ns(&self) -> u64 {
        self.config.max_ack_delay_ns
    }

    /// Forgets every packet of `space` without congestion events, keeping
    /// the space usable: a client after Retry (RFC 9002 §6.3) or after its
    /// 0-RTT packets were rejected. Returns the bytes removed from flight.
    pub fn reset_space(&mut self, space: PnSpace, now_ns: u64) -> u64 {
        let s = &mut self.spaces[space.index()];
        let mut removed = 0;
        for e in s.sent.drain(..) {
            if e.state == State::InFlight && e.pkt.in_flight {
                removed += u64::from(e.pkt.bytes);
            }
        }
        s.loss_time = None;
        s.time_of_last_ack_eliciting = None;
        s.ack_eliciting_in_flight = 0;
        self.bytes_in_flight -= removed;
        self.pto_count = 0;
        self.set_timer(now_ns);
        removed
    }

    pub fn set_handshake_status(&mut self, status: HandshakeStatus, now_ns: u64) {
        self.handshake = status;
        self.set_timer(now_ns);
    }

    /// A.5 `OnPacketSent`.
    pub fn on_packet_sent(&mut self, space: PnSpace, pkt: SentPacket) -> Result<(), RecoveryError> {
        let s = &mut self.spaces[space.index()];
        if s.discarded {
            return Err(RecoveryError::SpaceDiscarded(space));
        }
        if pkt.pn >= PN_LIMIT {
            return Err(RecoveryError::PnOutOfRange(pkt.pn));
        }
        if let Some(last) = s.last_sent_pn
            && pkt.pn <= last
        {
            return Err(RecoveryError::NonIncreasingPn { pn: pkt.pn, last });
        }
        if pkt.sent_ns < self.last_sent_ns {
            return Err(RecoveryError::ClockWentBackwards);
        }
        if !pkt.in_flight {
            // ACK-only (non-in-flight) packets need no loss detection and
            // need not be promptly acknowledged (RFC 9002 §3); recording
            // them would let receive-only traffic exhaust the bounded
            // queue. Only the packet number is kept, for ACK validation.
            s.last_sent_pn = Some(pkt.pn);
            self.last_sent_ns = pkt.sent_ns;
            return Ok(());
        }
        s.pop_tombstones();
        if s.sent.len() >= self.config.max_tracked_per_space {
            return Err(RecoveryError::TooManyTracked);
        }
        s.last_sent_pn = Some(pkt.pn);
        self.last_sent_ns = pkt.sent_ns;
        s.sent.push_back(Entry {
            pkt,
            state: State::InFlight,
        });
        if pkt.in_flight {
            if pkt.ack_eliciting {
                s.time_of_last_ack_eliciting = Some(pkt.sent_ns);
                s.ack_eliciting_in_flight += 1;
            }
            self.bytes_in_flight += u64::from(pkt.bytes);
            self.set_timer(pkt.sent_ns);
        }
        Ok(())
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
        let i = space.index();
        let s = &mut self.spaces[i];
        if s.discarded {
            return Err(RecoveryError::SpaceDiscarded(space));
        }
        if s.last_sent_pn.is_none_or(|last| ack.largest > last) {
            return Err(RecoveryError::AckOfUnsentPacket {
                largest_acked: ack.largest,
            });
        }
        s.largest_acked = Some(s.largest_acked.map_or(ack.largest, |l| l.max(ack.largest)));

        // DetectAndRemoveAckedPackets: mark, remembering the touched span.
        let mut outcome = AckOutcome::default();
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        let mut largest_newly: Option<Entry> = None;
        let mut any_ack_eliciting = false;
        for range in ack.ranges() {
            let start = s.sent.partition_point(|e| e.pkt.pn < range.smallest);
            let mut j = start;
            while let Some(e) = s.sent.get_mut(j) {
                if e.pkt.pn > range.largest {
                    break;
                }
                if e.state == State::InFlight {
                    e.state = State::NewlyAcked;
                    lo = lo.min(j);
                    hi = hi.max(j);
                    outcome.newly_acked += 1;
                    outcome.newly_acked_bytes += u64::from(e.pkt.bytes);
                    any_ack_eliciting |= e.pkt.ack_eliciting;
                    if largest_newly.is_none_or(|l| e.pkt.pn > l.pkt.pn) {
                        largest_newly = Some(*e);
                    }
                }
                j += 1;
            }
        }
        let Some(largest_newly) = largest_newly else {
            return Ok(outcome);
        };

        if largest_newly.pkt.pn == ack.largest && any_ack_eliciting {
            let latest = now_ns.saturating_sub(largest_newly.pkt.sent_ns);
            let delay = if space == PnSpace::Initial {
                0
            } else if self.handshake.confirmed {
                ack_delay_ns.min(self.config.max_ack_delay_ns)
            } else {
                ack_delay_ns
            };
            self.rtt.update(latest, delay);
            if self.first_rtt_sample_ns.is_none() {
                self.first_rtt_sample_ns = Some(now_ns);
            }
            outcome.rtt_sample = Some(RttSample {
                latest_ns: latest,
                min_ns: self.rtt.min_ns(),
                smoothed_ns: self.rtt.smoothed_ns(),
                ack_delay_ns: delay,
            });
        }

        if let Some(ecn) = ack.ecn {
            let s = &mut self.spaces[i];
            if ecn.ce > s.ecn_ce {
                outcome.ecn_ce_increase = ecn.ce - s.ecn_ce;
                s.ecn_ce = ecn.ce;
                sink(CongestionEvent::EcnCe {
                    count: outcome.ecn_ce_increase,
                });
            }
        }

        let batch = self.detect_lost(space, now_ns, sink);
        outcome.lost = batch.lost;
        outcome.lost_bytes = batch.lost_bytes;
        outcome.persistent_congestion = batch.persistent_congestion;

        // OnPacketsAcked.
        let s = &mut self.spaces[i];
        for j in lo..=hi {
            let e = &mut s.sent[j];
            if e.state != State::NewlyAcked {
                continue;
            }
            e.state = State::Acked;
            if e.pkt.in_flight {
                self.bytes_in_flight -= u64::from(e.pkt.bytes);
                if e.pkt.ack_eliciting {
                    s.ack_eliciting_in_flight -= 1;
                }
                sink(CongestionEvent::Acked {
                    packet: AckedPacket {
                        pn: e.pkt.pn,
                        sent_ns: e.pkt.sent_ns,
                        bytes: e.pkt.bytes,
                    },
                });
            }
        }
        s.pop_tombstones();

        if self.handshake.peer_completed_address_validation {
            self.pto_count = 0;
        }
        self.set_timer(now_ns);
        Ok(outcome)
    }

    /// A.10 `DetectAndRemoveLostPackets` plus §7.6 persistent congestion.
    fn detect_lost(
        &mut self,
        space: PnSpace,
        now_ns: u64,
        sink: &mut impl FnMut(CongestionEvent),
    ) -> LossBatch {
        let loss_delay = self.rtt.loss_delay_ns();
        let pc_duration = self.persistent_congestion_duration_ns();
        let first_sample = self.first_rtt_sample_ns;
        let s = &mut self.spaces[space.index()];
        let mut batch = LossBatch::default();
        s.loss_time = None;
        let Some(largest_acked) = s.largest_acked else {
            return batch;
        };
        let lost_send_time = now_ns.checked_sub(loss_delay);
        // Persistent-congestion streak: ack-eliciting losses (sent after the
        // first RTT sample) with no acknowledged packet in between.
        let mut streak: Option<(u64, u64)> = None;
        for e in s.sent.iter_mut() {
            if e.pkt.pn > largest_acked {
                break;
            }
            match e.state {
                State::Acked | State::NewlyAcked => {
                    streak = None;
                    continue;
                }
                State::Lost => continue,
                State::InFlight => {}
            }
            let by_time = lost_send_time.is_some_and(|t| e.pkt.sent_ns <= t);
            let by_count = largest_acked >= e.pkt.pn.saturating_add(K_PACKET_THRESHOLD);
            if by_time || by_count {
                e.state = State::Lost;
                batch.lost += 1;
                if e.pkt.in_flight {
                    batch.lost_bytes += u64::from(e.pkt.bytes);
                    self.bytes_in_flight -= u64::from(e.pkt.bytes);
                    if e.pkt.ack_eliciting {
                        s.ack_eliciting_in_flight -= 1;
                    }
                    sink(CongestionEvent::Lost {
                        packet: LostPacket {
                            pn: e.pkt.pn,
                            sent_ns: e.pkt.sent_ns,
                            bytes: e.pkt.bytes,
                        },
                    });
                }
                if e.pkt.ack_eliciting && first_sample.is_some_and(|f| e.pkt.sent_ns > f) {
                    let (start, _) = *streak.get_or_insert((e.pkt.sent_ns, e.pkt.sent_ns));
                    streak = Some((start, e.pkt.sent_ns));
                    if e.pkt.sent_ns - start >= pc_duration {
                        batch.persistent_congestion = true;
                    }
                }
            } else {
                let t = e.pkt.sent_ns.saturating_add(loss_delay);
                s.loss_time = Some(s.loss_time.map_or(t, |l| l.min(t)));
            }
        }
        if batch.persistent_congestion {
            sink(CongestionEvent::PersistentCongestion);
        }
        batch
    }

    /// §7.6.1: `(smoothed + max(4·rttvar, kGranularity) + max_ack_delay) × 3`.
    pub fn persistent_congestion_duration_ns(&self) -> u64 {
        self.rtt
            .pto_base_ns()
            .saturating_add(self.config.max_ack_delay_ns)
            .saturating_mul(K_PERSISTENT_CONGESTION_THRESHOLD)
    }

    fn backoff(&self, d: u64) -> u64 {
        d.checked_shl(self.pto_count)
            .filter(|v| self.pto_count < 64 && v >> self.pto_count == d)
            .unwrap_or(u64::MAX)
    }

    /// A.8 `GetPtoTimeAndSpace`.
    pub fn pto_time_and_space(&self, now_ns: u64) -> Option<(u64, PnSpace)> {
        let duration = self.backoff(self.rtt.pto_base_ns());
        if self.spaces.iter().all(|s| s.ack_eliciting_in_flight == 0) {
            if self.handshake.peer_completed_address_validation {
                return None;
            }
            let space = if self.handshake.has_handshake_keys {
                PnSpace::Handshake
            } else {
                PnSpace::Initial
            };
            return Some((now_ns.saturating_add(duration), space));
        }
        let mut best: Option<(u64, PnSpace)> = None;
        for space in PnSpace::ALL {
            let s = &self.spaces[space.index()];
            if s.ack_eliciting_in_flight == 0 {
                continue;
            }
            let mut d = duration;
            if space == PnSpace::ApplicationData {
                if !self.handshake.confirmed {
                    return best;
                }
                d = d.saturating_add(self.backoff(self.config.max_ack_delay_ns));
            }
            let Some(sent) = s.time_of_last_ack_eliciting else {
                continue;
            };
            let t = sent.saturating_add(d);
            if best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, space));
            }
        }
        best
    }

    fn loss_time_and_space(&self) -> Option<(u64, PnSpace)> {
        PnSpace::ALL
            .into_iter()
            .filter_map(|sp| self.spaces[sp.index()].loss_time.map(|t| (t, sp)))
            .min_by_key(|(t, _)| *t)
    }

    /// A.8 `SetLossDetectionTimer`.
    fn set_timer(&mut self, now_ns: u64) {
        self.timer = if let Some((t, space)) = self.loss_time_and_space() {
            Some(Timer {
                deadline_ns: t,
                kind: TimerKind::Loss(space),
            })
        } else if self.handshake.at_amplification_limit {
            None
        } else {
            self.pto_time_and_space(now_ns).map(|(t, space)| Timer {
                deadline_ns: t,
                kind: TimerKind::Pto(space),
            })
        };
    }

    /// A.9 `OnLossDetectionTimeout`.
    pub fn on_timeout(
        &mut self,
        now_ns: u64,
        sink: &mut impl FnMut(CongestionEvent),
    ) -> TimeoutAction {
        let Some(timer) = self.timer else {
            return TimeoutAction::None;
        };
        if now_ns < timer.deadline_ns {
            return TimeoutAction::None;
        }
        if let Some((_, space)) = self.loss_time_and_space() {
            let batch = self.detect_lost(space, now_ns, sink);
            self.spaces[space.index()].pop_tombstones();
            self.set_timer(now_ns);
            return TimeoutAction::LossDetected {
                space,
                lost: batch.lost,
            };
        }
        let action = if self.spaces.iter().all(|s| s.ack_eliciting_in_flight == 0) {
            let space = if self.handshake.has_handshake_keys {
                PnSpace::Handshake
            } else {
                PnSpace::Initial
            };
            TimeoutAction::SendProbe {
                space,
                packets: 1,
                anti_deadlock: true,
            }
        } else {
            let space = self
                .pto_time_and_space(now_ns)
                .map_or(PnSpace::Initial, |(_, sp)| sp);
            TimeoutAction::SendProbe {
                space,
                packets: 2,
                anti_deadlock: false,
            }
        };
        self.pto_count = self.pto_count.saturating_add(1);
        self.set_timer(now_ns);
        action
    }

    /// RFC 9002 §6.4 / A.11: keys for `space` were discarded. In-flight
    /// bytes are removed without congestion events; returns them.
    pub fn discard_space(&mut self, space: PnSpace, now_ns: u64) -> u64 {
        let s = &mut self.spaces[space.index()];
        let mut removed = 0;
        for e in s.sent.drain(..) {
            if e.state == State::InFlight && e.pkt.in_flight {
                removed += u64::from(e.pkt.bytes);
            }
        }
        s.discarded = true;
        s.loss_time = None;
        s.time_of_last_ack_eliciting = None;
        s.ack_eliciting_in_flight = 0;
        self.bytes_in_flight -= removed;
        self.pto_count = 0;
        self.set_timer(now_ns);
        removed
    }
}
