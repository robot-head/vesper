//! `.vchrono` parsing: limits, structural validation behind a valid MAC, and
//! no panics on arbitrary authenticated bytes.

use vesper_chrono::wire::{EFFECT_INOUT_WRITE, EFFECT_OBLIGATION};
use vesper_chrono::{
    Bundle, BundleError, BundleKey, BundleLimits, FlightRecorder, HopKind, RecorderConfig,
    RedactionPolicy,
};

fn key() -> BundleKey {
    BundleKey::new([5; 32])
}

fn sample() -> Bundle {
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    rec.record_effect(1, 1, 3, 4, b"hello").unwrap();
    rec.record_inout_write(1, 2, 0xAB, 9).unwrap();
    let id = rec.mint_obligation(1, 3, 1, None).unwrap();
    rec.capture_snapshot(4, b"img").unwrap();
    rec.record_obligation_hop(1, 5, id, HopKind::UnpackDischarge, 2)
        .unwrap();
    rec.record_effect_digest(1, 6, 3, 4, b"digest me").unwrap();
    Bundle::capture(&rec, &RedactionPolicy::new(), b"meta")
}

fn reseal_err(mutate: impl FnOnce(&mut Bundle)) -> BundleError {
    let mut b = sample();
    mutate(&mut b);
    let bytes = b.seal(&key()).unwrap();
    Bundle::open(&bytes, &key(), &BundleLimits::default()).unwrap_err()
}

#[test]
fn roundtrip() {
    let b = sample();
    let bytes = b.seal(&key()).unwrap();
    assert_eq!(
        Bundle::open(&bytes, &key(), &BundleLimits::default()).unwrap(),
        b
    );
}

#[test]
fn structurally_invalid_bundles_are_rejected_even_when_authenticated() {
    let malformed = |e: BundleError| matches!(e, BundleError::Malformed(_));
    assert!(malformed(reseal_err(|b| b.records.swap(0, 1))));
    assert!(malformed(reseal_err(|b| b.records[2].time_ns = 0)));
    assert!(malformed(reseal_err(|b| {
        b.records.remove(0);
    })));
    assert!(malformed(reseal_err(|b| b.evicted_records = 1)));
    assert!(malformed(reseal_err(|b| b.records[1].payload.push(0))));
    assert!(malformed(reseal_err(|b| {
        let r = b
            .records
            .iter_mut()
            .find(|r| r.effect_id == EFFECT_OBLIGATION)
            .unwrap();
        r.payload[16 + 4] = 9; // hop kind tag
    })));
    assert!(malformed(reseal_err(|b| {
        let r = b
            .records
            .iter_mut()
            .find(|r| r.effect_id == EFFECT_INOUT_WRITE)
            .unwrap();
        r.payload[15] = 1; // explicit padding
    })));
    assert!(malformed(reseal_err(|b| b.records[5]
        .payload
        .pop()
        .map(drop)
        .unwrap())));
    assert!(malformed(reseal_err(|b| b.snapshots[0].epoch_id = 2)));
    assert!(malformed(reseal_err(|b| b.records[0].effect_id = 0xFFF9)));
    assert!(malformed(reseal_err(
        |b| b.snapshots[0].start_lamport_seq = 99
    )));
    assert!(malformed(reseal_err(|b| b.redactions.push(
        vesper_chrono::RedactionEntry {
            target: vesper_chrono::RedactionTarget::Record { lamport_seq: 1 },
            offset: 3,
            len: 10,
        }
    ))));
}

#[test]
fn limits_and_truncation() {
    let bytes = sample().seal(&key()).unwrap();
    let tight = BundleLimits {
        max_bytes: bytes.len() - 1,
        ..BundleLimits::default()
    };
    assert!(matches!(
        Bundle::open(&bytes, &key(), &tight),
        Err(BundleError::TooLarge { .. })
    ));
    let few = BundleLimits {
        max_records: 2,
        ..BundleLimits::default()
    };
    assert_eq!(
        Bundle::open(&bytes, &key(), &few).unwrap_err(),
        BundleError::LimitExceeded("records")
    );
    assert_eq!(
        Bundle::open(&bytes[..10], &key(), &BundleLimits::default()).unwrap_err(),
        BundleError::Truncated
    );
    let mut magic = bytes.clone();
    magic[0] = b'X';
    assert_eq!(
        Bundle::open(&magic, &key(), &BundleLimits::default()).unwrap_err(),
        BundleError::BadMagic
    );
}

/// Arbitrary bytes sealed with the right key exercise every parser path;
/// each must return (a value or a typed error), never panic.
#[test]
fn authenticated_garbage_never_panics() {
    let good = sample().seal(&key()).unwrap();
    let body = &good[..good.len() - 32];
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut errors = 0;
    for i in 0..3_000 {
        let mut b = body.to_vec();
        match i % 3 {
            0 => {
                let at = 8 + (next() as usize) % (b.len() - 8);
                b[at] = next() as u8;
            }
            1 => b.truncate(8 + (next() as usize) % (b.len() - 8)),
            _ => {
                let at = 8 + (next() as usize) % (b.len() - 8);
                let n = (next() % 4) as usize;
                for _ in 0..n {
                    b.insert(at, next() as u8);
                }
            }
        }
        let mac = blake3::keyed_hash(&[5; 32], &b);
        b.extend_from_slice(mac.as_bytes());
        if Bundle::open(&b, &key(), &BundleLimits::default()).is_err() {
            errors += 1;
        }
    }
    assert!(errors > 1_000, "most mutations are rejected: {errors}");
}
