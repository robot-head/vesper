//! Grouped-query decode attention over a paged [`KvPool`] sequence.
//!
//! Query head `h` reads KV head `h / (query_heads / kv_heads)` (checked by
//! [`crate::shape::gqa_kv_head`]). Keys and values are read page by page
//! through [`KvPool::read_page`], so every access goes through a validated
//! page lease; positions past the sequence length are unreachable.
//!
//! Two schedules are provided:
//!
//! * [`AttentionMode::TwoPass`] materializes all scores, applies [`softmax`],
//!   then accumulates values, in the same order as llama2.c `run.c`.
//! * [`AttentionMode::Online`] is a single pass with a running maximum and sum
//!   per head, merged one 16-token page at a time (the FlashDecoding
//!   recurrence). It needs no score buffer. It is mathematically equal to the
//!   two-pass schedule but rounds differently; tests bound the difference.
use crate::kv::{KvFormat, KvPool, Sequence, TOKENS_PER_PAGE};
use crate::ops::softmax;
use crate::simd::{self, ByteElements, DotAcc, Kernel};
use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionMode {
    TwoPass,
    Online,
}

/// Shape of one attention call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttentionShape {
    pub query_heads: usize,
    pub kv_heads: usize,
    pub head_dim: usize,
}

/// Attends `query` (`[query_heads, head_dim]`) over every token stored for
/// `sequence`, writing `[query_heads, head_dim]` into `out`. `scores` is
/// scratch of at least `query_heads * tokens` values for
/// [`AttentionMode::TwoPass`] and may be empty for [`AttentionMode::Online`].
/// Performs no heap allocation.
pub fn paged_gqa_attention(
    pool: &KvPool,
    sequence: &Sequence,
    shape: AttentionShape,
    query: &[f32],
    scores: &mut [f32],
    out: &mut [f32],
    mode: AttentionMode,
) -> Result<(), Error> {
    paged_gqa_attention_with(
        pool,
        sequence,
        shape,
        query,
        scores,
        out,
        mode,
        Kernel::Scalar,
        None,
    )
}

/// [`paged_gqa_attention`] with a dot-product [`Kernel`] and an optional
/// causal limit: with `visible = Some(n)` only the first `n` stored tokens
/// are attended (`1 <= n <= len`), which is how batched prefill gives each
/// prompt token exactly the context a one-token decode step would see.
#[allow(clippy::too_many_arguments)]
pub fn paged_gqa_attention_with(
    pool: &KvPool,
    sequence: &Sequence,
    shape: AttentionShape,
    query: &[f32],
    scores: &mut [f32],
    out: &mut [f32],
    mode: AttentionMode,
    kernel: Kernel,
    visible: Option<usize>,
) -> Result<(), Error> {
    let AttentionShape {
        query_heads,
        kv_heads,
        head_dim,
    } = shape;
    let config = pool.config();
    if config.heads != kv_heads || config.head_dim != head_dim {
        return Err(Error::Shape("attention shape differs from the KV pool"));
    }
    let width = query_heads.checked_mul(head_dim).ok_or(Error::Overflow)?;
    if query.len() != width || out.len() != width {
        return Err(Error::Shape("attention query/output lengths"));
    }
    crate::shape::gqa_ratio(query_heads as u64, kv_heads as u64)?;
    let stored = pool.len(sequence).map_err(kv)?;
    let tokens = match visible {
        None => stored,
        Some(n) if n <= stored => n,
        Some(_) => return Err(Error::Shape("attention beyond the stored tokens")),
    };
    if tokens == 0 {
        return Err(Error::Shape("attention over an empty sequence"));
    }
    let pages = tokens.div_ceil(TOKENS_PER_PAGE);
    let elements = match pool.format() {
        KvFormat::F32 => ByteElements::F32Le,
        KvFormat::Bf16 => ByteElements::Bf16Le,
    };
    let width_bytes = elements.width();
    let scale = (head_dim as f32).sqrt();
    let kv_stride = kv_heads * head_dim;
    if mode == AttentionMode::TwoPass && scores.len() < query_heads * tokens {
        return Err(Error::Shape("attention score scratch too small"));
    }
    let head_bytes = |t: usize, kv_head: usize| -> std::ops::Range<usize> {
        let base = (t * kv_stride + kv_head * head_dim) * width_bytes;
        base..base + head_dim * width_bytes
    };
    for h in 0..query_heads {
        let kv_head =
            crate::shape::gqa_kv_head(query_heads as u64, kv_heads as u64, h as u64)? as usize;
        let q = &query[h * head_dim..(h + 1) * head_dim];
        let o = &mut out[h * head_dim..(h + 1) * head_dim];
        let dot = |keys: &[u8], t: usize| {
            let mut acc = DotAcc::ZERO;
            simd::dot_acc_bytes(kernel, &mut acc, q, &keys[head_bytes(t, kv_head)], elements);
            acc.finish(kernel) / scale
        };
        // Tokens of page `p` that are visible.
        let page_tokens =
            |page: &crate::kv::KvPage<'_>| page.tokens.min(tokens - page.first_position);
        match mode {
            AttentionMode::TwoPass => {
                let att = &mut scores[h * tokens..(h + 1) * tokens];
                for p in 0..pages {
                    let page = pool.read_page(sequence, p).map_err(kv)?;
                    for t in 0..page_tokens(&page) {
                        att[page.first_position + t] = dot(page.keys, t);
                    }
                }
                softmax(att)?;
                o.fill(0.0);
                for p in 0..pages {
                    let page = pool.read_page(sequence, p).map_err(kv)?;
                    for t in 0..page_tokens(&page) {
                        let a = att[page.first_position + t];
                        let v = &page.values[head_bytes(t, kv_head)];
                        simd::axpy_bytes(kernel, o, a, v, elements);
                    }
                }
            }
            AttentionMode::Online => {
                let mut max = f32::NEG_INFINITY;
                let mut sum = 0.0f32;
                o.fill(0.0);
                for p in 0..pages {
                    let page = pool.read_page(sequence, p).map_err(kv)?;
                    let n = page_tokens(&page);
                    let mut block = [0.0f32; TOKENS_PER_PAGE];
                    let mut block_max = f32::NEG_INFINITY;
                    for (t, b) in block.iter_mut().enumerate().take(n) {
                        *b = dot(page.keys, t);
                        block_max = block_max.max(*b);
                    }
                    if !block_max.is_finite() {
                        return Err(Error::NonFinite);
                    }
                    if block_max > max {
                        let rescale = (max - block_max).exp(); // exp(-inf) = 0 first time
                        sum *= rescale;
                        simd::scale(kernel, o, rescale);
                        max = block_max;
                    }
                    for (t, &s) in block.iter().enumerate().take(n) {
                        let weight = (s - max).exp();
                        sum += weight;
                        let v = &page.values[head_bytes(t, kv_head)];
                        simd::axpy_bytes(kernel, o, weight, v, elements);
                    }
                }
                for o in o.iter_mut() {
                    *o /= sum;
                }
            }
        }
    }
    if out.iter().any(|v| !v.is_finite()) {
        return Err(Error::NonFinite);
    }
    Ok(())
}

pub(crate) fn kv(error: crate::kv::KvError) -> Error {
    Error::Kv(error)
}
