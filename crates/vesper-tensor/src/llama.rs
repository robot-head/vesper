//! Decoder-only Llama-architecture inference on the CPU.
//!
//! [`Model::from_llama2c`] parses the llama2.c checkpoint format (a header of
//! seven little-endian `i32`s followed by FP32 weights) and
//! [`Tokenizer::from_llama2c`] its tokenizer format. [`Model::forward`] runs
//! one decode step: RMSNorm, Q/K/V projections, RoPE, a paged grouped-query
//! attention over a [`KvCache`] sequence, the output projection, a fused
//! SwiGLU feed-forward block and the classifier. [`generate_greedy`] decodes
//! with argmax sampling and [`generate`] with any [`Sampler`], prefilling the
//! prompt in one batched call.
//!
//! Models also load from GGUF ([`Model::from_gguf`], llama.cpp tensor names)
//! and Hugging Face-layout safetensors ([`Model::from_safetensors`]); weights
//! stay in the (optionally memory-mapped) file. [`Model::forward_batch`] runs
//! several tokens of several sequences in one call: each weight block is
//! decoded once for the whole batch, and each sequence's logits are
//! bit-identical to running its tokens one at a time through
//! [`Model::forward_with`] with the same kernel.
//!
//! The K/V cache is one [`KvPool`] per layer. A [`Session`] owns one
//! [`Sequence`] per layer and must be returned with [`KvCache::release`];
//! dropping an unreleased session is counted by [`leaked_sessions`] (its pages
//! stay leased). This is host-library discipline, not the Vesper source
//! linearity rules.
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::attention::{kv, paged_gqa_attention_with, AttentionMode, AttentionShape};
use crate::gguf::{self, GgmlType, Gguf};
use crate::kv::{KvConfig, KvFormat, KvPool, Sequence, TOKENS_PER_PAGE};
use crate::ops::{add_assign, argmax, rms_norm, rope, rope_half, silu, swiglu_matvec_with};
use crate::quant::{encode_bf16, Matrix, WeightFormat};
use crate::safetensors::{self, Dtype, SafeTensors};
use crate::sampling::{Sampler, SamplerConfig};
use crate::simd::{DotAcc, Kernel};
use crate::Error;

pub const BOS: u32 = 1;
pub const RMS_EPSILON: f32 = 1e-5;
pub const ROPE_BASE: f32 = 10_000.0;

/// Hard ceilings on accepted checkpoints.
pub const MAX_DIM: usize = 8192;
pub const MAX_LAYERS: usize = 256;
pub const MAX_VOCAB: usize = 1 << 20;
pub const MAX_SEQ_LEN: usize = 65_536;
pub const MAX_CHECKPOINT_BYTES: usize = 1 << 30;

/// How RoPE pairs the elements of a head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RopeStyle {
    /// Pairs `(2i, 2i + 1)`: llama2.c, GGML `LLAMA_ROPE_TYPE_NORM` (GGUF
    /// `llama`).
    Interleaved,
    /// Pairs `(i, i + head_dim / 2)`: Hugging Face `rotate_half`.
    Half,
}

/// Per-call execution choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunOptions {
    pub attention: AttentionMode,
    pub kernel: Kernel,
}
impl RunOptions {
    pub const SCALAR: Self = Self {
        attention: AttentionMode::TwoPass,
        kernel: Kernel::Scalar,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub dim: usize,
    pub hidden_dim: usize,
    pub layers: usize,
    pub heads: usize,
    pub kv_heads: usize,
    pub vocab: usize,
    pub seq_len: usize,
    /// The classifier reuses the token embedding (positive header vocab size).
    pub shared_classifier: bool,
    pub rope: RopeStyle,
    pub rope_base: f32,
    pub rms_epsilon: f32,
}
impl Config {
    /// Checks every dimension is positive and within the hard ceilings,
    /// `dim % heads == 0`, `heads % kv_heads == 0` and an even head width.
    pub fn validate(&self) -> Result<(), Error> {
        let bounds = [
            (self.dim, MAX_DIM, "dim"),
            (self.hidden_dim, 4 * MAX_DIM, "hidden_dim"),
            (self.layers, MAX_LAYERS, "layers"),
            (self.heads, MAX_DIM, "heads"),
            (self.kv_heads, MAX_DIM, "kv_heads"),
            (self.vocab, MAX_VOCAB, "vocab"),
            (self.seq_len, MAX_SEQ_LEN, "seq_len"),
        ];
        for (value, max, what) in bounds {
            if value == 0 {
                return Err(Error::Malformed(what));
            }
            if value > max {
                return Err(Error::Limit(what));
            }
        }
        if !self.dim.is_multiple_of(self.heads) {
            return Err(Error::Shape("dim must be a multiple of heads"));
        }
        crate::shape::gqa_ratio(self.heads as u64, self.kv_heads as u64)?;
        if !self.head_dim().is_multiple_of(2) {
            return Err(Error::Shape("RoPE needs an even head width"));
        }
        if !(self.rope_base.is_finite() && self.rope_base > 1.0) {
            return Err(Error::Shape("rope base"));
        }
        if !(self.rms_epsilon.is_finite() && self.rms_epsilon > 0.0) {
            return Err(Error::InvalidEpsilon);
        }
        Ok(())
    }
    /// The llama2.c configuration defaults (interleaved RoPE, base 10000,
    /// epsilon 1e-5).
    #[allow(clippy::too_many_arguments)]
    pub fn llama2c(
        dim: usize,
        hidden_dim: usize,
        layers: usize,
        heads: usize,
        kv_heads: usize,
        vocab: usize,
        seq_len: usize,
        shared_classifier: bool,
    ) -> Self {
        Self {
            dim,
            hidden_dim,
            layers,
            heads,
            kv_heads,
            vocab,
            seq_len,
            shared_classifier,
            rope: RopeStyle::Interleaved,
            rope_base: ROPE_BASE,
            rms_epsilon: RMS_EPSILON,
        }
    }
    pub fn head_dim(&self) -> usize {
        self.dim / self.heads
    }
    pub fn kv_dim(&self) -> usize {
        self.kv_heads * self.head_dim()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub rms_attention: Vec<f32>,
    pub wq: Matrix,
    pub wk: Matrix,
    pub wv: Matrix,
    pub wo: Matrix,
    pub rms_ffn: Vec<f32>,
    pub w1: Matrix,
    pub w2: Matrix,
    pub w3: Matrix,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    config: Config,
    embedding: Matrix,
    layers: Vec<Layer>,
    rms_final: Vec<f32>,
    classifier: Matrix,
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn i32(&mut self) -> Result<i32, Error> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Result<f32, Error> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.offset.checked_add(n).ok_or(Error::Overflow)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(Error::Malformed("truncated input"))?;
        self.offset = end;
        Ok(slice)
    }
    fn floats(&mut self, n: usize) -> Result<Vec<f32>, Error> {
        let bytes = self.take(n.checked_mul(4).ok_or(Error::Overflow)?)?;
        let mut out = Vec::new();
        out.try_reserve_exact(n)
            .map_err(|_| Error::Limit("allocation"))?;
        for b in bytes.as_chunks::<4>().0.iter() {
            let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            if !v.is_finite() {
                return Err(Error::NonFinite);
            }
            out.push(v);
        }
        Ok(out)
    }
    fn matrix(&mut self, rows: usize, cols: usize) -> Result<Matrix, Error> {
        let n = rows.checked_mul(cols).ok_or(Error::Overflow)?;
        Matrix::f32(rows, cols, self.floats(n)?)
    }
}

fn positive(v: i32, what: &'static str, max: usize) -> Result<usize, Error> {
    if v <= 0 {
        return Err(Error::Malformed(what));
    }
    let v = v as usize;
    if v > max {
        return Err(Error::Limit(what));
    }
    Ok(v)
}

impl Model {
    /// Parses a llama2.c (`run.c` "version 0") checkpoint. Every dimension is
    /// checked (positive, bounded, `dim % heads == 0`, `heads % kv_heads == 0`,
    /// even head width), the byte length must match the header exactly, and
    /// every weight must be finite.
    pub fn from_llama2c(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(Error::Limit("checkpoint bytes"));
        }
        let mut r = Reader { bytes, offset: 0 };
        let dim = positive(r.i32()?, "dim", MAX_DIM)?;
        let hidden_dim = positive(r.i32()?, "hidden_dim", 4 * MAX_DIM)?;
        let layers = positive(r.i32()?, "layers", MAX_LAYERS)?;
        let heads = positive(r.i32()?, "heads", MAX_DIM)?;
        let kv_heads = positive(r.i32()?, "kv_heads", MAX_DIM)?;
        let raw_vocab = r.i32()?;
        let shared_classifier = raw_vocab > 0;
        let vocab = positive(
            raw_vocab.checked_abs().ok_or(Error::Malformed("vocab"))?,
            "vocab",
            MAX_VOCAB,
        )?;
        let seq_len = positive(r.i32()?, "seq_len", MAX_SEQ_LEN)?;
        let config = Config::llama2c(
            dim,
            hidden_dim,
            layers,
            heads,
            kv_heads,
            vocab,
            seq_len,
            shared_classifier,
        );
        config.validate()?;
        let head_dim = config.head_dim();
        let kv_dim = config.kv_dim();
        let embedding = r.floats(vocab.checked_mul(dim).ok_or(Error::Overflow)?)?;
        let per_layer = |r: &mut Reader<'_>, n: usize| -> Result<Vec<Vec<f32>>, Error> {
            (0..layers).map(|_| r.floats(n)).collect()
        };
        let per_layer_matrix =
            |r: &mut Reader<'_>, rows: usize, cols: usize| -> Result<Vec<Matrix>, Error> {
                (0..layers).map(|_| r.matrix(rows, cols)).collect()
            };
        let rms_attention = per_layer(&mut r, dim)?;
        let wq = per_layer_matrix(&mut r, dim, dim)?;
        let wk = per_layer_matrix(&mut r, kv_dim, dim)?;
        let wv = per_layer_matrix(&mut r, kv_dim, dim)?;
        let wo = per_layer_matrix(&mut r, dim, dim)?;
        let rms_ffn = per_layer(&mut r, dim)?;
        let w1 = per_layer_matrix(&mut r, hidden_dim, dim)?;
        let w2 = per_layer_matrix(&mut r, dim, hidden_dim)?;
        let w3 = per_layer_matrix(&mut r, hidden_dim, dim)?;
        let rms_final = r.floats(dim)?;
        // Legacy precomputed RoPE tables (real and imaginary halves); unused
        // because RoPE is computed from the position.
        r.take(
            seq_len
                .checked_mul(head_dim)
                .and_then(|n| n.checked_mul(4))
                .ok_or(Error::Overflow)?,
        )?;
        let embedding = Matrix::f32(vocab, dim, embedding)?;
        let classifier = if shared_classifier {
            embedding.clone()
        } else {
            r.matrix(vocab, dim)?
        };
        if r.offset != bytes.len() {
            return Err(Error::Malformed("trailing bytes after checkpoint"));
        }
        let mut layer_vec = Vec::with_capacity(layers);
        let mut wq = wq.into_iter();
        let mut wk = wk.into_iter();
        let mut wv = wv.into_iter();
        let mut wo = wo.into_iter();
        let mut w1 = w1.into_iter();
        let mut w2 = w2.into_iter();
        let mut w3 = w3.into_iter();
        for (rms_attention, rms_ffn) in rms_attention.into_iter().zip(rms_ffn) {
            let missing = || Error::Malformed("layer");
            layer_vec.push(Layer {
                rms_attention,
                wq: wq.next().ok_or_else(missing)?,
                wk: wk.next().ok_or_else(missing)?,
                wv: wv.next().ok_or_else(missing)?,
                wo: wo.next().ok_or_else(missing)?,
                rms_ffn,
                w1: w1.next().ok_or_else(missing)?,
                w2: w2.next().ok_or_else(missing)?,
                w3: w3.next().ok_or_else(missing)?,
            });
        }
        Ok(Self {
            config,
            embedding,
            layers: layer_vec,
            rms_final,
            classifier,
        })
    }
    pub fn config(&self) -> Config {
        self.config
    }
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }
    pub fn embedding(&self) -> &Matrix {
        &self.embedding
    }
    pub fn rms_final(&self) -> &[f32] {
        &self.rms_final
    }
    pub fn classifier(&self) -> &Matrix {
        &self.classifier
    }
    /// Quantizes every FP32 layer projection whose input width is a whole
    /// number of `format` blocks to `format`. Other matrices (and the
    /// embedding, norms and classifier) are unchanged. Returns the model and
    /// the number of projections kept in their original format.
    pub fn quantized(&self, format: WeightFormat) -> Result<(Self, usize), Error> {
        let mut kept = 0usize;
        let mut q = |m: &Matrix| -> Result<Matrix, Error> {
            if format == WeightFormat::F32 {
                return Ok(m.clone());
            }
            if m.format() != WeightFormat::F32 || !m.cols().is_multiple_of(format.block_elements())
            {
                kept += 1;
                return Ok(m.clone());
            }
            m.quantize(format)
        };
        let mut layers = Vec::with_capacity(self.layers.len());
        for l in &self.layers {
            layers.push(Layer {
                rms_attention: l.rms_attention.clone(),
                wq: q(&l.wq)?,
                wk: q(&l.wk)?,
                wv: q(&l.wv)?,
                wo: q(&l.wo)?,
                rms_ffn: l.rms_ffn.clone(),
                w1: q(&l.w1)?,
                w2: q(&l.w2)?,
                w3: q(&l.w3)?,
            });
        }
        Ok((
            Self {
                config: self.config,
                embedding: self.embedding.clone(),
                layers,
                rms_final: self.rms_final.clone(),
                classifier: self.classifier.clone(),
            },
            kept,
        ))
    }
    /// Payload bytes of every weight the model owns (excluding container
    /// overhead). A shared classifier is a separate FP32 copy of the
    /// embedding and is counted.
    pub fn weight_bytes(&self) -> usize {
        let mut total = self.embedding.payload_bytes() + self.rms_final.len() * 4;
        total += self.classifier.payload_bytes();
        for l in &self.layers {
            total += (l.rms_attention.len() + l.rms_ffn.len()) * 4;
            for m in [&l.wq, &l.wk, &l.wv, &l.wo, &l.w1, &l.w2, &l.w3] {
                total += m.payload_bytes();
            }
        }
        total
    }
    fn matrices(&self) -> impl Iterator<Item = &Matrix> {
        [&self.embedding, &self.classifier].into_iter().chain(
            self.layers
                .iter()
                .flat_map(|l| [&l.wq, &l.wk, &l.wv, &l.wo, &l.w1, &l.w2, &l.w3]),
        )
    }
    /// Payload bytes of matrices borrowed from an operating-system file
    /// mapping (a shared classifier counts once per use).
    pub fn mapped_weight_bytes(&self) -> usize {
        self.matrices()
            .filter(|m| m.is_file_mapped())
            .map(Matrix::payload_bytes)
            .sum()
    }

    /// Runs one decode step for `token` at the next position of `session`,
    /// appending its K/V to every layer's pages, and returns the logits.
    /// After [`KvCache::begin`] warm-up this performs no heap allocation.
    pub fn forward<'s>(
        &self,
        cache: &mut KvCache,
        session: &'s mut Session,
        token: u32,
        mode: AttentionMode,
    ) -> Result<&'s [f32], Error> {
        self.forward_with(
            cache,
            session,
            token,
            RunOptions {
                attention: mode,
                kernel: Kernel::Scalar,
            },
        )
    }

    fn rope(&self, x: &mut [f32], position: usize) -> Result<(), Error> {
        let c = self.config;
        match c.rope {
            RopeStyle::Interleaved => rope(x, c.head_dim(), position, c.rope_base),
            RopeStyle::Half => rope_half(x, c.head_dim(), position, c.rope_base),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append_kv(
        &self,
        pool: &mut KvPool,
        sequence: &mut Sequence,
        position: usize,
        k: &[f32],
        v: &[f32],
        k16: &mut [u8],
        v16: &mut [u8],
    ) -> Result<(), Error> {
        let c = self.config;
        match pool.format() {
            KvFormat::F32 => pool.append_f32(sequence, position, k, v),
            KvFormat::Bf16 => {
                for (dst, src) in k16.as_chunks_mut::<2>().0.iter_mut().zip(k) {
                    dst.copy_from_slice(&encode_bf16(*src).to_le_bytes());
                }
                for (dst, src) in v16.as_chunks_mut::<2>().0.iter_mut().zip(v) {
                    dst.copy_from_slice(&encode_bf16(*src).to_le_bytes());
                }
                let step = |bytes| crate::kv::Bf16Step {
                    heads: c.kv_heads,
                    head_dim: c.head_dim(),
                    bytes,
                };
                pool.append(sequence, position, step(k16), step(v16))
            }
        }
        .map_err(kv)
    }

    /// [`Model::forward`] with explicit [`RunOptions`].
    pub fn forward_with<'s>(
        &self,
        cache: &mut KvCache,
        session: &'s mut Session,
        token: u32,
        options: RunOptions,
    ) -> Result<&'s [f32], Error> {
        let c = self.config;
        let kernel = options.kernel;
        if cache.identity != session.cache || cache.config != c {
            return Err(Error::InvalidId);
        }
        let token = token as usize;
        if token >= c.vocab {
            return Err(Error::Shape("token outside the vocabulary"));
        }
        let position = session.position;
        if position >= c.seq_len {
            return Err(Error::Limit("sequence length"));
        }
        let s = &mut session.scratch;
        self.embedding.dequantize_row(token, &mut s.x)?;
        let shape = self.attention_shape();
        for (l, layer) in self.layers.iter().enumerate() {
            rms_norm(&mut s.xb, &s.x, &layer.rms_attention, c.rms_epsilon)?;
            layer.wq.matvec_with(kernel, &s.xb, &mut s.q)?;
            layer.wk.matvec_with(kernel, &s.xb, &mut s.k)?;
            layer.wv.matvec_with(kernel, &s.xb, &mut s.v)?;
            self.rope(&mut s.q, position)?;
            self.rope(&mut s.k, position)?;
            let pool = &mut cache.pools[l];
            let sequence = &mut session.sequences[l];
            self.append_kv(pool, sequence, position, &s.k, &s.v, &mut s.k16, &mut s.v16)?;
            paged_gqa_attention_with(
                pool,
                sequence,
                shape,
                &s.q,
                &mut s.att,
                &mut s.xb,
                options.attention,
                kernel,
                None,
            )?;
            layer.wo.matvec_with(kernel, &s.xb, &mut s.xb2)?;
            add_assign(&mut s.x, &s.xb2)?;
            rms_norm(&mut s.xb, &s.x, &layer.rms_ffn, c.rms_epsilon)?;
            swiglu_matvec_with(kernel, &layer.w1, &layer.w3, &s.xb, &mut s.hb)?;
            layer.w2.matvec_with(kernel, &s.hb, &mut s.xb2)?;
            add_assign(&mut s.x, &s.xb2)?;
        }
        rms_norm(&mut s.xb, &s.x, &self.rms_final, c.rms_epsilon)?;
        self.classifier.matvec_with(kernel, &s.xb, &mut s.logits)?;
        if s.logits.iter().any(|v| !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        session.position += 1;
        Ok(&session.scratch.logits)
    }

    fn attention_shape(&self) -> AttentionShape {
        AttentionShape {
            query_heads: self.config.heads,
            kv_heads: self.config.kv_heads,
            head_dim: self.config.head_dim(),
        }
    }

    /// Scratch for [`Model::forward_batch`] calls of up to `max_tokens`
    /// tokens over up to `max_items` sequences.
    pub fn batch_scratch(
        &self,
        max_tokens: usize,
        max_items: usize,
    ) -> Result<BatchScratch, Error> {
        let c = self.config;
        if max_tokens == 0 || max_items == 0 || max_items > max_tokens {
            return Err(Error::Shape("batch capacity"));
        }
        if max_tokens > MAX_BATCH_TOKENS {
            return Err(Error::Limit("batch tokens"));
        }
        let buffer = |width: usize| -> Result<Vec<f32>, Error> {
            let n = width.checked_mul(max_tokens).ok_or(Error::Overflow)?;
            let mut v = Vec::new();
            v.try_reserve_exact(n)
                .map_err(|_| Error::Limit("allocation"))?;
            v.resize(n, 0.0);
            Ok(v)
        };
        let kv_dim = c.kv_dim();
        Ok(BatchScratch {
            model: c,
            max_tokens,
            max_items,
            x: buffer(c.dim)?,
            xb: buffer(c.dim)?,
            xb2: buffer(c.dim)?,
            q: buffer(c.dim)?,
            k: buffer(kv_dim)?,
            v: buffer(kv_dim)?,
            hb: buffer(c.hidden_dim)?,
            hb2: buffer(c.hidden_dim)?,
            k16: vec![0; kv_dim * 2],
            v16: vec![0; kv_dim * 2],
            att: vec![0.0; c.heads * c.seq_len],
            accs: vec![DotAcc::ZERO; max_tokens],
            last: vec![0.0; c.dim * max_items],
            logits: vec![0.0; c.vocab.checked_mul(max_items).ok_or(Error::Overflow)?],
        })
    }

    /// One forward call over several sequences, each with one or more new
    /// tokens (prefill or decode). Every token is appended to its sequence's
    /// pages in order and attends causally to the tokens before it. Afterwards
    /// each session's [`Session::logits`] holds the logits after its last
    /// token, bit-identical to feeding the same tokens one at a time through
    /// [`Model::forward_with`] with the same options.
    ///
    /// Everything that can fail is checked before any page is written: owners,
    /// token ids, sequence lengths, batch capacity and free pages. After the
    /// scratch is allocated the call performs no heap allocation.
    pub fn forward_batch(
        &self,
        cache: &mut KvCache,
        items: &mut [BatchItem<'_>],
        scratch: &mut BatchScratch,
        options: RunOptions,
    ) -> Result<(), Error> {
        let c = self.config;
        let kernel = options.kernel;
        if cache.config != c || scratch.model != c {
            return Err(Error::InvalidId);
        }
        if items.is_empty() || items.len() > scratch.max_items {
            return Err(Error::Shape("batch item count"));
        }
        let mut total = 0usize;
        for item in items.iter() {
            if item.session.cache != cache.identity {
                return Err(Error::InvalidId);
            }
            if item.tokens.is_empty() {
                return Err(Error::Shape("batch item without tokens"));
            }
            if item.tokens.iter().any(|t| *t as usize >= c.vocab) {
                return Err(Error::Shape("token outside the vocabulary"));
            }
            let end = item
                .session
                .position
                .checked_add(item.tokens.len())
                .ok_or(Error::Overflow)?;
            if end > c.seq_len {
                return Err(Error::Limit("sequence length"));
            }
            total = total
                .checked_add(item.tokens.len())
                .ok_or(Error::Overflow)?;
        }
        if total > scratch.max_tokens {
            return Err(Error::Limit("batch tokens"));
        }
        // Pages: a token at a multiple of 16 takes a new page in every layer.
        let new_pages: usize = items
            .iter()
            .map(|i| {
                let p = i.session.position;
                (p..p + i.tokens.len())
                    .filter(|q| q.is_multiple_of(TOKENS_PER_PAGE))
                    .count()
            })
            .sum();
        if cache.pools.iter().any(|p| p.stats().free_pages < new_pages) {
            return Err(Error::Kv(crate::kv::KvError::OutOfPages));
        }
        let (dim, kv_dim, hidden) = (c.dim, c.kv_dim(), c.hidden_dim);
        let s = scratch;
        let n = total;
        let accs = &mut s.accs[..n];
        {
            let mut t = 0;
            for item in items.iter() {
                for &token in item.tokens {
                    self.embedding
                        .dequantize_row(token as usize, &mut s.x[t * dim..(t + 1) * dim])?;
                    t += 1;
                }
            }
        }
        let shape = self.attention_shape();
        for (l, layer) in self.layers.iter().enumerate() {
            for t in 0..n {
                rms_norm(
                    &mut s.xb[t * dim..(t + 1) * dim],
                    &s.x[t * dim..(t + 1) * dim],
                    &layer.rms_attention,
                    c.rms_epsilon,
                )?;
            }
            layer
                .wq
                .matmul_with(kernel, &s.xb[..n * dim], &mut s.q[..n * dim], accs)?;
            layer
                .wk
                .matmul_with(kernel, &s.xb[..n * dim], &mut s.k[..n * kv_dim], accs)?;
            layer
                .wv
                .matmul_with(kernel, &s.xb[..n * dim], &mut s.v[..n * kv_dim], accs)?;
            let pool = &mut cache.pools[l];
            let mut t = 0;
            for item in items.iter_mut() {
                let start = item.session.position;
                for j in 0..item.tokens.len() {
                    self.rope(&mut s.q[t * dim..(t + 1) * dim], start + j)?;
                    self.rope(&mut s.k[t * kv_dim..(t + 1) * kv_dim], start + j)?;
                    self.append_kv(
                        pool,
                        &mut item.session.sequences[l],
                        start + j,
                        &s.k[t * kv_dim..(t + 1) * kv_dim],
                        &s.v[t * kv_dim..(t + 1) * kv_dim],
                        &mut s.k16,
                        &mut s.v16,
                    )?;
                    t += 1;
                }
            }
            let mut t = 0;
            for item in items.iter() {
                let start = item.session.position;
                for j in 0..item.tokens.len() {
                    paged_gqa_attention_with(
                        pool,
                        &item.session.sequences[l],
                        shape,
                        &s.q[t * dim..(t + 1) * dim],
                        &mut s.att,
                        &mut s.xb[t * dim..(t + 1) * dim],
                        options.attention,
                        kernel,
                        Some(start + j + 1),
                    )?;
                    t += 1;
                }
            }
            layer
                .wo
                .matmul_with(kernel, &s.xb[..n * dim], &mut s.xb2[..n * dim], accs)?;
            add_assign(&mut s.x[..n * dim], &s.xb2[..n * dim])?;
            for t in 0..n {
                rms_norm(
                    &mut s.xb[t * dim..(t + 1) * dim],
                    &s.x[t * dim..(t + 1) * dim],
                    &layer.rms_ffn,
                    c.rms_epsilon,
                )?;
            }
            layer
                .w1
                .matmul_with(kernel, &s.xb[..n * dim], &mut s.hb[..n * hidden], accs)?;
            layer
                .w3
                .matmul_with(kernel, &s.xb[..n * dim], &mut s.hb2[..n * hidden], accs)?;
            for (g, u) in s.hb[..n * hidden].iter_mut().zip(&s.hb2[..n * hidden]) {
                *g = silu(*g) * u;
            }
            layer
                .w2
                .matmul_with(kernel, &s.hb[..n * hidden], &mut s.xb2[..n * dim], accs)?;
            add_assign(&mut s.x[..n * dim], &s.xb2[..n * dim])?;
        }
        // Classifier only for each item's last token.
        let m = items.len();
        let mut t = 0;
        for (i, item) in items.iter().enumerate() {
            t += item.tokens.len();
            rms_norm(
                &mut s.last[i * dim..(i + 1) * dim],
                &s.x[(t - 1) * dim..t * dim],
                &self.rms_final,
                c.rms_epsilon,
            )?;
        }
        let vocab = c.vocab;
        self.classifier.matmul_with(
            kernel,
            &s.last[..m * dim],
            &mut s.logits[..m * vocab],
            &mut s.accs[..m],
        )?;
        if s.logits[..m * vocab].iter().any(|v| !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        for (i, item) in items.iter_mut().enumerate() {
            item.session
                .scratch
                .logits
                .copy_from_slice(&s.logits[i * vocab..(i + 1) * vocab]);
            item.session.position += item.tokens.len();
        }
        Ok(())
    }
}

/// Largest batch accepted by [`Model::batch_scratch`].
pub const MAX_BATCH_TOKENS: usize = 4096;

/// One sequence's share of a [`Model::forward_batch`] call.
#[derive(Debug)]
pub struct BatchItem<'a> {
    pub session: &'a mut Session,
    pub tokens: &'a [u32],
}

/// Preallocated activations for [`Model::forward_batch`].
#[derive(Debug)]
pub struct BatchScratch {
    model: Config,
    max_tokens: usize,
    max_items: usize,
    x: Vec<f32>,
    xb: Vec<f32>,
    xb2: Vec<f32>,
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    hb: Vec<f32>,
    hb2: Vec<f32>,
    k16: Vec<u8>,
    v16: Vec<u8>,
    att: Vec<f32>,
    accs: Vec<DotAcc>,
    last: Vec<f32>,
    logits: Vec<f32>,
}
impl BatchScratch {
    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }
    pub fn max_items(&self) -> usize {
        self.max_items
    }
}

#[derive(Debug)]
struct Scratch {
    x: Vec<f32>,
    xb: Vec<f32>,
    xb2: Vec<f32>,
    hb: Vec<f32>,
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    k16: Vec<u8>,
    v16: Vec<u8>,
    att: Vec<f32>,
    logits: Vec<f32>,
}

static LEAKED_SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Number of [`Session`]s dropped without [`KvCache::release`] in this process.
pub fn leaked_sessions() -> usize {
    LEAKED_SESSIONS.load(Ordering::Relaxed)
}

/// One generation request: a K/V sequence per layer plus preallocated
/// activations. Neither Clone nor Copy; release it with [`KvCache::release`].
#[must_use = "release the session with KvCache::release to reclaim its pages"]
#[derive(Debug)]
pub struct Session {
    cache: u64,
    position: usize,
    sequences: Vec<Sequence>,
    scratch: Box<Scratch>,
}
impl Session {
    /// Number of tokens already decoded (the next position).
    pub fn position(&self) -> usize {
        self.position
    }
    /// Logits after the last processed token.
    pub fn logits(&self) -> &[f32] {
        &self.scratch.logits
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.sequences.is_empty() {
            LEAKED_SESSIONS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Per-layer paged K/V storage for one model configuration.
#[derive(Debug)]
pub struct KvCache {
    identity: u64,
    config: Config,
    pools: Vec<KvPool>,
}
impl KvCache {
    /// One pool per layer, each with enough 16-token pages for
    /// `max_sequences` full-length sequences.
    pub fn new(config: Config, format: KvFormat, max_sequences: usize) -> Result<Self, Error> {
        config.validate()?;
        let pages_per_sequence = config.seq_len.div_ceil(TOKENS_PER_PAGE);
        let pages = pages_per_sequence
            .checked_mul(max_sequences)
            .ok_or(Error::Overflow)?;
        let kv_config = KvConfig {
            heads: config.kv_heads,
            head_dim: config.head_dim(),
            pages,
            max_sequences,
            max_tokens_per_sequence: config.seq_len,
            max_bytes: 64 * 1024 * 1024,
        };
        let mut pools = Vec::with_capacity(config.layers);
        for _ in 0..config.layers {
            pools.push(KvPool::with_format(kv_config, format).map_err(kv)?);
        }
        Ok(Self {
            identity: crate::next_owner()?,
            config,
            pools,
        })
    }
    pub fn pools(&self) -> &[KvPool] {
        &self.pools
    }
    /// Starts a sequence in every layer and allocates its activations.
    pub fn begin(&mut self) -> Result<Session, Error> {
        let c = self.config;
        let mut sequences = Vec::with_capacity(self.pools.len());
        for pool in &mut self.pools {
            match pool.begin() {
                Ok(s) => sequences.push(s),
                Err(e) => {
                    for (pool, s) in self.pools.iter_mut().zip(sequences) {
                        pool.release(s).map_err(|f| kv(f.error))?;
                    }
                    return Err(kv(e));
                }
            }
        }
        let kv_dim = c.kv_dim();
        Ok(Session {
            cache: self.identity,
            position: 0,
            sequences,
            scratch: Box::new(Scratch {
                x: vec![0.0; c.dim],
                xb: vec![0.0; c.dim],
                xb2: vec![0.0; c.dim],
                hb: vec![0.0; c.hidden_dim],
                q: vec![0.0; c.dim],
                k: vec![0.0; kv_dim],
                v: vec![0.0; kv_dim],
                k16: vec![0; kv_dim * 2],
                v16: vec![0; kv_dim * 2],
                att: vec![0.0; c.heads * c.seq_len],
                logits: vec![0.0; c.vocab],
            }),
        })
    }
    /// Consumes `session`, zeroing and freeing all its pages. A session from a
    /// different cache is returned unchanged with the error.
    pub fn release(&mut self, mut session: Session) -> Result<(), (Error, Session)> {
        if session.cache != self.identity {
            return Err((Error::InvalidId, session));
        }
        let sequences = std::mem::take(&mut session.sequences);
        let mut first_error = None;
        for (pool, sequence) in self.pools.iter_mut().zip(sequences) {
            if let Err(failure) = pool.release(sequence) {
                first_error.get_or_insert(failure.error);
                session.sequences.push(failure.sequence);
            }
        }
        match first_error {
            None => Ok(()),
            Some(e) => Err((kv(e), session)),
        }
    }
}

/// llama2.c greedy decoding: feed `prompt` (which must start with [`BOS`])
/// then argmax tokens until `steps` positions have been processed or `BOS` is
/// produced. Returns the generated tokens after the prompt's first token
/// (prompt continuation tokens included, exactly as `run.c` prints them).
pub fn generate_greedy(
    model: &Model,
    cache: &mut KvCache,
    session: &mut Session,
    prompt: &[u32],
    steps: usize,
    mode: AttentionMode,
) -> Result<Vec<u32>, Error> {
    if prompt.first() != Some(&BOS) {
        return Err(Error::Shape("prompt must start with BOS"));
    }
    if session.position != 0 {
        return Err(Error::Shape("generation needs a fresh session"));
    }
    if steps > model.config.seq_len {
        return Err(Error::Limit("steps exceed the model's sequence length"));
    }
    let mut out = Vec::with_capacity(steps);
    let mut token = prompt[0];
    for pos in 0..steps {
        let logits = model.forward(cache, session, token, mode)?;
        let next = match prompt.get(pos + 1) {
            Some(&t) => t,
            None => argmax(logits)? as u32,
        };
        if next == BOS {
            break;
        }
        out.push(next);
        token = next;
    }
    Ok(out)
}

/// A llama2.c tokenizer vocabulary (decoding only).
#[derive(Clone, Debug, PartialEq)]
pub struct Tokenizer {
    pieces: Vec<Vec<u8>>,
    scores: Vec<f32>,
    max_token_length: usize,
}
impl Tokenizer {
    /// Parses `tokenizer.bin`: `i32` max token length, then `vocab` entries
    /// of `f32` score, `i32` length and that many bytes.
    pub fn from_llama2c(bytes: &[u8], vocab: usize) -> Result<Self, Error> {
        if vocab == 0 || vocab > MAX_VOCAB {
            return Err(Error::Limit("vocab"));
        }
        let mut r = Reader { bytes, offset: 0 };
        let max_token_length = positive(r.i32()?, "max_token_length", 1024)?;
        let mut pieces = Vec::with_capacity(vocab);
        let mut scores = Vec::with_capacity(vocab);
        for _ in 0..vocab {
            let score = r.f32()?;
            if !score.is_finite() {
                return Err(Error::NonFinite);
            }
            let len = positive(r.i32()?, "token length", max_token_length)?;
            pieces.push(r.take(len)?.to_vec());
            scores.push(score);
        }
        if r.offset != bytes.len() {
            return Err(Error::Malformed("trailing bytes after tokenizer"));
        }
        Ok(Self {
            pieces,
            scores,
            max_token_length,
        })
    }
    pub fn vocab(&self) -> usize {
        self.pieces.len()
    }
    pub fn piece(&self, token: u32) -> Option<&[u8]> {
        self.pieces.get(token as usize).map(Vec::as_slice)
    }
    pub fn score(&self, token: u32) -> Option<f32> {
        self.scores.get(token as usize).copied()
    }
    pub fn max_token_length(&self) -> usize {
        self.max_token_length
    }
    /// Bytes `run.c` prints for `token` after `previous`: a leading space is
    /// dropped after BOS, `<0xHH>` pieces become that byte, and single
    /// non-printable, non-whitespace bytes are suppressed.
    pub fn decode(&self, previous: u32, token: u32) -> Result<Vec<u8>, Error> {
        let mut piece = self
            .piece(token)
            .ok_or(Error::Shape("token outside the vocabulary"))?;
        if previous == BOS && piece.first() == Some(&b' ') {
            piece = &piece[1..];
        }
        if let Some(byte) = raw_byte(piece) {
            piece = std::slice::from_ref(byte);
        }
        if piece.len() == 1 {
            let b = piece[0];
            if !(b.is_ascii_graphic() || b.is_ascii_whitespace() || b == b' ') {
                return Ok(Vec::new());
            }
        }
        Ok(piece.to_vec())
    }
    /// Concatenated [`Tokenizer::decode`] output for a generation that started
    /// from `first` (normally [`BOS`]).
    pub fn decode_all(&self, first: u32, tokens: &[u32]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut previous = first;
        for &t in tokens {
            out.extend(self.decode(previous, t)?);
            previous = t;
        }
        Ok(out)
    }
}

static BYTES: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = i as u8;
        i += 1;
    }
    table
};
fn raw_byte(piece: &[u8]) -> Option<&'static u8> {
    let hex = piece.strip_prefix(b"<0x")?.strip_suffix(b">")?;
    if hex.len() != 2 {
        return None;
    }
    let s = std::str::from_utf8(hex).ok()?;
    let v = u8::from_str_radix(s, 16).ok()?;
    Some(&BYTES[v as usize])
}

// ---------------------------------------------------------------------------
// GGUF and safetensors loading and export.

fn meta_usize(g: &Gguf, key: &str) -> Result<usize, Error> {
    g.get(key)
        .ok_or_else(|| Error::MissingTensor(key.to_string()))?
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or(Error::Malformed("GGUF integer metadata"))
}
fn meta_f32(g: &Gguf, key: &str, default: f32) -> Result<f32, Error> {
    match g.get(key) {
        None => Ok(default),
        Some(v) => v.as_f32().ok_or(Error::Malformed("GGUF float metadata")),
    }
}
fn expect_shape(m: &Matrix, rows: usize, cols: usize, what: &'static str) -> Result<(), Error> {
    if m.rows() != rows || m.cols() != cols {
        return Err(Error::Shape(what));
    }
    Ok(())
}
fn expect_len(v: &[f32], n: usize, what: &'static str) -> Result<(), Error> {
    if v.len() != n {
        return Err(Error::Shape(what));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(Error::NonFinite);
    }
    Ok(())
}

/// Hyperparameters a Hugging Face safetensors checkpoint does not carry
/// (they live in its `config.json`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HfConfig {
    pub heads: usize,
    pub kv_heads: usize,
    pub seq_len: usize,
    pub rope_base: f32,
    pub rms_epsilon: f32,
}

const HF_HEADS: &str = "vesper.num_attention_heads";
const HF_KV_HEADS: &str = "vesper.num_key_value_heads";
const HF_SEQ: &str = "vesper.max_position_embeddings";
const HF_ROPE: &str = "vesper.rope_theta";
const HF_EPS: &str = "vesper.rms_norm_eps";

impl HfConfig {
    /// Reads the `vesper.*` keys [`Model::to_safetensors`] writes into
    /// `__metadata__`.
    pub fn from_metadata(st: &SafeTensors) -> Result<Self, Error> {
        let get = |k: &str| -> Result<&str, Error> {
            st.metadata()
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .ok_or_else(|| Error::MissingTensor(k.to_string()))
        };
        let int = |k: &str| -> Result<usize, Error> {
            get(k)?
                .parse()
                .map_err(|_| Error::Malformed("safetensors integer metadata"))
        };
        let float = |k: &str| -> Result<f32, Error> {
            get(k)?
                .parse()
                .map_err(|_| Error::Malformed("safetensors float metadata"))
        };
        Ok(Self {
            heads: int(HF_HEADS)?,
            kv_heads: int(HF_KV_HEADS)?,
            seq_len: int(HF_SEQ)?,
            rope_base: float(HF_ROPE)?,
            rms_epsilon: float(HF_EPS)?,
        })
    }
}

/// Reorders the rows of a `[heads * head_dim, cols]` Q or K projection from
/// interleaved RoPE pairs to Hugging Face `rotate_half` pairs (the inverse of
/// llama.cpp's conversion `permute`). Works on any row format.
fn rows_to_half(m: &Matrix, heads: usize, head_dim: usize) -> Result<Vec<usize>, Error> {
    if m.rows() != heads * head_dim {
        return Err(Error::Shape("rope permutation rows"));
    }
    let half = head_dim / 2;
    let mut order = Vec::with_capacity(m.rows());
    for h in 0..heads {
        for i in 0..head_dim {
            let src = if i < half { 2 * i } else { 2 * (i - half) + 1 };
            order.push(h * head_dim + src);
        }
    }
    Ok(order)
}

fn matrix_bytes(m: &Matrix) -> Vec<u8> {
    match m.raw_bytes() {
        Some(b) => b.to_vec(),
        None => m
            .to_f32()
            .expect("FP32 matrix")
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
    }
}
fn permuted_rows(m: &Matrix, order: &[usize]) -> Vec<u8> {
    let bytes = matrix_bytes(m);
    let row = bytes.len() / m.rows();
    let mut out = Vec::with_capacity(bytes.len());
    for &r in order {
        out.extend_from_slice(&bytes[r * row..(r + 1) * row]);
    }
    out
}
fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

impl Model {
    /// Loads a GGUF `llama` model (llama.cpp tensor names, interleaved RoPE).
    /// Matrices borrow the file's bytes; norms are copied. Required keys:
    /// `llama.embedding_length`, `feed_forward_length`, `block_count`,
    /// `attention.head_count`, `context_length`; optional
    /// `attention.head_count_kv`, `attention.layer_norm_rms_epsilon`,
    /// `rope.freq_base`, and `rope.dimension_count` (must equal the head
    /// width). `output.weight` absent means a shared classifier. Tensors this
    /// loader does not use (for example `rope_freqs.weight`) are rejected
    /// rather than ignored.
    pub fn from_gguf(g: &Gguf) -> Result<Self, Error> {
        match g.get("general.architecture").and_then(gguf::Value::as_str) {
            Some("llama") => {}
            Some(other) => return Err(Error::Unsupported(format!("GGUF architecture {other}"))),
            None => return Err(Error::MissingTensor("general.architecture".into())),
        }
        let dim = meta_usize(g, "llama.embedding_length")?;
        let heads = meta_usize(g, "llama.attention.head_count")?;
        let kv_heads = match g.get("llama.attention.head_count_kv") {
            None => heads,
            Some(_) => meta_usize(g, "llama.attention.head_count_kv")?,
        };
        let embd = g
            .tensor("token_embd.weight")
            .ok_or_else(|| Error::MissingTensor("token_embd.weight".into()))?;
        let vocab = *embd.dims.get(1).ok_or(Error::Shape("token_embd.weight"))?;
        let shared = g.tensor("output.weight").is_none();
        let config = Config {
            dim,
            hidden_dim: meta_usize(g, "llama.feed_forward_length")?,
            layers: meta_usize(g, "llama.block_count")?,
            heads,
            kv_heads,
            vocab,
            seq_len: meta_usize(g, "llama.context_length")?,
            shared_classifier: shared,
            rope: RopeStyle::Interleaved,
            rope_base: meta_f32(g, "llama.rope.freq_base", ROPE_BASE)?,
            rms_epsilon: meta_f32(g, "llama.attention.layer_norm_rms_epsilon", RMS_EPSILON)?,
        };
        let bounds = [
            (config.dim, MAX_DIM, "dim"),
            (config.hidden_dim, 4 * MAX_DIM, "hidden_dim"),
            (config.layers, MAX_LAYERS, "layers"),
            (config.heads, MAX_DIM, "heads"),
            (config.kv_heads, MAX_DIM, "kv_heads"),
            (config.vocab, MAX_VOCAB, "vocab"),
            (config.seq_len, MAX_SEQ_LEN, "seq_len"),
        ];
        for (v, max, what) in bounds {
            if v == 0 {
                return Err(Error::Malformed(what));
            }
            if v > max {
                return Err(Error::Limit(what));
            }
        }
        config.validate()?;
        if let Some(n) = g.get("llama.rope.dimension_count") {
            if n.as_u64() != Some(config.head_dim() as u64) {
                return Err(Error::Unsupported("partial rotary embedding".into()));
            }
        }
        let mut used = vec![
            "token_embd.weight".to_string(),
            "output_norm.weight".to_string(),
        ];
        if !shared {
            used.push("output.weight".into());
        }
        let kv_dim = config.kv_dim();
        let embedding = g.matrix("token_embd.weight")?;
        expect_shape(&embedding, vocab, dim, "token_embd.weight")?;
        let mut layers = Vec::with_capacity(config.layers);
        for i in 0..config.layers {
            let name = |t: &str| format!("blk.{i}.{t}.weight");
            for t in [
                "attn_norm",
                "attn_q",
                "attn_k",
                "attn_v",
                "attn_output",
                "ffn_norm",
                "ffn_gate",
                "ffn_down",
                "ffn_up",
            ] {
                used.push(name(t));
            }
            let m = |t: &str, rows, cols, what| -> Result<Matrix, Error> {
                let m = g.matrix(&name(t))?;
                expect_shape(&m, rows, cols, what)?;
                Ok(m)
            };
            let rms_attention = g.vector(&name("attn_norm"))?;
            expect_len(&rms_attention, dim, "attn_norm")?;
            let rms_ffn = g.vector(&name("ffn_norm"))?;
            expect_len(&rms_ffn, dim, "ffn_norm")?;
            layers.push(Layer {
                rms_attention,
                wq: m("attn_q", dim, dim, "attn_q")?,
                wk: m("attn_k", kv_dim, dim, "attn_k")?,
                wv: m("attn_v", kv_dim, dim, "attn_v")?,
                wo: m("attn_output", dim, dim, "attn_output")?,
                rms_ffn,
                w1: m("ffn_gate", config.hidden_dim, dim, "ffn_gate")?,
                w2: m("ffn_down", dim, config.hidden_dim, "ffn_down")?,
                w3: m("ffn_up", config.hidden_dim, dim, "ffn_up")?,
            });
        }
        if let Some(extra) = g.tensors().iter().find(|t| !used.contains(&t.name)) {
            return Err(Error::Unsupported(format!(
                "unused GGUF tensor {}",
                extra.name
            )));
        }
        let rms_final = g.vector("output_norm.weight")?;
        expect_len(&rms_final, dim, "output_norm")?;
        let classifier = if shared {
            embedding.clone()
        } else {
            let m = g.matrix("output.weight")?;
            expect_shape(&m, vocab, dim, "output.weight")?;
            m
        };
        Ok(Self {
            config,
            embedding,
            layers,
            rms_final,
            classifier,
        })
    }

    /// Serializes an interleaved-RoPE model as a GGUF `llama` file with
    /// llama.cpp tensor names, optionally with its tokenizer
    /// (`tokenizer.ggml.*`, spaces written as U+2581). Matrices keep their
    /// format; FP8 has no GGUF type and is rejected.
    pub fn to_gguf(&self, tokenizer: Option<&Tokenizer>) -> Result<Vec<u8>, Error> {
        use gguf::{TensorData, Value};
        let c = self.config;
        if c.rope != RopeStyle::Interleaved {
            return Err(Error::Unsupported(
                "GGUF llama export needs interleaved RoPE".into(),
            ));
        }
        let u32v = |v: usize| Value::U32(v as u32);
        let mut meta = vec![
            (
                "general.architecture".to_string(),
                Value::String("llama".into()),
            ),
            (
                "general.alignment".to_string(),
                Value::U32(gguf::DEFAULT_ALIGNMENT as u32),
            ),
            ("llama.embedding_length".into(), u32v(c.dim)),
            ("llama.feed_forward_length".into(), u32v(c.hidden_dim)),
            ("llama.block_count".into(), u32v(c.layers)),
            ("llama.attention.head_count".into(), u32v(c.heads)),
            ("llama.attention.head_count_kv".into(), u32v(c.kv_heads)),
            ("llama.context_length".into(), u32v(c.seq_len)),
            ("llama.rope.dimension_count".into(), u32v(c.head_dim())),
            ("llama.rope.freq_base".into(), Value::F32(c.rope_base)),
            (
                "llama.attention.layer_norm_rms_epsilon".into(),
                Value::F32(c.rms_epsilon),
            ),
        ];
        if let Some(t) = tokenizer {
            let mut tokens = Vec::with_capacity(t.vocab());
            let mut scores = Vec::with_capacity(t.vocab());
            for id in 0..t.vocab() as u32 {
                let piece = String::from_utf8(t.piece(id).expect("id").to_vec())
                    .map_err(|_| Error::Unsupported("non-UTF-8 vocabulary piece".into()))?;
                tokens.push(Value::String(piece.replace(' ', "\u{2581}")));
                scores.push(Value::F32(t.score(id).expect("id")));
            }
            meta.push(("tokenizer.ggml.model".into(), Value::String("llama".into())));
            meta.push(("tokenizer.ggml.tokens".into(), Value::Array(tokens)));
            meta.push(("tokenizer.ggml.scores".into(), Value::Array(scores)));
            meta.push(("tokenizer.ggml.bos_token_id".into(), Value::U32(BOS)));
            meta.push((
                "tokenizer.ggml.eos_token_id".into(),
                Value::U32(crate::tokenizer::EOS),
            ));
        }
        let mat = |name: String, m: &Matrix| -> Result<TensorData, Error> {
            Ok(TensorData {
                name,
                dims: vec![m.cols(), m.rows()],
                ty: GgmlType::from_weight_format(m.format())?,
                bytes: matrix_bytes(m),
            })
        };
        let vec = |name: String, v: &[f32]| TensorData {
            name,
            dims: vec![v.len()],
            ty: GgmlType::F32,
            bytes: f32_bytes(v),
        };
        let mut tensors = vec![mat("token_embd.weight".into(), &self.embedding)?];
        for (i, l) in self.layers.iter().enumerate() {
            let n = |t: &str| format!("blk.{i}.{t}.weight");
            tensors.push(vec(n("attn_norm"), &l.rms_attention));
            tensors.push(mat(n("attn_q"), &l.wq)?);
            tensors.push(mat(n("attn_k"), &l.wk)?);
            tensors.push(mat(n("attn_v"), &l.wv)?);
            tensors.push(mat(n("attn_output"), &l.wo)?);
            tensors.push(vec(n("ffn_norm"), &l.rms_ffn));
            tensors.push(mat(n("ffn_gate"), &l.w1)?);
            tensors.push(mat(n("ffn_down"), &l.w2)?);
            tensors.push(mat(n("ffn_up"), &l.w3)?);
        }
        tensors.push(vec("output_norm.weight".into(), &self.rms_final));
        if !c.shared_classifier {
            tensors.push(mat("output.weight".into(), &self.classifier)?);
        }
        gguf::write(&meta, &tensors)
    }

    /// Loads a Hugging Face `LlamaForCausalLM`-layout safetensors checkpoint
    /// (`model.embed_tokens.weight`, `model.layers.N.self_attn.q_proj.weight`,
    /// ..., `model.norm.weight`, optional `lm_head.weight`) with `rotate_half`
    /// RoPE. Matrices in F32, F16, BF16 or FP8 borrow the file's bytes.
    pub fn from_safetensors(st: &SafeTensors, hf: HfConfig) -> Result<Self, Error> {
        let embedding = st.matrix("model.embed_tokens.weight")?;
        let (vocab, dim) = (embedding.rows(), embedding.cols());
        let mut layers_n = 0usize;
        while st
            .get(&format!("model.layers.{layers_n}.input_layernorm.weight"))
            .is_some()
        {
            layers_n += 1;
            if layers_n > MAX_LAYERS {
                return Err(Error::Limit("layers"));
            }
        }
        let gate0 = st.matrix("model.layers.0.mlp.gate_proj.weight")?;
        let shared = st.get("lm_head.weight").is_none();
        let config = Config {
            dim,
            hidden_dim: gate0.rows(),
            layers: layers_n,
            heads: hf.heads,
            kv_heads: hf.kv_heads,
            vocab,
            seq_len: hf.seq_len,
            shared_classifier: shared,
            rope: RopeStyle::Half,
            rope_base: hf.rope_base,
            rms_epsilon: hf.rms_epsilon,
        };
        if [config.heads, config.kv_heads, config.seq_len].contains(&0) {
            return Err(Error::Malformed("safetensors hyperparameters"));
        }
        if config.heads > MAX_DIM
            || config.kv_heads > MAX_DIM
            || config.seq_len > MAX_SEQ_LEN
            || dim > MAX_DIM
            || vocab > MAX_VOCAB
            || config.hidden_dim > 4 * MAX_DIM
        {
            return Err(Error::Limit("safetensors hyperparameters"));
        }
        config.validate()?;
        let kv_dim = config.kv_dim();
        let mut used = vec![
            "model.embed_tokens.weight".to_string(),
            "model.norm.weight".into(),
        ];
        if !shared {
            used.push("lm_head.weight".into());
        }
        let mut layers = Vec::with_capacity(layers_n);
        for i in 0..layers_n {
            let p = format!("model.layers.{i}.");
            let mut m = |t: &str, rows, cols| -> Result<Matrix, Error> {
                let name = format!("{p}{t}.weight");
                let m = st.matrix(&name)?;
                expect_shape(&m, rows, cols, "safetensors projection shape")?;
                used.push(name.clone());
                if st.get(&format!("{name}_scale")).is_some() {
                    used.push(format!("{name}_scale"));
                }
                Ok(m)
            };
            let wq = m("self_attn.q_proj", dim, dim)?;
            let wk = m("self_attn.k_proj", kv_dim, dim)?;
            let wv = m("self_attn.v_proj", kv_dim, dim)?;
            let wo = m("self_attn.o_proj", dim, dim)?;
            let w1 = m("mlp.gate_proj", config.hidden_dim, dim)?;
            let w2 = m("mlp.down_proj", dim, config.hidden_dim)?;
            let w3 = m("mlp.up_proj", config.hidden_dim, dim)?;
            let rms_attention = st.vector(&format!("{p}input_layernorm.weight"))?;
            expect_len(&rms_attention, dim, "input_layernorm")?;
            let rms_ffn = st.vector(&format!("{p}post_attention_layernorm.weight"))?;
            expect_len(&rms_ffn, dim, "post_attention_layernorm")?;
            used.push(format!("{p}input_layernorm.weight"));
            used.push(format!("{p}post_attention_layernorm.weight"));
            layers.push(Layer {
                rms_attention,
                wq,
                wk,
                wv,
                wo,
                rms_ffn,
                w1,
                w2,
                w3,
            });
        }
        if let Some(extra) = st.tensors().iter().find(|t| !used.contains(&t.name)) {
            return Err(Error::Unsupported(format!(
                "unused safetensors tensor {}",
                extra.name
            )));
        }
        let rms_final = st.vector("model.norm.weight")?;
        expect_len(&rms_final, dim, "model.norm")?;
        let classifier = if shared {
            embedding.clone()
        } else {
            let m = st.matrix("lm_head.weight")?;
            expect_shape(&m, vocab, dim, "lm_head.weight")?;
            m
        };
        Ok(Self {
            config,
            embedding,
            layers,
            rms_final,
            classifier,
        })
    }

    /// Serializes the model in Hugging Face safetensors layout. An
    /// interleaved-RoPE model has its Q and K rows permuted to `rotate_half`
    /// order, as Hugging Face checkpoints store them; the hyperparameters go
    /// into `__metadata__` (see [`HfConfig::from_metadata`]). Matrices must be
    /// F32, F16, BF16 or FP8 (FP8 scales are written as `<name>_scale`).
    pub fn to_safetensors(&self) -> Result<Vec<u8>, Error> {
        use safetensors::TensorData;
        let c = self.config;
        let dtype = |f: WeightFormat| -> Result<Dtype, Error> {
            Ok(match f {
                WeightFormat::F32 => Dtype::F32,
                WeightFormat::F16 => Dtype::F16,
                WeightFormat::Bf16 => Dtype::Bf16,
                WeightFormat::F8E4M3 => Dtype::F8E4M3,
                WeightFormat::F8E5M2 => Dtype::F8E5M2,
                other => {
                    return Err(Error::Unsupported(format!("{other:?} in safetensors")));
                }
            })
        };
        let mut tensors = Vec::new();
        let mut mat = |name: String, m: &Matrix, order: Option<Vec<usize>>| -> Result<(), Error> {
            let bytes = match order {
                Some(order) => permuted_rows(m, &order),
                None => matrix_bytes(m),
            };
            if matches!(m.format(), WeightFormat::F8E4M3 | WeightFormat::F8E5M2) {
                tensors.push(TensorData {
                    name: format!("{name}_scale"),
                    dtype: Dtype::F32,
                    shape: vec![],
                    bytes: m.scale().to_le_bytes().to_vec(),
                });
            }
            tensors.push(TensorData {
                name,
                dtype: dtype(m.format())?,
                shape: vec![m.rows(), m.cols()],
                bytes,
            });
            Ok(())
        };
        let permute = c.rope == RopeStyle::Interleaved;
        mat("model.embed_tokens.weight".into(), &self.embedding, None)?;
        let hd = c.head_dim();
        let mut vectors = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            let p = format!("model.layers.{i}.");
            let q_order = permute
                .then(|| rows_to_half(&l.wq, c.heads, hd))
                .transpose()?;
            let k_order = permute
                .then(|| rows_to_half(&l.wk, c.kv_heads, hd))
                .transpose()?;
            mat(format!("{p}self_attn.q_proj.weight"), &l.wq, q_order)?;
            mat(format!("{p}self_attn.k_proj.weight"), &l.wk, k_order)?;
            mat(format!("{p}self_attn.v_proj.weight"), &l.wv, None)?;
            mat(format!("{p}self_attn.o_proj.weight"), &l.wo, None)?;
            mat(format!("{p}mlp.gate_proj.weight"), &l.w1, None)?;
            mat(format!("{p}mlp.down_proj.weight"), &l.w2, None)?;
            mat(format!("{p}mlp.up_proj.weight"), &l.w3, None)?;
            vectors.push((
                format!("{p}input_layernorm.weight"),
                l.rms_attention.clone(),
            ));
            vectors.push((
                format!("{p}post_attention_layernorm.weight"),
                l.rms_ffn.clone(),
            ));
        }
        if !c.shared_classifier {
            mat("lm_head.weight".into(), &self.classifier, None)?;
        }
        vectors.push(("model.norm.weight".into(), self.rms_final.clone()));
        for (name, v) in vectors {
            tensors.push(TensorData {
                name,
                dtype: Dtype::F32,
                shape: vec![v.len()],
                bytes: f32_bytes(&v),
            });
        }
        let metadata = vec![
            (HF_HEADS.to_string(), c.heads.to_string()),
            (HF_KV_HEADS.to_string(), c.kv_heads.to_string()),
            (HF_SEQ.to_string(), c.seq_len.to_string()),
            (HF_ROPE.to_string(), format!("{:?}", c.rope_base)),
            (HF_EPS.to_string(), format!("{:?}", c.rms_epsilon)),
        ];
        safetensors::write(&tensors, &metadata)
    }
}

impl Tokenizer {
    /// The `tokenizer.ggml.*` SentencePiece vocabulary of a GGUF file
    /// (`tokenizer.ggml.model = "llama"`); U+2581 is mapped back to a space.
    pub fn from_gguf(g: &Gguf) -> Result<Self, Error> {
        match g.get("tokenizer.ggml.model").and_then(gguf::Value::as_str) {
            Some("llama") => {}
            Some(other) => return Err(Error::Unsupported(format!("GGUF tokenizer {other}"))),
            None => return Err(Error::MissingTensor("tokenizer.ggml.model".into())),
        }
        let tokens = g
            .get("tokenizer.ggml.tokens")
            .and_then(gguf::Value::as_array)
            .ok_or_else(|| Error::MissingTensor("tokenizer.ggml.tokens".into()))?;
        let scores = g
            .get("tokenizer.ggml.scores")
            .and_then(gguf::Value::as_array)
            .ok_or_else(|| Error::MissingTensor("tokenizer.ggml.scores".into()))?;
        if tokens.is_empty() || tokens.len() > MAX_VOCAB || tokens.len() != scores.len() {
            return Err(Error::Malformed("GGUF vocabulary"));
        }
        let mut pieces = Vec::with_capacity(tokens.len());
        let mut out_scores = Vec::with_capacity(tokens.len());
        for (t, s) in tokens.iter().zip(scores) {
            let piece = t.as_str().ok_or(Error::Malformed("GGUF token"))?;
            let score = s.as_f32().ok_or(Error::Malformed("GGUF token score"))?;
            if !score.is_finite() {
                return Err(Error::NonFinite);
            }
            if piece.is_empty() {
                return Err(Error::Malformed("empty GGUF token"));
            }
            pieces.push(piece.replace('\u{2581}', " ").into_bytes());
            out_scores.push(score);
        }
        let max_token_length = pieces.iter().map(Vec::len).max().unwrap_or(1);
        Ok(Self {
            pieces,
            scores: out_scores,
            max_token_length,
        })
    }
    /// A prompt [`crate::tokenizer::Encoder`] over this vocabulary.
    pub fn encoder(&self) -> crate::tokenizer::Encoder<'_> {
        crate::tokenizer::Encoder::new(self)
    }
}

/// Generation with any sampler: the whole `prompt` (which must start with
/// [`BOS`]) is prefilled in one [`Model::forward_batch`] call, then tokens are
/// sampled one decode step at a time until `steps` positions have been
/// processed or [`BOS`] is produced. Returns the tokens after the prompt's
/// first token, as [`generate_greedy`] does; with
/// [`SamplerConfig::GREEDY`] and the same kernel the result equals
/// [`generate_greedy`]'s token for token.
pub fn generate(
    model: &Model,
    cache: &mut KvCache,
    session: &mut Session,
    prompt: &[u32],
    steps: usize,
    sampler: &mut Sampler,
    options: RunOptions,
) -> Result<Vec<u32>, Error> {
    if prompt.first() != Some(&BOS) {
        return Err(Error::Shape("prompt must start with BOS"));
    }
    if session.position != 0 {
        return Err(Error::Shape("generation needs a fresh session"));
    }
    if steps > model.config.seq_len {
        return Err(Error::Limit("steps exceed the model's sequence length"));
    }
    let mut out = Vec::with_capacity(steps);
    // Prompt continuation tokens, exactly as generate_greedy reports them.
    out.extend_from_slice(&prompt[1..prompt.len().min(steps + 1)]);
    if let Some(stop) = out.iter().position(|t| *t == BOS) {
        out.truncate(stop);
        return Ok(out);
    }
    let prefill = &prompt[..prompt.len().min(steps)];
    if prefill.is_empty() {
        return Ok(out);
    }
    let mut scratch = model.batch_scratch(prefill.len(), 1)?;
    model.forward_batch(
        cache,
        &mut [BatchItem {
            session: &mut *session,
            tokens: prefill,
        }],
        &mut scratch,
        options,
    )?;
    if prompt.len() > steps {
        return Ok(out);
    }
    let mut next = sampler.sample(session.logits())?;
    while next != BOS {
        out.push(next);
        if session.position >= steps {
            break;
        }
        let logits = model.forward_with(cache, session, next, options)?;
        next = sampler.sample(logits)?;
    }
    Ok(out)
}

/// A greedy sampler for `model`'s vocabulary.
pub fn greedy_sampler(model: &Model) -> Result<Sampler, Error> {
    Sampler::new(model.config.vocab, SamplerConfig::GREEDY)
}
