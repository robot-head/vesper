//! Morton encoder/decoder tests: exhaustive per axis, linearity across axes
//! (so the per-axis results determine every input), a seeded million-input
//! property sweep, and the BMI2 path against the scalar one.

use vesper_kinetic::MortonError;
use vesper_kinetic::morton::{
    self, AXIS_MAX, CODE_MASK, decode, decode_fast, decode_masked, encode, encode_fast,
    encode_masked, spec_encode,
};

struct SplitMix(u64);
impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[test]
fn every_axis_value_matches_the_bitwise_specification() {
    for v in 0..=AXIS_MAX {
        assert_eq!(encode(v, 0, 0).unwrap(), spec_encode(v, 0, 0));
        assert_eq!(encode(0, v, 0).unwrap(), spec_encode(0, v, 0));
        assert_eq!(encode(0, 0, v).unwrap(), spec_encode(0, 0, v));
        assert_eq!(morton::compact(morton::spread(u32::from(v))), u32::from(v));
        // Axes occupy disjoint bit lanes.
        assert_eq!(encode_masked(v, 0, 0) & 0x1249_2492, 0);
    }
}

#[test]
fn encoding_is_the_or_of_its_axes_and_round_trips() {
    let mut rng = SplitMix(0x6b69_6e65);
    for _ in 0..1_000_000 {
        let r = rng.next();
        let (x, y, z) = (
            (r & 0x3ff) as u16,
            ((r >> 10) & 0x3ff) as u16,
            ((r >> 20) & 0x3ff) as u16,
        );
        let code = encode(x, y, z).unwrap();
        assert_eq!(
            code,
            encode_masked(x, 0, 0) | encode_masked(0, y, 0) | encode_masked(0, 0, z)
        );
        assert_eq!(code, spec_encode(x, y, z));
        assert_eq!(decode(code).unwrap(), (x, y, z));
        // Every 30-bit code decodes and re-encodes to itself.
        let any = (r >> 32) as u32 & CODE_MASK;
        let (a, b, c) = decode_masked(any);
        assert_eq!(encode(a, b, c).unwrap(), any);
    }
}

#[test]
fn z_order_is_monotone_in_each_axis() {
    for v in 0..AXIS_MAX {
        for (lo, hi) in [
            (encode_masked(v, 5, 9), encode_masked(v + 1, 5, 9)),
            (encode_masked(5, v, 9), encode_masked(5, v + 1, 9)),
            (encode_masked(5, 9, v), encode_masked(5, 9, v + 1)),
        ] {
            assert!(lo < hi);
        }
    }
}

#[test]
fn out_of_range_inputs_are_typed_errors() {
    assert_eq!(
        encode(1024, 0, 0),
        Err(MortonError::AxisOutOfRange {
            axis: 0,
            value: 1024
        })
    );
    assert_eq!(
        encode(0, 0, u16::MAX),
        Err(MortonError::AxisOutOfRange {
            axis: 2,
            value: u16::MAX
        })
    );
    assert_eq!(decode(1 << 30), Err(MortonError::CodeOutOfRange(1 << 30)));
    assert!(encode_fast(0, 2000, 0).is_err());
    assert!(decode_fast(u32::MAX).is_err());
    assert_eq!(encode(AXIS_MAX, AXIS_MAX, AXIS_MAX).unwrap(), CODE_MASK);
}

#[test]
fn arch_path_equals_scalar_path() {
    #[cfg(target_arch = "x86_64")]
    let have = morton::arch::bmi2_available();
    #[cfg(not(target_arch = "x86_64"))]
    let have = false;
    if !have {
        eprintln!("BMI2 not available: encode_fast uses the scalar path");
    }
    let mut rng = SplitMix(0xb312);
    for v in 0..=AXIS_MAX {
        let w = (rng.next() & 0x3ff) as u16;
        let u = (rng.next() & 0x3ff) as u16;
        for (x, y, z) in [(v, w, u), (w, v, u), (u, w, v)] {
            let code = encode_fast(x, y, z).unwrap();
            assert_eq!(code, encode_masked(x, y, z));
            assert_eq!(decode_fast(code).unwrap(), (x, y, z));
            #[cfg(target_arch = "x86_64")]
            if have {
                assert_eq!(morton::arch::encode_bmi2(x, y, z), encode_masked(x, y, z));
                assert_eq!(morton::arch::decode_bmi2(code), decode_masked(code));
            }
        }
    }
}
