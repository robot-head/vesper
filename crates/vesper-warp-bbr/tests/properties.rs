//! Seeded property tests (no external property-testing dependency): EDT
//! monotonicity across arbitrary interleavings, pacing accuracy, the send
//! guard, and robustness of the event API against arbitrary inputs.

use vesper_warp_bbr::{
    AckedPacket, BbrConfig, BbrError, Bbrv3, Delivered, LostPacket, PacketDeliveryState, RttSample,
    SentPacket, SplitMix64, clamp_target_cwnd, edt_step, pacing_delay_ns,
};

fn arbitrary_time(rng: &mut SplitMix64) -> u64 {
    match rng.below(6) {
        0 => 0,
        1 => u64::MAX - rng.below(1 << 20),
        2 => rng.next_u64(),
        _ => rng.below(10_000_000_000),
    }
}

#[test]
fn edt_schedule_never_regresses_across_arbitrary_interleavings() {
    for seed in 0..64 {
        let mut rng = SplitMix64(seed);
        let mut bbr = Bbrv3::new(BbrConfig::quic(1200), 0).unwrap();
        let mut last_launch = 0u64;
        let mut last_next = bbr.next_edt_ns();
        for _ in 0..5_000 {
            let now = arbitrary_time(&mut rng);
            let bytes = match rng.below(4) {
                0 => 0,
                1 => u32::MAX,
                _ => rng.below(65_536) as u32,
            };
            let launch = bbr.schedule_edt_ns(bytes, now);
            assert!(launch >= now, "launch before now");
            assert!(launch >= last_launch, "launch regressed");
            assert!(launch >= last_next, "launch before previous EDT");
            assert!(bbr.next_edt_ns() >= launch, "next EDT before launch");
            last_launch = launch;
            last_next = bbr.next_edt_ns();
        }
    }
    // The pure step, including saturation at the end of the clock.
    let mut rng = SplitMix64(99);
    for _ in 0..100_000 {
        let (prev, now, delay) = (
            arbitrary_time(&mut rng),
            arbitrary_time(&mut rng),
            arbitrary_time(&mut rng),
        );
        let (launch, next) = edt_step(prev, now, delay);
        assert_eq!(launch, prev.max(now));
        assert_eq!(next, launch.saturating_add(delay));
        assert!(next >= launch && next >= prev);
    }
}

#[test]
fn pacing_matches_pacing_rate_within_integer_rounding() {
    let mut rng = SplitMix64(7);
    for _ in 0..2_000 {
        let rate = 1 + rng.below(100_000_000_000);
        let bytes = 1 + rng.below(65_535) as u32;
        let d = pacing_delay_ns(bytes, rate) as u128;
        let exact = bytes as u128 * 1_000_000_000;
        // ceil: d * rate >= bytes * 1e9 > (d - 1) * rate
        assert!(d * rate as u128 >= exact);
        assert!((d - 1) * (rate as u128) < exact || d == 0);
    }
    // A burst of back-to-back packets: span / bytes matches the rate to
    // within one nanosecond per packet.
    let mut bbr = Bbrv3::new(BbrConfig::quic(1200), 0).unwrap();
    let rate = bbr.pacing_rate_bytes_per_sec();
    let n = 10_000u64;
    let first = bbr.schedule_edt_ns(1200, 5);
    for _ in 1..n {
        bbr.schedule_edt_ns(1200, 5);
    }
    let span = bbr.next_edt_ns() - first;
    let ideal = (n as u128 * 1200 * 1_000_000_000).div_ceil(rate as u128) as u64;
    assert!(
        span >= ideal && span <= ideal + n,
        "span {span} ideal {ideal}"
    );
}

#[test]
fn target_cwnd_lies_within_clamp_bounds() {
    let mut rng = SplitMix64(11);
    for _ in 0..100_000 {
        let cwnd = arbitrary_time(&mut rng);
        let mss = rng.next_u64() as u32;
        let hi = arbitrary_time(&mut rng);
        let t = clamp_target_cwnd(cwnd, mss, hi);
        let floor = 4 * mss as u64;
        assert!(t >= floor && t <= hi.max(floor));
    }
}

/// A transport-like driver feeding arbitrary, partly invalid input. Valid
/// calls must keep the accounting consistent; rejected calls must leave the
/// controller unchanged; nothing may panic.
#[test]
fn arbitrary_event_sequences_are_total_and_rejections_are_side_effect_free() {
    for seed in 0..48 {
        let mut rng = SplitMix64(seed ^ 0xfeed);
        let mut bbr = Bbrv3::new(BbrConfig::quic(1 + rng.below(1500) as u32), 0).unwrap();
        let mut outstanding: Vec<(SentPacket, PacketDeliveryState)> = Vec::new();
        let mut now = 0u64;
        let mut pn = 0u64;
        let mut tracked: u64 = 0;
        for _ in 0..4_000 {
            now += rng.below(3_000_000);
            let before = format!("{bbr:?}");
            let result: Result<(), BbrError> = match rng.below(10) {
                0..=3 => {
                    let bytes = rng.below(3000) as u32;
                    let packet = SentPacket {
                        pn: if rng.below(50) == 0 { u64::MAX } else { pn },
                        sent_ns: now,
                        bytes,
                        ack_eliciting: true,
                        in_flight: rng.below(10) != 0,
                    };
                    let r = if rng.below(8) == 0 {
                        bbr.on_probe_packet_sent(now, packet)
                    } else {
                        let allowed = bbr.can_send();
                        let r = bbr.on_packet_sent(now, packet);
                        if r.is_ok() && packet.in_flight {
                            assert!(allowed, "guard bypassed");
                            assert!(
                                bbr.bytes_in_flight() < bbr.cwnd_bytes() as u64 + bytes as u64,
                                "send left inflight >= cwnd + packet"
                            );
                        }
                        r
                    };
                    r.map(|state| {
                        pn += 1;
                        if packet.in_flight {
                            tracked += bytes as u64;
                        }
                        outstanding.push((packet, state));
                    })
                }
                4..=6 if !outstanding.is_empty() => {
                    let k = 1 + rng.below(outstanding.len().min(4) as u64) as usize;
                    let batch: Vec<Delivered> = outstanding
                        .iter()
                        .take(k)
                        .map(|(p, s)| Delivered {
                            packet: AckedPacket {
                                pn: p.pn,
                                sent_ns: p.sent_ns,
                                bytes: p.bytes,
                            },
                            state: *s,
                            previously_lost: false,
                        })
                        .collect();
                    let rtt = RttSample {
                        latest_ns: rng.below(100_000_000),
                        min_ns: rng.below(50_000_000),
                        smoothed_ns: rng.below(100_000_000),
                        ack_delay_ns: 0,
                    };
                    let rtt = (rng.below(4) != 0).then_some(rtt);
                    let r = bbr.on_ack(now, &batch, rtt);
                    if r.is_ok() {
                        for (p, _) in outstanding.drain(..k) {
                            if p.in_flight {
                                tracked -= p.bytes as u64;
                            }
                        }
                    }
                    r
                }
                7 if !outstanding.is_empty() => {
                    let i = rng.below(outstanding.len() as u64) as usize;
                    let (p, s) = outstanding[i];
                    let r = bbr.on_packet_lost(
                        now,
                        LostPacket {
                            pn: p.pn,
                            sent_ns: p.sent_ns,
                            bytes: p.bytes,
                        },
                        s,
                    );
                    if r.is_ok() {
                        outstanding.remove(i);
                        if p.in_flight {
                            tracked -= p.bytes as u64;
                        }
                    }
                    r
                }
                8 => bbr.on_ecn_ce(now, rng.below(3)),
                _ => match rng.below(5) {
                    0 => bbr.on_persistent_congestion(now),
                    1 => bbr.on_app_limited(now),
                    2 => bbr.on_spurious_loss_detected(now),
                    3 => bbr.on_app_limited(now.saturating_sub(1 << 40)),
                    _ => {
                        // A forged snapshot from the future.
                        let forged = PacketDeliveryState {
                            delivered: u64::MAX,
                            delivered_ns: 0,
                            first_sent_ns: 0,
                            is_app_limited: false,
                            tx_in_flight: 0,
                            lost: 0,
                            counted: true,
                        };
                        let d = [Delivered {
                            packet: AckedPacket {
                                pn: 1,
                                sent_ns: 0,
                                bytes: 1,
                            },
                            state: forged,
                            previously_lost: false,
                        }];
                        bbr.on_ack(now, &d, None)
                    }
                },
            };
            if result.is_err() {
                assert_eq!(before, format!("{bbr:?}"), "rejected call changed state");
            }
            assert_eq!(bbr.bytes_in_flight(), tracked, "inflight accounting");
            assert!(bbr.cwnd_bytes() > 0 && bbr.pacing_rate_bytes_per_sec() > 0);
        }
    }
}
