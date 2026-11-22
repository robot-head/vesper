//! Tensor kernels (design §28) on the Vulkan runtime, and a GPU decoder for
//! `vesper-tensor`'s llama2.c models.
//!
//! The kernels are WGSL, compiled ahead of time with naga into the committed
//! SPIR-V that [`SPIRV`] embeds ([`crate::shader`]): [`TensorKernels::matmul`]
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
use vesper_tensor::llama::{BOS, Config, Model, RMS_EPSILON, ROPE_BASE, RopeStyle};
use vesper_tensor::ops::argmax;
use vesper_tensor::quant::Matrix;

use crate::error::GpuError;
use crate::runtime::binding::{self, StepModel, StepSession, StepShape, TensorKs};
use crate::runtime::{Buffer, Context, Kernel, Recording, port_error};

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

/// Every WGSL source of this module with its entry point: the sources the
/// committed SPIR-V is compiled from (`tests/spirv.rs`).
pub const SOURCES: [(&str, &str); 6] = [
    ("matmul", MATMUL),
    ("rms_norm", RMS_NORM),
    ("rope", ROPE),
    ("swiglu", SWIGLU),
    ("kv_write", KV_WRITE),
    ("attention", ATTENTION),
];

const MATMUL_SPV: &[u8] = include_bytes!("shaders/spirv/matmul.spv");
const RMS_NORM_SPV: &[u8] = include_bytes!("shaders/spirv/rms_norm.spv");
const ROPE_SPV: &[u8] = include_bytes!("shaders/spirv/rope.spv");
const SWIGLU_SPV: &[u8] = include_bytes!("shaders/spirv/swiglu.spv");
const KV_WRITE_SPV: &[u8] = include_bytes!("shaders/spirv/kv_write.spv");
const ATTENTION_SPV: &[u8] = include_bytes!("shaders/spirv/attention.spv");

/// The committed SPIR-V of every kernel of this module with its entry point,
/// in the order of [`SOURCES`].
pub const SPIRV: [(&str, &[u8]); 6] = [
    ("matmul", MATMUL_SPV),
    ("rms_norm", RMS_NORM_SPV),
    ("rope", ROPE_SPV),
    ("swiglu", SWIGLU_SPV),
    ("kv_write", KV_WRITE_SPV),
    ("attention", ATTENTION_SPV),
];

fn u32_of(v: usize, what: &'static str) -> Result<u32, GpuError> {
    u32::try_from(v).map_err(|_| GpuError::Limit(what))
}

/// Converts a Vesper failure: a device or shape error is the crate's error,
/// anything else is a bug of the port.
macro_rules! vesper {
    ($call:expr, $err:ident) => {
        $call.map_err(|e| match e {
            binding::$err::GpuErr(g) => port_error(g),
            other => panic!("the Vesper GPU tensor kernels failed internally: {other:?}"),
        })
    };
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
            matmul: ctx.kernel(MATMUL_SPV, "matmul", 3, 16)?,
            rms_norm: ctx.kernel(RMS_NORM_SPV, "rms_norm", 3, 8)?,
            rope: ctx.kernel(ROPE_SPV, "rope", 2, 12)?,
            swiglu: ctx.kernel(SWIGLU_SPV, "swiglu", 2, 4)?,
            kv_write: ctx.kernel(KV_WRITE_SPV, "kv_write", 5, 8)?,
            attention: ctx.kernel(ATTENTION_SPV, "attention", 5, 16)?,
        })
    }

    fn raw(&self) -> TensorKs {
        TensorKs {
            matmul: self.matmul.raw(),
            rms_norm: self.rms_norm.raw(),
            rope: self.rope.raw(),
            swiglu: self.swiglu.raw(),
            kv_write: self.kv_write.raw(),
            attention: self.attention.raw(),
        }
    }

    fn all(&self) -> [&Kernel; 6] {
        [
            &self.matmul,
            &self.rms_norm,
            &self.rope,
            &self.swiglu,
            &self.kv_write,
            &self.attention,
        ]
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
        let (kern, ra, rw, ro) = (self.matmul.raw(), a.raw(), w.raw(), out.raw());
        rec.record_with(&[&self.matmul], &[a, w, out], 1, |dev, r, dry| {
            vesper!(
                binding::tk_matmul(
                    dev, r, kern, ra, rw, ro, m as u64, n as u64, k as u64, accumulate, dry
                ),
                TkMatmulError
            )
        })
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
        let (kern, rx, rw, ro) = (self.rms_norm.raw(), x.raw(), weight.raw(), out.raw());
        let eps = epsilon.to_bits();
        rec.record_with(&[&self.rms_norm], &[x, weight, out], 1, |dev, r, dry| {
            vesper!(
                binding::tk_rms_norm(dev, r, kern, rx, rw, ro, rows as u64, n as u64, eps, dry),
                TkRmsNormError
            )
        })
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
        let (kern, rx, rf) = (self.rope.raw(), x.raw(), freqs.raw());
        rec.record_with(&[&self.rope], &[x, freqs], 1, |dev, r, dry| {
            vesper!(
                binding::tk_rope(
                    dev,
                    r,
                    kern,
                    rx,
                    rf,
                    len as u64,
                    head_dim as u64,
                    position as u64,
                    dry
                ),
                TkRopeError
            )
        })
    }

    /// `gate[i] = silu(gate[i]) * up[i]` for `n` elements.
    pub fn swiglu<'a>(
        &'a self,
        rec: &mut Recording<'a>,
        gate: &'a Buffer,
        up: &'a Buffer,
        n: usize,
    ) -> Result<(), GpuError> {
        let (kern, rg, ru) = (self.swiglu.raw(), gate.raw(), up.raw());
        rec.record_with(&[&self.swiglu], &[gate, up], 1, |dev, r, dry| {
            vesper!(
                binding::tk_swiglu(dev, r, kern, rg, ru, n as u64, dry),
                TkSwigluError
            )
        })
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
        let kern = self.kv_write.raw();
        let (rk, rv, rt) = (k.raw(), v.raw(), table.raw());
        let (rkeys, rvalues) = (keys.raw(), values.raw());
        rec.record_with(
            &[&self.kv_write],
            &[k, v, table, keys, values],
            1,
            |dev, r, dry| {
                vesper!(
                    binding::tk_kv_write(
                        dev,
                        r,
                        kern,
                        rk,
                        rv,
                        rt,
                        rkeys,
                        rvalues,
                        position as u64,
                        kv_dim as u64,
                        dry
                    ),
                    TkKvWriteError
                )
            },
        )
    }

    /// Paged grouped-query attention of `q` (`[query_heads, head_dim]`) over
    /// `tokens` tokens, writing `[query_heads, head_dim]` to `out`. Out-of-range
    /// page-table entries cannot read outside `keys`/`values` (the
    /// kernels are compiled with naga's `Restrict` bounds checks, which clamp
    /// every access); they read wrong data,
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
        let kern = self.attention.raw();
        let (rq, rkeys, rvalues) = (q.raw(), keys.raw(), values.raw());
        let (rt, ro) = (table.raw(), out.raw());
        rec.record_with(
            &[&self.attention],
            &[q, keys, values, table, out],
            1,
            |dev, r, dry| {
                vesper!(
                    binding::tk_attention(
                        dev,
                        r,
                        kern,
                        rq,
                        rkeys,
                        rvalues,
                        rt,
                        ro,
                        query_heads as u64,
                        kv_heads as u64,
                        head_dim as u64,
                        tokens as u64,
                        dry
                    ),
                    TkAttentionError
                )
            },
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
    m.as_f32().ok_or(GpuError::Unsupported(
        "GPU decoding takes FP32 weights; quantized kernels are not implemented",
    ))
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
        // The kernels hard-code llama2.c's interleaved RoPE, its base and the
        // RMSNorm epsilon.
        if config.rope != RopeStyle::Interleaved
            || config.rope_base != ROPE_BASE
            || config.rms_epsilon != RMS_EPSILON
        {
            return Err(GpuError::Unsupported(
                "GPU decoding takes interleaved RoPE with base 10000 and RMSNorm epsilon 1e-5",
            ));
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
            embedding: f32_matrix(model.embedding())?.to_vec(),
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
        let shape = AttentionShape {
            query_heads: c.heads,
            kv_heads: c.kv_heads,
            head_dim: c.head_dim(),
        };
        vesper_tensor::shape::gqa_ratio(shape.query_heads as u64, shape.kv_heads as u64)?;
        let step = StepShape {
            dim: c.dim as u64,
            kv_dim: c.kv_dim() as u64,
            head_dim: c.head_dim() as u64,
            heads: c.heads as u64,
            kv_heads: c.kv_heads as u64,
            hidden_dim: c.hidden_dim as u64,
            vocab: c.vocab as u64,
            layers: self.layers.len() as u64,
            position: position as u64,
            epsilon: RMS_EPSILON.to_bits(),
        };
        let s = &*session;
        let raw_session = StepSession {
            table: s.table.raw(),
            x: s.x.raw(),
            xb: s.xb.raw(),
            q: s.q.raw(),
            k: s.k.raw(),
            v: s.v.raw(),
            att: s.att.raw(),
            hb: s.hb.raw(),
            hb2: s.hb2.raw(),
            logits: s.logits.raw(),
        };
        let model = StepModel {
            rms_final: self.rms_final.raw(),
            classifier: self.classifier.raw(),
            freqs: self.freqs.raw(),
        };
        let mut buffers: Vec<&Buffer> = vec![
            &s.table,
            &s.x,
            &s.xb,
            &s.q,
            &s.k,
            &s.v,
            &s.att,
            &s.hb,
            &s.hb2,
            &s.logits,
            &self.rms_final,
            &self.classifier,
            &self.freqs,
        ];
        let mut layers = Vec::with_capacity(self.layers.len() * 22);
        for (l, layer) in self.layers.iter().enumerate() {
            let own = [
                &layer.rms_attention,
                &layer.wq,
                &layer.wk,
                &layer.wv,
                &layer.wo,
                &layer.rms_ffn,
                &layer.w1,
                &layer.w2,
                &layer.w3,
                &cache.keys[l],
                &cache.values[l],
            ];
            for b in own {
                let raw = b.raw();
                layers.extend([raw.buffer, raw.len]);
                buffers.push(b);
            }
        }
        let k = &self.kernels;
        let kernels = k.raw();
        let mut rec = self.ctx.record()?;
        rec.record_with(
            &k.all(),
            &buffers,
            14 * self.layers.len() + 2,
            |dev, r, dry| {
                vesper!(
                    binding::tk_step(dev, r, kernels, step, raw_session, model, &layers, dry),
                    TkStepError
                )
            },
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
