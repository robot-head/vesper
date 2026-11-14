//! `std::arch` kernels for the 16-lane XPBD constraint batches (§36.3
//! `solve_xpbd_color_batch_simd`), with a portable scalar reference.
//!
//! A batch is one constraint AoSoA tile: up to [`LANES`] constraints of one
//! colour, gathered into `[f32; 16]` columns. The kernels compute, for every
//! lane, exactly the operation sequence of the scalar solver
//! ([`crate::world::World`]'s `project`): the same IEEE-754 binary32
//! additions, multiplications, divisions and square roots in the same order,
//! each correctly rounded to nearest-even, with no fused multiply-add and no
//! reassociation. Comparisons and selects reproduce the scalar branches
//! (including what they do with NaN), so every backend returns the same bits
//! as [`SimdBackend::Portable`] for every input. `tests/simd.rs` checks this
//! on random and adversarial lanes for each backend the host supports.
//!
//! Backends: AVX2 (two 8-lane halves) and AVX-512F (one 16-lane register) on
//! x86_64, NEON (four 4-lane quarters) on AArch64. They are chosen at run
//! time ([`SimdBackend::detect`]); asking for one the host lacks is a typed
//! error. This module is the only place in the crate that uses `unsafe`,
//! and only for `std::arch` intrinsics behind a runtime feature check.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::world::det;

/// Lanes in one constraint batch (one AoSoA tile).
pub const LANES: usize = crate::tile::TILE_WIDTH;

/// Denominator floor shared with the scalar solver.
pub(crate) const EPS: f32 = 1.0e-9;

/// Which instruction set runs the batch kernels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimdBackend {
    /// Plain lane loops (the reference; whatever the compiler makes of them).
    Portable,
    /// x86_64 AVX2: two 8-lane halves per batch.
    Avx2,
    /// x86_64 AVX-512F: one 16-lane register per column.
    Avx512,
    /// AArch64 Advanced SIMD (NEON): four 4-lane quarters per batch.
    Neon,
}

/// A backend that this host cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimdUnavailable(pub SimdBackend);

impl std::fmt::Display for SimdUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SIMD backend {:?} is not available on this host", self.0)
    }
}

impl std::error::Error for SimdUnavailable {}

impl SimdBackend {
    /// Every backend, in preference order (widest first).
    pub const ALL: [SimdBackend; 4] = [Self::Avx512, Self::Avx2, Self::Neon, Self::Portable];

    /// Whether this host can run the backend.
    pub fn is_available(self) -> bool {
        match self {
            Self::Portable => true,
            #[cfg(target_arch = "x86_64")]
            Self::Avx2 => std::arch::is_x86_feature_detected!("avx2"),
            #[cfg(target_arch = "x86_64")]
            Self::Avx512 => std::arch::is_x86_feature_detected!("avx512f"),
            #[cfg(target_arch = "aarch64")]
            Self::Neon => std::arch::is_aarch64_feature_detected!("neon"),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// The widest backend this host supports.
    pub fn detect() -> Self {
        Self::ALL
            .into_iter()
            .find(|b| b.is_available())
            .unwrap_or(Self::Portable)
    }

    /// The backends this host supports, widest first.
    pub fn available() -> impl Iterator<Item = SimdBackend> {
        Self::ALL.into_iter().filter(|b| b.is_available())
    }

    /// `Ok(self)` if available, otherwise a typed error.
    pub fn require(self) -> Result<Self, SimdUnavailable> {
        if self.is_available() {
            Ok(self)
        } else {
            Err(SimdUnavailable(self))
        }
    }
}

/// Gathered inputs of a batch of two-body constraints (joints or sphere
/// contacts). Unused lanes may hold anything; their outputs are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PairLanes {
    pub pa: [[f32; LANES]; 3],
    pub pb: [[f32; LANES]; 3],
    pub wa: [f32; LANES],
    pub wb: [f32; LANES],
    /// Rest length (joint) or radius sum (contact).
    pub target: [f32; LANES],
    pub alpha: [f32; LANES],
    /// Bit `l` set: lane `l` is an inequality (a contact, which only pushes).
    pub inequality: u16,
}

/// Per-lane results of a pair batch, before friction and scatter.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PairSolved {
    /// New positions of the `a` and `b` bodies.
    pub na: [[f32; LANES]; 3],
    pub nb: [[f32; LANES]; 3],
    /// Unit constraint normal (from `b` to `a`).
    pub normal: [[f32; LANES]; 3],
    /// `wa · Δλ` and `wb · Δλ`.
    pub sa: [f32; LANES],
    pub sb: [f32; LANES],
    /// `wa + wb`.
    pub w: [f32; LANES],
    /// Bit `l` set: lane `l` projects (scatter it); clear: leave both bodies.
    pub active: u16,
}

/// Gathered inputs of a batch of ground-plane contacts.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GroundLanes {
    /// Body height.
    pub py: [f32; LANES],
    pub radius: [f32; LANES],
    pub wa: [f32; LANES],
    pub alpha: [f32; LANES],
}

/// Per-lane results of a ground batch.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GroundSolved {
    /// New body height.
    pub y: [f32; LANES],
    /// Normal correction `wa · Δλ`.
    pub corr: [f32; LANES],
    pub active: u16,
}

/// Solve a pair batch on `backend`. Panics if the backend is unavailable
/// (callers hold a [`SimdBackend::require`]d value).
pub fn solve_pair(backend: SimdBackend, input: &PairLanes, inv_h2: f32) -> PairSolved {
    let mut out = PairSolved::default();
    match backend {
        SimdBackend::Portable => pair_portable(input, inv_h2, &mut out),
        #[cfg(target_arch = "x86_64")]
        SimdBackend::Avx2 => {
            assert!(backend.is_available(), "AVX2 unavailable");
            x86::pair_avx2(input, inv_h2, &mut out)
        }
        #[cfg(target_arch = "x86_64")]
        SimdBackend::Avx512 => {
            assert!(backend.is_available(), "AVX-512F unavailable");
            x86::pair_avx512(input, inv_h2, &mut out)
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            assert!(backend.is_available(), "NEON unavailable");
            arm::pair_neon(input, inv_h2, &mut out)
        }
        #[allow(unreachable_patterns)]
        other => panic!("{}", SimdUnavailable(other)),
    }
    out
}

/// Solve a ground batch on `backend` (see [`solve_pair`]).
pub fn solve_ground(
    backend: SimdBackend,
    input: &GroundLanes,
    ground: f32,
    inv_h2: f32,
) -> GroundSolved {
    let mut out = GroundSolved::default();
    match backend {
        SimdBackend::Portable => ground_portable(input, ground, inv_h2, &mut out),
        #[cfg(target_arch = "x86_64")]
        SimdBackend::Avx2 => {
            assert!(backend.is_available(), "AVX2 unavailable");
            x86::ground_avx2(input, ground, inv_h2, &mut out)
        }
        #[cfg(target_arch = "x86_64")]
        SimdBackend::Avx512 => {
            assert!(backend.is_available(), "AVX-512F unavailable");
            x86::ground_avx512(input, ground, inv_h2, &mut out)
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            assert!(backend.is_available(), "NEON unavailable");
            arm::ground_neon(input, ground, inv_h2, &mut out)
        }
        #[allow(unreachable_patterns)]
        other => panic!("{}", SimdUnavailable(other)),
    }
    out
}

/// The scalar reference: each lane is the scalar `project` sequence.
fn pair_portable(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
    for l in 0..LANES {
        let mut d = [
            det::sub(i.pa[0][l], i.pb[0][l]),
            det::sub(i.pa[1][l], i.pb[1][l]),
            det::sub(i.pa[2][l], i.pb[2][l]),
        ];
        let mut len = det::sqrt(det::add(
            det::add(det::mul(d[0], d[0]), det::mul(d[1], d[1])),
            det::mul(d[2], d[2]),
        ));
        if len <= EPS {
            len = 0.0;
            d = [0.0, 1.0, 0.0];
        }
        let c = det::sub(len, i.target[l]);
        let w = det::add(i.wa[l], i.wb[l]);
        let ineq = i.inequality >> l & 1 == 1;
        let active = !(ineq && c >= 0.0) && w != 0.0;
        let alpha_tilde = det::mul(i.alpha[l], inv_h2);
        let denom = det::max(det::add(w, alpha_tilde), EPS);
        let d_lambda = det::div(det::neg(c), denom);
        let inv_len = if len == 0.0 { 1.0 } else { det::div(1.0, len) };
        let sa = det::mul(i.wa[l], d_lambda);
        let sb = det::mul(i.wb[l], d_lambda);
        for k in 0..3 {
            let n = det::mul(d[k], inv_len);
            o.normal[k][l] = n;
            o.na[k][l] = det::add(i.pa[k][l], det::mul(sa, n));
            o.nb[k][l] = det::sub(i.pb[k][l], det::mul(sb, n));
        }
        o.sa[l] = sa;
        o.sb[l] = sb;
        o.w[l] = w;
        o.active |= u16::from(active) << l;
    }
}

fn ground_portable(i: &GroundLanes, ground: f32, inv_h2: f32, o: &mut GroundSolved) {
    for l in 0..LANES {
        let c = det::sub(det::sub(i.py[l], i.radius[l]), ground);
        let active = !(c >= 0.0 || i.wa[l] == 0.0);
        let alpha_tilde = det::mul(i.alpha[l], inv_h2);
        let denom = det::max(det::add(i.wa[l], alpha_tilde), EPS);
        let d_lambda = det::div(det::neg(c), denom);
        o.y[l] = det::add(i.py[l], det::mul(i.wa[l], d_lambda));
        o.corr[l] = det::mul(i.wa[l], d_lambda);
        o.active |= u16::from(active) << l;
    }
}

/// Lane operations every backend provides. Comparisons return all-ones
/// lane masks in the vector type; `select(m, a, b)` is `m ? a : b`.
/// Every operation is the IEEE-754 binary32 operation of the scalar code.
#[allow(unsafe_code)]
trait Lanes: Copy {
    const WIDTH: usize;
    /// # Safety
    /// `p` must be valid for `WIDTH` reads, and the backend's target
    /// feature must be available.
    unsafe fn load(p: *const f32) -> Self;
    /// # Safety
    /// `p` must be valid for `WIDTH` writes; feature available.
    unsafe fn store(self, p: *mut f32);
    /// # Safety: feature available (all methods below).
    unsafe fn splat(x: f32) -> Self;
    unsafe fn add(self, o: Self) -> Self;
    unsafe fn sub(self, o: Self) -> Self;
    unsafe fn mul(self, o: Self) -> Self;
    unsafe fn div(self, o: Self) -> Self;
    unsafe fn sqrt(self) -> Self;
    /// Flip the sign bit (scalar `-x`).
    unsafe fn neg(self) -> Self;
    /// Ordered `self > o` (false on NaN).
    unsafe fn gt(self, o: Self) -> Self;
    /// Ordered `self >= o`.
    unsafe fn ge(self, o: Self) -> Self;
    /// Ordered `self <= o`.
    unsafe fn le(self, o: Self) -> Self;
    /// Ordered `self == o`.
    unsafe fn eq(self, o: Self) -> Self;
    unsafe fn and(self, o: Self) -> Self;
    unsafe fn or(self, o: Self) -> Self;
    /// `!self & o`.
    unsafe fn andnot(self, o: Self) -> Self;
    unsafe fn select(mask: Self, a: Self, b: Self) -> Self;
    /// One bit per lane, lane 0 lowest.
    unsafe fn bits(self) -> u32;
    /// Lanes `base..base + WIDTH` of a 16-bit mask as an all-ones mask.
    unsafe fn from_bits(bits: u16, base: usize) -> Self;
}

/// The shared kernel body: written once over [`Lanes`], instantiated per
/// backend inside a `#[target_feature]` function.
#[allow(unsafe_code)]
#[inline(always)]
unsafe fn pair_kernel<S: Lanes>(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
    // SAFETY: every pointer below is `base..base + WIDTH` of a 16-element
    // array with `base + WIDTH <= 16`; the caller guarantees the feature.
    unsafe {
        let eps = S::splat(EPS);
        let zero = S::splat(0.0);
        let one = S::splat(1.0);
        let h2 = S::splat(inv_h2);
        let mut active_bits = 0u16;
        let mut base = 0;
        while base < LANES {
            macro_rules! ld {
                ($a:expr) => {
                    S::load($a.as_ptr().add(base))
                };
            }
            macro_rules! st {
                ($v:expr, $a:expr) => {
                    $v.store($a.as_mut_ptr().add(base))
                };
            }
            let pa = [ld!(i.pa[0]), ld!(i.pa[1]), ld!(i.pa[2])];
            let pb = [ld!(i.pb[0]), ld!(i.pb[1]), ld!(i.pb[2])];
            let wa = ld!(i.wa);
            let wb = ld!(i.wb);
            let d0 = pa[0].sub(pb[0]);
            let d1 = pa[1].sub(pb[1]);
            let d2 = pa[2].sub(pb[2]);
            let len = d0.mul(d0).add(d1.mul(d1)).add(d2.mul(d2)).sqrt();
            let coincident = len.le(eps);
            let len = S::select(coincident, zero, len);
            let d = [
                S::select(coincident, zero, d0),
                S::select(coincident, one, d1),
                S::select(coincident, zero, d2),
            ];
            let c = len.sub(ld!(i.target));
            let w = wa.add(wb);
            let ineq = S::from_bits(i.inequality, base);
            let pushing_out = ineq.and(c.ge(zero));
            let massless = w.eq(zero);
            let active = pushing_out
                .or(massless)
                .andnot(S::splat(f32::from_bits(!0)));
            let alpha_tilde = ld!(i.alpha).mul(h2);
            let sum = w.add(alpha_tilde);
            // det::max(sum, EPS) = if sum > EPS { sum } else { EPS }.
            let denom = S::select(sum.gt(eps), sum, eps);
            let d_lambda = c.neg().div(denom);
            let inv_len = S::select(len.eq(zero), one, one.div(len));
            let sa = wa.mul(d_lambda);
            let sb = wb.mul(d_lambda);
            for k in 0..3 {
                let n = d[k].mul(inv_len);
                st!(n, o.normal[k]);
                st!(pa[k].add(sa.mul(n)), o.na[k]);
                st!(pb[k].sub(sb.mul(n)), o.nb[k]);
            }
            st!(sa, o.sa);
            st!(sb, o.sb);
            st!(w, o.w);
            active_bits |= (active.bits() as u16) << base;
            base += S::WIDTH;
        }
        o.active = active_bits;
    }
}

#[allow(unsafe_code)]
#[inline(always)]
unsafe fn ground_kernel<S: Lanes>(i: &GroundLanes, ground: f32, inv_h2: f32, o: &mut GroundSolved) {
    // SAFETY: as in `pair_kernel`.
    unsafe {
        let eps = S::splat(EPS);
        let zero = S::splat(0.0);
        let g = S::splat(ground);
        let h2 = S::splat(inv_h2);
        let mut active_bits = 0u16;
        let mut base = 0;
        while base < LANES {
            let py = S::load(i.py.as_ptr().add(base));
            let r = S::load(i.radius.as_ptr().add(base));
            let wa = S::load(i.wa.as_ptr().add(base));
            let alpha = S::load(i.alpha.as_ptr().add(base));
            let c = py.sub(r).sub(g);
            let inactive = c.ge(zero).or(wa.eq(zero));
            let active = inactive.andnot(S::splat(f32::from_bits(!0)));
            let sum = wa.add(alpha.mul(h2));
            let denom = S::select(sum.gt(eps), sum, eps);
            let d_lambda = c.neg().div(denom);
            let corr = wa.mul(d_lambda);
            // Scalar: y = py + wa·Δλ, corr = wa·Δλ (the same product).
            py.add(corr).store(o.y.as_mut_ptr().add(base));
            corr.store(o.corr.as_mut_ptr().add(base));
            active_bits |= (active.bits() as u16) << base;
            base += S::WIDTH;
        }
        o.active = active_bits;
    }
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
mod x86 {
    use super::*;
    use std::arch::x86_64::*;

    #[derive(Clone, Copy)]
    struct Avx2(__m256);

    // SAFETY (whole impl): only reached from `#[target_feature(enable =
    // "avx2")]` functions after a runtime check.
    impl Lanes for Avx2 {
        const WIDTH: usize = 8;
        #[inline(always)]
        unsafe fn load(p: *const f32) -> Self {
            unsafe { Self(_mm256_loadu_ps(p)) }
        }
        #[inline(always)]
        unsafe fn store(self, p: *mut f32) {
            unsafe { _mm256_storeu_ps(p, self.0) }
        }
        #[inline(always)]
        unsafe fn splat(x: f32) -> Self {
            unsafe { Self(_mm256_set1_ps(x)) }
        }
        #[inline(always)]
        unsafe fn add(self, o: Self) -> Self {
            unsafe { Self(_mm256_add_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn sub(self, o: Self) -> Self {
            unsafe { Self(_mm256_sub_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn mul(self, o: Self) -> Self {
            unsafe { Self(_mm256_mul_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn div(self, o: Self) -> Self {
            unsafe { Self(_mm256_div_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn sqrt(self) -> Self {
            unsafe { Self(_mm256_sqrt_ps(self.0)) }
        }
        #[inline(always)]
        unsafe fn neg(self) -> Self {
            unsafe { Self(_mm256_xor_ps(self.0, _mm256_set1_ps(-0.0))) }
        }
        #[inline(always)]
        unsafe fn gt(self, o: Self) -> Self {
            unsafe { Self(_mm256_cmp_ps::<_CMP_GT_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn ge(self, o: Self) -> Self {
            unsafe { Self(_mm256_cmp_ps::<_CMP_GE_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn le(self, o: Self) -> Self {
            unsafe { Self(_mm256_cmp_ps::<_CMP_LE_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn eq(self, o: Self) -> Self {
            unsafe { Self(_mm256_cmp_ps::<_CMP_EQ_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn and(self, o: Self) -> Self {
            unsafe { Self(_mm256_and_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn or(self, o: Self) -> Self {
            unsafe { Self(_mm256_or_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn andnot(self, o: Self) -> Self {
            unsafe { Self(_mm256_andnot_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn select(mask: Self, a: Self, b: Self) -> Self {
            unsafe { Self(_mm256_blendv_ps(b.0, a.0, mask.0)) }
        }
        #[inline(always)]
        unsafe fn bits(self) -> u32 {
            unsafe { _mm256_movemask_ps(self.0) as u32 }
        }
        #[inline(always)]
        unsafe fn from_bits(bits: u16, base: usize) -> Self {
            unsafe {
                let b = _mm256_set1_epi32(i32::from(bits >> base & 0xff));
                let lane = _mm256_setr_epi32(1, 2, 4, 8, 16, 32, 64, 128);
                let m = _mm256_cmpeq_epi32(_mm256_and_si256(b, lane), lane);
                Self(_mm256_castsi256_ps(m))
            }
        }
    }

    #[derive(Clone, Copy)]
    struct Avx512(__m512);

    // AVX-512 comparisons produce `__mmask16`; they are widened to all-ones
    // lanes so the shared kernel can treat masks as vectors.
    #[inline(always)]
    unsafe fn widen(k: __mmask16) -> Avx512 {
        unsafe {
            Avx512(_mm512_castsi512_ps(_mm512_maskz_mov_epi32(
                k,
                _mm512_set1_epi32(-1),
            )))
        }
    }
    #[inline(always)]
    unsafe fn narrow(m: Avx512) -> __mmask16 {
        unsafe { _mm512_test_epi32_mask(_mm512_castps_si512(m.0), _mm512_castps_si512(m.0)) }
    }

    // SAFETY (whole impl): only reached from `#[target_feature(enable =
    // "avx512f")]` functions after a runtime check.
    impl Lanes for Avx512 {
        const WIDTH: usize = 16;
        #[inline(always)]
        unsafe fn load(p: *const f32) -> Self {
            unsafe { Self(_mm512_loadu_ps(p)) }
        }
        #[inline(always)]
        unsafe fn store(self, p: *mut f32) {
            unsafe { _mm512_storeu_ps(p, self.0) }
        }
        #[inline(always)]
        unsafe fn splat(x: f32) -> Self {
            unsafe { Self(_mm512_set1_ps(x)) }
        }
        #[inline(always)]
        unsafe fn add(self, o: Self) -> Self {
            unsafe { Self(_mm512_add_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn sub(self, o: Self) -> Self {
            unsafe { Self(_mm512_sub_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn mul(self, o: Self) -> Self {
            unsafe { Self(_mm512_mul_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn div(self, o: Self) -> Self {
            unsafe { Self(_mm512_div_ps(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn sqrt(self) -> Self {
            unsafe { Self(_mm512_sqrt_ps(self.0)) }
        }
        #[inline(always)]
        unsafe fn neg(self) -> Self {
            unsafe {
                Self(_mm512_castsi512_ps(_mm512_xor_si512(
                    _mm512_castps_si512(self.0),
                    _mm512_set1_epi32(i32::MIN),
                )))
            }
        }
        #[inline(always)]
        unsafe fn gt(self, o: Self) -> Self {
            unsafe { widen(_mm512_cmp_ps_mask::<_CMP_GT_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn ge(self, o: Self) -> Self {
            unsafe { widen(_mm512_cmp_ps_mask::<_CMP_GE_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn le(self, o: Self) -> Self {
            unsafe { widen(_mm512_cmp_ps_mask::<_CMP_LE_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn eq(self, o: Self) -> Self {
            unsafe { widen(_mm512_cmp_ps_mask::<_CMP_EQ_OQ>(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn and(self, o: Self) -> Self {
            unsafe {
                Self(_mm512_castsi512_ps(_mm512_and_si512(
                    _mm512_castps_si512(self.0),
                    _mm512_castps_si512(o.0),
                )))
            }
        }
        #[inline(always)]
        unsafe fn or(self, o: Self) -> Self {
            unsafe {
                Self(_mm512_castsi512_ps(_mm512_or_si512(
                    _mm512_castps_si512(self.0),
                    _mm512_castps_si512(o.0),
                )))
            }
        }
        #[inline(always)]
        unsafe fn andnot(self, o: Self) -> Self {
            unsafe {
                Self(_mm512_castsi512_ps(_mm512_andnot_si512(
                    _mm512_castps_si512(self.0),
                    _mm512_castps_si512(o.0),
                )))
            }
        }
        #[inline(always)]
        unsafe fn select(mask: Self, a: Self, b: Self) -> Self {
            unsafe { Self(_mm512_mask_blend_ps(narrow(mask), b.0, a.0)) }
        }
        #[inline(always)]
        unsafe fn bits(self) -> u32 {
            unsafe { u32::from(narrow(self)) }
        }
        #[inline(always)]
        unsafe fn from_bits(bits: u16, base: usize) -> Self {
            debug_assert_eq!(base, 0);
            unsafe { widen(bits) }
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn pair_avx2_inner(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
        unsafe { pair_kernel::<Avx2>(i, inv_h2, o) }
    }
    #[target_feature(enable = "avx2")]
    unsafe fn ground_avx2_inner(i: &GroundLanes, g: f32, inv_h2: f32, o: &mut GroundSolved) {
        unsafe { ground_kernel::<Avx2>(i, g, inv_h2, o) }
    }
    #[target_feature(enable = "avx512f")]
    unsafe fn pair_avx512_inner(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
        unsafe { pair_kernel::<Avx512>(i, inv_h2, o) }
    }
    #[target_feature(enable = "avx512f")]
    unsafe fn ground_avx512_inner(i: &GroundLanes, g: f32, inv_h2: f32, o: &mut GroundSolved) {
        unsafe { ground_kernel::<Avx512>(i, g, inv_h2, o) }
    }

    pub(super) fn pair_avx2(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
        // SAFETY: the caller checked `is_x86_feature_detected!("avx2")`.
        unsafe { pair_avx2_inner(i, inv_h2, o) }
    }
    pub(super) fn ground_avx2(i: &GroundLanes, g: f32, inv_h2: f32, o: &mut GroundSolved) {
        // SAFETY: as above.
        unsafe { ground_avx2_inner(i, g, inv_h2, o) }
    }
    pub(super) fn pair_avx512(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
        // SAFETY: the caller checked `is_x86_feature_detected!("avx512f")`.
        unsafe { pair_avx512_inner(i, inv_h2, o) }
    }
    pub(super) fn ground_avx512(i: &GroundLanes, g: f32, inv_h2: f32, o: &mut GroundSolved) {
        // SAFETY: as above.
        unsafe { ground_avx512_inner(i, g, inv_h2, o) }
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod arm {
    use super::*;
    use std::arch::aarch64::*;

    #[derive(Clone, Copy)]
    struct Neon(float32x4_t);

    #[inline(always)]
    unsafe fn mask(m: uint32x4_t) -> Neon {
        unsafe { Neon(vreinterpretq_f32_u32(m)) }
    }
    #[inline(always)]
    unsafe fn bits_of(v: Neon) -> uint32x4_t {
        unsafe { vreinterpretq_u32_f32(v.0) }
    }

    // SAFETY (whole impl): only reached from `#[target_feature(enable =
    // "neon")]` functions after a runtime check. NEON's FADD/FSUB/FMUL/FDIV/
    // FSQRT are the IEEE-754 operations in the FPCR rounding mode
    // (round-to-nearest by default, the same mode scalar code uses); flush-
    // to-zero is off by default on AArch64 Linux, macOS and Windows.
    impl Lanes for Neon {
        const WIDTH: usize = 4;
        #[inline(always)]
        unsafe fn load(p: *const f32) -> Self {
            unsafe { Self(vld1q_f32(p)) }
        }
        #[inline(always)]
        unsafe fn store(self, p: *mut f32) {
            unsafe { vst1q_f32(p, self.0) }
        }
        #[inline(always)]
        unsafe fn splat(x: f32) -> Self {
            unsafe { Self(vdupq_n_f32(x)) }
        }
        #[inline(always)]
        unsafe fn add(self, o: Self) -> Self {
            unsafe { Self(vaddq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn sub(self, o: Self) -> Self {
            unsafe { Self(vsubq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn mul(self, o: Self) -> Self {
            unsafe { Self(vmulq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn div(self, o: Self) -> Self {
            unsafe { Self(vdivq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn sqrt(self) -> Self {
            unsafe { Self(vsqrtq_f32(self.0)) }
        }
        #[inline(always)]
        unsafe fn neg(self) -> Self {
            unsafe { Self(vnegq_f32(self.0)) }
        }
        #[inline(always)]
        unsafe fn gt(self, o: Self) -> Self {
            unsafe { mask(vcgtq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn ge(self, o: Self) -> Self {
            unsafe { mask(vcgeq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn le(self, o: Self) -> Self {
            unsafe { mask(vcleq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn eq(self, o: Self) -> Self {
            unsafe { mask(vceqq_f32(self.0, o.0)) }
        }
        #[inline(always)]
        unsafe fn and(self, o: Self) -> Self {
            unsafe { mask(vandq_u32(bits_of(self), bits_of(o))) }
        }
        #[inline(always)]
        unsafe fn or(self, o: Self) -> Self {
            unsafe { mask(vorrq_u32(bits_of(self), bits_of(o))) }
        }
        #[inline(always)]
        unsafe fn andnot(self, o: Self) -> Self {
            // vbicq(a, b) = a & !b.
            unsafe { mask(vbicq_u32(bits_of(o), bits_of(self))) }
        }
        #[inline(always)]
        unsafe fn select(m: Self, a: Self, b: Self) -> Self {
            unsafe { Self(vbslq_f32(bits_of(m), a.0, b.0)) }
        }
        #[inline(always)]
        unsafe fn bits(self) -> u32 {
            unsafe {
                let weights: [u32; 4] = [1, 2, 4, 8];
                let w = vld1q_u32(weights.as_ptr());
                vaddvq_u32(vandq_u32(bits_of(self), w))
            }
        }
        #[inline(always)]
        unsafe fn from_bits(bits: u16, base: usize) -> Self {
            unsafe {
                let b = vdupq_n_u32(u32::from(bits >> base & 0xf));
                let weights: [u32; 4] = [1, 2, 4, 8];
                let w = vld1q_u32(weights.as_ptr());
                mask(vceqq_u32(vandq_u32(b, w), w))
            }
        }
    }

    #[target_feature(enable = "neon")]
    unsafe fn pair_neon_inner(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
        unsafe { pair_kernel::<Neon>(i, inv_h2, o) }
    }
    #[target_feature(enable = "neon")]
    unsafe fn ground_neon_inner(i: &GroundLanes, g: f32, inv_h2: f32, o: &mut GroundSolved) {
        unsafe { ground_kernel::<Neon>(i, g, inv_h2, o) }
    }

    pub(super) fn pair_neon(i: &PairLanes, inv_h2: f32, o: &mut PairSolved) {
        // SAFETY: the caller checked `is_aarch64_feature_detected!("neon")`.
        unsafe { pair_neon_inner(i, inv_h2, o) }
    }
    pub(super) fn ground_neon(i: &GroundLanes, g: f32, inv_h2: f32, o: &mut GroundSolved) {
        // SAFETY: as above.
        unsafe { ground_neon_inner(i, g, inv_h2, o) }
    }
}
