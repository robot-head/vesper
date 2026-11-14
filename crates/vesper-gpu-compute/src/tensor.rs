//! Tensor kernels (design §28) on the Vulkan runtime, and a GPU decoder for
//! `vesper-tensor`'s llama2.c models.
//!
//! The kernels are WGSL compiled by naga: [`TensorKernels::matmul`]
//! (row-major `[m, k] x [n, k]^T`, the llama2.c weight layout),
//! [`TensorKernels::rms_norm`], [`TensorKernels::rope`],
//! [`TensorKernels::swiglu`], [`TensorKernels::kv_write`] and paged
//! grouped-query [`TensorKernels::attention`]. They follow the arithmetic
//! order of the CPU kernels in `vesper-tensor`, but the GPU may contract
//! `a * b + c` into a fused multiply-add, reduces RMSNorm and softmax sums in
//! a tree, and uses the device's `exp`, `pow`, `sin` and `cos`, so results
//! are compared with a stated tolerance, not bit for bit.
//!
//! [`GpuModel`] runs the whole decode step of `vesper_tensor::llama::Model`
//! in one command buffer per token. Its K/V cache is paged exactly like the
//! CPU `KvPool`: 16-token pages leased from a shared free list when
//! `position % 16 == 0`, addressed through a per-session page table.
use vesper_tensor::attention::AttentionShape;
use vesper_tensor::kv::TOKENS_PER_PAGE;
use vesper_tensor::llama::{BOS, Config, Model, RMS_EPSILON, ROPE_BASE};
use vesper_tensor::ops::argmax;
use vesper_tensor::quant::Matrix;

use crate::error::GpuError;
use crate::runtime::{Buffer, Context, Kernel, Recording};

/// Longest sequence the attention kernel accepts (its workgroup score
/// buffer holds 2048 floats, 8 KiB of the 16 KiB Vulkan guarantees).
pub const MAX_ATTENTION_TOKENS: usize = 2048;
/// Largest head width the attention kernel accepts.
pub const MAX_HEAD_DIM: usize = 1024;

const MATMUL: &str = include_str!("shaders/matmul.wgsl");
const RMS_NORM: &str = include_str!("shaders/rms_norm.wgsl");
const ROPE: &str = include_str!("shaders/rope.wgsl");
const SWIGLU: &str = include_str!("shaders/swiglu.wgsl");
const KV_WRITE: &str = include_str!("shaders/kv_write.wgsl");
const ATTENTION: &str = include_str!("shaders/attention.wgsl");

/// Every WGSL source of this module with its entry point, for compile-only
/// checks on hosts without a device.
pub const SOURCES: [(&str, &str); 6] = [
    ("matmul", MATMUL),
    ("rms_norm", RMS_NORM),
    ("rope", ROPE),
    ("swiglu", SWIGLU),
    ("kv_write", KV_WRITE),
    ("attention", ATTENTION),
];

fn u32_of(v: usize, what: &'static str) -> Result<u32, GpuError> {
    u32::try_from(v).map_err(|_| GpuError::Limit(what))
}

fn need(buffer: &Buffer, floats: usize, what: &'static str) -> Result<(), GpuError> {
    let bytes = floats
        .checked_mul(4)
        .ok_or(GpuError::Limit("operand size"))?;
    if floats == 0 || (bytes as u64) > buffer.len() {
        return Err(GpuError::Shape(what));
    }
    Ok(())
}

fn groups_for(n: usize, per: usize) -> Result<u32, GpuError> {
    u32_of(n.div_ceil(per), "workgroup count")
}

/// The compiled tensor kernels of one context.
#[derive(Debug)]
pub struct TensorKernels {
    matmul: Kernel,
    rms_norm: Kernel,
    rope: Kernel,
    swiglu: Kernel,
    kv_write: Kernel,
    attention: Kernel,
}

impl TensorKernels {
    pub fn new(ctx: &Context) -> Result<Self, GpuError> {
        Ok(Self {
            matmul: ctx.kernel(MATMUL, "matmul", 3, 16)?,
            rms_norm: ctx.kernel(RMS_NORM, "rms_norm", 3, 8)?,
            rope: ctx.kernel(ROPE, "rope", 2, 12)?,
            swiglu: ctx.kernel(SWIGLU, "swiglu", 2, 4)?,
            kv_write: ctx.kernel(KV_WRITE, "kv_write", 5, 8)?,
            attention: ctx.kernel(ATTENTION, "attention", 5, 16)?,
        })
    }

    /// `out[r, c] = sum_i w[c, i] * a[r, i]` for `a: [m, k]`, `w: [n, k]`;
    /// with `accumulate` the sum is added to `out` instead.
    #[allow(clippy::too_many_arguments)]
    pub fn matmul<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        a: &'a Buffer,
        w: &'a Buffer,
        out: &'a Buffer,
        m: usize,
        n: usize,
        k: usize,
        accumulate: bool,
    ) -> Result<(), GpuError> {
        need(
            a,
            m.checked_mul(k).ok_or(GpuError::Limit("m * k"))?,
            "matmul a",
        )?;
        need(
            w,
            n.checked_mul(k).ok_or(GpuError::Limit("n * k"))?,
            "matmul w",
        )?;
        need(
            out,
            m.checked_mul(n).ok_or(GpuError::Limit("m * n"))?,
            "matmul out",
        )?;
        let imm = [
            u32_of(m, "m")?,
            u32_of(n, "n")?,
            u32_of(k, "k")?,
            u32::from(accumulate),
        ];
        rec.dispatch(
            &self.matmul,
            &[a, w, out],
            &imm,
            [groups_for(n, 64)?, u32_of(m, "matmul rows")?, 1],
        )
    }

    /// RMSNorm of each of `rows` rows of width `n`.
    #[allow(clippy::too_many_arguments)]
    pub fn rms_norm<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        x: &'a Buffer,
        weight: &'a Buffer,
        out: &'a Buffer,
        rows: usize,
        n: usize,
        epsilon: f32,
    ) -> Result<(), GpuError> {
        if !(epsilon.is_finite() && epsilon > 0.0) {
            return Err(GpuError::Shape("rms_norm epsilon"));
        }
        let total = rows.checked_mul(n).ok_or(GpuError::Limit("rows * n"))?;
        need(x, total, "rms_norm x")?;
        need(out, total, "rms_norm out")?;
        need(weight, n, "rms_norm weight")?;
        rec.dispatch(
            &self.rms_norm,
            &[x, weight, out],
            &[u32_of(n, "n")?, epsilon.to_bits()],
            [u32_of(rows, "rms_norm rows")?, 1, 1],
        )
    }

    /// RoPE in place over `len` floats of whole heads of width `head_dim`,
    /// with the `head_dim / 2` frequencies of [`rope_frequencies`] in `freqs`.
    #[allow(clippy::too_many_arguments)]
    pub fn rope<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        x: &'a Buffer,
        freqs: &'a Buffer,
        len: usize,
        head_dim: usize,
        position: usize,
    ) -> Result<(), GpuError> {
        if head_dim == 0 || !head_dim.is_multiple_of(2) || !len.is_multiple_of(head_dim) {
            return Err(GpuError::Shape("rope needs whole heads of even width"));
        }
        need(x, len, "rope x")?;
        need(freqs, head_dim / 2, "rope frequencies")?;
        rec.dispatch(
            &self.rope,
            &[x, freqs],
            &[
                u32_of(len, "len")?,
                u32_of(head_dim, "head_dim")?,
                u32_of(position, "position")?,
            ],
            [groups_for(len / 2, 64)?, 1, 1],
        )
    }

    /// `gate[i] = silu(gate[i]) * up[i]` for `n` elements.
    pub fn swiglu<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        gate: &'a Buffer,
        up: &'a Buffer,
        n: usize,
    ) -> Result<(), GpuError> {
        need(gate, n, "swiglu gate")?;
        need(up, n, "swiglu up")?;
        rec.dispatch(
            &self.swiglu,
            &[gate, up],
            &[u32_of(n, "n")?],
            [groups_for(n, 64)?, 1, 1],
        )
    }

    /// Stores `k` and `v` (`kv_dim` floats each) at `position` of the paged
    /// sequence whose page table is `table`.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_write<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        k: &'a Buffer,
        v: &'a Buffer,
        table: &'a Buffer,
        keys: &'a Buffer,
        values: &'a Buffer,
        position: usize,
        kv_dim: usize,
    ) -> Result<(), GpuError> {
        need(k, kv_dim, "kv_write k")?;
        need(v, kv_dim, "kv_write v")?;
        if (position / TOKENS_PER_PAGE + 1) * 4 > table.len() as usize {
            return Err(GpuError::Shape("kv_write position beyond the page table"));
        }
        let page_floats = TOKENS_PER_PAGE * kv_dim;
        need(keys, page_floats, "kv_write keys")?;
        need(values, page_floats, "kv_write values")?;
        rec.dispatch(
            &self.kv_write,
            &[k, v, table, keys, values],
            &[u32_of(position, "position")?, u32_of(kv_dim, "kv_dim")?],
            [groups_for(kv_dim, 64)?, 1, 1],
        )
    }

    /// Paged grouped-query attention of `q` (`[query_heads, head_dim]`) over
    /// `tokens` tokens, writing `[query_heads, head_dim]` to `out`. Out-of-range
    /// page-table entries cannot read outside `keys`/`values` (naga's
    /// `Restrict` bounds checks clamp every access); they read wrong data,
    /// which [`GpuModel`] rules out by construction.
    #[allow(clippy::too_many_arguments)]
    pub fn attention<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        q: &'a Buffer,
        keys: &'a Buffer,
        values: &'a Buffer,
        table: &'a Buffer,
        out: &'a Buffer,
        shape: AttentionShape,
        tokens: usize,
    ) -> Result<(), GpuError> {
        let AttentionShape {
            query_heads,
            kv_heads,
            head_dim,
        } = shape;
        vesper_tensor::shape::gqa_ratio(query_heads as u64, kv_heads as u64)?;
        if head_dim == 0 || head_dim > MAX_HEAD_DIM {
            return Err(GpuError::Limit("attention head_dim"));
        }
        if tokens == 0 {
            return Err(GpuError::Shape("attention over an empty sequence"));
        }
        if tokens > MAX_ATTENTION_TOKENS {
            return Err(GpuError::Limit("attention tokens"));
        }
        let width = query_heads
            .checked_mul(head_dim)
            .ok_or(GpuError::Limit("attention width"))?;
        need(q, width, "attention q")?;
        need(out, width, "attention out")?;
        if tokens.div_ceil(TOKENS_PER_PAGE) * 4 > table.len() as usize {
            return Err(GpuError::Shape("attention page table too short"));
        }
        let page_floats = TOKENS_PER_PAGE * kv_heads * head_dim;
        need(keys, page_floats, "attention keys")?;
        need(values, page_floats, "attention values")?;
        rec.dispatch(
            &self.attention,
            &[q, keys, values, table, out],
            &[
                u32_of(query_heads, "query_heads")?,
                u32_of(kv_heads, "kv_heads")?,
                u32_of(head_dim, "head_dim")?,
                u32_of(tokens, "tokens")?,
            ],
            [u32_of(query_heads, "query_heads")?, 1, 1],
        )
    }
}

/// RoPE frequencies `base^(-i / head_dim)` for even `i < head_dim`, by
/// exactly the CPU formula of `vesper_tensor::ops::rope`.
pub fn rope_frequencies(head_dim: usize, base: f32) -> Result<Vec<f32>, GpuError> {
    if head_dim == 0 || !head_dim.is_multiple_of(2) {
        return Err(GpuError::Shape("rope head_dim must be positive and even"));
    }
    if !(base.is_finite() && base > 1.0) {
        return Err(GpuError::Shape("rope base"));
    }
    Ok((0..head_dim)
        .step_by(2)
        .map(|i| 1.0 / base.powf(i as f32 / head_dim as f32))
        .collect())
}

fn f32_matrix(m: &Matrix) -> Result<&[f32], GpuError> {
    match m {
        Matrix::F32 { data, .. } => Ok(data),
        _ => Err(GpuError::Unsupported(
            "GPU decoding takes FP32 weights; Q8_0/Q4_0 kernels are not implemented",
        )),
    }
}

#[derive(Debug)]
struct GpuLayer {
    rms_attention: Buffer,
    wq: Buffer,
    wk: Buffer,
    wv: Buffer,
    wo: Buffer,
    rms_ffn: Buffer,
    w1: Buffer,
    w2: Buffer,
    w3: Buffer,
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static LEAKED_SESSIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Number of [`GpuSession`]s dropped without [`GpuKvCache::release`].
pub fn leaked_sessions() -> usize {
    LEAKED_SESSIONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// A `vesper_tensor::llama::Model` uploaded to the device (FP32 weights).
#[derive(Debug)]
pub struct GpuModel {
    ctx: Context,
    kernels: TensorKernels,
    config: Config,
    id: u64,
    embedding: Vec<f32>,
    layers: Vec<GpuLayer>,
    rms_final: Buffer,
    classifier: Buffer,
    freqs: Buffer,
}

impl GpuModel {
    /// Uploads `model`. Quantized weights are [`GpuError::Unsupported`];
    /// a sequence length above [`MAX_ATTENTION_TOKENS`] is
    /// [`GpuError::Limit`].
    pub fn upload(ctx: &Context, model: &Model) -> Result<Self, GpuError> {
        let config = model.config();
        config.validate()?;
        if config.seq_len > MAX_ATTENTION_TOKENS {
            return Err(GpuError::Limit(
                "model sequence length above the attention kernel's",
            ));
        }
        if config.head_dim() > MAX_HEAD_DIM {
            return Err(GpuError::Limit("model head width"));
        }
        let mut layers = Vec::with_capacity(model.layers().len());
        for l in model.layers() {
            layers.push(GpuLayer {
                rms_attention: ctx.buffer_from(&l.rms_attention)?,
                wq: ctx.buffer_from(f32_matrix(&l.wq)?)?,
                wk: ctx.buffer_from(f32_matrix(&l.wk)?)?,
                wv: ctx.buffer_from(f32_matrix(&l.wv)?)?,
                wo: ctx.buffer_from(f32_matrix(&l.wo)?)?,
                rms_ffn: ctx.buffer_from(&l.rms_ffn)?,
                w1: ctx.buffer_from(f32_matrix(&l.w1)?)?,
                w2: ctx.buffer_from(f32_matrix(&l.w2)?)?,
                w3: ctx.buffer_from(f32_matrix(&l.w3)?)?,
            });
        }
        Ok(Self {
            ctx: ctx.clone(),
            kernels: TensorKernels::new(ctx)?,
            config,
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            embedding: model.embedding().to_vec(),
            layers,
            rms_final: ctx.buffer_from(model.rms_final())?,
            classifier: ctx.buffer_from(f32_matrix(model.classifier())?)?,
            freqs: ctx.buffer_from(&rope_frequencies(config.head_dim(), ROPE_BASE)?)?,
        })
    }

    pub fn config(&self) -> Config {
        self.config
    }

    /// Runs one decode step for `token` at the next position of `session`
    /// and returns the logits read back from the device. The whole step is
    /// one submission: `14 * layers + 2` dispatches.
    pub fn forward<'s>(
        &self,
        cache: &mut GpuKvCache,
        session: &'s mut GpuSession,
        token: u32,
    ) -> Result<&'s [f32], GpuError> {
        let c = self.config;
        if cache.model != self.id || session.cache != cache.id {
            return Err(GpuError::ForeignObject);
        }
        let token = token as usize;
        if token >= c.vocab {
            return Err(GpuError::Shape("token outside the vocabulary"));
        }
        let position = session.position;
        if position >= c.seq_len {
            return Err(GpuError::Limit("sequence length"));
        }
        if position.is_multiple_of(TOKENS_PER_PAGE) {
            let page = cache.free.pop().ok_or(GpuError::Limit("K/V pages"))?;
            let slot = position / TOKENS_PER_PAGE;
            session.table.write((slot * 4) as u64, &[page])?;
            session.pages.push(page);
        }
        session
            .x
            .write(0, &self.embedding[token * c.dim..(token + 1) * c.dim])?;
        let head_dim = c.head_dim();
        let kv_dim = c.kv_dim();
        let shape = AttentionShape {
            query_heads: c.heads,
            kv_heads: c.kv_heads,
            head_dim,
        };
        let k = &self.kernels;
        let s = &*session;
        let mut rec = self.ctx.record()?;
        for (l, layer) in self.layers.iter().enumerate() {
            let (keys, values) = (&cache.keys[l], &cache.values[l]);
            k.rms_norm(
                &mut rec,
                &s.x,
                &layer.rms_attention,
                &s.xb,
                1,
                c.dim,
                RMS_EPSILON,
            )?;
            k.matmul(&mut rec, &s.xb, &layer.wq, &s.q, 1, c.dim, c.dim, false)?;
            k.matmul(&mut rec, &s.xb, &layer.wk, &s.k, 1, kv_dim, c.dim, false)?;
            k.matmul(&mut rec, &s.xb, &layer.wv, &s.v, 1, kv_dim, c.dim, false)?;
            k.rope(&mut rec, &s.q, &self.freqs, c.dim, head_dim, position)?;
            k.rope(&mut rec, &s.k, &self.freqs, kv_dim, head_dim, position)?;
            k.kv_write(
                &mut rec, &s.k, &s.v, &s.table, keys, values, position, kv_dim,
            )?;
            k.attention(
                &mut rec,
                &s.q,
                keys,
                values,
                &s.table,
                &s.att,
                shape,
                position + 1,
            )?;
            k.matmul(&mut rec, &s.att, &layer.wo, &s.x, 1, c.dim, c.dim, true)?;
            k.rms_norm(&mut rec, &s.x, &layer.rms_ffn, &s.xb, 1, c.dim, RMS_EPSILON)?;
            k.matmul(
                &mut rec,
                &s.xb,
                &layer.w1,
                &s.hb,
                1,
                c.hidden_dim,
                c.dim,
                false,
            )?;
            k.matmul(
                &mut rec,
                &s.xb,
                &layer.w3,
                &s.hb2,
                1,
                c.hidden_dim,
                c.dim,
                false,
            )?;
            k.swiglu(&mut rec, &s.hb, &s.hb2, c.hidden_dim)?;
            k.matmul(
                &mut rec,
                &s.hb,
                &layer.w2,
                &s.x,
                1,
                c.dim,
                c.hidden_dim,
                true,
            )?;
        }
        k.rms_norm(
            &mut rec,
            &s.x,
            &self.rms_final,
            &s.xb,
            1,
            c.dim,
            RMS_EPSILON,
        )?;
        k.matmul(
            &mut rec,
            &s.xb,
            &self.classifier,
            &s.logits,
            1,
            c.vocab,
            c.dim,
            false,
        )?;
        rec.submit()?.wait()?;
        session.logits.read(0, &mut session.host_logits)?;
        if session.host_logits.iter().any(|v| !v.is_finite()) {
            return Err(GpuError::NonFinite);
        }
        session.position += 1;
        Ok(&session.host_logits)
    }

    /// llama2.c greedy decoding on the device, with exactly the control flow
    /// of `vesper_tensor::llama::generate_greedy`: feed `prompt` (starting
    /// with BOS), then argmax tokens until `steps` positions or BOS.
    pub fn generate_greedy(
        &self,
        cache: &mut GpuKvCache,
        session: &mut GpuSession,
        prompt: &[u32],
        steps: usize,
    ) -> Result<Vec<u32>, GpuError> {
        if prompt.first() != Some(&BOS) {
            return Err(GpuError::Shape("prompt must start with BOS"));
        }
        if session.position != 0 {
            return Err(GpuError::Shape("generation needs a fresh session"));
        }
        if steps > self.config.seq_len {
            return Err(GpuError::Limit("steps exceed the model's sequence length"));
        }
        let mut out = Vec::with_capacity(steps);
        let mut token = prompt[0];
        for pos in 0..steps {
            let logits = self.forward(cache, session, token)?;
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
}

/// Paged K/V storage for one [`GpuModel`]: per layer, `pages` pages of 16
/// tokens, leased to sessions from one free list (page `p` of a session is
/// the same physical page index in every layer).
#[derive(Debug)]
pub struct GpuKvCache {
    model: u64,
    id: u64,
    config: Config,
    keys: Vec<Buffer>,
    values: Vec<Buffer>,
    free: Vec<u32>,
    pages: usize,
}

impl GpuKvCache {
    /// Allocates `pages` pages per layer. Pages are handed out from the top
    /// of the range down, so a session's page table is never the identity.
    pub fn new(model: &GpuModel, pages: usize) -> Result<Self, GpuError> {
        if pages == 0 {
            return Err(GpuError::Shape("K/V cache with zero pages"));
        }
        u32_of(pages, "K/V pages")?;
        let c = model.config;
        let floats = pages
            .checked_mul(TOKENS_PER_PAGE * c.kv_dim())
            .ok_or(GpuError::Limit("K/V cache size"))?;
        let bytes = (floats as u64)
            .checked_mul(4)
            .ok_or(GpuError::Limit("K/V cache size"))?;
        let mut keys = Vec::with_capacity(c.layers);
        let mut values = Vec::with_capacity(c.layers);
        for _ in 0..c.layers {
            keys.push(model.ctx.buffer(bytes)?);
            values.push(model.ctx.buffer(bytes)?);
        }
        Ok(Self {
            model: model.id,
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            config: c,
            keys,
            values,
            free: (0..pages as u32).collect(),
            pages,
        })
    }

    /// Pages not leased to any session.
    pub fn free_pages(&self) -> usize {
        self.free.len()
    }

    pub fn pages(&self) -> usize {
        self.pages
    }

    /// A fresh session with its own activation buffers and page table.
    pub fn begin(&mut self, model: &GpuModel) -> Result<GpuSession, GpuError> {
        if model.id != self.model {
            return Err(GpuError::ForeignObject);
        }
        let c = self.config;
        let ctx = &model.ctx;
        let f = |n: usize| ctx.buffer((n * 4) as u64);
        Ok(GpuSession {
            cache: self.id,
            position: 0,
            pages: Vec::with_capacity(c.seq_len.div_ceil(TOKENS_PER_PAGE)),
            table: f(c.seq_len.div_ceil(TOKENS_PER_PAGE))?,
            x: f(c.dim)?,
            xb: f(c.dim)?,
            q: f(c.dim)?,
            k: f(c.kv_dim())?,
            v: f(c.kv_dim())?,
            att: f(c.dim)?,
            hb: f(c.hidden_dim)?,
            hb2: f(c.hidden_dim)?,
            logits: f(c.vocab)?,
            host_logits: vec![0.0; c.vocab],
            released: false,
        })
    }

    /// Ends `session`, returning its pages to the free list. A session of
    /// another cache is returned (boxed) with the error.
    pub fn release(&mut self, mut session: GpuSession) -> Result<(), (GpuError, Box<GpuSession>)> {
        if session.cache != self.id {
            return Err((GpuError::ForeignObject, Box::new(session)));
        }
        // Pages go back in reverse lease order, so the next session leases
        // them in the same order as this one did.
        while let Some(p) = session.pages.pop() {
            self.free.push(p);
        }
        session.released = true;
        Ok(())
    }
}

/// One decoding sequence: position, leased pages and activation buffers.
/// Neither Clone nor Copy; its only exit is [`GpuKvCache::release`]. A
/// dropped unreleased session is counted by [`leaked_sessions`] and its pages
/// stay leased.
#[must_use = "a GpuSession must be released with GpuKvCache::release"]
#[derive(Debug)]
pub struct GpuSession {
    cache: u64,
    position: usize,
    pages: Vec<u32>,
    table: Buffer,
    x: Buffer,
    xb: Buffer,
    q: Buffer,
    k: Buffer,
    v: Buffer,
    att: Buffer,
    hb: Buffer,
    hb2: Buffer,
    logits: Buffer,
    host_logits: Vec<f32>,
    released: bool,
}

impl GpuSession {
    pub fn position(&self) -> usize {
        self.position
    }

    /// Physical page indices leased so far, in position order.
    pub fn pages(&self) -> &[u32] {
        &self.pages
    }
}

impl Drop for GpuSession {
    fn drop(&mut self) {
        if !self.released {
            LEAKED_SESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
