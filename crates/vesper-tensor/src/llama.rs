//! Decoder-only Llama-architecture inference on the CPU.
//!
//! [`Model::from_llama2c`] parses the llama2.c checkpoint format (a header of
//! seven little-endian `i32`s followed by FP32 weights) and
//! [`Tokenizer::from_llama2c`] its tokenizer format. [`Model::forward`] runs
//! one decode step: RMSNorm, Q/K/V projections, RoPE, a paged grouped-query
//! attention over a [`KvCache`] sequence, the output projection, a fused
//! SwiGLU feed-forward block and the classifier. [`generate_greedy`] decodes
//! with argmax sampling.
//!
//! The K/V cache is one [`KvPool`] per layer. A [`Session`] owns one
//! [`Sequence`] per layer and must be returned with [`KvCache::release`];
//! dropping an unreleased session is counted by [`leaked_sessions`] (its pages
//! stay leased). This is host-library discipline, not the Vesper source
//! linearity rules.
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::attention::{kv, paged_gqa_attention, AttentionMode, AttentionShape};
use crate::kv::{KvConfig, KvFormat, KvPool, Sequence, TOKENS_PER_PAGE};
use crate::ops::{add_assign, argmax, rms_norm, rope, swiglu_matvec};
use crate::quant::{encode_bf16, Matrix, WeightFormat};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}
impl Config {
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
    embedding: Vec<f32>,
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
        for b in bytes.chunks_exact(4) {
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
        if !dim.is_multiple_of(heads) {
            return Err(Error::Shape("dim must be a multiple of heads"));
        }
        crate::shape::gqa_ratio(heads as u64, kv_heads as u64)?;
        let config = Config {
            dim,
            hidden_dim,
            layers,
            heads,
            kv_heads,
            vocab,
            seq_len,
            shared_classifier,
        };
        let head_dim = config.head_dim();
        if !head_dim.is_multiple_of(2) {
            return Err(Error::Shape("RoPE needs an even head width"));
        }
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
        let classifier = if shared_classifier {
            Matrix::f32(vocab, dim, embedding.clone())?
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
    pub fn embedding(&self) -> &[f32] {
        &self.embedding
    }
    pub fn rms_final(&self) -> &[f32] {
        &self.rms_final
    }
    pub fn classifier(&self) -> &Matrix {
        &self.classifier
    }
    /// Quantizes every layer projection whose input width is a multiple of 32
    /// to `format`. Other matrices (and the embedding, norms and classifier)
    /// stay FP32. Returns the model and the number of matrices kept in FP32.
    pub fn quantized(&self, format: WeightFormat) -> Result<(Self, usize), Error> {
        let mut kept = 0usize;
        let mut q = |m: &Matrix| -> Result<Matrix, Error> {
            if format == WeightFormat::F32 || !m.cols().is_multiple_of(crate::quant::BLOCK) {
                if format != WeightFormat::F32 {
                    kept += 1;
                }
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
    /// Payload bytes of every weight (excluding container overhead).
    pub fn weight_bytes(&self) -> usize {
        let mut total = (self.embedding.len() + self.rms_final.len()) * 4;
        if !self.config.shared_classifier {
            total += self.classifier.payload_bytes();
        }
        for l in &self.layers {
            total += (l.rms_attention.len() + l.rms_ffn.len()) * 4;
            for m in [&l.wq, &l.wk, &l.wv, &l.wo, &l.w1, &l.w2, &l.w3] {
                total += m.payload_bytes();
            }
        }
        total
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
        let c = self.config;
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
        let head_dim = c.head_dim();
        let kv_dim = c.kv_dim();
        let s = &mut session.scratch;
        s.x.copy_from_slice(&self.embedding[token * c.dim..(token + 1) * c.dim]);
        let shape = AttentionShape {
            query_heads: c.heads,
            kv_heads: c.kv_heads,
            head_dim,
        };
        for (l, layer) in self.layers.iter().enumerate() {
            rms_norm(&mut s.xb, &s.x, &layer.rms_attention, RMS_EPSILON)?;
            layer.wq.matvec(&s.xb, &mut s.q)?;
            layer.wk.matvec(&s.xb, &mut s.k)?;
            layer.wv.matvec(&s.xb, &mut s.v)?;
            rope(&mut s.q, head_dim, position, ROPE_BASE)?;
            rope(&mut s.k, head_dim, position, ROPE_BASE)?;
            let pool = &mut cache.pools[l];
            let sequence = &mut session.sequences[l];
            match pool.format() {
                KvFormat::F32 => pool.append_f32(sequence, position, &s.k, &s.v),
                KvFormat::Bf16 => {
                    for (dst, src) in s.k16.chunks_exact_mut(2).zip(&s.k) {
                        dst.copy_from_slice(&encode_bf16(*src).to_le_bytes());
                    }
                    for (dst, src) in s.v16.chunks_exact_mut(2).zip(&s.v) {
                        dst.copy_from_slice(&encode_bf16(*src).to_le_bytes());
                    }
                    let step = |bytes| crate::kv::Bf16Step {
                        heads: c.kv_heads,
                        head_dim,
                        bytes,
                    };
                    pool.append(sequence, position, step(&s.k16), step(&s.v16))
                }
            }
            .map_err(kv)?;
            debug_assert_eq!(s.k.len(), kv_dim);
            paged_gqa_attention(pool, sequence, shape, &s.q, &mut s.att, &mut s.xb, mode)?;
            layer.wo.matvec(&s.xb, &mut s.xb2)?;
            add_assign(&mut s.x, &s.xb2)?;
            rms_norm(&mut s.xb, &s.x, &layer.rms_ffn, RMS_EPSILON)?;
            swiglu_matvec(&layer.w1, &layer.w3, &s.xb, &mut s.hb)?;
            layer.w2.matvec(&s.hb, &mut s.xb2)?;
            add_assign(&mut s.x, &s.xb2)?;
        }
        rms_norm(&mut s.xb, &s.x, &self.rms_final, RMS_EPSILON)?;
        self.classifier.matvec(&s.xb, &mut s.logits)?;
        if s.logits.iter().any(|v| !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        session.position += 1;
        Ok(&session.scratch.logits)
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
