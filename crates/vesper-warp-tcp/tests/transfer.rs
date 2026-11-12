//! Bulk transfer over the seeded lossy harness.

mod common;
use common::*;
use vesper_warp_tcp::harness::{Harness, LinkConfig, ReadPolicy, Side};
use vesper_warp_tcp::{Connection, Flags, IpPair, NewReno, State};

fn establish(h: &mut Harness<NewReno>) {
    assert!(
        h.run_until(30 * SEC, |h| h.both(State::Established))
            .unwrap(),
        "{:?} {:?}",
        h.a.conn.state(),
        h.b.conn.state()
    );
}

/// Transfer `data` A→B, close both ways and check exact delivery.
fn transfer(h: &mut Harness<NewReno>, data: &[u8]) {
    establish(h);
    h.a.write(data).unwrap();
    h.a.conn.close().unwrap();
    let ok = h.run_until(600 * SEC, |h| h.b.peer_finished).unwrap();
    assert!(
        ok,
        "t={} got {} of {}; a={:?} b={:?}",
        h.now,
        h.b.received.len(),
        data.len(),
        h.a.conn.stats(),
        h.b.conn.stats()
    );
    assert_eq!(h.b.received.len(), data.len());
    assert!(h.b.received == data, "byte stream differs");
    h.b.conn.close().unwrap();
    assert!(
        h.run_until(h.now + 600 * SEC, |h| h.both(State::Closed))
            .unwrap(),
        "{:?} {:?}",
        h.a.conn.state(),
        h.b.conn.state()
    );
    assert!(h.a.send_buf.is_empty(), "all send-buffer ranges released");
    assert!(
        h.a.errors.is_empty() && h.b.errors.is_empty(),
        "{:?} {:?}",
        h.a.errors,
        h.b.errors
    );
}

#[test]
fn clean_bulk_transfer_and_close() {
    let mut h = harness(cfg(40000), cfg(80), LinkConfig::default());
    let data = pattern(500_000, 7);
    transfer(&mut h, &data);
    assert_eq!(h.a.conn.stats().retransmits, 0);
    assert_eq!(h.a.conn.window_shifts(), (7, 7));
    assert!(h.a.conn.sack_enabled() && h.a.conn.timestamps_enabled());
    assert!(h.a.conn.rtt().has_sample());
}

#[test]
fn lossy_reordering_duplicating_link_delivers_exact_stream() {
    for seed in 1..=12u64 {
        let link = LinkConfig {
            delay_ns: 10 * MS,
            jitter_ns: 8 * MS,
            loss_ppm: 30_000,
            dup_ppm: 20_000,
            seed,
            ..LinkConfig::default()
        };
        let mut h = harness(cfg(40000), cfg(80), link);
        let data = pattern(300_000, seed);
        transfer(&mut h, &data);
        assert!(
            h.dropped > 0 && h.duplicated > 0,
            "seed {seed} exercised impairments"
        );
        assert!(h.a.conn.stats().retransmits > 0);
    }
}

#[test]
fn heavy_loss_without_sack_or_timestamps() {
    for seed in 20..=25u64 {
        let mut a = cfg(40000);
        a.sack = false;
        a.timestamps = false;
        a.window_scale = None;
        let link = LinkConfig {
            delay_ns: 3 * MS,
            jitter_ns: 4 * MS,
            loss_ppm: 80_000,
            dup_ppm: 10_000,
            seed,
            ..LinkConfig::default()
        };
        let mut h = harness(a, cfg(80), link);
        let data = pattern(150_000, seed);
        transfer(&mut h, &data);
        assert!(!h.a.conn.sack_enabled() && !h.b.conn.timestamps_enabled());
        assert_eq!(h.a.conn.window_shifts(), (0, 0));
    }
}

#[test]
fn zero_window_probing_then_resume() {
    let mut b = cfg(80);
    b.recv_buffer = 8 * 1024;
    let link = LinkConfig {
        delay_ns: 2 * MS,
        loss_ppm: 0,
        ..LinkConfig::default()
    };
    let mut h = harness(cfg(40000), b, link);
    h.b.read = ReadPolicy::StallUntil(3 * SEC);
    let data = pattern(100_000, 3);
    establish(&mut h);
    h.a.write(&data).unwrap();
    h.a.conn.close().unwrap();
    assert!(
        h.run_until(2 * SEC, |h| h.a.conn.stats().window_probes >= 2)
            .unwrap()
    );
    assert_eq!(h.a.conn.send_window(), 0);
    // While stalled nothing beyond the receive buffer is ever accepted.
    assert!(
        h.b.received.len() <= 8 * 1024 && h.b.received.len() > 7 * 1024,
        "{}",
        h.b.received.len()
    );
    assert!(h.run_until(120 * SEC, |h| h.b.peer_finished).unwrap());
    assert!(h.b.received == data);
    let s = h.a.conn.stats();
    assert_eq!(s.retransmits, 0, "probing is not loss: {s:?}");
    assert_eq!(
        h.a.conn.controller().ssthresh(),
        u32::MAX,
        "no congestion response to probing"
    );
}

#[test]
fn slow_reader_with_lossy_link_and_lost_window_updates() {
    for seed in 40..=44u64 {
        let mut b = cfg(80);
        b.recv_buffer = 6000;
        let link = LinkConfig {
            delay_ns: 4 * MS,
            jitter_ns: 3 * MS,
            loss_ppm: 50_000,
            dup_ppm: 30_000,
            seed,
            ..LinkConfig::default()
        };
        let mut h = harness(cfg(40000), b, link);
        h.b.read = ReadPolicy::Periodic {
            interval_ns: 7 * MS,
            chunk: 1500,
        };
        let data = pattern(60_000, seed);
        transfer(&mut h, &data);
    }
}

#[test]
fn sack_recovery_retransmits_only_missing_ranges() {
    let mut a = cfg(40000);
    a.initial_window_segments = 20;
    let link = LinkConfig {
        delay_ns: 10 * MS,
        ..LinkConfig::default()
    };
    let mut h = harness(a, cfg(80), link);
    establish(&mut h);
    let smss = h.a.conn.send_mss() as usize;
    // Drop the first transmissions of data segments #3, #4 and #9.
    let mut data_idx = 0u64;
    h.filter = Some(Box::new(move |side, seg, _| {
        if side != Side::A || seg.payload.is_empty() || seg.flags.contains(Flags::SYN) {
            return false;
        }
        data_idx += 1;
        matches!(data_idx, 3 | 4 | 9)
    }));
    let data = pattern(smss * 18, 11);
    h.a.write(&data).unwrap();
    assert!(
        h.run_until(30 * SEC, |h| h.b.received.len() == data.len())
            .unwrap()
    );
    assert!(h.b.received == data);
    let s = h.a.conn.stats();
    assert_eq!(s.rtos, 0, "{s:?}");
    assert_eq!(s.recoveries, 1, "{s:?}");
    assert_eq!(s.retransmits, 3, "{s:?}");
    assert_eq!(s.retransmitted_bytes as usize, 3 * smss, "{s:?}");
}

#[test]
fn newreno_fast_retransmit_without_sack() {
    let mut a = cfg(40000);
    a.sack = false;
    a.initial_window_segments = 20;
    let mut h = harness(
        a,
        cfg(80),
        LinkConfig {
            delay_ns: 10 * MS,
            ..LinkConfig::default()
        },
    );
    establish(&mut h);
    let mut n = 0u64;
    h.filter = Some(Box::new(move |side, seg, _| {
        if side != Side::A || seg.payload.is_empty() {
            return false;
        }
        n += 1;
        matches!(n, 2 | 6)
    }));
    let smss = h.a.conn.send_mss() as usize;
    let data = pattern(smss * 16, 12);
    h.a.write(&data).unwrap();
    assert!(
        h.run_until(30 * SEC, |h| h.b.received.len() == data.len())
            .unwrap()
    );
    assert!(h.b.received == data);
    let s = h.a.conn.stats();
    assert_eq!(s.rtos, 0, "{s:?}");
    assert_eq!(
        s.retransmits, 2,
        "partial ACK retransmits the second hole: {s:?}"
    );
}

#[test]
fn bidirectional_transfer() {
    let link = LinkConfig {
        delay_ns: 5 * MS,
        jitter_ns: 5 * MS,
        loss_ppm: 20_000,
        dup_ppm: 10_000,
        seed: 99,
        ..LinkConfig::default()
    };
    let mut h = harness(cfg(40000), cfg(80), link);
    establish(&mut h);
    let da = pattern(120_000, 1);
    let db = pattern(90_000, 2);
    h.a.write(&da).unwrap();
    h.b.write(&db).unwrap();
    h.a.conn.close().unwrap();
    h.b.conn.close().unwrap();
    assert!(
        h.run_until(300 * SEC, |h| h.both(State::Closed)).unwrap(),
        "{:?} {:?}",
        h.a.conn.state(),
        h.b.conn.state()
    );
    assert!(h.b.received == da);
    assert!(h.a.received == db);
}

#[test]
fn ipv6_and_tiny_mss_transfer() {
    let mut a = cfg(40000);
    a.mss = 64;
    let link = LinkConfig {
        jitter_ns: 2 * MS,
        loss_ppm: 10_000,
        seed: 5,
        ..LinkConfig::default()
    };
    let mut src = [0u8; 16];
    src[0] = 0xfd;
    src[15] = 1;
    let mut dst = src;
    dst[15] = 2;
    let mut h = Harness::with_ips(
        Connection::new(a).unwrap(),
        Connection::new(cfg(80)).unwrap(),
        link,
        IpPair::V6 { src, dst },
    )
    .unwrap();
    let data = pattern(20_000, 9);
    transfer(&mut h, &data);
    assert_eq!(h.a.conn.send_mss(), 64 - 12);
}
