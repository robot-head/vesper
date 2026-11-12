//! Behaviour of the controller on the deterministic link simulator.
//!
//! These check the modelled algorithm against the draft's intended
//! dynamics on an idealised path. They are not real-network performance
//! claims.

mod sim;

use sim::{Burst, LinkConfig, MS, SEC, Sim};
use vesper_warp_bbr::BbrState;

fn run(link: LinkConfig, secs: u64) -> Sim {
    let mut s = Sim::new(link);
    s.run_until(secs * SEC);
    // Every guarded send left bytes_in_flight below cwnd + one packet.
    assert!(
        s.stats.max_send_overshoot < link.packet_bytes as i64,
        "send overshoot {}",
        s.stats.max_send_overshoot
    );
    assert!(s.stats.launches.windows(2).all(|w| w[0] <= w[1]));
    s
}

#[test]
fn startup_exits_on_full_pipe_then_drains_into_probe_bw() {
    let link = LinkConfig::new(1_250_000, 40 * MS, 4000);
    let s = run(link, 3);
    let drain = s.first_time_in(BbrState::Drain).expect("left Startup");
    assert_eq!(
        s.stats.transitions[0].1,
        BbrState::Drain,
        "Startup -> Drain first"
    );
    // Exponential search reaches 1.25 MB/s from ~12 KB per 40 ms in well
    // under a second (log2(100) ≈ 7 rounds plus 3 plateau rounds).
    assert!(drain < 800 * MS, "Startup lasted {drain} ns");
    // Exit was the bandwidth plateau, not loss: nothing was dropped yet.
    let drops_before = s
        .stats
        .queue_at_enqueue
        .iter()
        .filter(|(t, q)| *t < drain && *q + 1200 > link.buffer_bytes)
        .count();
    assert_eq!(drops_before, 0);
    assert!(s.bbr.full_bw_reached());
    let max_bw = s.bbr.max_bw();
    assert!(
        max_bw * 100 >= link.rate_bytes_per_sec * 95
            && max_bw * 100 <= link.rate_bytes_per_sec * 105,
        "max_bw {max_bw}"
    );
    // Drain hands over to ProbeBW within a few round trips.
    let probe_bw = s
        .stats
        .transitions
        .iter()
        .find(|(_, st)| st.is_probe_bw())
        .expect("entered ProbeBW")
        .0;
    assert!(
        probe_bw - drain <= 4 * 45 * MS,
        "Drain lasted {} ns",
        probe_bw - drain
    );
    assert_eq!(s.bbr.inflight_hi(), None, "no loss, no inflight_hi");
}

#[test]
fn probe_rtt_follows_min_rtt_window_and_lasts_at_least_200ms() {
    let link = LinkConfig::new(1_250_000, 40 * MS, 2000);
    let s = run(link, 32);
    let entries = s.times_in(BbrState::ProbeRtt);
    assert!(entries.len() >= 2, "ProbeRTT entries {entries:?}");
    // First entry: the min_rtt sample (taken in the first RTT) expires
    // after 10 s; the next ACK enters ProbeRTT.
    assert!(
        entries[0] > 10 * SEC && entries[0] < 10 * SEC + 200 * MS,
        "{entries:?}"
    );
    for (i, &enter) in entries.iter().enumerate() {
        let exit = s
            .stats
            .transitions
            .iter()
            .find(|(t, st)| *t > enter && *st != BbrState::ProbeRtt)
            .expect("left ProbeRTT")
            .0;
        let held = exit - enter;
        assert!(
            (200 * MS..400 * MS).contains(&held),
            "ProbeRTT {i} held {held}"
        );
        // full_bw_reached, so ProbeRTT exits to ProbeBW_CRUISE.
        assert_eq!(
            s.stats
                .transitions
                .iter()
                .find(|(t, _)| *t == exit)
                .unwrap()
                .1,
            BbrState::ProbeBwCruise
        );
        if i > 0 {
            // Scheduled MinRTTFilterLen after the previous exit.
            let gap = enter - entries[i - 1];
            assert!(gap > 10 * SEC && gap < 10 * SEC + 600 * MS, "gap {gap}");
        }
    }
    // During ProbeRTT the window is capped near BDP/2.
    let bdp = link.bdp_bytes();
    let capped = s
        .stats
        .samples
        .iter()
        .filter(|x| x.state == BbrState::ProbeRtt)
        .all(|x| (x.cwnd as u64) <= bdp / 2 + 4 * 1200);
    assert!(capped);
}

fn assert_converges(rate: u64, rtt: u64, buffer_permille: u64, secs: u64) {
    let link = LinkConfig::new(rate, rtt, buffer_permille);
    let s = run(link, secs);
    let from = 3 * SEC;
    let to = secs * SEC;
    let goodput = s.goodput_bytes_per_sec(from, to);
    assert!(
        goodput * 100 >= rate * 93 && goodput <= rate,
        "rate {rate} rtt {rtt}: goodput {goodput}"
    );
    let bdp = link.bdp_bytes();
    let (mean, max) = s.queue_stats(from, to);
    assert!(
        mean * 2 <= bdp,
        "rate {rate} rtt {rtt}: mean queue {mean} bdp {bdp}"
    );
    assert!(
        max * 4 <= bdp * 5,
        "rate {rate} rtt {rtt}: max queue {max} bdp {bdp}"
    );
    assert_eq!(s.stats.dropped_packets, 0, "deep buffer: no drops");
}

#[test]
fn steady_state_converges_near_bottleneck_with_bounded_queue() {
    assert_converges(1_250_000, 40 * MS, 4000, 25); // 10 Mbit/s, 40 ms
    assert_converges(12_500_000, 10 * MS, 4000, 12); // 100 Mbit/s, 10 ms
    assert_converges(250_000, 100 * MS, 4000, 25); // 2 Mbit/s, 100 ms
}

#[test]
fn inflight_hi_reacts_to_loss_above_threshold() {
    // Shallow buffer (0.3 BDP): ProbeBW_UP overflows it and the loss rate
    // of that round exceeds 2%, so inflight_hi is set near BDP + buffer.
    let link = LinkConfig::new(1_250_000, 40 * MS, 300);
    let s = run(link, 20);
    assert!(s.stats.dropped_packets > 0);
    let hi = s.bbr.inflight_hi().expect("inflight_hi set by loss");
    let path = link.bdp_bytes() + link.buffer_bytes;
    assert!(
        hi <= path * 3 / 2 && hi >= path / 2,
        "inflight_hi {hi} path {path}"
    );
    // It stays bounded afterwards: losses per second fall once it is learned.
    let late_drops = s
        .stats
        .queue_at_enqueue
        .iter()
        .filter(|(t, q)| *t >= 10 * SEC && *q + 1200 > link.buffer_bytes)
        .count();
    let early_drops = s
        .stats
        .queue_at_enqueue
        .iter()
        .filter(|(t, q)| *t < 10 * SEC && *q + 1200 > link.buffer_bytes)
        .count();
    assert!(
        late_drops <= early_drops,
        "late {late_drops} early {early_drops}"
    );
    let goodput = s.goodput_bytes_per_sec(3 * SEC, 20 * SEC);
    assert!(goodput * 100 >= 1_250_000 * 90, "goodput {goodput}");

    // Heavy random loss (5%, above the 2% threshold) also caps inflight.
    let mut lossy = LinkConfig::new(1_250_000, 40 * MS, 4000);
    lossy.random_loss_ppm = 50_000;
    let s = run(lossy, 10);
    assert!(s.bbr.inflight_hi().is_some());
}

#[test]
fn ecn_ce_marks_are_a_congestion_signal() {
    let mut link = LinkConfig::new(1_250_000, 40 * MS, 8000);
    link.ecn_threshold_bytes = Some(link.bdp_bytes() / 4);
    let s = run(link, 20);
    assert_eq!(s.stats.dropped_packets, 0);
    assert!(s.stats.ce_marks > 0);
    assert!(
        s.bbr.inflight_hi().is_some(),
        "CE in ProbeBW_UP bounds inflight"
    );
    let (_, max) = s.queue_stats(5 * SEC, 20 * SEC);
    assert!(max <= link.bdp_bytes(), "queue {max}");
    let goodput = s.goodput_bytes_per_sec(3 * SEC, 20 * SEC);
    assert!(goodput * 100 >= 1_250_000 * 85, "goodput {goodput}");
}

#[test]
fn burst_loss_is_survived_deterministically() {
    let mut link = LinkConfig::new(1_250_000, 40 * MS, 2000);
    link.burst = Some(Burst {
        enter_ppm: 500,
        exit_ppm: 200_000,
        loss_ppm: 500_000,
    });
    link.seed = 42;
    let a = run(link, 15);
    let b = run(link, 15);
    assert_eq!(a.stats.transitions, b.stats.transitions, "seeded replay");
    assert!(a.stats.dropped_packets > 0);
    let goodput = a.goodput_bytes_per_sec(3 * SEC, 15 * SEC);
    assert!(goodput * 2 >= 1_250_000, "goodput {goodput}");
}

#[test]
fn app_limited_idle_keeps_bandwidth_estimate() {
    let link = LinkConfig::new(1_250_000, 40 * MS, 2000);
    let mut s = Sim::new(link);
    s.idle_windows.push((4 * SEC, 5 * SEC));
    s.run_until(4 * SEC);
    let before = s.bbr.max_bw();
    s.run_until(5 * SEC);
    assert_eq!(s.bbr.bytes_in_flight(), 0, "idle drained the pipe");
    assert!(
        s.bbr.max_bw() >= before,
        "app-limited idle did not lower max_bw"
    );
    s.run_until(8 * SEC);
    let goodput = s.goodput_bytes_per_sec(5 * SEC + 200 * MS, 8 * SEC);
    assert!(goodput * 100 >= 1_250_000 * 93, "restart goodput {goodput}");
}
