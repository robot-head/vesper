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
//! The Q4_K quantizer here is a simple min/max quantizer (documented on
//! [`quantize_q4_k`]); it is not GGML's iterative `make_qkx2_quants`, but its
//! output is a valid Q4_K block and dequantizes with GGML's rule. The MXFP4
//! quantizer follows GGML / gguf-py's rule with an exact exponent.
use crate::host::decode_fp16;
use crate::quant::encode_fp16;
use crate::Error;

pub const QK_K: usize = 256;
pub const Q4_K_BYTES: usize = 144;
pub const MXFP4_BLOCK: usize = 32;
pub const MXFP4_BYTES: usize = 17;

fn half(bytes: &[u8], at: usize) -> f32 {
    decode_fp16(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

/// GGML `get_scale_min_k4`: the 6-bit scale and minimum of sub-block `j`.
pub fn q4_k_scale_min(j: usize, scales: &[u8]) -> (u8, u8) {
    if j < 4 {
        (scales[j] & 63, scales[j + 4] & 63)
    } else {
        (
            (scales[j + 4] & 0x0f) | ((scales[j - 4] >> 6) << 4),
            (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4),
        )
    }
}

/// Dequantizes one 144-byte Q4_K block into 256 values.
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
            out[64 * j + l] = d1 * f32::from(q[l] & 0x0f) - m1;
            out[64 * j + 32 + l] = d2 * f32::from(q[l] >> 4) - m2;
        }
    }
}

/// Checks a Q4_K block's two scales are finite.
pub fn check_q4_k_block(block: &[u8]) -> Result<(), Error> {
    if half(block, 0).is_finite() && half(block, 2).is_finite() {
        Ok(())
    } else {
        Err(Error::NonFinite)
    }
}

/// Quantizes rows of 256-element super-blocks to Q4_K.
///
/// Per 32-element sub-block it takes `lo = min(0, min x)` and
/// `hi = max x`, scale `(hi - lo) / 15` and offset `-lo`. The super-block
/// `d` and `dmin` are the largest sub-block scale and offset divided by 63
/// (rounded to binary16), the 6-bit codes are rounded to nearest, and each
/// quant is `clamp(round((x + dmin*m) / (d*sc)), 0, 15)` using the decoded
/// scales, so the reconstruction error is that of the stored parameters.
pub fn quantize_q4_k(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    check_rows(data, cols, QK_K)?;
    let mut out = Vec::new();
    out.try_reserve_exact(data.len() / QK_K * Q4_K_BYTES)
        .map_err(|_| Error::Limit("allocation"))?;
    for block in data.as_chunks::<QK_K>().0.iter() {
        let mut scale = [0.0f32; 8];
        let mut offset = [0.0f32; 8];
        for (s, sub) in block.as_chunks::<32>().0.iter().enumerate() {
            let lo = sub.iter().copied().fold(0.0f32, f32::min);
            let hi = sub.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            scale[s] = (hi - lo) / 15.0;
            offset[s] = -lo;
        }
        let max_scale = scale.iter().copied().fold(0.0f32, f32::max);
        let max_offset = offset.iter().copied().fold(0.0f32, f32::max);
        let d_bits = encode_fp16(max_scale / 63.0);
        let dmin_bits = encode_fp16(max_offset / 63.0);
        let (d, dmin) = (decode_fp16(d_bits), decode_fp16(dmin_bits));
        if !(d.is_finite() && dmin.is_finite()) {
            return Err(Error::Limit("Q4_K scale exceeds binary16"));
        }
        let code = |v: f32, unit: f32| -> u8 {
            if unit > 0.0 {
                (v / unit).round().clamp(0.0, 63.0) as u8
            } else {
                0
            }
        };
        let mut sc = [0u8; 8];
        let mut mn = [0u8; 8];
        for s in 0..8 {
            sc[s] = code(scale[s], d);
            mn[s] = code(offset[s], dmin);
        }
        let mut scales = [0u8; 12];
        for j in 0..4 {
            scales[j] = sc[j] | ((sc[j + 4] >> 4) << 6);
            scales[j + 4] = mn[j] | ((mn[j + 4] >> 4) << 6);
            scales[j + 8] = (sc[j + 4] & 0x0f) | ((mn[j + 4] & 0x0f) << 4);
        }
        let mut qs = [0u8; 128];
        for (s, sub) in block.as_chunks::<32>().0.iter().enumerate() {
            let (c, m) = q4_k_scale_min(s, &scales);
            let unit = d * f32::from(c);
            let bias = dmin * f32::from(m);
            for (l, &v) in sub.iter().enumerate() {
                let q = if unit > 0.0 {
                    ((v + bias) / unit).round().clamp(0.0, 15.0) as u8
                } else {
                    0
                };
                let byte = &mut qs[32 * (s / 2) + l];
                *byte |= if s % 2 == 0 { q } else { q << 4 };
            }
        }
        out.extend_from_slice(&d_bits.to_le_bytes());
        out.extend_from_slice(&dmin_bits.to_le_bytes());
        out.extend_from_slice(&scales);
        out.extend_from_slice(&qs);
    }
    Ok(out)
}

/// E2M1 code values, doubled (GGML `kvalues_mxfp4`).
pub const E2M1_DOUBLED: [i8; 16] = [0, 1, 2, 3, 4, 6, 8, 12, 0, -1, -2, -3, -4, -6, -8, -12];

/// `2^(e - 128)` exactly (GGML `ggml_e8m0_to_fp32_half`), for `e < 255`.
pub fn e8m0_half(e: u8) -> f32 {
    let bits = if e < 2 {
        0x0020_0000u32 << e
    } else {
        u32::from(e - 1) << 23
    };
    f32::from_bits(bits)
}

pub fn check_mxfp4_block(block: &[u8]) -> Result<(), Error> {
    if block[0] == 0xff {
        Err(Error::NonFinite)
    } else {
        Ok(())
    }
}

pub fn dequantize_mxfp4_block(block: &[u8], out: &mut [f32]) {
    let d = e8m0_half(block[0]);
    for j in 0..16 {
        let b = block[1 + j];
        out[j] = d * f32::from(E2M1_DOUBLED[usize::from(b & 0x0f)]);
        out[j + 16] = d * f32::from(E2M1_DOUBLED[usize::from(b >> 4)]);
    }
}

/// `floor(log2(v))` for finite `v > 0`, exact (from the exponent bits).
fn floor_log2(v: f32) -> i32 {
    let bits = v.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32;
    if exp == 0 {
        // subnormal: value = mantissa * 2^-149
        let mantissa = bits & 0x7f_ffff;
        31 - mantissa.leading_zeros() as i32 - 149
    } else {
        exp - 127
    }
}

/// GGML / gguf-py MXFP4 quantization: `e = floor(log2(amax)) - 2 + 127`
/// (0 for an all-zero block, clamped to 0..=254), then each element takes the
/// E2M1 code minimizing `|2^(e-128) * kvalue - x|`, the first such code on
/// ties. The exponent is computed exactly; gguf-py takes it from an FP32
/// `log2`, which can differ by one for values within an ulp below a power
/// of two.
pub fn quantize_mxfp4(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    check_rows(data, cols, MXFP4_BLOCK)?;
    let mut out = Vec::new();
    out.try_reserve_exact(data.len() / MXFP4_BLOCK * MXFP4_BYTES)
        .map_err(|_| Error::Limit("allocation"))?;
    for block in data.as_chunks::<MXFP4_BLOCK>().0.iter() {
        let amax = block.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let e = if amax > 0.0 {
            (floor_log2(amax) - 2 + 127).clamp(0, 254) as u8
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
    pub fn decode_exact(self, code: u8) -> f32 {
        let sign = if code & 0x80 != 0 { -1.0f64 } else { 1.0 };
        let m_bits = self.mantissa_bits();
        let e_bits = 7 - m_bits;
        let exp = i32::from((code & 0x7f) >> m_bits);
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
    /// Whether `code` is a finite value.
    pub fn is_finite_code(self, code: u8) -> bool {
        self.decode(code).is_finite()
    }
    /// Round-to-nearest-even encoding; NaN becomes the canonical NaN code and
    /// magnitudes beyond the largest finite value (after rounding) saturate
    /// to it (also for E5M2, whose infinities are never produced).
    pub fn encode_saturating(self, value: f32) -> u8 {
        let sign: u8 = if value.is_sign_negative() { 0x80 } else { 0 };
        if value.is_nan() {
            return match self {
                Self::E4M3 => 0x7f,
                Self::E5M2 => 0x7e,
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
}

/// Per-tensor FP8 quantization: `scale = amax / max_finite` (1 for an
/// all-zero matrix) and codes `encode(x / scale)`. Element value is
/// `decode(code) * scale`.
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

fn check_rows(data: &[f32], cols: usize, block: usize) -> Result<(), Error> {
    if cols == 0 || !cols.is_multiple_of(block) || !data.len().is_multiple_of(cols) {
        return Err(Error::Shape("quantized rows need whole blocks"));
    }
    if data.iter().any(|v| !v.is_finite()) {
        return Err(Error::NonFinite);
    }
    Ok(())
}
