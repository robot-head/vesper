//! Q4_K, MXFP4 and FP8 (E4M3 / E5M2) element and block formats.
//!
//! * **Q4_K** is GGML's 256-element super-block of 144 bytes: binary16 `d`,
//!   binary16 `dmin`, 12 bytes packing eight 6-bit sub-block scales and eight
//!   6-bit sub-block minimums, then 128 bytes of 4-bit quants. Sub-block
//!   `2j` (elements `64j..64j+32`) uses the low nibbles of quant bytes
//!   `32j..32j+32` and sub-block `2j+1` their high nibbles. Element value:
//!   `(d * sc) * q - (dmin * m)`, exactly GGML's `dequantize_row_q4_K`.
//! * **MXFP4** is the OCP Microscaling FP4 format in GGML's 17-byte block: an
//!   E8M0 shared exponent `e` then 16 bytes of E2M1 codes, element `j` in the
//!   low nibble of byte `j` and element `j + 16` in its high nibble. Element
//!   value: `e2m1(code) * 2^(e - 127)`. GGML computes this as a doubled E2M1
//!   table times `2^(e - 128)`, which is the same number. `e = 0xFF` is the
//!   OCP NaN encoding and is rejected.
//! * **FP8** follows the OCP 8-bit floating point specification: E4M3 is the
//!   `FN` variant (bias 7, no infinities, `S.1111.111` is NaN, largest finite
//!   448) and E5M2 is IEEE-like (bias 15, infinities and NaNs). Weight
//!   matrices reject NaN and infinite codes.
//!
//! The Q4_K quantizer is a transliteration of llama.cpp's
//! `quantize_row_q4_K_ref` and `make_qkx2_quants` ([`quantize_q4_k`]) and
//! produces llama.cpp's bytes. The MXFP4 quantizer follows GGML / gguf-py's
//! rule with an exact exponent.
// The port-backed build (src/port.rs) replaces some Rust bodies, leaving
// their helpers unused there.
#![cfg_attr(vesper_port, allow(dead_code, unused_imports))]
use crate::host::decode_fp16;
use crate::quant::encode_fp16;
use crate::Error;

pub const QK_K: usize = 256;
pub const Q4_K_BYTES: usize = 144;
pub const MXFP4_BLOCK: usize = 32;
pub const MXFP4_BYTES: usize = 17;
/// GGML K-quant scales and minimums are 6 bits wide.
const K_SCALE_MASK: u8 = 0x3f;
/// The low nibble of a packed 4-bit pair.
const NIBBLE: u8 = 0x0f;
/// E8M0 (OCP MX v1.0 §5.4.1) reserves `0xff` for NaN.
const E8M0_NAN: u8 = 0xff;
/// `1.5 * 2^23`: adding it to an FP32 in `[-2^22, 2^22]` leaves the value
/// rounded to an integer in the low mantissa bits (GGML `nearest_int`).
const ROUND_MAGIC: f32 = 12_582_912.0;
/// The bias `ROUND_MAGIC` leaves in the low mantissa bits (`2^22`).
const ROUND_MAGIC_BIAS: i32 = 0x0040_0000;
/// The largest E8M0 exponent that is not NaN.
const E8M0_MAX_FINITE: i32 = 254;
/// FP8 (OCP 8-bit floating point v1.0) sign bit.
const FP8_SIGN: u8 = 0x80;
/// FP8 canonical NaN codes (without the sign bit): E4M3 has only S.1111.111,
/// E5M2's is the quiet NaN S.11111.10.
const E4M3_NAN: u8 = 0x7f;
const E5M2_NAN: u8 = 0x7e;

fn half(bytes: &[u8], at: usize) -> f32 {
    decode_fp16(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

/// GGML `get_scale_min_k4`: the 6-bit scale and minimum of sub-block `j`.
#[cfg(not(vesper_port))]
pub fn q4_k_scale_min(j: usize, scales: &[u8]) -> (u8, u8) {
    if j < 4 {
        (scales[j] & K_SCALE_MASK, scales[j + 4] & K_SCALE_MASK)
    } else {
        (
            (scales[j + 4] & NIBBLE) | ((scales[j - 4] >> 6) << 4),
            (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4),
        )
    }
}
#[cfg(vesper_port)]
pub fn q4_k_scale_min(j: usize, scales: &[u8]) -> (u8, u8) {
    crate::port::must(crate::port::binding::q4_k_scale_min(j as u64, scales))
}

/// Dequantizes one 144-byte Q4_K block into 256 values.
#[cfg(not(vesper_port))]
pub fn dequantize_q4_k_block(block: &[u8], out: &mut [f32]) {
    let d = half(block, 0);
    let dmin = half(block, 2);
    let scales = &block[4..16];
    let qs = &block[16..Q4_K_BYTES];
    for j in 0..4 {
        let (sc1, m1) = q4_k_scale_min(2 * j, scales);
        let (sc2, m2) = q4_k_scale_min(2 * j + 1, scales);
        let (d1, m1) = (d * f32::from(sc1), dmin * f32::from(m1));
        let (d2, m2) = (d * f32::from(sc2), dmin * f32::from(m2));
        let q = &qs[32 * j..32 * j + 32];
        for l in 0..32 {
            out[64 * j + l] = d1 * f32::from(q[l] & NIBBLE) - m1;
            out[64 * j + 32 + l] = d2 * f32::from(q[l] >> 4) - m2;
        }
    }
}
#[cfg(vesper_port)]
pub fn dequantize_q4_k_block(block: &[u8], out: &mut [f32]) {
    crate::port::must(crate::port::binding::dequantize_q4_k_block(
        block,
        crate::port::bits_mut(out),
    ));
}

/// Checks a Q4_K block's two scales are finite.
#[cfg(not(vesper_port))]
pub fn check_q4_k_block(block: &[u8]) -> Result<(), Error> {
    if half(block, 0).is_finite() && half(block, 2).is_finite() {
        Ok(())
    } else {
        Err(Error::NonFinite)
    }
}
#[cfg(vesper_port)]
pub fn check_q4_k_block(block: &[u8]) -> Result<(), Error> {
    if crate::port::must(crate::port::binding::check_q4_k_block(block)) {
        Ok(())
    } else {
        Err(Error::NonFinite)
    }
}

/// GGML `nearest_int`: round to nearest, ties to even, by adding
/// `1.5 * 2^23` in FP32 and reading the low mantissa bits. Exact for
/// `|v| <= 4194303`; outside that range it returns the same (meaningless)
/// bits as the C code, so callers keep GGML's behaviour bit for bit.
#[inline]
#[cfg(not(vesper_port))]
pub fn nearest_int(v: f32) -> i32 {
    let bits = (v + ROUND_MAGIC).to_bits() as i32;
    (bits & crate::host::ieee::F32_MANT_MASK as i32) - ROUND_MAGIC_BIAS
}
#[cfg(vesper_port)]
pub fn nearest_int(v: f32) -> i32 {
    crate::port::must(crate::port::binding::nearest_int(v.to_bits()))
}

/// GGML `make_qkx2_quants` for `x.len()` elements with non-negative
/// `weights`: an affine quantizer `x ≈ scale * L + min` with `L` in
/// `0..=nmax`, refined over `nstep + 1` candidate inverse scales
/// `(rmin + rdelta * is + nmax) / (max - min)` by weighted least squares.
/// Returns `(scale, -min)` and writes `l`. Every operation is FP32 in the C
/// source's order (no fused multiply-add), so the result is bit-identical to
/// a GGML build without FP contraction.
#[allow(clippy::too_many_arguments)]
#[cfg(not(vesper_port))]
pub fn make_qkx2_quants(
    nmax: i32,
    x: &[f32],
    weights: &[f32],
    l: &mut [u8],
    laux: &mut [u8],
    rmin: f32,
    rdelta: f32,
    nstep: i32,
    use_mad: bool,
) -> (f32, f32) {
    let n = x.len();
    let mut min = x[0];
    let mut max = x[0];
    let mut sum_w = weights[0];
    let mut sum_x = sum_w * x[0];
    for i in 1..n {
        if x[i] < min {
            min = x[i];
        }
        if x[i] > max {
            max = x[i];
        }
        let w = weights[i];
        sum_w += w;
        sum_x += w * x[i];
    }
    if min > 0.0 {
        min = 0.0;
    }
    if max == min {
        l[..n].fill(0);
        return (0.0, -min);
    }
    let nmax_f = nmax as f32;
    let mut iscale = nmax_f / (max - min);
    let mut scale = 1.0 / iscale;
    let err = |diff: f32| if use_mad { diff.abs() } else { diff * diff };
    let mut best_error = 0.0f32;
    for i in 0..n {
        let q = nearest_int(iscale * (x[i] - min)).clamp(0, nmax);
        l[i] = q as u8;
        let diff = scale * f32::from(l[i]) + min - x[i];
        best_error += weights[i] * err(diff);
    }
    if nstep < 1 {
        return (scale, -min);
    }
    for is in 0..=nstep {
        iscale = (rmin + rdelta * is as f32 + nmax_f) / (max - min);
        let (mut sum_l, mut sum_l2, mut sum_xl) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..n {
            let q = nearest_int(iscale * (x[i] - min)).clamp(0, nmax);
            laux[i] = q as u8;
            let w = weights[i];
            let lf = q as f32;
            sum_l += w * lf;
            sum_l2 += w * lf * lf;
            sum_xl += w * lf * x[i];
        }
        let d = sum_w * sum_l2 - sum_l * sum_l;
        if d > 0.0 {
            let mut this_scale = (sum_w * sum_xl - sum_x * sum_l) / d;
            let mut this_min = (sum_l2 * sum_x - sum_l * sum_xl) / d;
            if this_min > 0.0 {
                this_min = 0.0;
                this_scale = sum_xl / sum_l2;
            }
            let mut cur_error = 0.0f32;
            for i in 0..n {
                let diff = this_scale * f32::from(laux[i]) + this_min - x[i];
                cur_error += weights[i] * err(diff);
            }
            if cur_error < best_error {
                l[..n].copy_from_slice(&laux[..n]);
                best_error = cur_error;
                scale = this_scale;
                min = this_min;
            }
        }
    }
    (scale, -min)
}
#[cfg(vesper_port)]
#[allow(clippy::too_many_arguments)]
pub fn make_qkx2_quants(
    nmax: i32,
    x: &[f32],
    weights: &[f32],
    l: &mut [u8],
    laux: &mut [u8],
    rmin: f32,
    rdelta: f32,
    nstep: i32,
    use_mad: bool,
) -> (f32, f32) {
    let (scale, min) = crate::port::must(crate::port::binding::make_qkx2_quants(
        nmax,
        crate::port::bits(x),
        crate::port::bits(weights),
        l,
        laux,
        rmin.to_bits(),
        rdelta.to_bits(),
        nstep,
        use_mad,
    ));
    (f32::from_bits(scale), f32::from_bits(min))
}

/// Quantizes rows of 256-element super-blocks to Q4_K exactly as llama.cpp's
/// `quantize_row_q4_K_ref` (the path `llama-quantize` takes without an
/// importance matrix): per 32-element sub-block, weights
/// `sqrt(mean(x^2)) + |x|` and [`make_qkx2_quants`]`(15, …, -1, 0.1, 20)`;
/// 6-bit scale and minimum codes relative to the largest of each, `d` and
/// `dmin` rounded to binary16, and the 4-bit quants recomputed from the
/// stored parameters. The bytes equal llama.cpp's for the same FP32 input
/// (checked against a llama.cpp-quantized GGUF in `tests/kquant_interop.rs`).
#[cfg(not(vesper_port))]
pub fn quantize_q4_k(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    check_rows(data, cols, QK_K)?;
    let mut out = Vec::new();
    out.try_reserve_exact(data.len() / QK_K * Q4_K_BYTES)
        .map_err(|_| Error::Limit("allocation"))?;
    for x in data.as_chunks::<QK_K>().0.iter() {
        let block = quantize_q4_k_block(x);
        if !(half(&block, 0).is_finite() && half(&block, 2).is_finite()) {
            return Err(Error::Limit("Q4_K scale exceeds binary16"));
        }
        out.extend_from_slice(&block);
    }
    Ok(out)
}
#[cfg(vesper_port)]
pub fn quantize_q4_k(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    crate::port::check(
        crate::port::binding::quantize_q4_k(crate::port::bits(data), cols as u64),
        |fault| match fault {
            crate::port::Fault::Shape => Error::Shape("quantized rows need whole blocks"),
            crate::port::Fault::NonFinite => Error::NonFinite,
            _ => Error::Limit("Q4_K scale exceeds binary16"),
        },
    )
}

/// One llama.cpp `quantize_row_q4_K_ref` super-block.
#[cfg(not(vesper_port))]
pub fn quantize_q4_k_block(x: &[f32; QK_K]) -> [u8; Q4_K_BYTES] {
    let mut l = [0u8; QK_K];
    let mut laux = [0u8; 32];
    let mut weights = [0.0f32; 32];
    let mut mins = [0.0f32; 8];
    let mut scales = [0.0f32; 8];
    let mut max_scale = 0.0f32;
    let mut max_min = 0.0f32;
    for j in 0..8 {
        let sub = &x[32 * j..32 * j + 32];
        let mut sum_x2 = 0.0f32;
        for &v in sub {
            sum_x2 += v * v;
        }
        let av_x = (sum_x2 / 32.0).sqrt();
        for (w, &v) in weights.iter_mut().zip(sub) {
            *w = av_x + v.abs();
        }
        let (scale, min) = make_qkx2_quants(
            15,
            sub,
            &weights,
            &mut l[32 * j..32 * j + 32],
            &mut laux,
            -1.0,
            0.1,
            20,
            false,
        );
        scales[j] = scale;
        mins[j] = min;
        if scale > max_scale {
            max_scale = scale;
        }
        if min > max_min {
            max_min = min;
        }
    }
    let inv_scale = if max_scale > 0.0 {
        63.0 / max_scale
    } else {
        0.0
    };
    let inv_min = if max_min > 0.0 { 63.0 / max_min } else { 0.0 };
    let mut packed = [0u8; 12];
    for j in 0..8 {
        // C: `uint8_t ls = nearest_int(...)` truncates modulo 256 first.
        let ls = (nearest_int(inv_scale * scales[j]) as u8).min(63);
        let lm = (nearest_int(inv_min * mins[j]) as u8).min(63);
        if j < 4 {
            packed[j] = ls;
            packed[j + 4] = lm;
        } else {
            packed[j + 4] = (ls & NIBBLE) | ((lm & NIBBLE) << 4);
            packed[j - 4] |= (ls >> 4) << 6;
            packed[j] |= (lm >> 4) << 6;
        }
    }
    let d_bits = encode_fp16(max_scale / 63.0);
    let dmin_bits = encode_fp16(max_min / 63.0);
    let (d_all, dmin_all) = (decode_fp16(d_bits), decode_fp16(dmin_bits));
    for j in 0..8 {
        let (sc, m) = q4_k_scale_min(j, &packed);
        let d = d_all * f32::from(sc);
        if d == 0.0 {
            continue;
        }
        let dm = dmin_all * f32::from(m);
        for ii in 0..32 {
            let q = nearest_int((x[32 * j + ii] + dm) / d).clamp(0, 15);
            l[32 * j + ii] = q as u8;
        }
    }
    let mut block = [0u8; Q4_K_BYTES];
    block[0..2].copy_from_slice(&d_bits.to_le_bytes());
    block[2..4].copy_from_slice(&dmin_bits.to_le_bytes());
    block[4..16].copy_from_slice(&packed);
    for j in 0..4 {
        for i in 0..32 {
            block[16 + 32 * j + i] = l[64 * j + i] | (l[64 * j + 32 + i] << 4);
        }
    }
    block
}
#[cfg(vesper_port)]
pub fn quantize_q4_k_block(x: &[f32; QK_K]) -> [u8; Q4_K_BYTES] {
    let bytes = crate::port::must(crate::port::binding::quantize_q4_k_block(
        crate::port::bits(x),
    ));
    let mut block = [0u8; Q4_K_BYTES];
    block.copy_from_slice(&bytes);
    block
}

/// E2M1 code values, doubled (GGML `kvalues_mxfp4`).
pub const E2M1_DOUBLED: [i8; 16] = [0, 1, 2, 3, 4, 6, 8, 12, 0, -1, -2, -3, -4, -6, -8, -12];

/// `2^(e - 128)` exactly (GGML `ggml_e8m0_to_fp32_half`), for `e < 255`.
#[cfg(not(vesper_port))]
pub fn e8m0_half(e: u8) -> f32 {
    let bits = if e < 2 {
        0x0020_0000u32 << e
    } else {
        u32::from(e - 1) << 23
    };
    f32::from_bits(bits)
}
#[cfg(vesper_port)]
pub fn e8m0_half(e: u8) -> f32 {
    f32::from_bits(crate::port::must(crate::port::binding::e8m0_half(e)))
}

#[cfg(not(vesper_port))]
pub fn check_mxfp4_block(block: &[u8]) -> Result<(), Error> {
    if block[0] == E8M0_NAN {
        Err(Error::NonFinite)
    } else {
        Ok(())
    }
}
#[cfg(vesper_port)]
pub fn check_mxfp4_block(block: &[u8]) -> Result<(), Error> {
    if crate::port::must(crate::port::binding::check_mxfp4_block(block)) {
        Ok(())
    } else {
        Err(Error::NonFinite)
    }
}

#[cfg(not(vesper_port))]
pub fn dequantize_mxfp4_block(block: &[u8], out: &mut [f32]) {
    let d = e8m0_half(block[0]);
    for j in 0..16 {
        let b = block[1 + j];
        out[j] = d * f32::from(E2M1_DOUBLED[usize::from(b & NIBBLE)]);
        out[j + 16] = d * f32::from(E2M1_DOUBLED[usize::from(b >> 4)]);
    }
}
#[cfg(vesper_port)]
pub fn dequantize_mxfp4_block(block: &[u8], out: &mut [f32]) {
    crate::port::must(crate::port::binding::dequantize_mxfp4_block(
        block,
        crate::port::bits_mut(out),
    ));
}

/// `floor(log2(v))` for finite `v > 0`, exact (from the exponent bits).
fn floor_log2(v: f32) -> i32 {
    use crate::host::ieee::*;
    let bits = v.to_bits();
    let exp = ((bits >> F32_MANT_BITS) & F32_EXP_FIELD) as i32;
    if exp == 0 {
        // subnormal: value = mantissa * 2^-149
        let mantissa = bits & F32_MANT_MASK;
        31 - mantissa.leading_zeros() as i32 - F32_SUBNORMAL_EXP
    } else {
        exp - F32_BIAS
    }
}

/// GGML / gguf-py MXFP4 quantization: `e = floor(log2(amax)) - 2 + 127`
/// (0 for an all-zero block, clamped to 0..=254), then each element takes the
/// E2M1 code minimizing `|2^(e-128) * kvalue - x|`, the first such code on
/// ties. The exponent is computed exactly; gguf-py takes it from an FP32
/// `log2`, which can differ by one for values within an ulp below a power
/// of two.
#[cfg(not(vesper_port))]
pub fn quantize_mxfp4(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    check_rows(data, cols, MXFP4_BLOCK)?;
    let mut out = Vec::new();
    out.try_reserve_exact(data.len() / MXFP4_BLOCK * MXFP4_BYTES)
        .map_err(|_| Error::Limit("allocation"))?;
    for block in data.as_chunks::<MXFP4_BLOCK>().0.iter() {
        let amax = block.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let e = if amax > 0.0 {
            (floor_log2(amax) - 2 + crate::host::ieee::F32_BIAS).clamp(0, E8M0_MAX_FINITE) as u8
        } else {
            0
        };
        let d = e8m0_half(e);
        let code = |x: f32| -> u8 {
            let mut best = 0usize;
            let mut best_err = f32::INFINITY;
            for (i, k) in E2M1_DOUBLED.iter().enumerate() {
                let err = (d * f32::from(*k) - x).abs();
                if err < best_err {
                    best = i;
                    best_err = err;
                }
            }
            best as u8
        };
        out.push(e);
        for j in 0..16 {
            out.push(code(block[j]) | (code(block[j + 16]) << 4));
        }
    }
    Ok(out)
}
#[cfg(vesper_port)]
pub fn quantize_mxfp4(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    crate::port::check(
        crate::port::binding::quantize_mxfp4(crate::port::bits(data), cols as u64),
        |fault| match fault {
            crate::port::Fault::Shape => Error::Shape("quantized rows need whole blocks"),
            _ => Error::NonFinite,
        },
    )
}

/// An OCP FP8 encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fp8 {
    /// E4M3FN: bias 7, no infinities, largest finite 448.
    E4M3,
    /// E5M2: bias 15, IEEE infinities and NaNs, largest finite 57344.
    E5M2,
}
impl Fp8 {
    const fn mantissa_bits(self) -> u32 {
        match self {
            Self::E4M3 => 3,
            Self::E5M2 => 2,
        }
    }
    const fn bias(self) -> i32 {
        match self {
            Self::E4M3 => 7,
            Self::E5M2 => 15,
        }
    }
    pub const fn max_finite(self) -> f32 {
        match self {
            Self::E4M3 => 448.0,
            Self::E5M2 => 57_344.0,
        }
    }
    /// Decodes one code; NaN codes give NaN and E5M2 infinity codes give
    /// infinities. Table lookup (built once, statically, from
    /// [`Fp8::decode_exact`]).
    #[inline]
    pub fn decode(self, code: u8) -> f32 {
        static E4M3: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
        static E5M2: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
        let table = match self {
            Self::E4M3 => &E4M3,
            Self::E5M2 => &E5M2,
        };
        table.get_or_init(|| std::array::from_fn(|c| self.decode_exact(c as u8)))[usize::from(code)]
    }
    /// The defining computation behind [`Fp8::decode`].
    #[cfg(not(vesper_port))]
    pub fn decode_exact(self, code: u8) -> f32 {
        let sign = if code & FP8_SIGN != 0 { -1.0f64 } else { 1.0 };
        let m_bits = self.mantissa_bits();
        let e_bits = 7 - m_bits;
        let exp = i32::from((code & !FP8_SIGN) >> m_bits);
        let mant = i32::from(code & ((1 << m_bits) - 1));
        let e_max = (1 << e_bits) - 1;
        match self {
            Self::E4M3 if exp == e_max && mant == (1 << m_bits) - 1 => return f32::NAN,
            Self::E5M2 if exp == e_max => {
                return if mant == 0 {
                    (sign * f64::INFINITY) as f32
                } else {
                    f32::NAN
                };
            }
            _ => {}
        }
        let value = if exp == 0 {
            f64::from(mant) * 2f64.powi(1 - self.bias() - m_bits as i32)
        } else {
            (1.0 + f64::from(mant) / f64::from(1 << m_bits)) * 2f64.powi(exp - self.bias())
        };
        (sign * value) as f32
    }
    #[cfg(vesper_port)]
    pub fn decode_exact(self, code: u8) -> f32 {
        f32::from_bits(crate::port::must(crate::port::binding::fp8_decode(
            self == Self::E5M2,
            code,
        )))
    }
    /// Whether `code` is a finite value.
    pub fn is_finite_code(self, code: u8) -> bool {
        self.decode(code).is_finite()
    }
    /// Round-to-nearest-even encoding; NaN becomes the canonical NaN code and
    /// magnitudes beyond the largest finite value (after rounding) saturate
    /// to it (also for E5M2, whose infinities are never produced).
    #[cfg(not(vesper_port))]
    pub fn encode_saturating(self, value: f32) -> u8 {
        let sign: u8 = if value.is_sign_negative() {
            FP8_SIGN
        } else {
            0
        };
        if value.is_nan() {
            return match self {
                Self::E4M3 => E4M3_NAN,
                Self::E5M2 => E5M2_NAN,
            } | sign;
        }
        let a = f64::from(value.abs());
        let m_bits = self.mantissa_bits() as i32;
        let emin = 1 - self.bias();
        let max = f64::from(self.max_finite());
        let rounded = if a == 0.0 {
            0.0
        } else if a.is_infinite() {
            max
        } else {
            let e = floor_log2(value.abs()).max(emin);
            let quantum = 2f64.powi(e - m_bits);
            let scaled = a / quantum;
            let mut r = scaled.floor();
            let frac = scaled - r;
            if frac > 0.5 || (frac == 0.5 && r % 2.0 == 1.0) {
                r += 1.0;
            }
            (r * quantum).min(max)
        };
        if rounded == 0.0 {
            return sign;
        }
        let min_normal = 2f64.powi(emin);
        let bits = if rounded < min_normal {
            (rounded / 2f64.powi(emin - m_bits)) as u8
        } else {
            let e = floor_log2(rounded as f32);
            let field = (e + self.bias()) as u8;
            let mant = (rounded / 2f64.powi(e - m_bits)) as i32 - (1 << m_bits);
            (field << m_bits) | mant as u8
        };
        sign | bits
    }
    #[cfg(vesper_port)]
    pub fn encode_saturating(self, value: f32) -> u8 {
        crate::port::must(crate::port::binding::fp8_encode(
            self == Self::E5M2,
            value.to_bits(),
        ))
    }
}

/// Per-tensor FP8 quantization: `scale = amax / max_finite` (1 for an
/// all-zero matrix) and codes `encode(x / scale)`. Element value is
/// `decode(code) * scale`.
#[cfg(not(vesper_port))]
pub fn quantize_fp8(data: &[f32], format: Fp8) -> Result<(Vec<u8>, f32), Error> {
    if data.iter().any(|v| !v.is_finite()) {
        return Err(Error::NonFinite);
    }
    let amax = data.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let scale = if amax > 0.0 {
        amax / format.max_finite()
    } else {
        1.0
    };
    if !(scale.is_finite() && scale > 0.0) {
        return Err(Error::NonFinite);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(data.len())
        .map_err(|_| Error::Limit("allocation"))?;
    out.extend(data.iter().map(|v| format.encode_saturating(v / scale)));
    Ok((out, scale))
}
#[cfg(vesper_port)]
pub fn quantize_fp8(data: &[f32], format: Fp8) -> Result<(Vec<u8>, f32), Error> {
    let e5m2 = format == Fp8::E5M2;
    let scale = crate::port::check(
        crate::port::binding::fp8_scale(crate::port::bits(data), e5m2),
        |_| Error::NonFinite,
    )?;
    let codes = crate::port::must(crate::port::binding::fp8_codes(
        crate::port::bits(data),
        e5m2,
        scale,
    ));
    Ok((codes, f32::from_bits(scale)))
}

fn check_rows(data: &[f32], cols: usize, block: usize) -> Result<(), Error> {
    if cols == 0 || !cols.is_multiple_of(block) || !data.len().is_multiple_of(cols) {
        return Err(Error::Shape("quantized rows need whole blocks"));
    }
    if data.iter().any(|v| !v.is_finite()) {
        return Err(Error::NonFinite);
    }
    Ok(())
}
