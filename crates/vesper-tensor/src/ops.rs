//! FP32 decoder kernels: RMSNorm, RoPE, SwiGLU, softmax and argmax.
//!
//! Every kernel writes into caller-provided buffers and performs no heap
//! allocation. Arithmetic is ordered FP32 without `mul_add`, in the same order
//! as the llama2.c reference (`run.c`) so results are reproducible; there is no
//! claim of cross-ISA bit equality for `exp`, `powf`, `sin` and `cos`, which
//! come from the platform math library.
use crate::quant::Matrix;
use crate::Error;

/// `out[i] = weight[i] * (x[i] / sqrt(mean(x^2) + epsilon))`.
pub fn rms_norm(out: &mut [f32], x: &[f32], weight: &[f32], epsilon: f32) -> Result<(), Error> {
    if x.is_empty() || out.len() != x.len() || weight.len() != x.len() {
        return Err(Error::Shape("rms_norm lengths"));
    }
    if !(epsilon.is_finite() && epsilon > 0.0) {
        return Err(Error::InvalidEpsilon);
    }
    let mut ss = 0.0f32;
    for v in x {
        ss += v * v;
    }
    ss /= x.len() as f32;
    ss += epsilon;
    let inv = 1.0 / ss.sqrt();
    for ((o, w), v) in out.iter_mut().zip(weight).zip(x) {
        *o = w * (inv * v);
    }
    Ok(())
}

/// Rotary position embedding on interleaved pairs `(2i, 2i + 1)` of each head,
/// with frequency `base^(-2i / head_dim)` and angle `position * frequency`.
/// `x` holds whole heads of width `head_dim` (which must be even).
pub fn rope(x: &mut [f32], head_dim: usize, position: usize, base: f32) -> Result<(), Error> {
    if head_dim == 0 || !head_dim.is_multiple_of(2) {
        return Err(Error::Shape("rope head_dim must be positive and even"));
    }
    if x.is_empty() || !x.len().is_multiple_of(head_dim) {
        return Err(Error::Shape("rope needs whole heads"));
    }
    if !(base.is_finite() && base > 1.0) {
        return Err(Error::Shape("rope base"));
    }
    for head in x.chunks_exact_mut(head_dim) {
        for i in (0..head_dim).step_by(2) {
            let (fcr, fci) = rope_rotation(i, head_dim, position, base);
            let (v0, v1) = (head[i], head[i + 1]);
            head[i] = v0 * fcr - v1 * fci;
            head[i + 1] = v0 * fci + v1 * fcr;
        }
    }
    Ok(())
}
pub(crate) fn rope_rotation(i: usize, head_dim: usize, position: usize, base: f32) -> (f32, f32) {
    let freq = 1.0 / base.powf(i as f32 / head_dim as f32);
    let angle = position as f32 * freq;
    (angle.cos(), angle.sin())
}

/// `silu(v) = v * sigmoid(v)`.
pub fn silu(v: f32) -> f32 {
    v * (1.0 / (1.0 + (-v).exp()))
}

/// Unfused SwiGLU gate: `gate[i] = silu(gate[i]) * up[i]`.
pub fn swiglu(gate: &mut [f32], up: &[f32]) -> Result<(), Error> {
    if gate.len() != up.len() || gate.is_empty() {
        return Err(Error::Shape("swiglu lengths"));
    }
    for (g, u) in gate.iter_mut().zip(up) {
        *g = silu(*g) * u;
    }
    Ok(())
}

/// Hand-fused SwiGLU projection: `out[i] = silu(w1[i]·x) * (w3[i]·x)` row by
/// row, without materializing either projection. Bit-identical to
/// `w1.matvec`, `w3.matvec`, then [`swiglu`] (same per-row sums, same order).
pub fn swiglu_matvec(w1: &Matrix, w3: &Matrix, x: &[f32], out: &mut [f32]) -> Result<(), Error> {
    if w1.rows() != w3.rows()
        || w1.cols() != w3.cols()
        || x.len() != w1.cols()
        || out.len() != w1.rows()
    {
        return Err(Error::Shape("swiglu_matvec operand lengths"));
    }
    for (row, out) in out.iter_mut().enumerate() {
        let gate = w1.row_dot(row, x);
        let up = w3.row_dot(row, x);
        *out = silu(gate) * up;
    }
    Ok(())
}

/// In-place numerically stable softmax (max subtraction, ascending sums).
pub fn softmax(x: &mut [f32]) -> Result<(), Error> {
    if x.is_empty() {
        return Err(Error::Shape("softmax of empty vector"));
    }
    let max = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        return Err(Error::NonFinite);
    }
    let mut sum = 0.0f32;
    for v in x.iter_mut() {
        *v = (*v - max).exp();
        sum += *v;
    }
    for v in x.iter_mut() {
        *v /= sum;
    }
    Ok(())
}

/// Index of the largest value; the first one wins ties. Non-finite inputs fail.
pub fn argmax(x: &[f32]) -> Result<usize, Error> {
    let mut best = (0usize, f32::NEG_INFINITY);
    if x.is_empty() {
        return Err(Error::Shape("argmax of empty vector"));
    }
    for (i, &v) in x.iter().enumerate() {
        if !v.is_finite() {
            return Err(Error::NonFinite);
        }
        if v > best.1 {
            best = (i, v);
        }
    }
    Ok(best.0)
}

/// `acc[i] += x[i]`.
pub fn add_assign(acc: &mut [f32], x: &[f32]) -> Result<(), Error> {
    if acc.len() != x.len() {
        return Err(Error::Shape("residual lengths"));
    }
    for (a, x) in acc.iter_mut().zip(x) {
        *a += x;
    }
    Ok(())
}
