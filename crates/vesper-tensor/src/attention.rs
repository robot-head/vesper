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
use crate::quant::decode_bf16;
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

fn element(bytes: &[u8], format: KvFormat, index: usize) -> f32 {
    match format {
        KvFormat::F32 => {
            let b = &bytes[index * 4..index * 4 + 4];
            f32::from_le_bytes([b[0], b[1], b[2], b[3]])
        }
        KvFormat::Bf16 => {
            let b = &bytes[index * 2..index * 2 + 2];
            decode_bf16(u16::from_le_bytes([b[0], b[1]]))
        }
    }
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
    let tokens = pool.len(sequence).map_err(kv)?;
    if tokens == 0 {
        return Err(Error::Shape("attention over an empty sequence"));
    }
    let pages = pool.page_count(sequence).map_err(kv)?;
    let format = pool.format();
    let scale = (head_dim as f32).sqrt();
    let kv_stride = kv_heads * head_dim;
    if mode == AttentionMode::TwoPass && scores.len() < query_heads * tokens {
        return Err(Error::Shape("attention score scratch too small"));
    }
    for h in 0..query_heads {
        let kv_head =
            crate::shape::gqa_kv_head(query_heads as u64, kv_heads as u64, h as u64)? as usize;
        let q = &query[h * head_dim..(h + 1) * head_dim];
        let o = &mut out[h * head_dim..(h + 1) * head_dim];
        let dot = |keys: &[u8], t: usize| {
            let base = t * kv_stride + kv_head * head_dim;
            let mut s = 0.0f32;
            for (i, q) in q.iter().enumerate() {
                s += q * element(keys, format, base + i);
            }
            s / scale
        };
        match mode {
            AttentionMode::TwoPass => {
                let att = &mut scores[h * tokens..(h + 1) * tokens];
                for p in 0..pages {
                    let page = pool.read_page(sequence, p).map_err(kv)?;
                    for t in 0..page.tokens {
                        att[page.first_position + t] = dot(page.keys, t);
                    }
                }
                softmax(att)?;
                o.fill(0.0);
                for p in 0..pages {
                    let page = pool.read_page(sequence, p).map_err(kv)?;
                    for t in 0..page.tokens {
                        let a = att[page.first_position + t];
                        let base = t * kv_stride + kv_head * head_dim;
                        for (i, o) in o.iter_mut().enumerate() {
                            *o += a * element(page.values, format, base + i);
                        }
                    }
                }
            }
            AttentionMode::Online => {
                let mut max = f32::NEG_INFINITY;
                let mut sum = 0.0f32;
                o.fill(0.0);
                for p in 0..pages {
                    let page = pool.read_page(sequence, p).map_err(kv)?;
                    let mut block = [0.0f32; TOKENS_PER_PAGE];
                    let mut block_max = f32::NEG_INFINITY;
                    for (t, b) in block.iter_mut().enumerate().take(page.tokens) {
                        *b = dot(page.keys, t);
                        block_max = block_max.max(*b);
                    }
                    if !block_max.is_finite() {
                        return Err(Error::NonFinite);
                    }
                    if block_max > max {
                        let rescale = (max - block_max).exp(); // exp(-inf) = 0 first time
                        sum *= rescale;
                        for o in o.iter_mut() {
                            *o *= rescale;
                        }
                        max = block_max;
                    }
                    for (t, &s) in block.iter().enumerate().take(page.tokens) {
                        let weight = (s - max).exp();
                        sum += weight;
                        let base = t * kv_stride + kv_head * head_dim;
                        for (i, o) in o.iter_mut().enumerate() {
                            *o += weight * element(page.values, format, base + i);
                        }
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
