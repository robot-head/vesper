//! `std::arch` SIMD for the §32.3 SVF voice tile, with the scalar loop as the
//! reference.
//!
//! One call advances all 16 voices of an [`SvfVoiceTile`] by one sample:
//!
//! ```text
//! a1 = 1 / (1 + g (g + k)); a2 = g a1; a3 = g a2
//! v3 = v0 - ic2eq; v1 = a1 ic1eq + a2 v3; v2 = ic2eq + a2 ic1eq + a3 v3
//! ic1eq = 2 v1 - ic1eq; ic2eq = 2 v2 - ic2eq
//! y = gain v2; out_l = y pan_l; out_r = y pan_r
//! ```
//!
//! Every path evaluates exactly these IEEE-754 single-precision operations,
//! in this order, per lane: adds, subtracts, multiplies and one correctly
//! rounded division, with no fused multiply-add (Rust never contracts
//! `a * b + c`, and the vector code uses separate multiply and add
//! instructions). So the SIMD paths are bit-identical to [`SvfLanes::Scalar`]
//! for every input, including under the `ConstDenorm` scope, where both
//! scalar and vector instructions flush subnormals the same way (MXCSR
//! governs SSE/AVX scalar and packed forms alike; FPCR.FZ governs AArch64
//! scalar and Advanced SIMD alike).
//!
//! Paths:
//! * x86_64: SSE2 (baseline on every x86_64 CPU, 4 lanes x 4) and AVX
//!   (8 lanes x 2, chosen at runtime with `is_x86_feature_detected!`).
//! * AArch64: Advanced SIMD (NEON, baseline, 4 lanes x 4).
//! * Elsewhere: only the scalar loop.
//!
//! The unsafe code here is limited to unaligned vector loads and stores of
//! the tile's fixed-size `[f32; 16]` arrays and the calls into the
//! `#[target_feature]` kernels (SSE2 and NEON are baseline on their targets;
//! AVX is called only after runtime detection). That is the lane's unsafe
//! allowance: `std::arch` SIMD with a scalar reference.

#![allow(unsafe_code)]

use crate::kernels::{LANES, SvfVoiceTile};

/// Which implementation advances a voice tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SvfLanes {
    /// Plain Rust loop over the 16 lanes: the reference.
    Scalar,
    /// x86_64 SSE2, four 4-lane vectors.
    Sse2,
    /// x86_64 AVX, two 8-lane vectors (runtime-detected).
    Avx,
    /// AArch64 Advanced SIMD, four 4-lane vectors.
    Neon,
}

/// A requested SIMD path is not available on this CPU or target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimdUnavailable {
    /// The path that was requested.
    pub requested: SvfLanes,
}

impl std::fmt::Display for SimdUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} is not available on this host", self.requested)
    }
}

impl std::error::Error for SimdUnavailable {}

impl SvfLanes {
    /// Whether this path runs on the current CPU.
    pub fn is_available(self) -> bool {
        match self {
            SvfLanes::Scalar => true,
            SvfLanes::Sse2 => cfg!(target_arch = "x86_64"),
            SvfLanes::Avx => avx_detected(),
            SvfLanes::Neon => cfg!(target_arch = "aarch64"),
        }
    }

    /// `self` if it runs here, otherwise a typed error.
    pub fn checked(self) -> Result<Self, SimdUnavailable> {
        if self.is_available() {
            Ok(self)
        } else {
            Err(SimdUnavailable { requested: self })
        }
    }

    /// The widest path this CPU supports.
    pub fn best() -> Self {
        if avx_detected() {
            SvfLanes::Avx
        } else if cfg!(target_arch = "x86_64") {
            SvfLanes::Sse2
        } else if cfg!(target_arch = "aarch64") {
            SvfLanes::Neon
        } else {
            SvfLanes::Scalar
        }
    }

    /// Every path that runs on this CPU, scalar first.
    pub fn available() -> impl Iterator<Item = SvfLanes> {
        [
            SvfLanes::Scalar,
            SvfLanes::Sse2,
            SvfLanes::Avx,
            SvfLanes::Neon,
        ]
        .into_iter()
        .filter(|p| p.is_available())
    }
}

#[cfg(target_arch = "x86_64")]
fn avx_detected() -> bool {
    std::arch::is_x86_feature_detected!("avx")
}

#[cfg(not(target_arch = "x86_64"))]
fn avx_detected() -> bool {
    false
}

/// Per-lane stereo pan weights (left, right) of a 16-voice tile.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, align(64))]
pub struct PanLanes {
    /// Left weight per voice.
    pub left: [f32; LANES],
    /// Right weight per voice.
    pub right: [f32; LANES],
}

/// Per-lane products of one sample, before the (scalar, in-order) stereo sum.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, align(64))]
pub struct LaneOut {
    /// `gain * v2 * pan_left` per voice.
    pub left: [f32; LANES],
    /// `gain * v2 * pan_right` per voice.
    pub right: [f32; LANES],
}

impl LaneOut {
    /// All zeros.
    pub const ZERO: LaneOut = LaneOut {
        left: [0.0; LANES],
        right: [0.0; LANES],
    };

    /// Stereo sum in voice order (`0.0 + l0 + l1 + ...`), the same order as
    /// the scalar reference.
    pub fn sum(&self) -> (f32, f32) {
        let mut l = 0.0f32;
        let mut r = 0.0f32;
        for v in 0..LANES {
            l += self.left[v];
            r += self.right[v];
        }
        (l, r)
    }
}

/// Advance all 16 voices of `t` by one sample of input `v0`, using `path`.
/// A path that is not available here falls back to the scalar loop (callers
/// that must know use [`SvfLanes::checked`] first).
pub fn svf_tick(path: SvfLanes, t: &mut SvfVoiceTile, pan: &PanLanes, v0: f32, out: &mut LaneOut) {
    match path {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: SSE2 is in the x86_64 baseline (every x86_64 CPU has it),
        // the only precondition of this `target_feature` function.
        SvfLanes::Sse2 => unsafe { x86::tick_sse2(t, pan, v0, out) },
        #[cfg(target_arch = "x86_64")]
        SvfLanes::Avx if avx_detected() => {
            // SAFETY: AVX support was detected at runtime just above, which
            // is the only precondition of this `target_feature` function.
            unsafe { x86::tick_avx(t, pan, v0, out) }
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: Advanced SIMD is in the AArch64 baseline, the only
        // precondition of this `target_feature` function.
        SvfLanes::Neon => unsafe { arm::tick_neon(t, pan, v0, out) },
        _ => tick_scalar(t, pan, v0, out),
    }
}

/// The scalar reference.
pub fn tick_scalar(t: &mut SvfVoiceTile, pan: &PanLanes, v0: f32, out: &mut LaneOut) {
    for v in 0..LANES {
        let g = t.g_coeff[v];
        let k = t.k_damping[v];
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = v0 - t.ic2eq[v];
        let v1 = a1 * t.ic1eq[v] + a2 * v3;
        let v2 = t.ic2eq[v] + a2 * t.ic1eq[v] + a3 * v3;
        t.ic1eq[v] = 2.0 * v1 - t.ic1eq[v];
        t.ic2eq[v] = 2.0 * v2 - t.ic2eq[v];
        let y = t.gain[v] * v2;
        out.left[v] = y * pan.left[v];
        out.right[v] = y * pan.right[v];
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{LaneOut, PanLanes};
    use crate::kernels::SvfVoiceTile;
    use std::arch::x86_64::*;

    /// Load four lanes starting at `i` of a 16-lane array.
    #[target_feature(enable = "sse2")]
    #[inline]
    fn ld(a: &[f32; 16], i: usize) -> __m128 {
        let s = &a[i..i + 4];
        // SAFETY: `s` is exactly 4 initialized f32s; `loadu` has no
        // alignment requirement.
        unsafe { _mm_loadu_ps(s.as_ptr()) }
    }

    #[target_feature(enable = "sse2")]
    #[inline]
    fn st(a: &mut [f32; 16], i: usize, v: __m128) {
        let s = &mut a[i..i + 4];
        // SAFETY: `s` is exactly 4 writable f32s; `storeu` has no alignment
        // requirement.
        unsafe { _mm_storeu_ps(s.as_mut_ptr(), v) }
    }

    /// SSE2 is part of the x86_64 baseline, so callers may invoke this on any
    /// x86_64 CPU.
    #[target_feature(enable = "sse2")]
    pub fn tick_sse2(t: &mut SvfVoiceTile, pan: &PanLanes, v0: f32, out: &mut LaneOut) {
        let one = _mm_set1_ps(1.0);
        let two = _mm_set1_ps(2.0);
        let x = _mm_set1_ps(v0);
        for i in (0..16).step_by(4) {
            let g = ld(&t.g_coeff, i);
            let k = ld(&t.k_damping, i);
            let ic1 = ld(&t.ic1eq, i);
            let ic2 = ld(&t.ic2eq, i);
            let a1 = _mm_div_ps(one, _mm_add_ps(one, _mm_mul_ps(g, _mm_add_ps(g, k))));
            let a2 = _mm_mul_ps(g, a1);
            let a3 = _mm_mul_ps(g, a2);
            let v3 = _mm_sub_ps(x, ic2);
            let v1 = _mm_add_ps(_mm_mul_ps(a1, ic1), _mm_mul_ps(a2, v3));
            let v2 = _mm_add_ps(_mm_add_ps(ic2, _mm_mul_ps(a2, ic1)), _mm_mul_ps(a3, v3));
            st(&mut t.ic1eq, i, _mm_sub_ps(_mm_mul_ps(two, v1), ic1));
            st(&mut t.ic2eq, i, _mm_sub_ps(_mm_mul_ps(two, v2), ic2));
            let y = _mm_mul_ps(ld(&t.gain, i), v2);
            st(&mut out.left, i, _mm_mul_ps(y, ld(&pan.left, i)));
            st(&mut out.right, i, _mm_mul_ps(y, ld(&pan.right, i)));
        }
    }

    #[target_feature(enable = "avx")]
    #[inline]
    fn ld8(a: &[f32; 16], i: usize) -> __m256 {
        let s = &a[i..i + 8];
        // SAFETY: `s` is exactly 8 initialized f32s; `loadu` has no
        // alignment requirement. Only called from the AVX function.
        unsafe { _mm256_loadu_ps(s.as_ptr()) }
    }

    #[target_feature(enable = "avx")]
    #[inline]
    fn st8(a: &mut [f32; 16], i: usize, v: __m256) {
        let s = &mut a[i..i + 8];
        // SAFETY: `s` is exactly 8 writable f32s; `storeu` has no alignment
        // requirement. Only called from the AVX function.
        unsafe { _mm256_storeu_ps(s.as_mut_ptr(), v) }
    }

    /// Callers must have detected AVX at runtime.
    #[target_feature(enable = "avx")]
    pub fn tick_avx(t: &mut SvfVoiceTile, pan: &PanLanes, v0: f32, out: &mut LaneOut) {
        let one = _mm256_set1_ps(1.0);
        let two = _mm256_set1_ps(2.0);
        let x = _mm256_set1_ps(v0);
        for i in [0, 8] {
            let g = ld8(&t.g_coeff, i);
            let k = ld8(&t.k_damping, i);
            let ic1 = ld8(&t.ic1eq, i);
            let ic2 = ld8(&t.ic2eq, i);
            let a1 = _mm256_div_ps(
                one,
                _mm256_add_ps(one, _mm256_mul_ps(g, _mm256_add_ps(g, k))),
            );
            let a2 = _mm256_mul_ps(g, a1);
            let a3 = _mm256_mul_ps(g, a2);
            let v3 = _mm256_sub_ps(x, ic2);
            let v1 = _mm256_add_ps(_mm256_mul_ps(a1, ic1), _mm256_mul_ps(a2, v3));
            let v2 = _mm256_add_ps(
                _mm256_add_ps(ic2, _mm256_mul_ps(a2, ic1)),
                _mm256_mul_ps(a3, v3),
            );
            st8(&mut t.ic1eq, i, _mm256_sub_ps(_mm256_mul_ps(two, v1), ic1));
            st8(&mut t.ic2eq, i, _mm256_sub_ps(_mm256_mul_ps(two, v2), ic2));
            let y = _mm256_mul_ps(ld8(&t.gain, i), v2);
            st8(&mut out.left, i, _mm256_mul_ps(y, ld8(&pan.left, i)));
            st8(&mut out.right, i, _mm256_mul_ps(y, ld8(&pan.right, i)));
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::{LaneOut, PanLanes};
    use crate::kernels::SvfVoiceTile;
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    #[inline]
    fn ld(a: &[f32; 16], i: usize) -> float32x4_t {
        let s = &a[i..i + 4];
        // SAFETY: `s` is exactly 4 initialized f32s; `vld1q_f32` needs only
        // element alignment.
        unsafe { vld1q_f32(s.as_ptr()) }
    }

    #[target_feature(enable = "neon")]
    #[inline]
    fn st(a: &mut [f32; 16], i: usize, v: float32x4_t) {
        let s = &mut a[i..i + 4];
        // SAFETY: `s` is exactly 4 writable f32s; `vst1q_f32` needs only
        // element alignment.
        unsafe { vst1q_f32(s.as_mut_ptr(), v) }
    }

    /// Advanced SIMD is part of the AArch64 baseline.
    #[target_feature(enable = "neon")]
    pub fn tick_neon(t: &mut SvfVoiceTile, pan: &PanLanes, v0: f32, out: &mut LaneOut) {
        let one = vdupq_n_f32(1.0);
        let two = vdupq_n_f32(2.0);
        let x = vdupq_n_f32(v0);
        for i in (0..16).step_by(4) {
            let g = ld(&t.g_coeff, i);
            let k = ld(&t.k_damping, i);
            let ic1 = ld(&t.ic1eq, i);
            let ic2 = ld(&t.ic2eq, i);
            // vmulq/vaddq are separate (unfused) operations; vdivq is the
            // correctly rounded IEEE division.
            let a1 = vdivq_f32(one, vaddq_f32(one, vmulq_f32(g, vaddq_f32(g, k))));
            let a2 = vmulq_f32(g, a1);
            let a3 = vmulq_f32(g, a2);
            let v3 = vsubq_f32(x, ic2);
            let v1 = vaddq_f32(vmulq_f32(a1, ic1), vmulq_f32(a2, v3));
            let v2 = vaddq_f32(vaddq_f32(ic2, vmulq_f32(a2, ic1)), vmulq_f32(a3, v3));
            st(&mut t.ic1eq, i, vsubq_f32(vmulq_f32(two, v1), ic1));
            st(&mut t.ic2eq, i, vsubq_f32(vmulq_f32(two, v2), ic2));
            let y = vmulq_f32(ld(&t.gain, i), v2);
            st(&mut out.left, i, vmulq_f32(y, ld(&pan.left, i)));
            st(&mut out.right, i, vmulq_f32(y, ld(&pan.right, i)));
        }
    }
}
