//! Q8_0 and Q4_0 block quantization, binary16/BF16 encoding and dequantizing
//! matrix-vector kernels.
//!
//! Both block formats cover 32 consecutive row elements and start with a
//! little-endian binary16 scale `d`:
//!
//! * Q8_0 is 34 bytes: `d` then 32 two's-complement `i8` values; element
//!   `i` is `d * q[i]`.
//! * Q4_0 is 18 bytes: `d` then 16 packed bytes; byte `j` holds element `j` in
//!   its low nibble and element `j + 16` in its high nibble, each biased by 8;
//!   element `i` is `d * (nibble[i] - 8)`.
//!
//! These are the GGML `Q8_0`/`Q4_0` layouts. The quantizers follow GGML's
//! reference (scalar) rounding rules. Blocks are parsed from bytes with no
//! unsafe casts and no padding assumptions.
use crate::host::decode_fp16;
use crate::Error;

pub const BLOCK: usize = 32;
pub const Q8_0_BYTES: usize = 34;
pub const Q4_0_BYTES: usize = 18;

/// IEEE binary32 → binary16 with round-to-nearest-even. Overflow becomes a
/// signed infinity; NaNs stay NaNs (quiet, top payload bits kept).
pub fn encode_fp16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x7f_ffff;
    if exponent == 0xff {
        return if mantissa == 0 {
            sign | 0x7c00
        } else {
            sign | 0x7e00 | (mantissa >> 13) as u16
        };
    }
    let e = exponent - 112; // rebias 127 -> 15
    if e >= 31 {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign; // below half the smallest subnormal: rounds to zero
        }
        let m = mantissa | 0x80_0000;
        let shift = (14 - e) as u32;
        return sign | round_shift(m, shift) as u16;
    }
    // Carry out of the mantissa correctly increments the exponent (and can
    // produce infinity).
    sign | round_shift(((e as u32) << 23) | mantissa, 13) as u16
}

fn round_shift(value: u32, shift: u32) -> u32 {
    let kept = value >> shift;
    let rest = value & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    if rest > half || (rest == half && kept & 1 == 1) {
        kept + 1
    } else {
        kept
    }
}

/// IEEE binary32 → BF16 with round-to-nearest-even; NaNs stay quiet NaNs.
pub fn encode_bf16(value: f32) -> u16 {
    let bits = value.to_bits();
    if value.is_nan() {
        return ((bits >> 16) as u16) | 0x0040;
    }
    round_shift_carry(bits)
}
fn round_shift_carry(bits: u32) -> u16 {
    let kept = bits >> 16;
    let rest = bits & 0xffff;
    let rounded = if rest > 0x8000 || (rest == 0x8000 && kept & 1 == 1) {
        kept + 1
    } else {
        kept
    };
    rounded as u16
}
pub fn decode_bf16(bits: u16) -> f32 {
    f32::from_bits(u32::from(bits) << 16)
}

/// Quantization format of a weight matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeightFormat {
    F32,
    Q8_0,
    Q4_0,
}

/// A row-major `[rows, cols]` weight matrix in one of the supported formats.
#[derive(Clone, Debug, PartialEq)]
pub enum Matrix {
    F32 {
        rows: usize,
        cols: usize,
        data: Vec<f32>,
    },
    /// `cols` is a multiple of 32; `bytes` holds `rows * cols / 32` blocks.
    Q8_0 {
        rows: usize,
        cols: usize,
        bytes: Vec<u8>,
    },
    /// `cols` is a multiple of 32; `bytes` holds `rows * cols / 32` blocks.
    Q4_0 {
        rows: usize,
        cols: usize,
        bytes: Vec<u8>,
    },
}

impl Matrix {
    pub fn f32(rows: usize, cols: usize, data: Vec<f32>) -> Result<Self, Error> {
        if rows == 0 || cols == 0 || rows.checked_mul(cols) != Some(data.len()) {
            return Err(Error::Shape("f32 matrix length"));
        }
        if data.iter().any(|v| !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        Ok(Self::F32 { rows, cols, data })
    }
    /// Validates block count and that every scale is finite.
    pub fn quantized(
        format: WeightFormat,
        rows: usize,
        cols: usize,
        bytes: Vec<u8>,
    ) -> Result<Self, Error> {
        let block_bytes = match format {
            WeightFormat::Q8_0 => Q8_0_BYTES,
            WeightFormat::Q4_0 => Q4_0_BYTES,
            WeightFormat::F32 => return Err(Error::Shape("not a quantized format")),
        };
        if rows == 0 || cols == 0 || !cols.is_multiple_of(BLOCK) {
            return Err(Error::Shape(
                "quantized matrix needs whole 32-element row blocks",
            ));
        }
        let expected = rows
            .checked_mul(cols / BLOCK)
            .and_then(|n| n.checked_mul(block_bytes))
            .ok_or(Error::Overflow)?;
        if bytes.len() != expected {
            return Err(Error::Shape("quantized matrix byte length"));
        }
        for block in bytes.chunks_exact(block_bytes) {
            if !decode_fp16(u16::from_le_bytes([block[0], block[1]])).is_finite() {
                return Err(Error::NonFinite);
            }
        }
        Ok(match format {
            WeightFormat::Q8_0 => Self::Q8_0 { rows, cols, bytes },
            _ => Self::Q4_0 { rows, cols, bytes },
        })
    }
    pub fn rows(&self) -> usize {
        match self {
            Self::F32 { rows, .. } | Self::Q8_0 { rows, .. } | Self::Q4_0 { rows, .. } => *rows,
        }
    }
    pub fn cols(&self) -> usize {
        match self {
            Self::F32 { cols, .. } | Self::Q8_0 { cols, .. } | Self::Q4_0 { cols, .. } => *cols,
        }
    }
    pub fn format(&self) -> WeightFormat {
        match self {
            Self::F32 { .. } => WeightFormat::F32,
            Self::Q8_0 { .. } => WeightFormat::Q8_0,
            Self::Q4_0 { .. } => WeightFormat::Q4_0,
        }
    }
    /// Payload bytes of the weights (excluding container overhead).
    pub fn payload_bytes(&self) -> usize {
        match self {
            Self::F32 { data, .. } => data.len() * 4,
            Self::Q8_0 { bytes, .. } | Self::Q4_0 { bytes, .. } => bytes.len(),
        }
    }
    /// Quantizes an FP32 matrix. Matrices whose column count is not a multiple
    /// of 32 are rejected with a shape error; callers choose whether to keep
    /// them in FP32 instead.
    pub fn quantize(&self, format: WeightFormat) -> Result<Self, Error> {
        let Self::F32 { rows, cols, data } = self else {
            return Err(Error::Shape("only FP32 matrices can be quantized"));
        };
        match format {
            WeightFormat::F32 => Ok(self.clone()),
            WeightFormat::Q8_0 => {
                Self::quantized(format, *rows, *cols, quantize_q8_0(data, *cols)?)
            }
            WeightFormat::Q4_0 => {
                Self::quantized(format, *rows, *cols, quantize_q4_0(data, *cols)?)
            }
        }
    }
    /// Dequantizes row `row` into `out` (length `cols`).
    pub fn dequantize_row(&self, row: usize, out: &mut [f32]) -> Result<(), Error> {
        if row >= self.rows() || out.len() != self.cols() {
            return Err(Error::Shape("dequantize row"));
        }
        let cols = self.cols();
        match self {
            Self::F32 { data, .. } => out.copy_from_slice(&data[row * cols..(row + 1) * cols]),
            Self::Q8_0 { bytes, .. } => {
                let row_bytes =
                    &bytes[row * (cols / BLOCK) * Q8_0_BYTES..][..(cols / BLOCK) * Q8_0_BYTES];
                for (block, out) in row_bytes
                    .chunks_exact(Q8_0_BYTES)
                    .zip(out.chunks_exact_mut(BLOCK))
                {
                    dequantize_q8_0_block(block, out);
                }
            }
            Self::Q4_0 { bytes, .. } => {
                let row_bytes =
                    &bytes[row * (cols / BLOCK) * Q4_0_BYTES..][..(cols / BLOCK) * Q4_0_BYTES];
                for (block, out) in row_bytes
                    .chunks_exact(Q4_0_BYTES)
                    .zip(out.chunks_exact_mut(BLOCK))
                {
                    dequantize_q4_0_block(block, out);
                }
            }
        }
        Ok(())
    }
    /// `out[r] = sum_c W[r, c] * x[c]`, accumulated in FP32 in ascending column
    /// order. Quantized weights are dequantized element by element
    /// (`d * q`, rounded to FP32) before the product; activations stay FP32.
    pub fn matvec(&self, x: &[f32], out: &mut [f32]) -> Result<(), Error> {
        if x.len() != self.cols() || out.len() != self.rows() {
            return Err(Error::Shape("matvec operand lengths"));
        }
        for (row, out) in out.iter_mut().enumerate() {
            *out = self.row_dot(row, x);
        }
        Ok(())
    }
    /// Dot product of row `row` with `x`; lengths are checked by callers.
    pub(crate) fn row_dot(&self, row: usize, x: &[f32]) -> f32 {
        let cols = self.cols();
        let mut sum = 0.0f32;
        match self {
            Self::F32 { data, .. } => {
                for (w, x) in data[row * cols..(row + 1) * cols].iter().zip(x) {
                    sum += w * x;
                }
            }
            Self::Q8_0 { bytes, .. } => {
                let blocks = cols / BLOCK;
                let row_bytes = &bytes[row * blocks * Q8_0_BYTES..(row + 1) * blocks * Q8_0_BYTES];
                for (block, x) in row_bytes
                    .chunks_exact(Q8_0_BYTES)
                    .zip(x.chunks_exact(BLOCK))
                {
                    let d = decode_fp16(u16::from_le_bytes([block[0], block[1]]));
                    for (q, x) in block[2..].iter().zip(x) {
                        sum += (d * f32::from(*q as i8)) * x;
                    }
                }
            }
            Self::Q4_0 { bytes, .. } => {
                let blocks = cols / BLOCK;
                let row_bytes = &bytes[row * blocks * Q4_0_BYTES..(row + 1) * blocks * Q4_0_BYTES];
                for (block, x) in row_bytes
                    .chunks_exact(Q4_0_BYTES)
                    .zip(x.chunks_exact(BLOCK))
                {
                    let d = decode_fp16(u16::from_le_bytes([block[0], block[1]]));
                    for (i, x) in x.iter().enumerate() {
                        let packed = block[2 + i % 16];
                        let nibble = if i < 16 { packed & 15 } else { packed >> 4 };
                        sum += (d * f32::from(i16::from(nibble) - 8)) * x;
                    }
                }
            }
        }
        sum
    }
}

pub fn dequantize_q8_0_block(block: &[u8], out: &mut [f32]) {
    let d = decode_fp16(u16::from_le_bytes([block[0], block[1]]));
    for (q, out) in block[2..Q8_0_BYTES].iter().zip(out) {
        *out = d * f32::from(*q as i8);
    }
}
pub fn dequantize_q4_0_block(block: &[u8], out: &mut [f32]) {
    let d = decode_fp16(u16::from_le_bytes([block[0], block[1]]));
    for (i, out) in out.iter_mut().enumerate().take(BLOCK) {
        let packed = block[2 + i % 16];
        let nibble = if i < 16 { packed & 15 } else { packed >> 4 };
        *out = d * f32::from(i16::from(nibble) - 8);
    }
}

fn check_rows(data: &[f32], cols: usize) -> Result<(), Error> {
    if cols == 0 || !cols.is_multiple_of(BLOCK) || !data.len().is_multiple_of(cols) {
        return Err(Error::Shape("quantized rows need whole 32-element blocks"));
    }
    if data.iter().any(|v| !v.is_finite()) {
        return Err(Error::NonFinite);
    }
    Ok(())
}

/// GGML reference Q8_0: `d = amax / 127`, `q = round(x / d)` (half away from
/// zero), scale stored as binary16.
pub fn quantize_q8_0(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    check_rows(data, cols)?;
    let mut out = Vec::with_capacity(data.len() / BLOCK * Q8_0_BYTES);
    for block in data.chunks_exact(BLOCK) {
        let amax = block.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let d = amax / 127.0;
        let id = if d != 0.0 { 1.0 / d } else { 0.0 };
        out.extend_from_slice(&encode_fp16(d).to_le_bytes());
        out.extend(block.iter().map(|v| ((v * id).round() as i8) as u8));
    }
    Ok(out)
}

/// GGML reference Q4_0: the signed value with the largest magnitude `m` gives
/// `d = m / -8`; `q = min(15, trunc(x / d + 8.5))`.
pub fn quantize_q4_0(data: &[f32], cols: usize) -> Result<Vec<u8>, Error> {
    check_rows(data, cols)?;
    let mut out = Vec::with_capacity(data.len() / BLOCK * Q4_0_BYTES);
    for block in data.chunks_exact(BLOCK) {
        let mut amax = 0.0f32;
        let mut max = 0.0f32;
        for &v in block {
            if amax < v.abs() {
                amax = v.abs();
                max = v;
            }
        }
        let d = max / -8.0;
        let id = if d != 0.0 { 1.0 / d } else { 0.0 };
        out.extend_from_slice(&encode_fp16(d).to_le_bytes());
        let q = |v: f32| ((v * id + 8.5) as i8).min(15) as u8;
        for j in 0..16 {
            out.push(q(block[j]) | (q(block[j + 16]) << 4));
        }
    }
    Ok(out)
}
