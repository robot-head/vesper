//! Weight matrices in every supported format, Q8_0 and Q4_0 block
//! quantization, binary16/BF16 encoding and dequantizing matrix-vector and
//! batched matrix kernels. Q4_K, MXFP4 and FP8 live in [`crate::formats`].
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
use crate::formats::{self, Fp8, MXFP4_BYTES, Q4_K_BYTES, QK_K};
use crate::host::decode_fp16;
use crate::mmap::WeightBytes;
use crate::simd::{self, DotAcc, Kernel};
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

/// Encoding of a weight matrix's elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WeightFormat {
    F32,
    /// IEEE binary16 elements.
    F16,
    /// BF16 elements.
    Bf16,
    Q8_0,
    Q4_0,
    /// GGML Q4_K 256-element super-blocks (see [`crate::formats`]).
    Q4K,
    /// OCP MXFP4 in GGML's 32-element block (see [`crate::formats`]).
    Mxfp4,
    /// OCP FP8 E4M3FN elements times a per-matrix FP32 scale.
    F8E4M3,
    /// OCP FP8 E5M2 elements times a per-matrix FP32 scale.
    F8E5M2,
}
impl WeightFormat {
    /// Elements per block (1 for element formats).
    pub const fn block_elements(self) -> usize {
        match self {
            Self::F32 | Self::F16 | Self::Bf16 | Self::F8E4M3 | Self::F8E5M2 => 1,
            Self::Q8_0 | Self::Q4_0 | Self::Mxfp4 => BLOCK,
            Self::Q4K => QK_K,
        }
    }
    /// Bytes per block.
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 | Self::Bf16 => 2,
            Self::F8E4M3 | Self::F8E5M2 => 1,
            Self::Q8_0 => Q8_0_BYTES,
            Self::Q4_0 => Q4_0_BYTES,
            Self::Q4K => Q4_K_BYTES,
            Self::Mxfp4 => MXFP4_BYTES,
        }
    }
    /// Payload bytes of one `cols`-element row; `cols` must be a whole number
    /// of blocks.
    pub fn row_bytes(self, cols: usize) -> Result<usize, Error> {
        if cols == 0 || !cols.is_multiple_of(self.block_elements()) {
            return Err(Error::QuantBlockAlignment {
                cols,
                block: self.block_elements(),
            });
        }
        (cols / self.block_elements())
            .checked_mul(self.block_bytes())
            .ok_or(Error::Overflow)
    }
    fn fp8(self) -> Option<Fp8> {
        match self {
            Self::F8E4M3 => Some(Fp8::E4M3),
            Self::F8E5M2 => Some(Fp8::E5M2),
            _ => None,
        }
    }
}

/// Elements decoded per chunk for element formats: a multiple of eight, so
/// chunked SIMD accumulation matches whole-row accumulation exactly.
const ELEMENT_CHUNK: usize = 256;

#[derive(Clone, Debug)]
enum Storage {
    F32(Vec<f32>),
    Bytes(WeightBytes),
}

/// A row-major `[rows, cols]` weight matrix in one of the [`WeightFormat`]s.
/// Byte-backed matrices may borrow a memory-mapped file (see
/// [`crate::mmap`]); every element is decoded from bytes, never cast.
#[derive(Clone, Debug)]
pub struct Matrix {
    rows: usize,
    cols: usize,
    format: WeightFormat,
    scale: f32,
    storage: Storage,
}

impl PartialEq for Matrix {
    fn eq(&self, other: &Self) -> bool {
        if (self.rows, self.cols, self.format, self.scale.to_bits())
            != (other.rows, other.cols, other.format, other.scale.to_bits())
        {
            return false;
        }
        match (&self.storage, &other.storage) {
            (Storage::F32(a), Storage::F32(b)) => a
                .iter()
                .map(|v| v.to_bits())
                .eq(b.iter().map(|v| v.to_bits())),
            (Storage::Bytes(a), Storage::Bytes(b)) => a.as_slice() == b.as_slice(),
            (Storage::F32(a), Storage::Bytes(b)) | (Storage::Bytes(b), Storage::F32(a)) => a
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .eq(b.as_slice().iter().copied()),
        }
    }
}

impl Matrix {
    pub fn f32(rows: usize, cols: usize, data: Vec<f32>) -> Result<Self, Error> {
        if rows == 0 || cols == 0 || rows.checked_mul(cols) != Some(data.len()) {
            return Err(Error::Shape("f32 matrix length"));
        }
        if data.iter().any(|v| !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        Ok(Self {
            rows,
            cols,
            format: WeightFormat::F32,
            scale: 1.0,
            storage: Storage::F32(data),
        })
    }
    /// An owned byte-backed matrix with scale 1 (see [`Matrix::from_bytes`]).
    pub fn quantized(
        format: WeightFormat,
        rows: usize,
        cols: usize,
        bytes: Vec<u8>,
    ) -> Result<Self, Error> {
        Self::from_bytes(format, rows, cols, WeightBytes::Owned(bytes), 1.0)
    }
    /// A byte-backed matrix. Checks the exact byte length (`cols` must be a
    /// whole number of blocks) and that every block scale or element is
    /// finite. `scale` multiplies every element; it must be 1 except for the
    /// FP8 formats, where it must be finite and positive.
    pub fn from_bytes(
        format: WeightFormat,
        rows: usize,
        cols: usize,
        bytes: WeightBytes,
        scale: f32,
    ) -> Result<Self, Error> {
        if rows == 0 || cols == 0 {
            return Err(Error::Shape("matrix dimensions must be positive"));
        }
        let expected = rows
            .checked_mul(format.row_bytes(cols)?)
            .ok_or(Error::Overflow)?;
        let raw = bytes.as_slice();
        if raw.len() != expected {
            return Err(Error::Shape("matrix byte length"));
        }
        match format.fp8() {
            Some(_) if !(scale.is_finite() && scale > 0.0) => return Err(Error::NonFinite),
            None if scale != 1.0 => return Err(Error::Shape("only FP8 matrices carry a scale")),
            _ => {}
        }
        let finite = match format {
            WeightFormat::F32 => raw
                .as_chunks::<4>()
                .0
                .iter()
                .all(|b| f32::from_le_bytes(*b).is_finite()),
            WeightFormat::F16 => raw
                .as_chunks::<2>()
                .0
                .iter()
                .all(|b| decode_fp16(u16::from_le_bytes(*b)).is_finite()),
            WeightFormat::Bf16 => raw
                .as_chunks::<2>()
                .0
                .iter()
                .all(|b| decode_bf16(u16::from_le_bytes(*b)).is_finite()),
            WeightFormat::F8E4M3 | WeightFormat::F8E5M2 => {
                let fp8 = format.fp8().expect("FP8 format");
                raw.iter().all(|b| fp8.is_finite_code(*b))
            }
            WeightFormat::Q8_0 | WeightFormat::Q4_0 => raw
                .chunks_exact(format.block_bytes())
                .all(|b| decode_fp16(u16::from_le_bytes([b[0], b[1]])).is_finite()),
            WeightFormat::Q4K => raw
                .as_chunks::<Q4_K_BYTES>()
                .0
                .iter()
                .all(|b| formats::check_q4_k_block(b).is_ok()),
            WeightFormat::Mxfp4 => raw
                .as_chunks::<MXFP4_BYTES>()
                .0
                .iter()
                .all(|b| formats::check_mxfp4_block(b).is_ok()),
        };
        if !finite {
            return Err(Error::NonFinite);
        }
        Ok(Self {
            rows,
            cols,
            format,
            scale,
            storage: Storage::Bytes(bytes),
        })
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn format(&self) -> WeightFormat {
        self.format
    }
    /// Per-matrix multiplier (1 except for FP8).
    pub fn scale(&self) -> f32 {
        self.scale
    }
    /// Payload bytes of the weights (excluding container overhead).
    pub fn payload_bytes(&self) -> usize {
        match &self.storage {
            Storage::F32(data) => data.len() * 4,
            Storage::Bytes(bytes) => bytes.len(),
        }
    }
    /// Whether the payload is borrowed from an operating-system file mapping.
    pub fn is_file_mapped(&self) -> bool {
        matches!(&self.storage, Storage::Bytes(b) if b.is_file_mapped())
    }
    /// The raw payload bytes of a byte-backed matrix (`None` for owned FP32).
    pub fn raw_bytes(&self) -> Option<&[u8]> {
        match &self.storage {
            Storage::F32(_) => None,
            Storage::Bytes(b) => Some(b.as_slice()),
        }
    }
    /// Quantizes an FP32 matrix (owned or byte-backed) to `format`. Rows must
    /// be whole blocks of the target format; callers choose whether to keep
    /// other matrices in FP32 instead.
    pub fn quantize(&self, format: WeightFormat) -> Result<Self, Error> {
        if self.format != WeightFormat::F32 {
            return Err(Error::Shape("only FP32 matrices can be quantized"));
        }
        let owned;
        let data: &[f32] = match &self.storage {
            Storage::F32(data) => data,
            Storage::Bytes(_) => {
                owned = self.to_f32()?;
                &owned
            }
        };
        let (rows, cols) = (self.rows, self.cols);
        match format {
            WeightFormat::F32 => Self::f32(rows, cols, data.to_vec()),
            WeightFormat::F16 => Self::quantized(
                format,
                rows,
                cols,
                data.iter()
                    .flat_map(|v| encode_fp16(*v).to_le_bytes())
                    .collect(),
            ),
            WeightFormat::Bf16 => Self::quantized(
                format,
                rows,
                cols,
                data.iter()
                    .flat_map(|v| encode_bf16(*v).to_le_bytes())
                    .collect(),
            ),
            WeightFormat::Q8_0 => Self::quantized(format, rows, cols, quantize_q8_0(data, cols)?),
            WeightFormat::Q4_0 => Self::quantized(format, rows, cols, quantize_q4_0(data, cols)?),
            WeightFormat::Q4K => {
                Self::quantized(format, rows, cols, formats::quantize_q4_k(data, cols)?)
            }
            WeightFormat::Mxfp4 => {
                Self::quantized(format, rows, cols, formats::quantize_mxfp4(data, cols)?)
            }
            WeightFormat::F8E4M3 | WeightFormat::F8E5M2 => {
                let (bytes, scale) = formats::quantize_fp8(data, format.fp8().expect("FP8"))?;
                Self::from_bytes(format, rows, cols, WeightBytes::Owned(bytes), scale)
            }
        }
    }
    /// Every element as FP32, row-major.
    pub fn to_f32(&self) -> Result<Vec<f32>, Error> {
        let n = self.rows.checked_mul(self.cols).ok_or(Error::Overflow)?;
        let mut out = Vec::new();
        out.try_reserve_exact(n)
            .map_err(|_| Error::Limit("allocation"))?;
        out.resize(n, 0.0);
        for (row, out) in out.chunks_exact_mut(self.cols).enumerate() {
            self.dequantize_row(row, out)?;
        }
        Ok(out)
    }
    /// Dequantizes row `row` into `out` (length `cols`).
    pub fn dequantize_row(&self, row: usize, out: &mut [f32]) -> Result<(), Error> {
        if row >= self.rows || out.len() != self.cols {
            return Err(Error::Shape("dequantize row"));
        }
        self.row_chunks(row, |start, values| {
            out[start..start + values.len()].copy_from_slice(values);
        });
        Ok(())
    }
    /// Calls `f(first_column, values)` for consecutive pieces of row `row`,
    /// decoded to FP32. Every piece but the last has a length that is a
    /// multiple of eight.
    fn row_chunks(&self, row: usize, mut f: impl FnMut(usize, &[f32])) {
        let cols = self.cols;
        let bytes = match &self.storage {
            Storage::F32(data) => {
                f(0, &data[row * cols..(row + 1) * cols]);
                return;
            }
            Storage::Bytes(b) => b.as_slice(),
        };
        let row_bytes = (cols / self.format.block_elements()) * self.format.block_bytes();
        let raw = &bytes[row * row_bytes..(row + 1) * row_bytes];
        let mut buf = [0.0f32; ELEMENT_CHUNK];
        match self.format {
            WeightFormat::F32
            | WeightFormat::F16
            | WeightFormat::Bf16
            | WeightFormat::F8E4M3
            | WeightFormat::F8E5M2 => {
                let width = self.format.block_bytes();
                for (c, chunk) in raw.chunks(ELEMENT_CHUNK * width).enumerate() {
                    let n = chunk.len() / width;
                    let out = &mut buf[..n];
                    match self.format {
                        WeightFormat::F32 => {
                            for (o, b) in out.iter_mut().zip(chunk.as_chunks::<4>().0) {
                                *o = f32::from_le_bytes(*b);
                            }
                        }
                        WeightFormat::F16 => {
                            for (o, b) in out.iter_mut().zip(chunk.as_chunks::<2>().0) {
                                *o = decode_fp16(u16::from_le_bytes(*b));
                            }
                        }
                        WeightFormat::Bf16 => {
                            for (o, b) in out.iter_mut().zip(chunk.as_chunks::<2>().0) {
                                *o = decode_bf16(u16::from_le_bytes(*b));
                            }
                        }
                        _ => {
                            let fp8 = self.format.fp8().expect("FP8");
                            for (o, b) in out.iter_mut().zip(chunk) {
                                *o = fp8.decode(*b) * self.scale;
                            }
                        }
                    }
                    f(c * ELEMENT_CHUNK, out);
                }
            }
            WeightFormat::Q8_0 | WeightFormat::Q4_0 | WeightFormat::Mxfp4 => {
                let out = &mut buf[..BLOCK];
                for (b, block) in raw.chunks_exact(self.format.block_bytes()).enumerate() {
                    match self.format {
                        WeightFormat::Q8_0 => dequantize_q8_0_block(block, out),
                        WeightFormat::Q4_0 => dequantize_q4_0_block(block, out),
                        _ => formats::dequantize_mxfp4_block(block, out),
                    }
                    f(b * BLOCK, out);
                }
            }
            WeightFormat::Q4K => {
                for (b, block) in raw.as_chunks::<Q4_K_BYTES>().0.iter().enumerate() {
                    formats::dequantize_q4_k_block(block, &mut buf);
                    f(b * QK_K, &buf);
                }
            }
        }
    }
    /// `out[r] = sum_c W[r, c] * x[c]`, accumulated in FP32 in ascending column
    /// order ([`Kernel::Scalar`]). Quantized weights are dequantized element by
    /// element (rounded to FP32) before the product; activations stay FP32.
    pub fn matvec(&self, x: &[f32], out: &mut [f32]) -> Result<(), Error> {
        self.matvec_with(Kernel::Scalar, x, out)
    }
    /// [`Matrix::matvec`] with the given dot-product kernel.
    pub fn matvec_with(&self, kernel: Kernel, x: &[f32], out: &mut [f32]) -> Result<(), Error> {
        if x.len() != self.cols || out.len() != self.rows {
            return Err(Error::Shape("matvec operand lengths"));
        }
        for (row, out) in out.iter_mut().enumerate() {
            *out = self.row_dot(kernel, row, x);
        }
        Ok(())
    }
    /// Batched product of `n = accs.len()` vectors: `xs` is `[n, cols]` and
    /// `out` is `[n, rows]`. Each weight block is decoded once and applied to
    /// every vector; each result is bit-identical to [`Matrix::matvec_with`]
    /// on that vector with the same kernel. `accs` is caller scratch.
    pub fn matmul_with(
        &self,
        kernel: Kernel,
        xs: &[f32],
        out: &mut [f32],
        accs: &mut [DotAcc],
    ) -> Result<(), Error> {
        let n = accs.len();
        if n == 0
            || Some(xs.len()) != n.checked_mul(self.cols)
            || Some(out.len()) != n.checked_mul(self.rows)
        {
            return Err(Error::Shape("matmul operand lengths"));
        }
        let cols = self.cols;
        for row in 0..self.rows {
            accs.fill(DotAcc::ZERO);
            self.row_chunks(row, |start, values| {
                for (t, acc) in accs.iter_mut().enumerate() {
                    let x = &xs[t * cols + start..t * cols + start + values.len()];
                    simd::dot_acc(kernel, acc, values, x);
                }
            });
            for (t, acc) in accs.iter().enumerate() {
                out[t * self.rows + row] = acc.finish(kernel);
            }
        }
        Ok(())
    }
    /// Dot product of row `row` with `x`; lengths are checked by callers.
    pub(crate) fn row_dot(&self, kernel: Kernel, row: usize, x: &[f32]) -> f32 {
        let mut acc = DotAcc::ZERO;
        self.row_chunks(row, |start, values| {
            simd::dot_acc(kernel, &mut acc, values, &x[start..start + values.len()]);
        });
        acc.finish(kernel)
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
    for block in data.as_chunks::<BLOCK>().0.iter() {
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
    for block in data.as_chunks::<BLOCK>().0.iter() {
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
