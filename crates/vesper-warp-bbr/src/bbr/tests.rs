//! Unit tests for the state transitions and congestion responses. Some
//! tests set internal fields directly to reach a precondition; the
//! transitions themselves always run through the public event API.

use super::*;
use crate::events::AckedPacket;
use std::collections::VecDeque;

const MSS: u32 = 1200;
const MS: u64 = 1_000_000;

/// An ideal bottleneck (rate, fixed RTT, unbounded FIFO) with manual loss.
struct Pipe {
    bbr: Bbrv3,
    now: u64,
    pn: u64,
    rate: u64,
    rtt: u64,
    link_free: u64,
    queue: VecDeque<(SentPacket, PacketDeliveryState, u64)>,
    min_rtt: u64,
}

impl Pipe {
    fn new(rate: u64, rtt: u64) -> Self {
        Self::with(BbrConfig::quic(MSS), rate, rtt)
    }

    fn with(config: BbrConfig, rate: u64, rtt: u64) -> Self {
        Self {
            bbr: Bbrv3::new(config, 0).unwrap(),
            now: 0,
            pn: 0,
            rate,
            rtt,
            link_free: 0,
            queue: VecDeque::new(),
            min_rtt: u64::MAX,
        }
    }

    fn send(&mut self, guarded: bool) {
        let launch = self.bbr.schedule_edt_ns(MSS, self.now);
        self.now = self.now.max(launch);
        let packet = SentPacket {
            pn: self.pn,
            sent_ns: launch,
            bytes: MSS,
            ack_eliciting: true,
            in_flight: true,
        };
        self.pn += 1;
        let state = if guarded {
            self.bbr.on_packet_sent(self.now, packet).unwrap()
        } else {
            self.bbr.on_probe_packet_sent(self.now, packet).unwrap()
        };
        let start = self.link_free.max(self.now);
        self.link_free = start + (MSS as u64 * NANOS_PER_SEC).div_ceil(self.rate);
        self.queue
            .push_back((packet, state, self.link_free + self.rtt));
    }

    fn ack_next(&mut self) {
        let (packet, state, at) = self.queue.pop_front().unwrap();
        self.now = self.now.max(at);
        let latest = self.now - packet.sent_ns;
        self.min_rtt = self.min_rtt.min(latest);
        let rtt = RttSample {
            latest_ns: latest,
            min_ns: self.min_rtt,
            smoothed_ns: latest,
            ack_delay_ns: 0,
        };
        let d = [Delivered {
            packet: AckedPacket {
                pn: packet.pn,
                sent_ns: packet.sent_ns,
                bytes: packet.bytes,
            },
            state,
        }];
        self.bbr.on_ack(self.now, &d, Some(rtt)).unwrap();
    }

    fn lose_next(&mut self) {
        let (packet, state, _) = self.queue.pop_front().unwrap();
        let lost = LostPacket {
            pn: packet.pn,
            sent_ns: packet.sent_ns,
            bytes: packet.bytes,
        };
        self.bbr.on_packet_lost(self.now, lost, state).unwrap();
    }

    /// One event: send if the guard and EDT allow before the next ACK.
    fn step(&mut self) {
        let next_ack = self.queue.front().map(|q| q.2);
        let send_at = self
            .bbr
            .can_send()
            .then(|| self.now.max(self.bbr.next_edt_ns()));
        match (send_at, next_ack) {
            (Some(s), Some(a)) if s <= a => {
                self.now = s;
                self.send(true);
            }
            (Some(s), None) => {
                self.now = s;
                self.send(true);
            }
            (_, Some(_)) => self.ack_next(),
            (None, None) => panic!("deadlock"),
        }
    }

    fn run_until(&mut self, mut done: impl FnMut(&Bbrv3) -> bool, limit_ns: u64) -> bool {
        while self.now < limit_ns {
            self.step();
            if done(&self.bbr) {
                return true;
            }
        }
        false
    }

    fn reach_probe_bw(&mut self) {
        assert!(self.run_until(|b| b.state.is_probe_bw(), 5_000 * MS));
    }
}

const RATE: u64 = 1_250_000;
const RTT: u64 = 40 * MS;

#[test]
fn config_validation_and_initial_state() {
    let mut bad = BbrConfig::quic(MSS);
    bad.smss = 0;
    assert!(matches!(
        Bbrv3::new(bad, 0),
        Err(BbrError::InvalidConfig(_))
    ));
    bad.smss = 70_000;
    assert!(matches!(
        Bbrv3::new(bad, 0),
        Err(BbrError::InvalidConfig(_))
    ));
    let mut bad = BbrConfig::quic(MSS);
    bad.initial_cwnd = MSS - 1;
    assert!(Bbrv3::new(bad, 0).is_err());

    let c = BbrConfig::quic(MSS);
    assert_eq!(c.initial_cwnd, 12_000);
    let b = Bbrv3::new(c, 7).unwrap();
    assert_eq!(b.state(), BbrState::Startup);
    assert_eq!(b.pacing_gain(), STARTUP_PACING_GAIN);
    assert_eq!(b.cwnd_gain(), DEFAULT_CWND_GAIN);
    assert_eq!(b.cwnd_bytes(), 12_000);
    // InitPacingRate: 2.77 * InitialCwnd / 1 ms.
    assert_eq!(b.pacing_rate_bytes_per_sec(), 12_000 * 1000 * 2770 / 1000);
    assert_eq!(b.inflight_hi(), None);
    assert_eq!(b.min_rtt_ns(), None);
    assert!(b.can_send());
}

#[test]
fn first_rtt_sample_reinitialises_pacing_from_srtt() {
    let mut p = Pipe::new(RATE, RTT);
    p.send(true);
    p.ack_next();
    // nominal = 12000 B / 40.96 ms, times 2.77 (or the first bw sample if
    // larger); never the 1 ms placeholder.
    assert!(p.bbr.pacing_rate_bytes_per_sec() < 12_000 * 1000 * 2770 / 1000);
}

#[test]
fn rate_sample_is_delivered_over_interval() {
    let mut p = Pipe::new(RATE, RTT);
    for _ in 0..4 {
        p.send(true);
    }
    for _ in 0..4 {
        p.ack_next();
    }
    let rs = p.bbr.last_rate_sample();
    assert_eq!(rs.newly_acked, 1200);
    assert!(rs.interval_ns > 0);
    assert_eq!(
        rs.delivery_rate,
        (rs.delivered as u128 * 1_000_000_000 / rs.interval_ns as u128) as u64
    );
    assert_eq!(p.bbr.delivered_bytes(), 4800);
    assert_eq!(p.bbr.bytes_in_flight(), 0);
}

#[test]
fn full_pipe_detector_needs_three_rounds_without_25pct_growth() {
    let mut b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    b.round_start = true;
    for (rate, count, reached) in [
        (100, 0, false),
        (124, 1, false), // < 1.25x: no growth
        (125, 0, false), // >= 1.25x of 100: new baseline, reset
        (150, 1, false),
        (150, 2, false),
        (156, 3, true),
    ] {
        b.rs.delivery_rate = rate;
        b.check_full_bw_reached();
        assert_eq!(
            (b.full_bw_count, b.full_bw_reached),
            (count, reached),
            "rate {rate}"
        );
    }
    // App-limited samples and non-round-start ACKs are ignored.
    let mut c = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    c.round_start = true;
    c.rs.delivery_rate = 10;
    c.check_full_bw_reached();
    c.rs.is_app_limited = true;
    for _ in 0..5 {
        c.check_full_bw_reached();
    }
    c.rs.is_app_limited = false;
    c.round_start = false;
    for _ in 0..5 {
        c.check_full_bw_reached();
    }
    assert!(!c.full_bw_reached);
}

#[test]
fn startup_to_drain_to_probe_bw_on_bandwidth_plateau() {
    let mut p = Pipe::new(RATE, RTT);
    assert!(p.run_until(|b| b.state() != BbrState::Startup, 2_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::Drain);
    assert!(p.bbr.full_bw_reached());
    assert_eq!(p.bbr.pacing_gain(), DRAIN_PACING_GAIN);
    assert_eq!(p.bbr.undo_state, None, "plateau exit, not loss");
    let drain_round = p.bbr.round_count();
    assert!(p.run_until(|b| b.state() != BbrState::Drain, 2_000 * MS));
    assert!(p.bbr.state().is_probe_bw());
    assert!(p.bbr.round_count() <= drain_round + 4);
}

#[test]
fn drain_exits_after_three_rounds_even_if_inflight_stays_high() {
    let mut p = Pipe::new(RATE, RTT);
    assert!(p.run_until(|b| b.state() == BbrState::Drain, 2_000 * MS));
    // Keep a large standing flight: one extra (probe) send per ACK.
    for _ in 0..200 {
        p.send(false);
    }
    let start = p.bbr.drain_start_round;
    while p.bbr.state() == BbrState::Drain {
        p.ack_next();
        p.send(false);
        assert!(p.now < 10_000 * MS);
    }
    assert!(p.bbr.round_count() > start + 3);
    assert!(p.bbr.bytes_in_flight() > p.bbr.bdp_bytes());
}

#[test]
fn probe_bw_cycle_down_cruise_refill_up_down() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    // Force a fresh DOWN with a standing queue so the DOWN -> CRUISE exit
    // is observable.
    for _ in 0..30 {
        p.send(false);
    }
    p.bbr.start_probe_bw_down(p.now);
    assert_eq!(p.bbr.pacing_gain(), PROBE_DOWN_PACING_GAIN);
    assert!(p.run_until(|b| b.state() != BbrState::ProbeBwDown, 20_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::ProbeBwCruise);
    assert!(p.bbr.bytes_in_flight() <= p.bbr.inflight_with_gain(UNIT_PACING_GAIN));
    assert_eq!(p.bbr.pacing_gain(), UNIT_PACING_GAIN);

    let cruise_at = p.now;
    assert!(p.run_until(|b| b.state() != BbrState::ProbeBwCruise, 20_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::ProbeBwRefill);
    assert_eq!(p.bbr.bw_lo(), None, "REFILL resets the short-term model");
    // Either the randomized 2-3 s wall-clock wait or the Reno-coexistence
    // round bound (BDP in packets, here ~42 rounds ≈ 1.7 s) fired.
    assert!(p.now - cruise_at <= 3_100 * MS);

    let refill_round = p.bbr.round_count();
    assert!(p.run_until(|b| b.state() != BbrState::ProbeBwRefill, 20_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::ProbeBwUp);
    assert_eq!(
        p.bbr.round_count(),
        refill_round + 1,
        "REFILL lasts one round"
    );
    assert!(p.bbr.check_probe_loss);
    assert_eq!(
        (p.bbr.pacing_gain(), p.bbr.cwnd_gain()),
        (PROBE_UP_PACING_GAIN, PROBE_UP_CWND_GAIN)
    );

    assert!(p.run_until(|b| b.state() != BbrState::ProbeBwUp, 20_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::ProbeBwDown);
    assert!(!p.bbr.prev_probe_too_high, "plateau exit from UP");
    // One round after leaving UP the bw filter advances a cycle.
    let cycles = p.bbr.cycle_count();
    assert!(p.run_until(|b| b.cycle_count() > cycles, 20_000 * MS));
}

#[test]
fn probe_rtt_entered_after_min_rtt_window_and_exits_to_cruise() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    let stamp = p.bbr.min_rtt_stamp_ns;
    let saved_cwnd = p.bbr.cwnd;
    assert!(p.run_until(|b| b.state() == BbrState::ProbeRtt, 20_000 * MS));
    assert!(p.now > stamp + MIN_RTT_FILTER_LEN_NS);
    assert_eq!(p.bbr.cwnd_gain(), PROBE_RTT_CWND_GAIN);
    assert_eq!(p.bbr.pacing_gain(), UNIT_PACING_GAIN);
    assert!(p.bbr.prior_cwnd >= saved_cwnd.min(p.bbr.prior_cwnd));
    let entered = p.now;
    assert!(p.run_until(|b| b.state() != BbrState::ProbeRtt, 30_000 * MS));
    assert!(p.now - entered >= PROBE_RTT_DURATION_NS);
    assert_eq!(p.bbr.state(), BbrState::ProbeBwCruise);
    assert_eq!(p.bbr.min_rtt_stamp_ns, p.now, "next ProbeRTT scheduled");
    assert_eq!(p.bbr.inflight_lo(), None, "short-term model reset");
}

#[test]
fn probe_rtt_bounds_cwnd_to_half_bdp() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    assert!(p.run_until(|b| b.state() == BbrState::ProbeRtt, 20_000 * MS));
    p.step();
    let bound = p.bbr.probe_rtt_cwnd_bound();
    assert!(p.bbr.cwnd <= bound);
    assert!(bound <= p.bbr.bdp_bytes() / 2 + 4 * MSS as u64);
}

#[test]
fn probe_rtt_exits_to_startup_if_pipe_never_filled() {
    let mut b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    b.state = BbrState::ProbeRtt;
    b.probe_rtt_done_stamp_ns = Some(100);
    b.check_probe_rtt_done(101);
    assert_eq!(b.state(), BbrState::Startup);
}

#[test]
fn startup_exits_on_high_loss_and_spurious_undo_restores_startup() {
    let mut p = Pipe::new(RATE, RTT);
    // A few rounds of Startup growth.
    assert!(p.run_until(|b| b.round_count() >= 3, 2_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::Startup);
    // A large flight A; the first loss opens a loss round, a later flight B
    // is sent, then more of A is declared lost while B is in flight. When B
    // is ACKed the loss round ends with 8 loss events (> 6) and RS.lost
    // (losses since B was sent) above 2% of B's tx_in_flight.
    for _ in 0..40 {
        p.send(false);
    }
    p.ack_next();
    p.lose_next();
    for _ in 0..10 {
        p.send(false);
    }
    for _ in 0..7 {
        p.lose_next();
    }
    // ACK until the loss round ends: CheckStartupHighLoss fires.
    assert!(p.run_until(|b| b.state() != BbrState::Startup, 2_000 * MS));
    assert_eq!(p.bbr.state(), BbrState::Drain);
    assert_eq!(p.bbr.undo_state, Some(BbrState::Startup));
    let hi = p.bbr.inflight_hi().expect("inflight_hi from loss");
    assert!(hi >= p.bbr.inflight_latest.min(hi));

    p.bbr.on_spurious_loss_detected(p.now).unwrap();
    assert_eq!(p.bbr.state(), BbrState::Startup);
    assert!(!p.bbr.full_bw_reached());
    assert!(!p.bbr.in_recovery());
    // inflight_hi restored to at least the pre-loss value (Infinity).
    assert_eq!(p.bbr.inflight_hi(), None);
}

#[test]
fn startup_ignores_too_few_loss_events_with_selective_acks() {
    let mut p = Pipe::new(RATE, RTT);
    assert!(p.run_until(|b| b.round_count() >= 3, 2_000 * MS));
    for _ in 0..40 {
        p.send(false);
    }
    p.ack_next();
    p.lose_next();
    for _ in 0..10 {
        p.send(false);
    }
    for _ in 0..2 {
        p.lose_next();
    }
    let r = p.bbr.round_count();
    assert!(p.run_until(|b| b.round_count() >= r + 2, 2_000 * MS));
    assert!(p.bbr.undo_state != Some(BbrState::Startup));
}

#[test]
fn without_selective_acks_any_recovery_loss_exits_startup() {
    let mut c = BbrConfig::quic(MSS);
    c.has_selective_acks = false;
    c.offload = OffloadModel::Tcp;
    let mut p = Pipe::with(c, RATE, RTT);
    assert!(p.run_until(|b| b.round_count() >= 3, 2_000 * MS));
    for _ in 0..20 {
        p.send(false);
    }
    p.ack_next();
    p.lose_next();
    for _ in 0..5 {
        p.send(false);
    }
    p.lose_next();
    assert!(p.run_until(|b| b.state() != BbrState::Startup, 2_000 * MS));
    assert_eq!(p.bbr.undo_state, Some(BbrState::Startup));
}

fn into_probe_up(p: &mut Pipe) {
    p.reach_probe_bw();
    assert!(p.run_until(|b| b.state() == BbrState::ProbeBwUp, 20_000 * MS));
}

#[test]
fn loss_above_threshold_in_probe_up_sets_inflight_hi_and_goes_down() {
    let mut p = Pipe::new(RATE, RTT);
    into_probe_up(&mut p);
    assert_eq!(p.bbr.inflight_hi(), None);
    // Grow the flight then lose 3 of ~50 packets (6% > 2%).
    for _ in 0..10 {
        p.send(false);
    }
    let target = p.bbr.target_inflight();
    for _ in 0..3 {
        p.lose_next();
        if p.bbr.state() != BbrState::ProbeBwUp {
            break;
        }
    }
    assert_eq!(p.bbr.state(), BbrState::ProbeBwDown);
    assert!(p.bbr.prev_probe_too_high);
    assert!(!p.bbr.check_probe_loss, "react once per probe");
    assert_eq!(p.bbr.undo_state, Some(BbrState::ProbeBwUp));
    let hi = p.bbr.inflight_hi().expect("set");
    assert!(hi >= mul_gain(target, BETA));

    // Spurious: back to REFILL, inflight_hi restored to Infinity.
    p.bbr.on_spurious_loss_detected(p.now).unwrap();
    assert_eq!(p.bbr.state(), BbrState::ProbeBwRefill);
    assert_eq!(p.bbr.inflight_hi(), None);
}

#[test]
fn loss_at_or_below_threshold_does_not_cut_inflight_hi() {
    let b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    // Exactly 2% is not "too high"; one byte more is.
    assert!(!b.is_inflight_too_high(1200, 60_000));
    assert!(b.is_inflight_too_high(1201, 60_000));
    assert!(!b.is_inflight_too_high(0, 0));
    let mut p = Pipe::new(RATE, RTT);
    into_probe_up(&mut p);
    for _ in 0..80 {
        p.send(false);
    }
    // One loss of a packet sent with >= 60 packets in flight is <= 2%.
    let (pkt, st, _) = p.queue.pop_back().unwrap();
    assert!(
        1200 * 1000 <= st.tx_in_flight * 20,
        "tx_in_flight {}",
        st.tx_in_flight
    );
    let lost = LostPacket {
        pn: pkt.pn,
        sent_ns: pkt.sent_ns,
        bytes: pkt.bytes,
    };
    p.bbr.on_packet_lost(p.now, lost, st).unwrap();
    assert_eq!(p.bbr.state(), BbrState::ProbeBwUp);
    assert_eq!(p.bbr.inflight_hi(), None);
}

#[test]
fn loss_while_cruising_cuts_short_term_model_by_beta_once_per_round() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    assert!(p.run_until(|b| b.state() == BbrState::ProbeBwCruise, 20_000 * MS));
    let max_bw = p.bbr.max_bw();
    p.lose_next();
    assert!(p.bbr.is_loss_in_round);
    assert!(p.run_until(|b| b.bw_lo().is_some(), 5_000 * MS));
    let bw_lo = p.bbr.bw_lo().unwrap();
    // InitLowerBounds then LossLowerBounds: max(bw_latest, 0.7 * max_bw).
    assert!(bw_lo >= mul_gain(max_bw, BETA) && bw_lo <= max_bw);
    assert!(p.bbr.inflight_lo().is_some());
    assert!(p.bbr.bw() <= bw_lo);
    assert!(!p.bbr.is_loss_in_round, "consumed at round end");
}

#[test]
fn inflight_hi_grows_in_probe_up_when_cwnd_limited() {
    let mut b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    b.state = BbrState::ProbeBwUp;
    b.cwnd = 100_000;
    b.inflight_longterm = 100_000;
    b.is_cwnd_limited = true;
    b.bw_probe_up_rounds = 0;
    b.raise_inflight_longterm_slope();
    assert_eq!(b.probe_up_acked_per_inc, 100_000);
    b.rs.newly_acked = 250_000;
    b.round_start = true;
    b.probe_inflight_longterm_upward();
    assert_eq!(b.inflight_longterm, 100_000 + 2 * 1200);
    assert_eq!(b.bw_probe_up_acked, 50_000);
    // Slope doubles each round: per-increment volume halves.
    assert_eq!(b.probe_up_acked_per_inc, 50_000);
    // Not cwnd-limited: no growth.
    b.is_cwnd_limited = false;
    b.probe_inflight_longterm_upward();
    assert_eq!(b.inflight_longterm, 100_000 + 2 * 1200);
}

#[test]
fn precautionary_probe_stops_up_at_inflight_hi() {
    let mut b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    b.prev_probe_too_high = true;
    b.inflight_longterm = 10_000;
    b.inflight = 10_000;
    assert!(b.is_time_to_go_down());
    assert!(b.prev_probe_precautionary);
    // And if the precautionary probe saw no excess loss, the end of its
    // feedback round goes straight back to REFILL.
    let mut c = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    c.state = BbrState::ProbeBwDown;
    c.is_bw_probe_stopping = true;
    c.round_start = true;
    c.prev_probe_precautionary = true;
    c.prev_probe_too_high = false;
    assert!(c.adapt_long_term_model(0));
    assert_eq!(c.state(), BbrState::ProbeBwRefill);
}

#[test]
fn ecn_ce_above_threshold_in_probe_up_goes_down() {
    let mut p = Pipe::new(RATE, RTT);
    into_probe_up(&mut p);
    p.bbr.on_ecn_ce(p.now, 5).unwrap();
    p.ack_next();
    assert_eq!(p.bbr.state(), BbrState::ProbeBwDown);
    assert!(p.bbr.prev_probe_too_high);
    assert!(p.bbr.inflight_hi().is_some());
    assert_eq!(p.bbr.lost_bytes(), 0, "CE is not loss");
}

#[test]
fn ecn_ce_while_cruising_cuts_short_term_model() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    assert!(p.run_until(|b| b.state() == BbrState::ProbeBwCruise, 20_000 * MS));
    p.bbr.on_ecn_ce(p.now, 1).unwrap();
    assert!(p.run_until(|b| b.bw_lo().is_some(), 5_000 * MS));
}

#[test]
fn persistent_congestion_collapses_cwnd_then_floors_at_min_pipe() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    let inflight = p.bbr.bytes_in_flight();
    p.bbr.on_persistent_congestion(p.now).unwrap();
    assert_eq!(p.bbr.cwnd, inflight + MSS as u64);
    assert!(p.bbr.in_recovery());
    // Losing everything outstanding and ACKing new data restores cwnd to
    // at least the pipelining floor and, on recovery exit, the saved cwnd.
    while !p.queue.is_empty() {
        p.lose_next();
    }
    p.send(false);
    p.ack_next();
    assert!(p.bbr.cwnd >= 4 * MSS as u64);
    assert!(!p.bbr.in_recovery());
}

#[test]
fn app_limited_samples_do_not_lower_max_bw_and_marker_clears() {
    let mut p = Pipe::new(RATE, RTT);
    p.reach_probe_bw();
    while !p.queue.is_empty() {
        p.ack_next();
    }
    let max_bw = p.bbr.max_bw();
    // A slow, app-limited trickle: low rate samples flagged app-limited.
    for _ in 0..5 {
        p.now += 100 * MS;
        p.bbr.on_app_limited(p.now).unwrap();
        p.send(true);
        p.ack_next();
        assert!(p.bbr.last_rate_sample().is_app_limited);
        assert!(p.bbr.max_bw() >= max_bw);
    }
    // Restart from idle in ProbeBW paces at exactly bw * 0.99.
    if p.bbr.state().is_probe_bw() {
        p.bbr.on_app_limited(p.now).unwrap();
        p.send(true);
        assert_eq!(
            p.bbr.pacing_rate_bytes_per_sec(),
            (p.bbr.bw() as u128 * 99 / 100) as u64
        );
        p.ack_next();
    }
    // Once delivery passes the marker, it clears.
    p.bbr.app_limited = p.bbr.delivered + 1;
    p.send(true);
    p.ack_next();
    assert!(!p.bbr.is_app_limited());
}

#[test]
fn inflight_at_loss_solves_for_the_threshold_crossing() {
    // 100 packets in flight before this one, none lost before: the 2%
    // crossing is at 100_000 + (0.02 * 100_000) / 0.98.
    assert_eq!(
        inflight_at_loss(1000, 101_000, 1000),
        100_000 + 2_000_000 / 980
    );
    // Already above threshold before this packet: crossing at inflight_prev.
    assert_eq!(inflight_at_loss(1000, 11_000, 6000), 10_000);
    // Degenerate sizes saturate at zero.
    assert_eq!(inflight_at_loss(5000, 1000, 0), 0);
}

#[test]
fn target_cwnd_bytes_respects_clamp() {
    let mut b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    b.cwnd = 1000;
    assert_eq!(b.target_cwnd_bytes(MSS), 4800);
    b.cwnd = 1_000_000;
    assert_eq!(b.target_cwnd_bytes(MSS), 1_000_000);
    b.inflight_longterm = 60_000;
    assert_eq!(b.target_cwnd_bytes(MSS), 60_000);
}

#[test]
fn rejected_inputs_leave_state_unchanged() {
    let mut p = Pipe::new(RATE, RTT);
    for _ in 0..3 {
        p.send(true);
    }
    let snapshot = format!("{:?}", p.bbr);
    let (pkt, st, _) = *p.queue.front().unwrap();
    let ack = |pn: u64, sent: u64, bytes: u32, state| Delivered {
        packet: AckedPacket {
            pn,
            sent_ns: sent,
            bytes,
        },
        state,
    };
    let now = p.now + RTT;
    let b = &mut p.bbr;
    assert!(matches!(
        b.on_ack(now - RTT - 1, &[ack(pkt.pn, pkt.sent_ns, MSS, st)], None),
        Err(BbrError::TimeWentBackwards { .. })
    ));
    assert!(matches!(
        b.on_ack(now, &[ack(MAX_PACKET_NUMBER + 1, 0, MSS, st)], None),
        Err(BbrError::PacketNumberOutOfRange { .. })
    ));
    assert!(matches!(
        b.on_ack(now, &[ack(pkt.pn, now + 1, MSS, st)], None),
        Err(BbrError::AckedBeforeSent { .. })
    ));
    assert!(matches!(
        b.on_ack(now, &[ack(pkt.pn, pkt.sent_ns, 10 * MSS, st)], None),
        Err(BbrError::InflightUnderflow { .. })
    ));
    let mut forged = st;
    forged.delivered = 1;
    assert!(matches!(
        b.on_ack(now, &[ack(pkt.pn, pkt.sent_ns, MSS, forged)], None),
        Err(BbrError::ForeignSnapshot { .. })
    ));
    let mut forged = st;
    forged.lost = 1;
    assert!(matches!(
        b.on_packet_lost(
            now,
            LostPacket {
                pn: pkt.pn,
                sent_ns: pkt.sent_ns,
                bytes: MSS
            },
            forged
        ),
        Err(BbrError::ForeignSnapshot { .. })
    ));
    let empty = SentPacket {
        pn: 9,
        sent_ns: now,
        bytes: 0,
        ack_eliciting: true,
        in_flight: true,
    };
    assert!(matches!(
        b.on_packet_sent(now, empty),
        Err(BbrError::EmptyInFlightPacket { .. })
    ));
    assert_eq!(snapshot, format!("{:?}", p.bbr));
    // Fill the window: the guard refuses the next send.
    let mut q = Pipe::new(RATE, RTT);
    while q.bbr.can_send() {
        q.send(true);
    }
    let before = format!("{:?}", q.bbr);
    let next = SentPacket {
        pn: q.pn,
        sent_ns: q.now,
        bytes: MSS,
        ack_eliciting: true,
        in_flight: true,
    };
    assert!(matches!(
        q.bbr.on_packet_sent(q.now, next),
        Err(BbrError::CwndFull { .. })
    ));
    assert_eq!(before, format!("{:?}", q.bbr));
    assert!(q.bbr.bytes_in_flight() < q.bbr.cwnd + MSS as u64);
}

#[test]
fn pure_ack_packets_never_count() {
    let mut b = Bbrv3::new(BbrConfig::quic(MSS), 0).unwrap();
    let ack_only = SentPacket {
        pn: 1,
        sent_ns: 10,
        bytes: 50,
        ack_eliciting: false,
        in_flight: false,
    };
    let st = b.on_packet_sent(10, ack_only).unwrap();
    assert!(!st.counted);
    assert_eq!(b.bytes_in_flight(), 0);
    let d = [Delivered {
        packet: AckedPacket {
            pn: 1,
            sent_ns: 10,
            bytes: 50,
        },
        state: st,
    }];
    b.on_ack(20, &d, None).unwrap();
    assert_eq!(b.delivered_bytes(), 0);
    b.on_packet_lost(
        30,
        LostPacket {
            pn: 1,
            sent_ns: 10,
            bytes: 50,
        },
        st,
    )
    .unwrap();
    assert_eq!(b.lost_bytes(), 0);
}

#[test]
fn contract_event_dispatch() {
    let mut p = Pipe::new(RATE, RTT);
    p.send(true);
    let (pkt, st, at) = p.queue.pop_front().unwrap();
    let d = [Delivered {
        packet: AckedPacket {
            pn: pkt.pn,
            sent_ns: pkt.sent_ns,
            bytes: pkt.bytes,
        },
        state: st,
    }];
    p.bbr
        .on_event(at, CongestionEvent::EcnCe { count: 1 })
        .unwrap();
    p.bbr
        .on_event(
            at,
            CongestionEvent::Acked {
                packets: &d,
                rtt: None,
            },
        )
        .unwrap();
    p.bbr.on_event(at, CongestionEvent::AppLimited).unwrap();
    p.bbr
        .on_event(at, CongestionEvent::PersistentCongestion)
        .unwrap();
    assert_eq!(p.bbr.delivered_bytes(), MSS as u64);
    assert!(p.bbr.in_recovery());
}
