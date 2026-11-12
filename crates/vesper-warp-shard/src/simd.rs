//! Sixteen-lane kernels with runtime ISA dispatch.
//!
//! This is the only module of the crate allowed to use `unsafe`. Every
//! `unsafe` block is one of two kinds:
//!
//! 1. An unaligned vector load or store from a `&[T; 16]` / `&mut [T; 16]`
//!    array at a constant lane offset that keeps the whole vector inside the
//!    array. The offsets are written out per call so each bound can be read
//!    against the array length (16 lanes).
//! 2. A call into a `#[target_feature]` function. Those calls happen only
//!    through [`Kernels`], whose private `path` field is set solely by
//!    [`Kernels::new`] after [`IsaPath::is_supported`] confirmed the feature
//!    on the running CPU, so the feature precondition holds.
//!
//! The scalar implementations in [`scalar`] are the reference semantics; the
//! test suite checks every supported path against them.

#![allow(unsafe_code)]

use core::fmt;

/// Number of connection lanes per tile.
pub const LANES: usize = 16;

/// Instruction-set path used by the tile kernels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IsaPath {
    /// Portable scalar reference loops.
    Scalar,
    /// x86_64 SSE2 (128-bit) intrinsics.
    Sse2,
    /// x86_64 AVX2 (256-bit) intrinsics.
    Avx2,
    /// AArch64 Advanced SIMD (NEON, 128-bit) intrinsics.
    Neon,
}

impl IsaPath {
    /// All paths in preference order (best last).
    pub const ALL: [IsaPath; 4] = [IsaPath::Scalar, IsaPath::Sse2, IsaPath::Neon, IsaPath::Avx2];

    /// Whether this path can run on the current CPU (runtime detection).
    pub fn is_supported(self) -> bool {
        match self {
            IsaPath::Scalar => true,
            IsaPath::Sse2 => x86_feature_sse2(),
            IsaPath::Avx2 => x86_feature_avx2(),
            IsaPath::Neon => aarch64_feature_neon(),
        }
    }

    /// The widest supported path on the current CPU.
    pub fn detect() -> IsaPath {
        IsaPath::ALL
            .iter()
            .rev()
            .copied()
            .find(|p| p.is_supported())
            .unwrap_or(IsaPath::Scalar)
    }

    /// Stable lowercase name used in reports.
    pub fn name(self) -> &'static str {
        match self {
            IsaPath::Scalar => "scalar",
            IsaPath::Sse2 => "sse2",
            IsaPath::Avx2 => "avx2",
            IsaPath::Neon => "neon",
        }
    }
}

impl fmt::Display for IsaPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(target_arch = "x86_64")]
fn x86_feature_sse2() -> bool {
    std::arch::is_x86_feature_detected!("sse2")
}
#[cfg(not(target_arch = "x86_64"))]
fn x86_feature_sse2() -> bool {
    false
}
#[cfg(target_arch = "x86_64")]
fn x86_feature_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}
#[cfg(not(target_arch = "x86_64"))]
fn x86_feature_avx2() -> bool {
    false
}
#[cfg(target_arch = "aarch64")]
fn aarch64_feature_neon() -> bool {
    std::arch::is_aarch64_feature_detected!("neon")
}
#[cfg(not(target_arch = "aarch64"))]
fn aarch64_feature_neon() -> bool {
    false
}

/// The requested ISA path is not available on this CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedIsa(pub IsaPath);

impl fmt::Display for UnsupportedIsa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ISA path {} is not supported on this CPU", self.0)
    }
}

impl std::error::Error for UnsupportedIsa {}

/// A validated kernel dispatcher.
///
/// Invariant: `path.is_supported()` was true when this value was built. The
/// field is private, so the only constructors are [`Kernels::new`],
/// [`Kernels::detect`] and [`Kernels::scalar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kernels {
    path: IsaPath,
}

impl Kernels {
    /// Builds a dispatcher for `path`, or reports that the CPU lacks it.
    pub fn new(path: IsaPath) -> Result<Kernels, UnsupportedIsa> {
        if path.is_supported() {
            Ok(Kernels { path })
        } else {
            Err(UnsupportedIsa(path))
        }
    }

    /// Dispatcher for the widest supported path.
    pub fn detect() -> Kernels {
        Kernels {
            path: IsaPath::detect(),
        }
    }

    /// The scalar reference dispatcher.
    pub const fn scalar() -> Kernels {
        Kernels {
            path: IsaPath::Scalar,
        }
    }

    /// The path this dispatcher executes.
    pub fn path(self) -> IsaPath {
        self.path
    }

    /// Bit `i` set iff `lanes[i] == key`.
    pub fn eq_mask_u32(self, lanes: &[u32; LANES], key: u32) -> u16 {
        match self.path {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: SSE2 was detected.
            IsaPath::Sse2 => unsafe { x86::eq_mask_u32_sse2(lanes, key) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: AVX2 was detected.
            IsaPath::Avx2 => unsafe { x86::eq_mask_u32_avx2(lanes, key) },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: `Kernels` invariant: NEON was detected.
            IsaPath::Neon => unsafe { arm::eq_mask_u32_neon(lanes, key) },
            _ => scalar::eq_mask_u32(lanes, key),
        }
    }

    /// Bit `i` set iff `a[i] > b[i]` (unsigned).
    pub fn gt_mask_u64(self, a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        match self.path {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: SSE2 was detected.
            IsaPath::Sse2 => unsafe { x86::gt_mask_u64_sse2(a, b) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: AVX2 was detected.
            IsaPath::Avx2 => unsafe { x86::gt_mask_u64_avx2(a, b) },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: `Kernels` invariant: NEON was detected.
            IsaPath::Neon => unsafe { arm::gt_mask_u64_neon(a, b) },
            _ => scalar::gt_mask_u64(a, b),
        }
    }

    /// Bit `i` set iff `lanes[i] <= bound` (unsigned).
    pub fn le_mask_u64(self, lanes: &[u64; LANES], bound: u64) -> u16 {
        !self.gt_mask_u64(lanes, &[bound; LANES])
    }

    /// `acc[i] = max(acc[i], other[i])` (unsigned).
    pub fn max_u64(self, acc: &mut [u64; LANES], other: &[u64; LANES]) {
        match self.path {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: SSE2 was detected.
            IsaPath::Sse2 => unsafe { x86::max_u64_sse2(acc, other) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: AVX2 was detected.
            IsaPath::Avx2 => unsafe { x86::max_u64_avx2(acc, other) },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: `Kernels` invariant: NEON was detected.
            IsaPath::Neon => unsafe { arm::max_u64_neon(acc, other) },
            _ => scalar::max_u64(acc, other),
        }
    }

    /// `acc[i] = acc[i].wrapping_add(other[i])`. Callers bound the operands
    /// so that wrapping cannot occur (see `table::stage_apply`).
    pub fn add_u64(self, acc: &mut [u64; LANES], other: &[u64; LANES]) {
        match self.path {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: SSE2 was detected.
            IsaPath::Sse2 => unsafe { x86::add_u64_sse2(acc, other) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: AVX2 was detected.
            IsaPath::Avx2 => unsafe { x86::add_u64_avx2(acc, other) },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: `Kernels` invariant: NEON was detected.
            IsaPath::Neon => unsafe { arm::add_u64_neon(acc, other) },
            _ => scalar::add_u64(acc, other),
        }
    }

    /// `acc[i] = acc[i].wrapping_sub(other[i])`. Lanes that would wrap are
    /// detected by the caller with [`Kernels::gt_mask_u64`] beforehand and
    /// their results discarded.
    pub fn sub_u64(self, acc: &mut [u64; LANES], other: &[u64; LANES]) {
        match self.path {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: SSE2 was detected.
            IsaPath::Sse2 => unsafe { x86::sub_u64_sse2(acc, other) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernels` invariant: AVX2 was detected.
            IsaPath::Avx2 => unsafe { x86::sub_u64_avx2(acc, other) },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: `Kernels` invariant: NEON was detected.
            IsaPath::Neon => unsafe { arm::sub_u64_neon(acc, other) },
            _ => scalar::sub_u64(acc, other),
        }
    }
}

/// Scalar reference kernels.
pub mod scalar {
    use super::LANES;

    /// Bit `i` set iff `lanes[i] == key`.
    pub fn eq_mask_u32(lanes: &[u32; LANES], key: u32) -> u16 {
        lanes
            .iter()
            .enumerate()
            .fold(0u16, |m, (i, &v)| m | (u16::from(v == key) << i))
    }

    /// Bit `i` set iff `a[i] > b[i]`.
    pub fn gt_mask_u64(a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        (0..LANES).fold(0u16, |m, i| m | (u16::from(a[i] > b[i]) << i))
    }

    /// `acc[i] = max(acc[i], other[i])`.
    pub fn max_u64(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for (a, &o) in acc.iter_mut().zip(other) {
            *a = (*a).max(o);
        }
    }

    /// Lane-wise wrapping add.
    pub fn add_u64(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for (a, &o) in acc.iter_mut().zip(other) {
            *a = a.wrapping_add(o);
        }
    }

    /// Lane-wise wrapping subtract.
    pub fn sub_u64(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for (a, &o) in acc.iter_mut().zip(other) {
            *a = a.wrapping_sub(o);
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::LANES;
    use std::arch::x86_64::*;

    // ---- SSE2: 4 x u32 or 2 x u64 per register ----

    #[target_feature(enable = "sse2")]
    fn load128_u32(lanes: &[u32; LANES], at: usize) -> __m128i {
        assert!(at + 4 <= LANES);
        // SAFETY: lanes `at..at + 4` are inside the 16-lane array (checked
        // above; every caller passes a constant multiple of 4 below 16).
        unsafe { _mm_loadu_si128(lanes.as_ptr().add(at).cast()) }
    }

    #[target_feature(enable = "sse2")]
    fn load128_u64(lanes: &[u64; LANES], at: usize) -> __m128i {
        assert!(at + 2 <= LANES);
        // SAFETY: lanes `at..at + 2` are inside the 16-lane array.
        unsafe { _mm_loadu_si128(lanes.as_ptr().add(at).cast()) }
    }

    #[target_feature(enable = "sse2")]
    fn store128_u64(lanes: &mut [u64; LANES], at: usize, v: __m128i) {
        assert!(at + 2 <= LANES);
        // SAFETY: lanes `at..at + 2` are inside the 16-lane array and the
        // exclusive borrow guarantees no aliasing.
        unsafe { _mm_storeu_si128(lanes.as_mut_ptr().add(at).cast(), v) }
    }

    #[target_feature(enable = "sse2")]
    pub(super) fn eq_mask_u32_sse2(lanes: &[u32; LANES], key: u32) -> u16 {
        let k = _mm_set1_epi32(key as i32);
        let mut mask = 0u16;
        for q in 0..4 {
            let eq = _mm_cmpeq_epi32(load128_u32(lanes, q * 4), k);
            let bits = _mm_movemask_ps(_mm_castsi128_ps(eq)) as u16;
            mask |= bits << (q * 4);
        }
        mask
    }

    /// Unsigned 64-bit `a > b` emulated with 32-bit SSE2 compares: a lane is
    /// greater iff its high dword is greater, or the high dwords are equal and
    /// the low dword is greater (dwords compared unsigned via a sign flip).
    #[target_feature(enable = "sse2")]
    fn gt_u64_sse2(a: __m128i, b: __m128i) -> __m128i {
        let sign = _mm_set1_epi32(i32::MIN);
        let gt = _mm_cmpgt_epi32(_mm_xor_si128(a, sign), _mm_xor_si128(b, sign));
        let eq = _mm_cmpeq_epi32(a, b);
        let gt_hi = _mm_shuffle_epi32::<0b11_11_01_01>(gt);
        let gt_lo = _mm_shuffle_epi32::<0b10_10_00_00>(gt);
        let eq_hi = _mm_shuffle_epi32::<0b11_11_01_01>(eq);
        _mm_or_si128(gt_hi, _mm_and_si128(eq_hi, gt_lo))
    }

    #[target_feature(enable = "sse2")]
    pub(super) fn gt_mask_u64_sse2(a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        let mut mask = 0u16;
        for p in 0..8 {
            let gt = gt_u64_sse2(load128_u64(a, p * 2), load128_u64(b, p * 2));
            let bits = _mm_movemask_pd(_mm_castsi128_pd(gt)) as u16;
            mask |= bits << (p * 2);
        }
        mask
    }

    #[target_feature(enable = "sse2")]
    pub(super) fn max_u64_sse2(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for p in 0..8 {
            let a = load128_u64(acc, p * 2);
            let o = load128_u64(other, p * 2);
            let gt = gt_u64_sse2(a, o);
            let m = _mm_or_si128(_mm_and_si128(gt, a), _mm_andnot_si128(gt, o));
            store128_u64(acc, p * 2, m);
        }
    }

    #[target_feature(enable = "sse2")]
    pub(super) fn add_u64_sse2(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for p in 0..8 {
            let s = _mm_add_epi64(load128_u64(acc, p * 2), load128_u64(other, p * 2));
            store128_u64(acc, p * 2, s);
        }
    }

    #[target_feature(enable = "sse2")]
    pub(super) fn sub_u64_sse2(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for p in 0..8 {
            let s = _mm_sub_epi64(load128_u64(acc, p * 2), load128_u64(other, p * 2));
            store128_u64(acc, p * 2, s);
        }
    }

    // ---- AVX2: 8 x u32 or 4 x u64 per register ----

    #[target_feature(enable = "avx2")]
    fn load256_u32(lanes: &[u32; LANES], at: usize) -> __m256i {
        assert!(at + 8 <= LANES);
        // SAFETY: lanes `at..at + 8` are inside the 16-lane array.
        unsafe { _mm256_loadu_si256(lanes.as_ptr().add(at).cast()) }
    }

    #[target_feature(enable = "avx2")]
    fn load256_u64(lanes: &[u64; LANES], at: usize) -> __m256i {
        assert!(at + 4 <= LANES);
        // SAFETY: lanes `at..at + 4` are inside the 16-lane array.
        unsafe { _mm256_loadu_si256(lanes.as_ptr().add(at).cast()) }
    }

    #[target_feature(enable = "avx2")]
    fn store256_u64(lanes: &mut [u64; LANES], at: usize, v: __m256i) {
        assert!(at + 4 <= LANES);
        // SAFETY: lanes `at..at + 4` are inside the 16-lane array and the
        // exclusive borrow guarantees no aliasing.
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().add(at).cast(), v) }
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn eq_mask_u32_avx2(lanes: &[u32; LANES], key: u32) -> u16 {
        let k = _mm256_set1_epi32(key as i32);
        let lo = _mm256_cmpeq_epi32(load256_u32(lanes, 0), k);
        let hi = _mm256_cmpeq_epi32(load256_u32(lanes, 8), k);
        let lo = _mm256_movemask_ps(_mm256_castsi256_ps(lo)) as u16;
        let hi = _mm256_movemask_ps(_mm256_castsi256_ps(hi)) as u16;
        lo | (hi << 8)
    }

    /// Unsigned 64-bit `a > b` via the signed AVX2 compare after flipping the
    /// sign bit of both operands.
    #[target_feature(enable = "avx2")]
    fn gt_u64_avx2(a: __m256i, b: __m256i) -> __m256i {
        let sign = _mm256_set1_epi64x(i64::MIN);
        _mm256_cmpgt_epi64(_mm256_xor_si256(a, sign), _mm256_xor_si256(b, sign))
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn gt_mask_u64_avx2(a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        let mut mask = 0u16;
        for q in 0..4 {
            let gt = gt_u64_avx2(load256_u64(a, q * 4), load256_u64(b, q * 4));
            let bits = _mm256_movemask_pd(_mm256_castsi256_pd(gt)) as u16;
            mask |= bits << (q * 4);
        }
        mask
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn max_u64_avx2(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for q in 0..4 {
            let a = load256_u64(acc, q * 4);
            let o = load256_u64(other, q * 4);
            let gt = gt_u64_avx2(a, o);
            store256_u64(acc, q * 4, _mm256_blendv_epi8(o, a, gt));
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn add_u64_avx2(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for q in 0..4 {
            let s = _mm256_add_epi64(load256_u64(acc, q * 4), load256_u64(other, q * 4));
            store256_u64(acc, q * 4, s);
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn sub_u64_avx2(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for q in 0..4 {
            let s = _mm256_sub_epi64(load256_u64(acc, q * 4), load256_u64(other, q * 4));
            store256_u64(acc, q * 4, s);
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::LANES;
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    fn load_u32(lanes: &[u32; LANES], at: usize) -> uint32x4_t {
        assert!(at + 4 <= LANES);
        // SAFETY: lanes `at..at + 4` are inside the 16-lane array.
        unsafe { vld1q_u32(lanes.as_ptr().add(at)) }
    }

    #[target_feature(enable = "neon")]
    fn load_u64(lanes: &[u64; LANES], at: usize) -> uint64x2_t {
        assert!(at + 2 <= LANES);
        // SAFETY: lanes `at..at + 2` are inside the 16-lane array.
        unsafe { vld1q_u64(lanes.as_ptr().add(at)) }
    }

    #[target_feature(enable = "neon")]
    fn store_u64(lanes: &mut [u64; LANES], at: usize, v: uint64x2_t) {
        assert!(at + 2 <= LANES);
        // SAFETY: lanes `at..at + 2` are inside the 16-lane array and the
        // exclusive borrow guarantees no aliasing.
        unsafe { vst1q_u64(lanes.as_mut_ptr().add(at), v) }
    }

    #[target_feature(enable = "neon")]
    pub(super) fn eq_mask_u32_neon(lanes: &[u32; LANES], key: u32) -> u16 {
        let k = vdupq_n_u32(key);
        let weights: [u32; 4] = [1, 2, 4, 8];
        // SAFETY: `weights` has exactly four u32 lanes.
        let w = unsafe { vld1q_u32(weights.as_ptr()) };
        let mut mask = 0u16;
        for q in 0..4 {
            let eq = vceqq_u32(load_u32(lanes, q * 4), k);
            let bits = vaddvq_u32(vandq_u32(eq, w)) as u16;
            mask |= bits << (q * 4);
        }
        mask
    }

    #[target_feature(enable = "neon")]
    fn movemask_u64(v: uint64x2_t) -> u16 {
        ((vgetq_lane_u64::<0>(v) & 1) | ((vgetq_lane_u64::<1>(v) & 1) << 1)) as u16
    }

    #[target_feature(enable = "neon")]
    pub(super) fn gt_mask_u64_neon(a: &[u64; LANES], b: &[u64; LANES]) -> u16 {
        let mut mask = 0u16;
        for p in 0..8 {
            let gt = vcgtq_u64(load_u64(a, p * 2), load_u64(b, p * 2));
            mask |= movemask_u64(gt) << (p * 2);
        }
        mask
    }

    #[target_feature(enable = "neon")]
    pub(super) fn max_u64_neon(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for p in 0..8 {
            let a = load_u64(acc, p * 2);
            let o = load_u64(other, p * 2);
            store_u64(acc, p * 2, vbslq_u64(vcgtq_u64(a, o), a, o));
        }
    }

    #[target_feature(enable = "neon")]
    pub(super) fn add_u64_neon(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for p in 0..8 {
            let s = vaddq_u64(load_u64(acc, p * 2), load_u64(other, p * 2));
            store_u64(acc, p * 2, s);
        }
    }

    #[target_feature(enable = "neon")]
    pub(super) fn sub_u64_neon(acc: &mut [u64; LANES], other: &[u64; LANES]) {
        for p in 0..8 {
            let s = vsubq_u64(load_u64(acc, p * 2), load_u64(other, p * 2));
            store_u64(acc, p * 2, s);
        }
    }
}
