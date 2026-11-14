//! Concrete-run counterparts of `lean/ResonanceStability.lean`.
//!
//! The Lean theorems are about exact rational arithmetic. These tests run the
//! crate's `f32` kernels (every SIMD path) over the stated coefficient range
//! and check the same bounds, with a small relative slack for rounding:
//!
//! * SVF: with damping `0 < k < 2` fixed, cutoff `0 <= g[n] <= G` changing
//!   every sample and `|x[n]| <= M`, the Lyapunov value
//!   `V = ic1² + k ic1 ic2 + ic2²` stays below
//!   `K = (2 G² k² + 8 (1 + G² + G k)) M² / k²` (`svf_bibo`), so the states
//!   and the low-pass output stay below `2K / (2 - k)` (`svf_state_bounded`,
//!   `svf_output_bounded`).
//! * PID: every command satisfies `|u| <= torque_limit` and the integrator
//!   stays within `integral_limit`, for any input sequence including NaN,
//!   infinities and overflowing values; faults command safe torque 0
//!   (`pid_invariant`).

use vesper_resonance::kernels::{LANES, PidController, PidGains, SvfBank, SvfVoiceTile};
use vesper_resonance::simd::{LaneOut, PanLanes, SvfLanes, svf_tick};

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
}

fn lyapunov(k: f64, c1: f64, c2: f64) -> f64 {
    c1 * c1 + k * c1 * c2 + c2 * c2
}

fn bound(k: f64, g_max: f64, m: f64) -> f64 {
    (2.0 * g_max * g_max * k * k + 8.0 * (1.0 + g_max * g_max + g_max * k)) * m * m / (k * k)
}

#[test]
fn svf_stays_inside_the_proved_bound() {
    let mut pan = PanLanes {
        left: [0.0; LANES],
        right: [0.0; LANES],
    };
    for v in 0..LANES {
        (pan.left[v], pan.right[v]) = SvfBank::pan(v);
    }
    let mut rng = Rng(0x0b1b_0005);
    // Per lane: damping from barely damped to near the edge of the range,
    // cutoff bounds from 20 Hz to near Nyquist at 48 kHz.
    let g_max_choices = [0.0013f32, 0.05, 0.5, 3.0, 40.0];
    let m = 1.0f32;
    for path in SvfLanes::available() {
        for (trial, &g_max) in g_max_choices.iter().enumerate() {
            let mut t = SvfVoiceTile {
                ic1eq: [0.0; LANES],
                ic2eq: [0.0; LANES],
                g_coeff: [0.0; LANES],
                k_damping: [0.0; LANES],
                gain: [1.0; LANES],
                patch_id: [0; LANES],
            };
            for (v, k) in t.k_damping.iter_mut().enumerate() {
                *k = 0.02 + 1.96 * v as f32 / (LANES - 1) as f32;
            }
            let mut out = LaneOut::ZERO;
            let mut worst = 0.0f64;
            for n in 0..20_000 {
                for g in &mut t.g_coeff {
                    *g = rng.range(0.0, g_max);
                }
                // Square wave at the resonance-ish rate plus noise, |x| <= M.
                let x = if (n / 37) % 2 == 0 { m } else { -m } * rng.range(0.5, 1.0);
                let prev_ic2 = t.ic2eq;
                svf_tick(path, &mut t, &pan, x, &mut out);
                for v in 0..LANES {
                    let k = f64::from(t.k_damping[v]);
                    let kb = bound(k, f64::from(g_max), f64::from(m));
                    let val = lyapunov(k, f64::from(t.ic1eq[v]), f64::from(t.ic2eq[v]));
                    assert!(
                        val <= kb * (1.0 + 1e-3),
                        "{path:?} trial {trial} lane {v} n {n}: V {val} > K {kb}"
                    );
                    worst = worst.max(val / kb);
                    let state = f64::from(t.ic1eq[v]).powi(2) + f64::from(t.ic2eq[v]).powi(2);
                    assert!((2.0 - k) * state <= 2.0 * kb * (1.0 + 1e-3));
                    // Low-pass output v2 = (ic2 + ic2') / 2.
                    let v2 = (f64::from(prev_ic2[v]) + f64::from(t.ic2eq[v])) / 2.0;
                    assert!((2.0 - k) * v2 * v2 <= 2.0 * kb * (1.0 + 1e-3));
                }
            }
            eprintln!("{path:?} G={g_max}: max V/K = {worst:.4}");
        }
    }
}

#[test]
fn pid_command_never_exceeds_the_torque_clamp() {
    let gains = PidGains {
        kp: 50.0,
        ki: 400.0,
        kd: 2.0,
        dt: 0.001,
        torque_limit: 1.5,
        integral_limit: 0.75,
    };
    let mut pid = PidController::new(gains).unwrap();
    let mut rng = Rng(0x7091_0006);
    let specials = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        f32::MIN,
        f32::MIN_POSITIVE,
        0.0,
    ];
    let mut faults = 0;
    for _ in 0..200_000 {
        let mut pick = || match rng.next() % 10 {
            0 => specials[(rng.next() % specials.len() as u64) as usize],
            1 => f32::from_bits(rng.next() as u32),
            _ => rng.range(-10.0, 10.0),
        };
        let (sp, meas) = (pick(), pick());
        // The design's rule: a fault commands safe torque.
        let u = pid.step(sp, meas).unwrap_or_else(|_| {
            faults += 1;
            0.0
        });
        assert!(u.is_finite());
        assert!(u.abs() <= gains.torque_limit, "torque {u}");
        assert!(pid.integral().abs() <= gains.integral_limit);
    }
    assert!(
        faults > 0,
        "the adversarial inputs exercised the fault path"
    );
}
