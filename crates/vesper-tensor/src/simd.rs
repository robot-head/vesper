//! Dot-product and AXPY kernels: a scalar reference and `std::arch` SIMD.
//!
//! [`Kernel::Scalar`] accumulates a dot product sequentially in ascending
//! index order, exactly as the original llama2.c-order kernels do.
//!
//! [`Kernel::Simd`] uses eight independent FP32 lane accumulators: element
//! `8k + i` is multiplied and added into lane `i` (a separate rounded multiply
//! and add, never a fused multiply-add), elements past the last whole group of
//! eight go into a sequential tail sum, and [`DotAcc::finish`] adds lanes 0..8
//! in order and then the tail. That order is fixed, so the AVX2 (x86_64) and
//! NEON (AArch64) implementations and the portable scalar emulation
//! [`lanes_reference_dot`] give bit-identical results; tests check this.
//! Hosts without either instruction set run the emulation.
//!
//! Element-wise kernels ([`axpy`], [`scale`]) perform one rounding per
//! element, so their SIMD and scalar results are always identical.
//!
//! `unsafe` here is limited to `std::arch` intrinsics behind runtime feature
//! detection (AVX2) or the AArch64 baseline (NEON), with unaligned loads from
//! slices whose lengths are checked first.
#![allow(unsafe_code)]

/// Which arithmetic schedule a kernel uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kernel {
    /// Sequential ascending-order accumulation (the reference).
    Scalar,
    /// Eight-lane accumulation through `std::arch` where available.
    Simd,
}

/// The instruction set [`Kernel::Simd`] uses on this host: `"avx2"`,
/// `"neon"`, or `None` when it falls back to the portable lane emulation.
pub fn simd_isa() -> Option<&'static str> {
    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            return Some("avx2");
        }
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    {
        return Some("neon");
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(all(target_arch = "x86_64", target_endian = "little"))]
fn avx2() -> bool {
    std::is_x86_feature_detected!("avx2")
}

/// Element encoding of a byte operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteElements {
    /// Little-endian IEEE binary32.
    F32Le,
    /// Little-endian BF16 (upper half of a binary32).
    Bf16Le,
}
impl ByteElements {
    pub const fn width(self) -> usize {
        match self {
            Self::F32Le => 4,
            Self::Bf16Le => 2,
        }
    }
    #[inline]
    fn get(self, bytes: &[u8], i: usize) -> f32 {
        match self {
            Self::F32Le => {
                let b = &bytes[i * 4..i * 4 + 4];
                f32::from_le_bytes([b[0], b[1], b[2], b[3]])
            }
            Self::Bf16Le => f32::from_bits(
                u32::from(u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]])) << 16,
            ),
        }
    }
}

/// A partially accumulated dot product (see the module documentation for the
/// exact order). Feed it with [`dot_acc`] or [`dot_acc_bytes`] in
/// consecutive pieces whose lengths are multiples of eight (except the last)
/// and read it with [`DotAcc::finish`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DotAcc {
    lanes: [f32; 8],
    tail: f32,
}
impl DotAcc {
    pub const ZERO: Self = Self {
        lanes: [0.0; 8],
        tail: 0.0,
    };
    pub fn finish(&self, kernel: Kernel) -> f32 {
        match kernel {
            Kernel::Scalar => self.tail,
            Kernel::Simd => {
                let mut sum = self.lanes[0];
                for lane in &self.lanes[1..] {
                    sum += lane;
                }
                sum + self.tail
            }
        }
    }
}

/// Accumulates `a · b` (equal lengths, checked) into `acc`.
pub fn dot_acc(kernel: Kernel, acc: &mut DotAcc, a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "dot operand lengths");
    match kernel {
        Kernel::Scalar => {
            for (x, y) in a.iter().zip(b) {
                acc.tail += x * y;
            }
        }
        Kernel::Simd => {
            let whole = a.len() / 8 * 8;
            let done = simd_dot_f32(&mut acc.lanes, &a[..whole], &b[..whole]);
            if !done {
                for (i, (x, y)) in a[..whole].iter().zip(&b[..whole]).enumerate() {
                    acc.lanes[i % 8] += x * y;
                }
            }
            for (x, y) in a[whole..].iter().zip(&b[whole..]) {
                acc.tail += x * y;
            }
        }
    }
}

/// Accumulates `a · decode(bytes)`; `bytes` holds exactly `a.len()` elements.
pub fn dot_acc_bytes(
    kernel: Kernel,
    acc: &mut DotAcc,
    a: &[f32],
    bytes: &[u8],
    elements: ByteElements,
) {
    assert_eq!(
        a.len() * elements.width(),
        bytes.len(),
        "dot operand lengths"
    );
    match kernel {
        Kernel::Scalar => {
            for (i, x) in a.iter().enumerate() {
                acc.tail += x * elements.get(bytes, i);
            }
        }
        Kernel::Simd => {
            let whole = a.len() / 8 * 8;
            let done = simd_dot_bytes(
                &mut acc.lanes,
                &a[..whole],
                &bytes[..whole * elements.width()],
                elements,
            );
            if !done {
                for (i, x) in a[..whole].iter().enumerate() {
                    acc.lanes[i % 8] += x * elements.get(bytes, i);
                }
            }
            for (i, x) in a.iter().enumerate().skip(whole) {
                acc.tail += x * elements.get(bytes, i);
            }
        }
    }
}

/// `a · b` with the given kernel.
pub fn dot(kernel: Kernel, a: &[f32], b: &[f32]) -> f32 {
    let mut acc = DotAcc::ZERO;
    dot_acc(kernel, &mut acc, a, b);
    acc.finish(kernel)
}

/// The portable emulation of [`Kernel::Simd`]'s dot product, written without
/// `std::arch`: the reference the intrinsics are tested against.
pub fn lanes_reference_dot(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "dot operand lengths");
    let mut lanes = [0.0f32; 8];
    let whole = a.len() / 8 * 8;
    for i in 0..whole {
        lanes[i % 8] += a[i] * b[i];
    }
    let mut tail = 0.0f32;
    for i in whole..a.len() {
        tail += a[i] * b[i];
    }
    let mut sum = lanes[0];
    for lane in &lanes[1..] {
        sum += lane;
    }
    sum + tail
}

/// `out[i] += alpha * v[i]` (one rounding of the product, one of the sum).
pub fn axpy(kernel: Kernel, out: &mut [f32], alpha: f32, v: &[f32]) {
    assert_eq!(out.len(), v.len(), "axpy operand lengths");
    if kernel == Kernel::Simd && simd_axpy_f32(out, alpha, v) {
        return;
    }
    for (o, v) in out.iter_mut().zip(v) {
        *o += alpha * v;
    }
}

/// `out[i] += alpha * decode(bytes)[i]`.
pub fn axpy_bytes(
    kernel: Kernel,
    out: &mut [f32],
    alpha: f32,
    bytes: &[u8],
    elements: ByteElements,
) {
    assert_eq!(
        out.len() * elements.width(),
        bytes.len(),
        "axpy operand lengths"
    );
    let whole = if kernel == Kernel::Simd && simd_axpy_bytes(out, alpha, bytes, elements) {
        out.len() / 8 * 8
    } else {
        0
    };
    for (i, o) in out.iter_mut().enumerate().skip(whole) {
        *o += alpha * elements.get(bytes, i);
    }
}

/// `out[i] *= factor`.
pub fn scale(kernel: Kernel, out: &mut [f32], factor: f32) {
    let _ = kernel; // one rounding per element: every schedule is identical
    for o in out.iter_mut() {
        *o *= factor;
    }
}

// ---------------------------------------------------------------------------
// Dispatch. Each returns false when no intrinsic path exists on this host, so
// the caller runs the portable emulation of the same schedule.

fn simd_dot_f32(lanes: &mut [f32; 8], a: &[f32], b: &[f32]) -> bool {
    debug_assert!(a.len() == b.len() && a.len().is_multiple_of(8));
    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    if avx2() {
        // SAFETY: AVX2 was detected at run time; lengths are equal multiples
        // of eight, checked by the caller.
        unsafe { x86::dot_f32(lanes, a, b) };
        return true;
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    {
        // SAFETY: NEON is part of the AArch64 baseline; lengths as above.
        unsafe { arm::dot_f32(lanes, a, b) };
        return true;
    }
    #[allow(unreachable_code)]
    {
        let _ = (lanes, a, b);
        false
    }
}

fn simd_dot_bytes(lanes: &mut [f32; 8], a: &[f32], bytes: &[u8], e: ByteElements) -> bool {
    debug_assert!(a.len().is_multiple_of(8) && bytes.len() == a.len() * e.width());
    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    if avx2() {
        // SAFETY: AVX2 detected; `bytes` holds exactly `a.len()` elements.
        unsafe { x86::dot_bytes(lanes, a, bytes, e) };
        return true;
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    {
        // SAFETY: NEON baseline; lengths as above.
        unsafe { arm::dot_bytes(lanes, a, bytes, e) };
        return true;
    }
    #[allow(unreachable_code)]
    {
        let _ = (lanes, a, bytes, e);
        false
    }
}

fn simd_axpy_f32(out: &mut [f32], alpha: f32, v: &[f32]) -> bool {
    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    if avx2() {
        // SAFETY: AVX2 detected; equal lengths checked by the caller.
        unsafe { x86::axpy_f32(out, alpha, v) };
        return true;
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    {
        // SAFETY: NEON baseline; equal lengths checked by the caller.
        unsafe { arm::axpy_f32(out, alpha, v) };
        return true;
    }
    #[allow(unreachable_code)]
    {
        let _ = (out, alpha, v);
        false
    }
}

/// Handles the whole groups of eight; the caller finishes the tail.
fn simd_axpy_bytes(out: &mut [f32], alpha: f32, bytes: &[u8], e: ByteElements) -> bool {
    let whole = out.len() / 8 * 8;
    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    if avx2() {
        // SAFETY: AVX2 detected; `bytes` holds `out.len()` elements.
        unsafe { x86::axpy_bytes(&mut out[..whole], alpha, &bytes[..whole * e.width()], e) };
        return true;
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    {
        // SAFETY: NEON baseline; lengths as above.
        unsafe { arm::axpy_bytes(&mut out[..whole], alpha, &bytes[..whole * e.width()], e) };
        return true;
    }
    #[allow(unreachable_code)]
    {
        let _ = (whole, out, alpha, bytes, e);
        false
    }
}

#[cfg(all(target_arch = "x86_64", target_endian = "little"))]
mod x86 {
    use super::ByteElements;
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    pub unsafe fn dot_f32(lanes: &mut [f32; 8], a: &[f32], b: &[f32]) {
        unsafe {
            let mut acc = _mm256_loadu_ps(lanes.as_ptr());
            for (a, b) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0.iter()) {
                let prod = _mm256_mul_ps(_mm256_loadu_ps(a.as_ptr()), _mm256_loadu_ps(b.as_ptr()));
                acc = _mm256_add_ps(acc, prod);
            }
            _mm256_storeu_ps(lanes.as_mut_ptr(), acc);
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn load(bytes: &[u8], e: ByteElements) -> __m256 {
        unsafe {
            match e {
                ByteElements::F32Le => _mm256_loadu_ps(bytes.as_ptr().cast::<f32>()),
                ByteElements::Bf16Le => {
                    let halves = _mm_loadu_si128(bytes.as_ptr().cast::<__m128i>());
                    _mm256_castsi256_ps(_mm256_slli_epi32::<16>(_mm256_cvtepu16_epi32(halves)))
                }
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn dot_bytes(lanes: &mut [f32; 8], a: &[f32], bytes: &[u8], e: ByteElements) {
        unsafe {
            let mut acc = _mm256_loadu_ps(lanes.as_ptr());
            for (a, b) in a
                .as_chunks::<8>()
                .0
                .iter()
                .zip(bytes.chunks_exact(8 * e.width()))
            {
                acc = _mm256_add_ps(acc, _mm256_mul_ps(_mm256_loadu_ps(a.as_ptr()), load(b, e)));
            }
            _mm256_storeu_ps(lanes.as_mut_ptr(), acc);
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn axpy_f32(out: &mut [f32], alpha: f32, v: &[f32]) {
        unsafe {
            let a = _mm256_set1_ps(alpha);
            let whole = out.len() / 8 * 8;
            for (o, v) in out[..whole]
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip(v.as_chunks::<8>().0.iter())
            {
                let sum = _mm256_add_ps(
                    _mm256_loadu_ps(o.as_ptr()),
                    _mm256_mul_ps(a, _mm256_loadu_ps(v.as_ptr())),
                );
                _mm256_storeu_ps(o.as_mut_ptr(), sum);
            }
            for (o, v) in out[whole..].iter_mut().zip(&v[whole..]) {
                *o += alpha * v;
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn axpy_bytes(out: &mut [f32], alpha: f32, bytes: &[u8], e: ByteElements) {
        unsafe {
            let a = _mm256_set1_ps(alpha);
            for (o, b) in out
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip(bytes.chunks_exact(8 * e.width()))
            {
                let sum = _mm256_add_ps(_mm256_loadu_ps(o.as_ptr()), _mm256_mul_ps(a, load(b, e)));
                _mm256_storeu_ps(o.as_mut_ptr(), sum);
            }
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
mod arm {
    use super::ByteElements;
    use std::arch::aarch64::*;

    #[inline]
    unsafe fn load(bytes: &[u8], e: ByteElements) -> (float32x4_t, float32x4_t) {
        unsafe {
            match e {
                ByteElements::F32Le => {
                    let p = bytes.as_ptr().cast::<f32>();
                    (vld1q_f32(p), vld1q_f32(p.add(4)))
                }
                ByteElements::Bf16Le => {
                    let h = vld1q_u16(bytes.as_ptr().cast::<u16>());
                    (
                        vreinterpretq_f32_u32(vshlq_n_u32::<16>(vmovl_u16(vget_low_u16(h)))),
                        vreinterpretq_f32_u32(vshlq_n_u32::<16>(vmovl_high_u16(h))),
                    )
                }
            }
        }
    }

    pub unsafe fn dot_f32(lanes: &mut [f32; 8], a: &[f32], b: &[f32]) {
        unsafe {
            let mut lo = vld1q_f32(lanes.as_ptr());
            let mut hi = vld1q_f32(lanes.as_ptr().add(4));
            for (a, b) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0.iter()) {
                let (pa, pb) = (a.as_ptr(), b.as_ptr());
                lo = vaddq_f32(lo, vmulq_f32(vld1q_f32(pa), vld1q_f32(pb)));
                hi = vaddq_f32(hi, vmulq_f32(vld1q_f32(pa.add(4)), vld1q_f32(pb.add(4))));
            }
            vst1q_f32(lanes.as_mut_ptr(), lo);
            vst1q_f32(lanes.as_mut_ptr().add(4), hi);
        }
    }

    pub unsafe fn dot_bytes(lanes: &mut [f32; 8], a: &[f32], bytes: &[u8], e: ByteElements) {
        unsafe {
            let mut lo = vld1q_f32(lanes.as_ptr());
            let mut hi = vld1q_f32(lanes.as_ptr().add(4));
            for (a, b) in a
                .as_chunks::<8>()
                .0
                .iter()
                .zip(bytes.chunks_exact(8 * e.width()))
            {
                let (blo, bhi) = load(b, e);
                lo = vaddq_f32(lo, vmulq_f32(vld1q_f32(a.as_ptr()), blo));
                hi = vaddq_f32(hi, vmulq_f32(vld1q_f32(a.as_ptr().add(4)), bhi));
            }
            vst1q_f32(lanes.as_mut_ptr(), lo);
            vst1q_f32(lanes.as_mut_ptr().add(4), hi);
        }
    }

    pub unsafe fn axpy_f32(out: &mut [f32], alpha: f32, v: &[f32]) {
        unsafe {
            let a = vdupq_n_f32(alpha);
            let whole = out.len() / 4 * 4;
            for (o, v) in out[..whole]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(v.as_chunks::<4>().0.iter())
            {
                let sum = vaddq_f32(vld1q_f32(o.as_ptr()), vmulq_f32(a, vld1q_f32(v.as_ptr())));
                vst1q_f32(o.as_mut_ptr(), sum);
            }
            for (o, v) in out[whole..].iter_mut().zip(&v[whole..]) {
                *o += alpha * v;
            }
        }
    }

    pub unsafe fn axpy_bytes(out: &mut [f32], alpha: f32, bytes: &[u8], e: ByteElements) {
        unsafe {
            let a = vdupq_n_f32(alpha);
            for (o, b) in out
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip(bytes.chunks_exact(8 * e.width()))
            {
                let (blo, bhi) = load(b, e);
                let p = o.as_mut_ptr();
                vst1q_f32(p, vaddq_f32(vld1q_f32(p), vmulq_f32(a, blo)));
                vst1q_f32(p.add(4), vaddq_f32(vld1q_f32(p.add(4)), vmulq_f32(a, bhi)));
            }
        }
    }
}
