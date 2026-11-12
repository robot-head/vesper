//! RFC 9002 scripted traces: packet/time-threshold loss, PTO with backoff,
//! persistent congestion, RTT/ack-delay rules and typed errors.

use vesper_warp::PnSpace;
use vesper_warp::frame::{AckFrame, AckRange, EcnCounts};
use vesper_warp::recovery::{
    CongestionEvent, HandshakeStatus, LossRecovery, RecoveryConfig, RecoveryError, RttEstimator,
    SentPacket, TimeoutAction, TimerKind, ack_delay_ns,
};

const MS: u64 = 1_000_000;
const APP: PnSpace = PnSpace::ApplicationData;

fn confirmed() -> HandshakeStatus {
    HandshakeStatus {
        confirmed: true,
        peer_completed_address_validation: true,
        has_handshake_keys: true,
        at_amplification_limit: false,
    }
}

fn pkt(pn: u64, sent_ns: u64) -> SentPacket {
    SentPacket {
        pn,
        sent_ns,
        bytes: 1000,
        ack_eliciting: true,
        in_flight: true,
    }
}

fn ack(range: &[AckRange]) -> AckFrame<'_> {
    AckFrame::from_ranges(range, 0, None).unwrap()
}

fn single(smallest: u64, largest: u64) -> [AckRange; 1] {
    [AckRange { smallest, largest }]
}

#[derive(Default)]
struct Events {
    acked: Vec<u64>,
    lost: Vec<u64>,
    ce: u64,
    persistent: bool,
    order: Vec<char>,
}

impl Events {
    fn sink(&mut self) -> impl FnMut(CongestionEvent) + '_ {
        |e| match e {
            CongestionEvent::Acked { packet } => {
                self.acked.push(packet.pn);
                self.order.push('a');
            }
            CongestionEvent::Lost { packet } => {
                self.lost.push(packet.pn);
                self.order.push('l');
            }
            CongestionEvent::EcnCe { count } => self.ce += count,
            CongestionEvent::PersistentCongestion => self.persistent = true,
            CongestionEvent::AppLimited => unreachable!("recovery never emits AppLimited"),
        }
    }
}

#[test]
fn packet_threshold_then_time_threshold() {
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    for pn in 0..10 {
        r.on_packet_sent(APP, pkt(pn, pn * MS)).unwrap();
    }
    assert_eq!(r.bytes_in_flight(), 10_000);
    let mut ev = Events::default();
    let range = single(9, 9);
    let out = r
        .on_ack_received(APP, &ack(&range), 0, 50 * MS, &mut ev.sink())
        .unwrap();
    // latest_rtt = 41 ms, loss_delay = 9/8 · 41 ms = 46.125 ms.
    let rtt = out.rtt_sample.unwrap();
    assert_eq!(rtt.latest_ns, 41 * MS);
    assert_eq!(r.rtt().loss_delay_ns(), 46_125_000);
    // pn ≤ 6 lost by kPacketThreshold; 7 and 8 sent after 50 − 46.125 ms.
    assert_eq!(ev.lost, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(ev.acked, [9]);
    assert_eq!(ev.order.last(), Some(&'a'), "lost before acked (RFC order)");
    assert!(!ev.persistent, "losses sent before the first RTT sample");
    assert_eq!(out.lost, 7);
    assert_eq!(r.bytes_in_flight(), 2000);
    let timer = r.timer().unwrap();
    assert_eq!(timer.kind, TimerKind::Loss(APP));
    assert_eq!(timer.deadline_ns, 7 * MS + 46_125_000);

    let mut ev = Events::default();
    assert_eq!(
        r.on_timeout(timer.deadline_ns - 1, &mut ev.sink()),
        TimeoutAction::None
    );
    assert_eq!(
        r.on_timeout(timer.deadline_ns, &mut ev.sink()),
        TimeoutAction::LossDetected {
            space: APP,
            lost: 1
        }
    );
    assert_eq!(ev.lost, [7]);
    assert_eq!(r.timer().unwrap().deadline_ns, 8 * MS + 46_125_000);
    r.on_timeout(8 * MS + 46_125_000, &mut ev.sink());
    assert_eq!(ev.lost, [7, 8]);
    assert_eq!(r.bytes_in_flight(), 0);
    assert_eq!(r.timer(), None, "nothing in flight, address validated");
}

#[test]
fn time_threshold_alone() {
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    r.on_packet_sent(APP, pkt(1, 10 * MS)).unwrap();
    let mut ev = Events::default();
    let range = single(1, 1);
    r.on_ack_received(APP, &ack(&range), 0, 30 * MS, &mut ev.sink())
        .unwrap();
    // rtt 20 ms → loss_delay 22.5 ms → packets sent ≤ 7.5 ms are lost.
    assert_eq!(ev.lost, [0]);
    assert_eq!(ev.acked, [1]);
}

#[test]
fn pto_backoff_and_reset() {
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    // No RTT sample: srtt 333 ms, rttvar 166.5 ms, + max_ack_delay 25 ms.
    let base = 333 * MS + 666 * MS;
    let t = r.timer().unwrap();
    assert_eq!(t.kind, TimerKind::Pto(APP));
    assert_eq!(t.deadline_ns, base + 25 * MS);
    let mut ev = Events::default();
    assert_eq!(
        r.on_timeout(t.deadline_ns, &mut ev.sink()),
        TimeoutAction::SendProbe {
            space: APP,
            packets: 2,
            anti_deadlock: false
        }
    );
    assert_eq!(r.pto_count(), 1);
    assert_eq!(r.timer().unwrap().deadline_ns, 2 * (base + 25 * MS));
    r.on_timeout(2 * (base + 25 * MS), &mut ev.sink());
    assert_eq!(r.pto_count(), 2);
    assert_eq!(r.timer().unwrap().deadline_ns, 4 * (base + 25 * MS));
    assert!(ev.lost.is_empty(), "PTO declares nothing lost");
    let range = single(0, 0);
    r.on_ack_received(APP, &ack(&range), 0, 5000 * MS, &mut ev.sink())
        .unwrap();
    assert_eq!(r.pto_count(), 0);
}

#[test]
fn application_pto_waits_for_handshake_confirmation_and_anti_deadlock() {
    let client = HandshakeStatus::default();
    let mut r = LossRecovery::new(RecoveryConfig::default(), client);
    // Nothing in flight, address not validated: anti-deadlock PTO in Initial.
    r.set_handshake_status(client, 0);
    let t = r.timer().unwrap();
    assert_eq!(t.kind, TimerKind::Pto(PnSpace::Initial));
    let mut ev = Events::default();
    assert_eq!(
        r.on_timeout(t.deadline_ns, &mut ev.sink()),
        TimeoutAction::SendProbe {
            space: PnSpace::Initial,
            packets: 1,
            anti_deadlock: true
        }
    );
    // An application packet alone does not arm a PTO before confirmation.
    let mut r = LossRecovery::new(
        RecoveryConfig::default(),
        HandshakeStatus {
            peer_completed_address_validation: true,
            ..HandshakeStatus::default()
        },
    );
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    assert_eq!(r.timer(), None);
    r.on_packet_sent(PnSpace::Handshake, pkt(0, 1)).unwrap();
    assert_eq!(r.timer().unwrap().kind, TimerKind::Pto(PnSpace::Handshake));
    // At the amplification limit the timer is cancelled.
    r.set_handshake_status(
        HandshakeStatus {
            peer_completed_address_validation: true,
            at_amplification_limit: true,
            ..HandshakeStatus::default()
        },
        2,
    );
    assert_eq!(r.timer(), None);
}

fn sampled(config: RecoveryConfig) -> LossRecovery {
    let mut r = LossRecovery::new(config, confirmed());
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    let range = single(0, 0);
    r.on_ack_received(APP, &ack(&range), 0, 10 * MS, &mut |_| {})
        .unwrap();
    r
}

#[test]
fn persistent_congestion_detected() {
    let mut r = sampled(RecoveryConfig::default());
    // srtt 10 ms, rttvar 5 ms: (10 + 20 + 25) · 3 = 165 ms.
    assert_eq!(r.persistent_congestion_duration_ns(), 165 * MS);
    for pn in 1..=20 {
        r.on_packet_sent(APP, pkt(pn, pn * 20 * MS)).unwrap();
    }
    r.on_packet_sent(APP, pkt(21, 410 * MS)).unwrap();
    let mut ev = Events::default();
    let range = single(21, 21);
    let out = r
        .on_ack_received(APP, &ack(&range), 0, 420 * MS, &mut ev.sink())
        .unwrap();
    assert_eq!(ev.lost, (1..=20).collect::<Vec<_>>());
    assert!(out.persistent_congestion && ev.persistent);
}

#[test]
fn persistent_congestion_needs_the_full_period_and_no_ack_between() {
    // Span 100 ms < 165 ms.
    let mut r = sampled(RecoveryConfig::default());
    for pn in 1..=3 {
        r.on_packet_sent(APP, pkt(pn, (20 + (pn - 1) * 50) * MS))
            .unwrap();
    }
    for pn in 4..=7 {
        r.on_packet_sent(APP, pkt(pn, (130 + pn) * MS)).unwrap();
    }
    let mut ev = Events::default();
    let range = single(7, 7);
    r.on_ack_received(APP, &ack(&range), 0, 140 * MS, &mut ev.sink())
        .unwrap();
    assert!(ev.lost.contains(&1) && ev.lost.contains(&3));
    assert!(!ev.persistent);

    // A long span with an acknowledged packet in the middle.
    let mut r = sampled(RecoveryConfig::default());
    for pn in 1..=20 {
        r.on_packet_sent(APP, pkt(pn, pn * 20 * MS)).unwrap();
    }
    r.on_packet_sent(APP, pkt(21, 410 * MS)).unwrap();
    let mut ev = Events::default();
    let ranges = [
        AckRange {
            smallest: 21,
            largest: 21,
        },
        AckRange {
            smallest: 10,
            largest: 10,
        },
    ];
    r.on_ack_received(APP, &ack(&ranges), 0, 420 * MS, &mut ev.sink())
        .unwrap();
    // Losses 1..=9 span 160 ms and 11..=20 span 180 ms but are split by 10.
    assert_eq!(ev.lost.len(), 19);
    assert!(ev.persistent, "11..=20 alone spans 180 ms ≥ 165 ms");

    let mut r = sampled(RecoveryConfig::default());
    for pn in 1..=16 {
        r.on_packet_sent(APP, pkt(pn, pn * 20 * MS)).unwrap();
    }
    r.on_packet_sent(APP, pkt(17, 330 * MS)).unwrap();
    let mut ev = Events::default();
    let ranges = [
        AckRange {
            smallest: 17,
            largest: 17,
        },
        AckRange {
            smallest: 8,
            largest: 8,
        },
    ];
    r.on_ack_received(APP, &ack(&ranges), 0, 340 * MS, &mut ev.sink())
        .unwrap();
    // 1..=7 span 120 ms, 9..=16 span 140 ms: neither reaches 165 ms.
    assert_eq!(ev.lost.len(), 15);
    assert!(!ev.persistent);
}

#[test]
fn rtt_estimator_and_ack_delay_rules() {
    let mut e = RttEstimator::new(333 * MS);
    e.update(100 * MS, 0);
    assert_eq!(
        (e.min_ns(), e.smoothed_ns(), e.rttvar_ns()),
        (100 * MS, 100 * MS, 50 * MS)
    );
    // ack_delay subtracted only when latest ≥ min + delay.
    e.update(120 * MS, 10 * MS);
    assert_eq!(e.smoothed_ns(), (7 * 100 * MS + 110 * MS) / 8);
    assert_eq!(e.rttvar_ns(), (3 * 50 * MS + 10 * MS) / 4);
    let before = e.smoothed_ns();
    e.update(105 * MS, 50 * MS);
    assert_eq!(
        e.smoothed_ns(),
        (7 * before + 105 * MS) / 8,
        "delay not subtracted"
    );
    e.update(u64::MAX, u64::MAX);
    assert!(e.smoothed_ns() > 0, "no overflow on extreme samples");

    // Initial ignores ack delay; confirmed handshake caps it.
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    r.on_packet_sent(PnSpace::Initial, pkt(0, 0)).unwrap();
    r.on_packet_sent(PnSpace::Handshake, pkt(0, 0)).unwrap();
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    let range = single(0, 0);
    let s = r
        .on_ack_received(PnSpace::Initial, &ack(&range), 5 * MS, 50 * MS, &mut |_| {})
        .unwrap()
        .rtt_sample
        .unwrap();
    assert_eq!(s.ack_delay_ns, 0);
    let s = r
        .on_ack_received(APP, &ack(&range), 500 * MS, 60 * MS, &mut |_| {})
        .unwrap()
        .rtt_sample
        .unwrap();
    assert_eq!(s.ack_delay_ns, 25 * MS, "capped at max_ack_delay");
    assert_eq!(ack_delay_ns(1000, 3), Ok(8_000_000));
    assert_eq!(ack_delay_ns(u64::MAX >> 2, 20), Ok(u64::MAX));
    assert_eq!(
        ack_delay_ns(1, 21),
        Err(RecoveryError::BadAckDelayExponent(21))
    );
}

#[test]
fn no_rtt_sample_without_new_largest_or_ack_eliciting() {
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    r.on_packet_sent(
        APP,
        SentPacket {
            ack_eliciting: false,
            in_flight: false,
            ..pkt(1, 0)
        },
    )
    .unwrap();
    let range = single(1, 1);
    let out = r
        .on_ack_received(APP, &ack(&range), 0, 10 * MS, &mut |_| {})
        .unwrap();
    assert_eq!(out.newly_acked, 1);
    assert!(
        out.rtt_sample.is_none(),
        "only a non-ack-eliciting packet acked"
    );
    // Re-acknowledging is a no-op.
    let out = r
        .on_ack_received(APP, &ack(&range), 0, 11 * MS, &mut |_| {})
        .unwrap();
    assert_eq!(out.newly_acked, 0);
}

#[test]
fn ecn_ce_increase_is_reported_once() {
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    r.on_packet_sent(APP, pkt(0, 0)).unwrap();
    r.on_packet_sent(APP, pkt(1, 1)).unwrap();
    let range = single(0, 0);
    let ecn = EcnCounts {
        ect0: 1,
        ect1: 0,
        ce: 2,
    };
    let mut ev = Events::default();
    let a = AckFrame::from_ranges(&range, 0, Some(ecn)).unwrap();
    assert_eq!(
        r.on_ack_received(APP, &a, 0, 10, &mut ev.sink())
            .unwrap()
            .ecn_ce_increase,
        2
    );
    let range = single(0, 1);
    let a = AckFrame::from_ranges(&range, 0, Some(ecn)).unwrap();
    r.on_ack_received(APP, &a, 0, 11, &mut ev.sink()).unwrap();
    assert_eq!(ev.ce, 2);
}

#[test]
fn typed_errors_and_space_discard() {
    let mut r = LossRecovery::new(RecoveryConfig::default(), confirmed());
    let range = single(0, 0);
    assert_eq!(
        r.on_ack_received(APP, &ack(&range), 0, 0, &mut |_| {}),
        Err(RecoveryError::AckOfUnsentPacket { largest_acked: 0 })
    );
    r.on_packet_sent(APP, pkt(5, 10)).unwrap();
    assert_eq!(
        r.on_packet_sent(APP, pkt(5, 10)),
        Err(RecoveryError::NonIncreasingPn { pn: 5, last: 5 })
    );
    assert_eq!(
        r.on_packet_sent(APP, pkt(6, 9)),
        Err(RecoveryError::ClockWentBackwards)
    );
    assert_eq!(
        r.on_packet_sent(APP, pkt(1 << 62, 10)),
        Err(RecoveryError::PnOutOfRange(1 << 62))
    );
    let range = single(6, 6);
    assert_eq!(
        r.on_ack_received(APP, &ack(&range), 0, 20, &mut |_| {}),
        Err(RecoveryError::AckOfUnsentPacket { largest_acked: 6 })
    );
    assert_eq!(r.largest_acked(APP), None, "rejected ACK changed nothing");

    r.on_packet_sent(PnSpace::Initial, pkt(0, 10)).unwrap();
    assert_eq!(r.bytes_in_flight(), 2000);
    assert_eq!(r.discard_space(PnSpace::Initial, 11), 1000);
    assert_eq!(r.bytes_in_flight(), 1000);
    assert_eq!(
        r.on_packet_sent(PnSpace::Initial, pkt(1, 12)),
        Err(RecoveryError::SpaceDiscarded(PnSpace::Initial))
    );

    let mut small = LossRecovery::new(
        RecoveryConfig {
            max_tracked_per_space: 2,
            ..RecoveryConfig::default()
        },
        confirmed(),
    );
    small.on_packet_sent(APP, pkt(0, 0)).unwrap();
    small.on_packet_sent(APP, pkt(1, 0)).unwrap();
    assert_eq!(
        small.on_packet_sent(APP, pkt(2, 0)),
        Err(RecoveryError::TooManyTracked)
    );
    let range = single(0, 0);
    small
        .on_ack_received(APP, &ack(&range), 0, 1, &mut |_| {})
        .unwrap();
    small.on_packet_sent(APP, pkt(2, 1)).unwrap();
    assert_eq!(small.tracked(APP), 2);
}
