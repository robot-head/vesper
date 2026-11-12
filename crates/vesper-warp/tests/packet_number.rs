//! Packet-number properties: the Rust decoder against the Lean theorem's
//! statement (window recovery and congruence) on random and boundary inputs.

mod common;
use common::Rng;
use vesper_warp::PN_LIMIT;
use vesper_warp::packet_number::{
    PnError, PnLen, TruncatedPn, decode_with_largest, encode, encoded_len,
};

const BITS: [u32; 4] = [8, 16, 24, 32];

/// RFC 9000 A.3 verbatim in signed 128-bit arithmetic (the reference).
fn rfc_reference(largest: Option<u64>, truncated: u64, nbits: u32) -> i128 {
    let expected = largest.map_or(0, |l| i128::from(l) + 1);
    let win = 1i128 << nbits;
    let hwin = win / 2;
    let mask = win - 1;
    let candidate = (expected & !mask) | i128::from(truncated);
    if candidate <= expected - hwin && candidate < (1i128 << 62) - win {
        candidate + win
    } else if candidate > expected + hwin && candidate >= win {
        candidate - win
    } else {
        candidate
    }
}

fn check_window(largest: Option<u64>, p: u64, nbits: u32) {
    let expected = largest.map_or(0, |l| l + 1);
    let win = 1u64 << nbits;
    let hwin = win / 2;
    let t = p % win;
    let got = decode_with_largest(largest, t, nbits);
    let in_window = expected < p + hwin && p <= expected + hwin && p < PN_LIMIT;
    if in_window {
        assert_eq!(got, Ok(p), "largest={largest:?} p={p} bits={nbits}");
    }
    match got {
        Ok(r) => {
            assert_eq!(r % win, t, "congruence");
            assert!(r < PN_LIMIT, "bound");
            assert_eq!(i128::from(r), rfc_reference(largest, t, nbits));
        }
        Err(e) => {
            assert_eq!(e, PnError::Exhausted);
            assert!(rfc_reference(largest, t, nbits) >= i128::from(PN_LIMIT));
        }
    }
}

#[test]
fn random_windows_recover_true_packet_number() {
    let mut rng = Rng(0x77a2_0001);
    for _ in 0..200_000 {
        let nbits = BITS[rng.below(4) as usize];
        let hwin = 1u64 << (nbits - 1);
        let largest = match rng.below(4) {
            0 => None,
            1 => Some(rng.below(1 << 20)),
            2 => Some(PN_LIMIT - 1 - rng.below(1 << 33)),
            _ => Some(rng.below(PN_LIMIT)),
        };
        let expected = largest.map_or(0, |l| l + 1);
        // p in (expected - hwin, expected + hwin], clipped to [0, 2^62).
        let offset = rng.below(2 * hwin) as i128 - hwin as i128 + 1;
        let p = i128::from(expected) + offset;
        if p < 0 || p >= i128::from(PN_LIMIT) {
            continue;
        }
        check_window(largest, p as u64, nbits);
        // And an arbitrary truncated value: congruence and bound only.
        let t = rng.below(1 << nbits);
        check_window(largest, t, nbits);
    }
}

#[test]
fn boundary_values() {
    for nbits in BITS {
        let win = 1u64 << nbits;
        let hwin = win / 2;
        for largest in [
            None,
            Some(0),
            Some(hwin - 1),
            Some(hwin),
            Some(win - 1),
            Some(win),
            Some(PN_LIMIT - win - 1),
            Some(PN_LIMIT - hwin - 1),
            Some(PN_LIMIT - 2),
            Some(PN_LIMIT - 1),
        ] {
            let expected = largest.map_or(0, |l| l + 1);
            for p in [
                expected.saturating_sub(hwin) + 1,
                expected.saturating_sub(hwin),
                expected,
                expected + hwin,
                expected + hwin + 1,
                0,
                PN_LIMIT - 1,
            ] {
                check_window(largest, p, nbits);
            }
        }
    }
    assert_eq!(
        decode_with_largest(Some(PN_LIMIT - 2), (PN_LIMIT - 1) & 0xff, 8),
        Ok(PN_LIMIT - 1)
    );
}

#[test]
fn encode_then_decode_roundtrips_when_peer_has_largest_acked() {
    let mut rng = Rng(0x77a2_0002);
    let mut out = [0u8; 4];
    for _ in 0..100_000 {
        let acked = rng.below(PN_LIMIT - (1 << 32));
        let span = 1u64 << rng.below(32);
        let full = acked + 1 + rng.below(span);
        let len = encode(full, Some(acked), &mut out).unwrap();
        assert_eq!(len, encoded_len(full, Some(acked)).unwrap());
        // The peer's largest received is at least `acked` and below `full`.
        let peer_largest = acked + rng.below(full - acked);
        let t = TruncatedPn::read(len, &out).unwrap();
        assert_eq!(
            t.decode(Some(peer_largest)),
            Ok(full),
            "acked={acked} full={full}"
        );
    }
    assert_eq!(encoded_len(0, None), Ok(PnLen::new(1).unwrap()));
    assert_eq!(encoded_len(127, None), Ok(PnLen::new(1).unwrap()));
    assert_eq!(encoded_len(128, None), Ok(PnLen::new(2).unwrap()));
}
