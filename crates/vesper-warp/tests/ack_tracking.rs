//! Received-packet tracking: duplicates, range merging, bounded ACK frames.

mod common;
use common::Rng;
use std::collections::BTreeSet;
use vesper_warp::PnSpace;
use vesper_warp::ack::{AckError, Ecn, ReceivedPackets};
use vesper_warp::frame::{AckRange, EcnCounts, Frame, parse_frame};

#[test]
fn merges_ranges_and_detects_duplicates() {
    let mut r = ReceivedPackets::<8>::new(PnSpace::ApplicationData, 25_000_000);
    for pn in [0, 1, 2, 5, 4, 9] {
        r.on_packet_received(pn, 0, true, Ecn::NotEct).unwrap();
    }
    assert_eq!(
        r.ranges(),
        &[
            AckRange {
                smallest: 9,
                largest: 9
            },
            AckRange {
                smallest: 4,
                largest: 5
            },
            AckRange {
                smallest: 0,
                largest: 2
            }
        ]
    );
    assert_eq!(
        r.on_packet_received(4, 0, true, Ecn::NotEct),
        Err(AckError::Duplicate(4))
    );
    let got = r.on_packet_received(3, 1, true, Ecn::NotEct).unwrap();
    assert!(got.out_of_order && !got.largest);
    assert_eq!(r.ranges().len(), 2, "3 joins [0,2] and [4,5]");
    assert_eq!(r.largest(), Some(9));
    assert_eq!(
        r.on_packet_received(1 << 62, 0, true, Ecn::NotEct),
        Err(AckError::OutOfRange(1 << 62))
    );
    assert_eq!(
        r.on_packet_received(20, 0, true, Ecn::NotEct),
        Err(AckError::ClockWentBackwards)
    );
}

#[test]
fn ack_frame_encodes_ranges_delay_and_ecn() {
    let mut r = ReceivedPackets::<8>::new(PnSpace::ApplicationData, 25_000_000);
    r.on_packet_received(0, 0, true, Ecn::Ect0).unwrap();
    r.on_packet_received(2, 1_000_000, true, Ecn::Ce).unwrap();
    // Out of order and CE: immediate ACK.
    assert_eq!(r.ack_deadline(), Some(1_000_000));
    let ack = r.ack_frame(9_000_000, 3).unwrap().unwrap();
    // 8 ms since largest = 8000 us >> 3 = 1000.
    assert_eq!(ack.ack_delay, 1000);
    assert_eq!(
        ack.ecn,
        Some(EcnCounts {
            ect0: 1,
            ect1: 0,
            ce: 1
        })
    );
    let mut buf = [0u8; 64];
    let n = Frame::Ack(ack).encode(&mut buf).unwrap();
    let (Frame::Ack(parsed), _) = parse_frame(&buf[..n], vesper_warp::PacketType::OneRtt).unwrap()
    else {
        panic!()
    };
    assert_eq!(parsed.ranges().collect::<Vec<_>>(), r.ranges());
    r.on_ack_sent();
    assert_eq!(r.ack_deadline(), None);
    assert!(r.ack_frame(0, 21).is_err());
}

#[test]
fn delayed_ack_policy() {
    let mut app = ReceivedPackets::<4>::new(PnSpace::ApplicationData, 25_000_000);
    app.on_packet_received(0, 100, true, Ecn::NotEct).unwrap();
    assert_eq!(
        app.ack_deadline(),
        Some(25_000_100),
        "first packet: max_ack_delay"
    );
    app.on_packet_received(1, 200, true, Ecn::NotEct).unwrap();
    assert_eq!(
        app.ack_deadline(),
        Some(200),
        "second ack-eliciting: immediate"
    );
    let mut hs = ReceivedPackets::<4>::new(PnSpace::Handshake, 25_000_000);
    hs.on_packet_received(0, 100, true, Ecn::NotEct).unwrap();
    assert_eq!(hs.ack_deadline(), Some(100), "handshake spaces ack at once");
    let mut quiet = ReceivedPackets::<4>::new(PnSpace::ApplicationData, 25_000_000);
    quiet
        .on_packet_received(0, 100, false, Ecn::NotEct)
        .unwrap();
    assert_eq!(quiet.ack_deadline(), None, "non-ack-eliciting owes no ACK");
}

#[test]
fn range_bound_and_ack_of_ack_floor() {
    let mut r = ReceivedPackets::<3>::new(PnSpace::ApplicationData, 0);
    for pn in [0, 2, 4, 6, 8] {
        r.on_packet_received(pn, 0, true, Ecn::NotEct).unwrap();
        assert!(r.ranges().len() <= 3);
    }
    assert_eq!(
        r.ranges().iter().map(|x| x.largest).collect::<Vec<_>>(),
        [8, 6, 4]
    );
    // Forgotten ranges are reported as too old rather than accepted twice.
    assert_eq!(
        r.on_packet_received(0, 0, true, Ecn::NotEct),
        Err(AckError::TooOld(0))
    );
    assert_eq!(
        r.on_packet_received(1, 0, true, Ecn::NotEct),
        Err(AckError::TooOld(1))
    );
    let ack = r.ack_frame(0, 3).unwrap().unwrap();
    assert_eq!(ack.range_count(), 3);

    let mut r = ReceivedPackets::<8>::new(PnSpace::ApplicationData, 0);
    for pn in [0, 1, 2, 3, 6, 7, 8, 12] {
        r.on_packet_received(pn, 0, true, Ecn::NotEct).unwrap();
    }
    r.on_ack_of_ack(7);
    assert_eq!(
        r.ranges(),
        &[
            AckRange {
                smallest: 12,
                largest: 12
            },
            AckRange {
                smallest: 8,
                largest: 8
            }
        ]
    );
    assert_eq!(
        r.on_packet_received(5, 0, true, Ecn::NotEct),
        Err(AckError::TooOld(5))
    );
    r.on_packet_received(9, 0, true, Ecn::NotEct).unwrap();
}

/// Against a set model: with enough ranges, tracking is exact.
#[test]
fn random_arrivals_match_set_model() {
    let mut rng = Rng(0x77a2_0301);
    for _ in 0..200 {
        let mut r = ReceivedPackets::<64>::new(PnSpace::ApplicationData, 0);
        let mut model = BTreeSet::new();
        for _ in 0..100 {
            let pn = rng.below(120);
            let got = r.on_packet_received(pn, 0, rng.below(2) == 0, Ecn::NotEct);
            if model.insert(pn) {
                got.unwrap();
            } else {
                assert_eq!(got, Err(AckError::Duplicate(pn)));
            }
        }
        let mut from_ranges = BTreeSet::new();
        for w in r.ranges().windows(2) {
            assert!(w[1].largest + 1 < w[0].smallest, "descending, non-adjacent");
        }
        for range in r.ranges() {
            from_ranges.extend(range.smallest..=range.largest);
        }
        assert_eq!(from_ranges, model);
    }
}
