//! The crate's former in-module unit tests (seq, rtt, cc, wire), translated
//! to the public API so that they run against both builds (port wave P1
//! lane 11, #908; see docs/maintenance/porting-a-crate.md, "The unit-test
//! rule").

use vesper_warp_tcp::cc::{
    AckedPacket, CongestionController, CongestionEvent, LostPacket, NewReno,
};
use vesper_warp_tcp::rtt::{RtoConfig, RttEstimator};
use vesper_warp_tcp::seq::{Seq, SeqMap};
use vesper_warp_tcp::wire::{SackBlock, TcpOptions, Timestamps, WireError, fold, sum_bytes};

// ---- seq.rs
#[test]
fn wraparound_ordering() {
    let a = Seq(u32::MAX - 5);
    let b = a.add(10);
    assert_eq!(b, Seq(4));
    assert!(a.lt(b) && b.gt(a) && a.le(a) && a.ge(a));
    assert_eq!(b.diff(a), 10);
    assert_eq!(a.diff(b), -10);
    assert!(Seq(0).in_window(a, b));
    assert!(!b.in_window(a, b));
}

#[test]
fn lift_absolute_positions() {
    let map = SeqMap {
        isn: Seq(u32::MAX - 1),
    };
    assert_eq!(map.seq(0), Seq(u32::MAX - 1));
    assert_eq!(map.seq(3), Seq(1));
    assert_eq!(map.abs(0, Seq(1)), Some(3));
    let far = 5u64 << 32;
    assert_eq!(map.abs(far, map.seq(far + 7)), Some(far + 7));
    assert_eq!(map.abs(far, map.seq(far - 7)), Some(far - 7));
    assert_eq!(map.abs(0, Seq(u32::MAX - 3)), None);
}

// ---- rtt.rs
#[test]
fn rfc6298_formulas_and_backoff() {
    let cfg = RtoConfig {
        initial_ns: 1000,
        min_ns: 10,
        max_ns: 100_000,
        granularity_ns: 1,
    };
    let mut e = RttEstimator::new(cfg);
    assert_eq!(e.rto_ns(), 1000);
    e.sample(800);
    assert_eq!((e.srtt_ns(), e.rttvar_ns()), (800, 400));
    assert_eq!(e.rto_ns(), 800 + 1600);
    e.sample(400);
    // RTTVAR = 300 + 100 = 400, SRTT = 700 + 50 = 750.
    assert_eq!((e.srtt_ns(), e.rttvar_ns()), (750, 400));
    assert_eq!(e.rto_ns(), 2350);
    e.backoff();
    assert_eq!(e.rto_ns(), 4700);
    for _ in 0..40 {
        e.backoff();
    }
    assert_eq!(e.rto_ns(), 100_000);
    e.sample(750);
    assert_eq!(e.min_ns(), 400);
    assert!(e.rto_ns() < 100_000);
}

// ---- cc.rs
fn acked(sent_ns: u64, bytes: u32) -> CongestionEvent {
    CongestionEvent::Acked {
        packet: AckedPacket {
            pn: 0,
            sent_ns,
            bytes,
        },
        bytes_in_flight: 0,
    }
}

#[test]
fn slow_start_then_one_reduction_per_episode() {
    let mut cc = NewReno::new(1000, 2);
    cc.on_event(acked(1, 1000), 2);
    assert_eq!(cc.cwnd_bytes(), 3000);
    let lost = |sent_ns| CongestionEvent::Lost {
        packet: LostPacket {
            pn: 0,
            sent_ns,
            bytes: 1000,
        },
        bytes_in_flight: 3000,
    };
    cc.on_event(lost(5), 10);
    assert_eq!(cc.cwnd_bytes(), 2000);
    cc.on_event(lost(6), 11); // same episode
    assert_eq!(cc.cwnd_bytes(), 2000);
    cc.on_event(acked(9, 1000), 12); // sent before recovery: no growth
    assert_eq!(cc.cwnd_bytes(), 2000);
    cc.on_event(acked(20, 2000), 30); // congestion avoidance
    assert_eq!(cc.cwnd_bytes(), 3000);
    cc.on_event(CongestionEvent::PersistentCongestion, 40);
    assert_eq!(cc.cwnd_bytes(), 1000);
    assert_eq!(cc.ssthresh(), 2000);
    assert_eq!(cc.schedule_edt_ns(100, 77), 77);
}

// ---- wire.rs
#[test]
fn rfc1071_numeric_example() {
    // RFC 1071 §3: bytes 00 01 f2 03 f4 f5 f6 f7 sum to ddf2.
    let bytes = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
    assert_eq!(fold(sum_bytes(0, &bytes)), 0xddf2);
    assert_eq!(!fold(sum_bytes(0, &bytes)), 0x220d);
}

#[test]
fn options_roundtrip() {
    let mut o = TcpOptions {
        mss: Some(1460),
        window_scale: Some(7),
        sack_permitted: true,
        timestamps: Some(Timestamps { val: 1, ecr: 2 }),
        ..TcpOptions::default()
    };
    let mut buf = [0u8; 40];
    let n = o.write(&mut buf).unwrap();
    assert_eq!(n, 24);
    assert_eq!(TcpOptions::parse(&buf[..n]).unwrap(), o);
    o = TcpOptions {
        timestamps: Some(Timestamps { val: 9, ecr: 8 }),
        ..TcpOptions::default()
    };
    o.sack_len = 3;
    for (k, block) in (0u32..).zip(o.sack_blocks.iter_mut().take(3)) {
        *block = SackBlock {
            left: Seq(k * 10),
            right: Seq(k * 10 + 5),
        };
    }
    let n = o.write(&mut buf).unwrap();
    assert_eq!(n, 40);
    assert_eq!(TcpOptions::parse(&buf[..n]).unwrap(), o);
}

#[test]
fn malformed_options_are_errors() {
    for bad in [
        &[2u8][..],
        &[2, 3, 0][..],
        &[2, 4, 0][..],
        &[3, 2][..],
        &[5, 9, 0, 0, 0, 0, 0, 0, 0][..],
        &[5, 2][..],
        &[8, 9, 0, 0, 0, 0, 0, 0, 0][..],
        &[99, 0][..],
        &[99, 1][..],
        &[4, 2, 4, 2][..],
    ] {
        assert!(
            matches!(
                TcpOptions::parse(bad),
                Err(WireError::MalformedOption { .. })
            ),
            "{bad:?}"
        );
    }
    // Unknown options are skipped; EOL ends the list.
    let o = TcpOptions::parse(&[99, 4, 1, 2, 0, 77, 77]).unwrap();
    assert_eq!(o, TcpOptions::default());
}
