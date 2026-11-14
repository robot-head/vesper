//! `std::arch` SIMD SVF paths are bit-identical to the scalar reference.
//!
//! Every path available on the host (scalar, SSE2 and AVX on x86_64, NEON on
//! AArch64) is compared bit for bit against `tick_scalar` and against the
//! wave-five fused loop `SvfBank::render_reference`, including ramps,
//! sample-accurate events, extreme values and the `ConstDenorm` scope.

use vesper_resonance::denorm::with_const_denorm;
use vesper_resonance::events::{RtParamEvent, process_with_events};
use vesper_resonance::kernels::{LANES, SvfBank, SvfVoiceTile, VoicePatch, VoicePatchBank};
use vesper_resonance::simd::{LaneOut, PanLanes, SvfLanes, svf_tick, tick_scalar};

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
    /// Any value an attacker could put in a tile: normal, subnormal, zero,
    /// huge, infinite, NaN.
    fn wild(&mut self) -> f32 {
        match self.next() % 8 {
            0 => f32::from_bits(self.next() as u32 & 0x807f_ffff), // subnormal or zero
            1 => f32::from_bits(self.next() as u32),               // any bit pattern
            2 => [
                0.0,
                -0.0,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::MAX,
                f32::MIN,
            ][(self.next() % 6) as usize],
            _ => self.range(-4.0, 4.0),
        }
    }
}

fn pan() -> PanLanes {
    let mut p = PanLanes {
        left: [0.0; LANES],
        right: [0.0; LANES],
    };
    for v in 0..LANES {
        (p.left[v], p.right[v]) = SvfBank::pan(v);
    }
    p
}

fn tile(rng: &mut Rng, wild: bool) -> SvfVoiceTile {
    let mut t = SvfVoiceTile {
        ic1eq: [0.0; LANES],
        ic2eq: [0.0; LANES],
        g_coeff: [0.0; LANES],
        k_damping: [0.0; LANES],
        gain: [0.0; LANES],
        patch_id: [0; LANES],
    };
    for v in 0..LANES {
        if wild {
            t.ic1eq[v] = rng.wild();
            t.ic2eq[v] = rng.wild();
            t.g_coeff[v] = rng.wild();
            t.k_damping[v] = rng.wild();
            t.gain[v] = rng.wild();
        } else {
            t.ic1eq[v] = rng.range(-1.0, 1.0);
            t.ic2eq[v] = rng.range(-1.0, 1.0);
            t.g_coeff[v] = rng.range(0.0, 3.0);
            t.k_damping[v] = rng.range(0.05, 2.0);
            t.gain[v] = rng.range(0.0, 1.0);
        }
    }
    t
}

fn bits(a: &[f32; LANES]) -> [u32; LANES] {
    a.map(f32::to_bits)
}

/// NaN payload and sign are not specified by IEEE-754 for arithmetic
/// results; everything else must match bit for bit.
fn same(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn assert_tiles_same(
    path: SvfLanes,
    a: &SvfVoiceTile,
    b: &SvfVoiceTile,
    oa: &LaneOut,
    ob: &LaneOut,
) {
    for v in 0..LANES {
        for (name, x, y) in [
            ("ic1eq", a.ic1eq[v], b.ic1eq[v]),
            ("ic2eq", a.ic2eq[v], b.ic2eq[v]),
            ("left", oa.left[v], ob.left[v]),
            ("right", oa.right[v], ob.right[v]),
        ] {
            assert!(
                same(x, y),
                "{path:?} lane {v} {name}: {x:e} vs scalar {y:e}"
            );
        }
    }
}

#[test]
fn host_reports_its_paths() {
    let best = SvfLanes::best();
    assert!(best.is_available());
    assert!(SvfLanes::Scalar.is_available());
    #[cfg(target_arch = "x86_64")]
    {
        assert!(SvfLanes::Sse2.is_available());
        assert_eq!(
            SvfLanes::Neon.checked().unwrap_err().requested,
            SvfLanes::Neon
        );
        assert_eq!(
            SvfLanes::Avx.is_available(),
            std::arch::is_x86_feature_detected!("avx")
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        assert_eq!(best, SvfLanes::Neon);
        assert!(SvfLanes::Sse2.checked().is_err());
        assert!(SvfLanes::Avx.checked().is_err());
    }
    let paths: Vec<_> = SvfLanes::available().collect();
    assert!(paths.len() >= 2 || cfg!(not(any(target_arch = "x86_64", target_arch = "aarch64"))));
    eprintln!("SVF lane paths on this host: {paths:?} (best {best:?})");
    // A bank refuses a path the host cannot run.
    let p = VoicePatch {
        g: 0.1,
        k: 1.0,
        gain: 1.0,
        patch_id: 0,
    };
    for path in [SvfLanes::Sse2, SvfLanes::Avx, SvfLanes::Neon] {
        assert_eq!(SvfBank::with_lanes(p, path).is_ok(), path.is_available());
    }
}

#[test]
fn tick_is_bit_identical_over_filter_range() {
    let pan = pan();
    let mut rng = Rng(0x5eed_0001);
    for path in SvfLanes::available() {
        for _ in 0..200 {
            let start = tile(&mut rng, false);
            let (mut a, mut b) = (start, start);
            let (mut oa, mut ob) = (LaneOut::ZERO, LaneOut::ZERO);
            for _ in 0..64 {
                let x = rng.range(-1.0, 1.0);
                svf_tick(path, &mut a, &pan, x, &mut oa);
                tick_scalar(&mut b, &pan, x, &mut ob);
                assert_eq!(bits(&a.ic1eq), bits(&b.ic1eq), "{path:?}");
                assert_eq!(bits(&a.ic2eq), bits(&b.ic2eq), "{path:?}");
                assert_eq!(bits(&oa.left), bits(&ob.left), "{path:?}");
                assert_eq!(bits(&oa.right), bits(&ob.right), "{path:?}");
            }
        }
    }
}

#[test]
fn tick_is_bit_identical_on_extreme_values() {
    let pan = pan();
    let mut rng = Rng(0xdead_beef_0002);
    for path in SvfLanes::available() {
        for _ in 0..5_000 {
            let start = tile(&mut rng, true);
            let (mut a, mut b) = (start, start);
            let (mut oa, mut ob) = (LaneOut::ZERO, LaneOut::ZERO);
            let x = rng.wild();
            svf_tick(path, &mut a, &pan, x, &mut oa);
            tick_scalar(&mut b, &pan, x, &mut ob);
            assert_tiles_same(path, &a, &b, &oa, &ob);
        }
    }
}

#[test]
fn tick_is_bit_identical_inside_const_denorm() {
    let pan = pan();
    let mut rng = Rng(0x0dd_0003);
    let checked = with_const_denorm(|| {
        let mut n = 0;
        for path in SvfLanes::available() {
            for _ in 0..2_000 {
                let mut start = tile(&mut rng, false);
                // Subnormal states: flushed on input (DAZ / FZ) on every path.
                for v in 0..LANES {
                    if rng.next().is_multiple_of(2) {
                        start.ic1eq[v] = f32::from_bits((rng.next() as u32) & 0x007f_ffff);
                        start.ic2eq[v] = -f32::from_bits((rng.next() as u32) & 0x007f_ffff);
                    }
                }
                let (mut a, mut b) = (start, start);
                let (mut oa, mut ob) = (LaneOut::ZERO, LaneOut::ZERO);
                let x = std::hint::black_box(f32::from_bits(1));
                svf_tick(path, &mut a, &pan, x, &mut oa);
                tick_scalar(&mut b, &pan, x, &mut ob);
                assert_tiles_same(path, &a, &b, &oa, &ob);
                n += 1;
            }
        }
        n
    });
    match checked {
        Ok(n) => assert!(n >= 2_000),
        Err(e) => eprintln!("ConstDenorm unavailable, skipped: {e}"),
    }
}

fn patch_bank(rng: &mut Rng) -> VoicePatchBank {
    let mut voices = [VoicePatch {
        g: 0.0,
        k: 1.0,
        gain: 0.0,
        patch_id: 0,
    }; LANES];
    for (v, p) in voices.iter_mut().enumerate() {
        p.g = rng.range(0.001, 1.5);
        p.k = rng.range(0.1, 2.0);
        p.gain = rng.range(0.0, 1.0);
        p.patch_id = v as u32;
    }
    VoicePatchBank { voices }
}

fn events(rng: &mut Rng, block: usize) -> Vec<RtParamEvent> {
    let n = (rng.next() % 12) as usize;
    let mut evs: Vec<RtParamEvent> = (0..n)
        .map(|_| RtParamEvent {
            sample_offset: (rng.next() % block as u64) as u16,
            node_id: (rng.next() % LANES as u64) as u16,
            param_id: (rng.next() % 3) as u32,
            target: match rng.next() % 3 {
                0 => rng.range(0.0, 1.0),
                1 => rng.range(0.001, 2.0),
                _ => rng.range(0.1, 2.0),
            },
            ramp_samples: (rng.next() % 80) as u32,
        })
        .collect();
    evs.sort_by_key(|e| e.sample_offset);
    evs
}

/// Whole-bank rendering with ramps and sample-accurate events: every path
/// equals the wave-five fused scalar loop bit for bit over 64 blocks.
#[test]
fn bank_render_matches_reference_with_events() {
    let mut rng = Rng(0xfeed_0004);
    let block = 64;
    for path in SvfLanes::available() {
        for _trial in 0..10 {
            let bank = patch_bank(&mut rng);
            let mut fast = SvfBank::with_lanes(bank.voices[0], path).unwrap();
            fast.apply_patch(&bank);
            let mut reference = fast.clone();
            let (mut lf, mut rf) = (vec![0.0f32; block], vec![0.0f32; block]);
            let (mut lr, mut rr) = (vec![0.0f32; block], vec![0.0f32; block]);
            for _ in 0..64 {
                let input: Vec<f32> = (0..block).map(|_| rng.range(-1.0, 1.0)).collect();
                let evs = events(&mut rng, block);
                process_with_events(&evs, block, &mut fast, |b, s, e| {
                    b.render(&input, &mut lf, &mut rf, s, e)
                })
                .unwrap();
                process_with_events(&evs, block, &mut reference, |b, s, e| {
                    b.render_reference(&input, &mut lr, &mut rr, s, e)
                })
                .unwrap();
                for i in 0..block {
                    assert_eq!(lf[i].to_bits(), lr[i].to_bits(), "{path:?} left {i}");
                    assert_eq!(rf[i].to_bits(), rr[i].to_bits(), "{path:?} right {i}");
                }
                assert_eq!(fast.tile, reference.tile);
            }
        }
    }
}

/// Throughput of each path (run with `--release -- --ignored --nocapture`).
/// Not part of CI; numbers are only meaningful in release builds.
#[test]
#[ignore]
fn throughput() {
    let pan = pan();
    let mut rng = Rng(0x7_0007);
    let start = tile(&mut rng, false);
    let ticks = 2_000_000u32;
    for path in SvfLanes::available() {
        let mut t = start;
        let mut out = LaneOut::ZERO;
        let begin = std::time::Instant::now();
        let mut acc = 0.0f32;
        for i in 0..ticks {
            svf_tick(
                path,
                &mut t,
                &pan,
                std::hint::black_box((i % 64) as f32 / 64.0),
                &mut out,
            );
            acc += out.left[3];
        }
        let dt = begin.elapsed();
        std::hint::black_box(acc);
        eprintln!(
            "{path:?}: {:.1} ns per 16-voice tick ({:.1} M voice-samples/s)",
            dt.as_nanos() as f64 / f64::from(ticks),
            f64::from(ticks) * 16.0 / dt.as_secs_f64() / 1e6
        );
    }
}
