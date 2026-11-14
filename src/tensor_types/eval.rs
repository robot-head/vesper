//! Evaluator for checked tensor programs.
//!
//! The elaborated program runs on the existing one-shot effect machine
//! ([`crate::runtime::Runtime::begin_external`]). Every `Tensor` and
//! `KvCachePool` operation suspends the machine; this host services it with
//! the `vesper-tensor` kernels (`ops`, `quant::Matrix`, `kv::KvPool`,
//! `attention::paged_gqa_attention`) and resumes. Shapes are re-checked
//! against the concrete buffers at every operation, so a disagreement between
//! the static rules and the host is a typed error, never a wrong read.
use super::dim::Dim;
use super::infer::{Analysis, FnSig};
use super::types::{DType, Ty};
use super::wire::QuantFormat;
use super::{ACTIVE_SEQ, DIMS, KV_ERROR, LEASE, TENSOR};
use crate::runtime::{Execution, Runtime, Value};
use std::collections::BTreeMap;
use vesper_tensor::attention::{AttentionMode, AttentionShape, paged_gqa_attention};
use vesper_tensor::kv::{Bf16Step, KvConfig, KvError, KvFormat, KvPool, Sequence, TOKENS_PER_PAGE};
use vesper_tensor::quant::{Matrix, decode_bf16, encode_bf16};

/// Element storage of a host tensor.
#[derive(Clone, Debug, PartialEq)]
pub enum HostData {
    F32(Vec<f32>),
    /// BF16 bit patterns.
    Bf16(Vec<u16>),
    /// Packed blocks of the wire struct `block`.
    Quant {
        block: String,
        bytes: Vec<u8>,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct HostTensor {
    pub dims: Vec<usize>,
    pub data: HostData,
}
impl HostTensor {
    pub fn f32(dims: Vec<usize>, data: Vec<f32>) -> Self {
        Self {
            dims,
            data: HostData::F32(data),
        }
    }
    pub fn bf16_from_f32(dims: Vec<usize>, data: &[f32]) -> Self {
        Self {
            dims,
            data: HostData::Bf16(data.iter().map(|v| encode_bf16(*v)).collect()),
        }
    }
    pub fn quantized(block: &str, rows: usize, cols: usize, bytes: Vec<u8>) -> Self {
        Self {
            dims: vec![rows, cols],
            data: HostData::Quant {
                block: block.into(),
                bytes,
            },
        }
    }
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &self.data {
            HostData::F32(v) => Some(v),
            _ => None,
        }
    }
    fn elements(&self) -> usize {
        self.dims.iter().product()
    }
}

/// Entry arguments and results.
#[derive(Clone, Debug, PartialEq)]
pub enum HostValue {
    Tensor(HostTensor),
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Tuple(Vec<HostValue>),
    /// An enum or struct value, such as `Ok(t)` or `KvOomError::OutOfPages`.
    Data {
        ty: String,
        constructor: String,
        fields: BTreeMap<String, HostValue>,
    },
    /// A linear pool owner returned to the host (its pages stay allocated).
    Lease(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    /// The program has diagnostics.
    NotChecked(Vec<String>),
    /// Bad entry function or arguments.
    Entry(String),
    /// A runtime shape or dtype disagreement.
    Shape(String),
    /// A host kernel rejected its input.
    Kernel(String),
    /// A resource limit.
    Limit(String),
    /// The effect machine failed.
    Runtime(String),
}
impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EvalError {}

/// Host bounds.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Physical 16-token KV pages in the pool.
    pub pages: usize,
    pub max_sequences: usize,
    pub max_tensor_elements: usize,
    pub max_tensors: usize,
    /// Effect-machine fuel.
    pub fuel: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            pages: 64,
            max_sequences: 16,
            max_tensor_elements: 1 << 22,
            max_tensors: 1 << 14,
            fuel: 10_000_000,
        }
    }
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tensor and pool operations serviced.
    pub operations: u64,
    pub sequences_begun: u64,
    pub sequences_released: u64,
    pub pages_leased: u64,
    pub pages_released: u64,
    /// Pool pages still allocated when the entry returned.
    pub allocated_pages_at_exit: usize,
    /// Dimensions bound at the entry.
    pub dims: BTreeMap<String, u64>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub value: HostValue,
    pub stats: Stats,
}

struct Host<'a> {
    analysis: &'a Analysis,
    options: Options,
    tensors: Vec<HostTensor>,
    pool: Option<KvPool>,
    sequences: BTreeMap<u64, Sequence>,
    next_owner: u64,
    stats: Stats,
}

fn uint(v: u64) -> Value {
    Value::UInt(v, 64)
}
fn data(ty: &str, constructor: &str, fields: Vec<(&str, Value)>) -> Value {
    Value::Data {
        ty: ty.into(),
        constructor: constructor.into(),
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        resource_identity: None,
    }
}
fn ok(v: Value) -> Value {
    data("Result", "Ok", vec![("0", v)])
}
fn err(constructor: &str) -> Value {
    data(
        "Result",
        "Err",
        vec![("0", data(KV_ERROR, constructor, vec![]))],
    )
}
fn field_u64(v: &Value, ty: &str, field: &str) -> Result<u64, EvalError> {
    match v {
        Value::Data { ty: t, fields, .. } if t == ty => match fields.get(field) {
            Some(Value::UInt(n, _)) => Ok(*n),
            Some(Value::Int(n)) if *n >= 0 => Ok(*n as u64),
            _ => Err(EvalError::Runtime(format!("malformed {ty} value"))),
        },
        _ => Err(EvalError::Runtime(format!(
            "expected a {ty} value, found {v}"
        ))),
    }
}
fn scalar_u64(v: &Value) -> Result<u64, EvalError> {
    match v {
        Value::UInt(n, _) => Ok(*n),
        Value::Int(n) if *n >= 0 => Ok(*n as u64),
        _ => Err(EvalError::Runtime(format!(
            "expected an unsigned integer, found {v}"
        ))),
    }
}
fn scalar_f64(v: &Value) -> Result<f64, EvalError> {
    match v {
        Value::Float(f) => Ok(*f),
        _ => Err(EvalError::Runtime(format!("expected f64, found {v}"))),
    }
}
fn kernel(e: impl std::fmt::Debug) -> EvalError {
    EvalError::Kernel(format!("{e:?}"))
}
fn kv_error(e: KvError) -> Result<Value, EvalError> {
    match e {
        KvError::OutOfPages | KvError::SequenceLimit | KvError::Budget => Ok(err("OutOfPages")),
        KvError::TokenLimit => Ok(err("TokenLimit")),
        other => Err(EvalError::Kernel(format!("KV pool: {other:?}"))),
    }
}

/// Bind entry dimensions from the argument tensors.
fn bind_dims(sig: &FnSig, args: &[HostValue]) -> Result<BTreeMap<String, u64>, EvalError> {
    let mut env = BTreeMap::new();
    let mut checks: Vec<(Dim, usize, String)> = vec![];
    let mut dtypes: BTreeMap<String, DType> = BTreeMap::new();
    struct Env<'a> {
        dims: &'a mut BTreeMap<String, u64>,
        dtypes: &'a mut BTreeMap<String, DType>,
        checks: &'a mut Vec<(Dim, usize, String)>,
    }
    fn visit(t: &Ty, v: &HostValue, what: &str, cx: &mut Env<'_>) -> Result<(), EvalError> {
        let (env, checks) = (&mut *cx.dims, &mut *cx.checks);
        match (t, v) {
            (Ty::Tensor { dtype, shape }, HostValue::Tensor(h)) => {
                let found = match &h.data {
                    HostData::F32(_) => DType::F32,
                    HostData::Bf16(_) => DType::Bf16,
                    HostData::Quant { block, .. } => DType::Block(block.clone()),
                };
                let ok = match dtype {
                    // A dtype parameter takes the element type of the first
                    // tensor that mentions it; later ones must agree.
                    DType::Var(p) => cx.dtypes.entry(p.clone()).or_insert(found.clone()) == &found,
                    d => *d == found,
                };
                if !ok {
                    return Err(EvalError::Entry(format!(
                        "{what}: expected element type {dtype}"
                    )));
                }
                if shape.len() != h.dims.len() {
                    return Err(EvalError::Entry(format!(
                        "{what}: expected rank {}, found {:?}",
                        shape.len(),
                        h.dims
                    )));
                }
                for (d, n) in shape.iter().zip(&h.dims) {
                    if let Some(v) = d.as_var()
                        && !env.contains_key(v)
                    {
                        env.insert(v.to_string(), *n as u64);
                    }
                    checks.push((d.clone(), *n, what.to_string()));
                }
                Ok(())
            }
            (Ty::Tuple(ts), HostValue::Tuple(vs)) if ts.len() == vs.len() => {
                for (i, (t, v)) in ts.iter().zip(vs).enumerate() {
                    visit(t, v, &format!("{what}.{i}"), cx)?;
                }
                Ok(())
            }
            (t, _) if t.has_tensor() => Err(EvalError::Entry(format!("{what}: expected {t}"))),
            _ => Ok(()),
        }
    }
    if args.len() != sig.params.len() {
        return Err(EvalError::Entry(format!(
            "'{}' takes {} arguments, found {}",
            sig.name,
            sig.params.len(),
            args.len()
        )));
    }
    let mut cx = Env {
        dims: &mut env,
        dtypes: &mut dtypes,
        checks: &mut checks,
    };
    for (i, ((_, t), v)) in sig.params.iter().zip(args).enumerate() {
        visit(t, v, &format!("argument {}", i + 1), &mut cx)?;
    }
    // A dimension that occurs only in products (`[1, D * 2]`) is solved once
    // every other variable of the product is known and the division is exact.
    loop {
        let mut progress = false;
        for (d, n, _) in &checks {
            let mut unknown = d.variables().filter(|v| !env.contains_key(*v));
            let (Some(v), None) = (unknown.next().cloned(), unknown.next()) else {
                continue;
            };
            if d.vars.get(&v) != Some(&1) {
                continue;
            }
            let rest = d
                .variables()
                .filter(|w| **w != v)
                .try_fold(d.coeff, |a, w| a.checked_mul(env[w]));
            if let Some(r) = rest
                && r != 0
                && (*n as u64).is_multiple_of(r)
                && *n as u64 / r > 0
            {
                env.insert(v, *n as u64 / r);
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    for d in sig.dims() {
        if !env.contains_key(d) {
            return Err(EvalError::Entry(format!(
                "dimension '{d}' of '{}' is not determined by the argument shapes",
                sig.name
            )));
        }
    }
    for (d, n, what) in checks {
        if d.eval(&env) != Some(n as u64) {
            return Err(EvalError::Entry(format!(
                "{what}: dimension {d} is {} but the tensor has {n}",
                d.eval(&env).map_or("unbounded".into(), |v| v.to_string())
            )));
        }
    }
    for ob in &sig.obligations {
        if ob.holds(&env) != Some(true) {
            return Err(EvalError::Entry(format!(
                "entry precondition of '{}' fails: {ob}",
                sig.name
            )));
        }
    }
    Ok(env)
}

/// Run `entry` of a checked tensor program.
pub fn run(
    analysis: &Analysis,
    entry: &str,
    args: Vec<HostValue>,
    options: Options,
) -> Result<Outcome, EvalError> {
    if !analysis.checked.diagnostics.is_empty() {
        return Err(EvalError::NotChecked(
            analysis
                .checked
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect(),
        ));
    }
    let sig = analysis
        .signatures
        .get(entry)
        .ok_or_else(|| EvalError::Entry(format!("no function '{entry}'")))?;
    let env = bind_dims(sig, &args)?;
    let mut host = Host {
        analysis,
        options,
        tensors: vec![],
        pool: None,
        sequences: BTreeMap::new(),
        next_owner: 1,
        stats: Stats {
            dims: env.clone(),
            ..Stats::default()
        },
    };
    if let Some((kv, hd)) = &sig.pool {
        let (Some(heads), Some(head_dim)) = (kv.eval(&env), hd.eval(&env)) else {
            return Err(EvalError::Entry(
                "pool dimensions are not determined".into(),
            ));
        };
        let config = KvConfig {
            heads: heads as usize,
            head_dim: head_dim as usize,
            pages: options.pages,
            max_sequences: options.max_sequences,
            max_tokens_per_sequence: options.pages * TOKENS_PER_PAGE,
            max_bytes: 64 * 1024 * 1024,
        };
        host.pool = Some(KvPool::with_format(config, KvFormat::Bf16).map_err(kernel)?);
    }
    let mut values = vec![];
    for d in sig.dims() {
        values.push(uint(env[d]));
    }
    for v in args {
        values.push(host.import(v)?);
    }
    let mut runtime = Runtime::new(&analysis.checked.world);
    runtime.fuel = options.fuel;
    let mut execution = runtime
        .begin_external(entry, values)
        .map_err(EvalError::Runtime)?;
    loop {
        match execution {
            Execution::Complete(v) => {
                let value = host.export(&v)?;
                if let HostValue::Tensor(t) = &value
                    && let Ty::Tensor { shape, .. } = &sig.result
                {
                    let want: Option<Vec<usize>> = shape
                        .iter()
                        .map(|d| d.eval(&env).map(|n| n as usize))
                        .collect();
                    if want.as_ref() != Some(&t.dims) {
                        return Err(EvalError::Shape(format!(
                            "result has shape {:?}, declared {}",
                            t.dims, sig.result
                        )));
                    }
                }
                host.stats.allocated_pages_at_exit =
                    host.pool.as_ref().map_or(0, |p| p.stats().allocated_pages);
                return Ok(Outcome {
                    value,
                    stats: host.stats,
                });
            }
            Execution::Suspended(s) => {
                let request = s.request();
                let effect = request.effect.name.clone();
                let op = request.operation.clone();
                let arguments = request.arguments.clone();
                host.stats.operations += 1;
                let result = match effect.as_str() {
                    TENSOR => host.tensor_op(&op, &arguments)?,
                    super::KV_EFFECT => host.pool_op(&op, &arguments)?,
                    other => {
                        return Err(EvalError::Runtime(format!(
                            "unhandled external effect {other}::{op}"
                        )));
                    }
                };
                execution = s.resume(result).map_err(EvalError::Runtime)?;
            }
        }
    }
}

impl Host<'_> {
    fn add(&mut self, t: HostTensor) -> Result<Value, EvalError> {
        if self.tensors.len() >= self.options.max_tensors {
            return Err(EvalError::Limit("too many tensors".into()));
        }
        if t.elements() > self.options.max_tensor_elements {
            return Err(EvalError::Limit("tensor too large".into()));
        }
        self.tensors.push(t);
        Ok(data(
            TENSOR,
            TENSOR,
            vec![("id", uint(self.tensors.len() as u64 - 1))],
        ))
    }
    /// Refuse an output shape over the element limit before allocating it.
    fn room(&self, shape: &[usize]) -> Result<(), EvalError> {
        match shape.iter().try_fold(1usize, |a, d| a.checked_mul(*d)) {
            Some(n) if n <= self.options.max_tensor_elements => Ok(()),
            _ => Err(EvalError::Limit(format!(
                "tensor of shape {shape:?} is too large"
            ))),
        }
    }
    fn get(&self, v: &Value) -> Result<&HostTensor, EvalError> {
        let id = field_u64(v, TENSOR, "id")?;
        self.tensors
            .get(id as usize)
            .ok_or_else(|| EvalError::Runtime("unknown tensor".into()))
    }
    fn f32s(&self, v: &Value, what: &str) -> Result<(Vec<usize>, Vec<f32>), EvalError> {
        let t = self.get(v)?;
        match &t.data {
            HostData::F32(d) => Ok((t.dims.clone(), d.clone())),
            _ => Err(EvalError::Shape(format!("{what}: expected an F32 tensor"))),
        }
    }
    fn import(&mut self, v: HostValue) -> Result<Value, EvalError> {
        Ok(match v {
            HostValue::Tensor(t) => {
                let expected: usize = t.dims.iter().product();
                match &t.data {
                    HostData::F32(d) if d.len() == expected => {}
                    HostData::Bf16(d) if d.len() == expected => {}
                    HostData::Quant { block, bytes } => {
                        let format = self.format(block)?;
                        let [rows, cols] = t.dims[..] else {
                            return Err(EvalError::Entry("quantized tensors are rank 2".into()));
                        };
                        Matrix::quantized(format.weight_format(), rows, cols, bytes.clone())
                            .map_err(kernel)?;
                    }
                    _ => {
                        return Err(EvalError::Entry(format!(
                            "tensor of shape {:?} has the wrong number of elements",
                            t.dims
                        )));
                    }
                }
                self.add(t)?
            }
            HostValue::U64(n) => uint(n),
            HostValue::I64(n) => Value::Int(n),
            HostValue::F64(f) => Value::Float(f),
            HostValue::Bool(b) => Value::Bool(b),
            HostValue::Tuple(vs) => Value::Tuple(
                vs.into_iter()
                    .map(|v| self.import(v))
                    .collect::<Result<_, _>>()?,
            ),
            HostValue::Data { .. } | HostValue::Lease(_) => {
                return Err(EvalError::Entry(
                    "only tensors and scalars are entry arguments".into(),
                ));
            }
        })
    }
    fn export(&self, v: &Value) -> Result<HostValue, EvalError> {
        Ok(match v {
            Value::Data { ty, .. } if ty == TENSOR => HostValue::Tensor(self.get(v)?.clone()),
            Value::Data { ty, .. } if ty == LEASE || ty == ACTIVE_SEQ => {
                HostValue::Lease(ty.clone())
            }
            Value::Data {
                ty,
                constructor,
                fields,
                ..
            } => HostValue::Data {
                ty: ty.clone(),
                constructor: constructor.clone(),
                fields: fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), self.export(v)?)))
                    .collect::<Result<_, EvalError>>()?,
            },
            Value::UInt(n, _) => HostValue::U64(*n),
            Value::Int(n) => HostValue::I64(*n),
            Value::Float(f) => HostValue::F64(*f),
            Value::Bool(b) => HostValue::Bool(*b),
            Value::Tuple(vs) => HostValue::Tuple(
                vs.iter()
                    .map(|v| self.export(v))
                    .collect::<Result<_, _>>()?,
            ),
            other => return Err(EvalError::Runtime(format!("cannot export {other}"))),
        })
    }
    fn format(&self, block: &str) -> Result<QuantFormat, EvalError> {
        self.analysis
            .wires
            .get(block)
            .and_then(|w| w.format)
            .ok_or_else(|| EvalError::Shape(format!("'{block}' is not a quantized block")))
    }
    fn tensor_op(&mut self, op: &str, a: &[Value]) -> Result<Value, EvalError> {
        let arity = |n: usize| {
            if a.len() == n {
                Ok(())
            } else {
                Err(EvalError::Runtime(format!(
                    "Tensor::{op}: wrong argument count"
                )))
            }
        };
        let shape_err = |m: String| EvalError::Shape(format!("Tensor::{op}: {m}"));
        match op {
            "matmul" => {
                arity(2)?;
                let (sa, da) = self.f32s(&a[0], op)?;
                let (sb, db) = self.f32s(&a[1], op)?;
                let (&[m, k], &[k2, n]) = (&sa[..], &sb[..]) else {
                    return Err(shape_err("expected rank-2 operands".into()));
                };
                if k != k2 {
                    return Err(shape_err(format!("inner dimensions {k} and {k2}")));
                }
                self.room(&[m, n])?;
                // Rows of B^T, so each output row is one host matvec.
                let mut bt = vec![0.0; k * n];
                for r in 0..k {
                    for c in 0..n {
                        bt[c * k + r] = db[r * n + c];
                    }
                }
                let w = Matrix::f32(n, k, bt).map_err(kernel)?;
                let mut out = vec![0.0; m * n];
                for (row, chunk) in out.chunks_mut(n).enumerate() {
                    w.matvec(&da[row * k..(row + 1) * k], chunk)
                        .map_err(kernel)?;
                }
                self.add(HostTensor::f32(vec![m, n], out))
            }
            "matvec" => {
                arity(2)?;
                let (sa, da) = self.f32s(&a[0], op)?;
                let (sx, dx) = self.f32s(&a[1], op)?;
                let (&[m, k], &[k2]) = (&sa[..], &sx[..]) else {
                    return Err(shape_err("expected [M, K] and [K]".into()));
                };
                if k != k2 {
                    return Err(shape_err(format!("inner dimensions {k} and {k2}")));
                }
                let w = Matrix::f32(m, k, da).map_err(kernel)?;
                let mut out = vec![0.0; m];
                w.matvec(&dx, &mut out).map_err(kernel)?;
                self.add(HostTensor::f32(vec![m], out))
            }
            "add" | "mul" => {
                arity(2)?;
                let (sa, da) = self.f32s(&a[0], op)?;
                let (sb, db) = self.f32s(&a[1], op)?;
                self.room(&broadcast_shape(&sa, &sb).map_err(shape_err)?)?;
                let (shape, out) =
                    broadcast_apply(
                        &sa,
                        &da,
                        &sb,
                        &db,
                        |x, y| {
                            if op == "add" { x + y } else { x * y }
                        },
                    )
                    .map_err(shape_err)?;
                self.add(HostTensor::f32(shape, out))
            }
            "swiglu" => {
                arity(2)?;
                let (sg, mut dg) = self.f32s(&a[0], op)?;
                let (su, du) = self.f32s(&a[1], op)?;
                if sg != su {
                    return Err(shape_err(format!("{sg:?} and {su:?}")));
                }
                vesper_tensor::ops::swiglu(&mut dg, &du).map_err(kernel)?;
                self.add(HostTensor::f32(sg, dg))
            }
            "reshape" => {
                arity(2)?;
                let t = self.get(&a[0])?.clone();
                let dims = dims_of(&a[1])?;
                if dims.iter().product::<usize>() != t.elements() {
                    return Err(shape_err(format!("{:?} to {dims:?}", t.dims)));
                }
                self.add(HostTensor { dims, data: t.data })
            }
            "transpose" => {
                arity(1)?;
                let (s, d) = self.f32s(&a[0], op)?;
                let [r, c] = s[..] else {
                    return Err(shape_err("expected rank 2".into()));
                };
                let mut out = vec![0.0; r * c];
                for i in 0..r {
                    for j in 0..c {
                        out[j * r + i] = d[i * c + j];
                    }
                }
                self.add(HostTensor::f32(vec![c, r], out))
            }
            "softmax" => {
                arity(1)?;
                let (s, mut d) = self.f32s(&a[0], op)?;
                vesper_tensor::ops::softmax(&mut d).map_err(kernel)?;
                self.add(HostTensor::f32(s, d))
            }
            "silu" => {
                arity(1)?;
                let (s, d) = self.f32s(&a[0], op)?;
                let out = d.iter().map(|v| vesper_tensor::ops::silu(*v)).collect();
                self.add(HostTensor::f32(s, out))
            }
            "to_bf16" => {
                arity(1)?;
                let (s, d) = self.f32s(&a[0], op)?;
                self.add(HostTensor::bf16_from_f32(s, &d))
            }
            "to_f32" => {
                arity(1)?;
                let t = self.get(&a[0])?.clone();
                let HostData::Bf16(bits) = &t.data else {
                    return Err(shape_err("expected a Bf16 tensor".into()));
                };
                let out = bits.iter().map(|b| decode_bf16(*b)).collect();
                self.add(HostTensor::f32(t.dims, out))
            }
            "argmax" => {
                arity(1)?;
                let (_, d) = self.f32s(&a[0], op)?;
                Ok(uint(vesper_tensor::ops::argmax(&d).map_err(kernel)? as u64))
            }
            "fill" => {
                arity(2)?;
                let value = scalar_f64(&a[0])? as f32;
                let dims = dims_of(&a[1])?;
                let n = dims
                    .iter()
                    .try_fold(1usize, |acc, d| acc.checked_mul(*d))
                    .filter(|n| *n <= self.options.max_tensor_elements)
                    .ok_or_else(|| EvalError::Limit("tensor too large".into()))?;
                self.add(HostTensor::f32(dims, vec![value; n]))
            }
            "rms_norm" => {
                arity(3)?;
                let (s, d) = self.f32s(&a[0], op)?;
                let (_, w) = self.f32s(&a[1], op)?;
                let eps = scalar_f64(&a[2])? as f32;
                let mut out = vec![0.0; d.len()];
                vesper_tensor::ops::rms_norm(&mut out, &d, &w, eps).map_err(kernel)?;
                self.add(HostTensor::f32(s, out))
            }
            "rope" => {
                arity(2)?;
                let (s, mut d) = self.f32s(&a[0], op)?;
                let position = scalar_u64(&a[1])? as usize;
                let [_, head_dim] = s[..] else {
                    return Err(shape_err("expected [Heads, HeadDim]".into()));
                };
                vesper_tensor::ops::rope(&mut d, head_dim, position, ROPE_BASE).map_err(kernel)?;
                self.add(HostTensor::f32(s, d))
            }
            "qmatvec" => {
                arity(2)?;
                let w = self.get(&a[0])?.clone();
                let HostData::Quant { block, bytes } = &w.data else {
                    return Err(shape_err("expected quantized weights".into()));
                };
                let format = self.format(block)?;
                let [rows, cols] = w.dims[..] else {
                    return Err(shape_err("expected rank-2 weights".into()));
                };
                let (_, x) = self.f32s(&a[1], op)?;
                let m = Matrix::quantized(format.weight_format(), rows, cols, bytes.clone())
                    .map_err(kernel)?;
                let mut out = vec![0.0; rows];
                m.matvec(&x, &mut out).map_err(kernel)?;
                self.add(HostTensor::f32(vec![rows], out))
            }
            "block_scale" => {
                arity(3)?;
                let w = self.get(&a[0])?;
                let HostData::Quant { block, bytes } = &w.data else {
                    return Err(shape_err("expected quantized weights".into()));
                };
                let wire = &self.analysis.wires[block];
                let size = wire.layout.size();
                let [rows, cols] = w.dims[..] else {
                    return Err(shape_err("expected rank-2 weights".into()));
                };
                let (row, b) = (scalar_u64(&a[1])? as usize, scalar_u64(&a[2])? as usize);
                let per_row = cols / vesper_tensor::quant::BLOCK;
                if row >= rows || b >= per_row {
                    return Err(shape_err(format!("block ({row}, {b}) is out of range")));
                }
                let offset = (row * per_row + b) * size;
                let view = vesper_ffi::view_wire(&wire.layout, &bytes[offset..offset + size])
                    .map_err(kernel)?;
                let scale_field = wire.layout.fields()[0].name().to_string();
                let bits = view.read_u64(&[scale_field.as_str()]).map_err(kernel)?;
                Ok(Value::Float(f64::from(vesper_tensor::host::decode_fp16(
                    bits as u16,
                ))))
            }
            other => Err(EvalError::Runtime(format!("unknown Tensor::{other}"))),
        }
    }
    fn pool(&mut self) -> Result<&mut KvPool, EvalError> {
        self.pool.as_mut().ok_or_else(|| {
            EvalError::Entry("the entry's row has no KvCachePool[KvHeads, HeadDim]".into())
        })
    }
    fn owner(&self, v: &Value, ty: &str, field: &str) -> Result<u64, EvalError> {
        field_u64(v, ty, field)
    }
    fn step(&self, v: &Value) -> Result<Vec<u8>, EvalError> {
        let t = self.get(v)?;
        let HostData::Bf16(bits) = &t.data else {
            return Err(EvalError::Shape("K/V steps are Bf16 tensors".into()));
        };
        Ok(bits.iter().flat_map(|b| b.to_le_bytes()).collect())
    }
    fn append(&mut self, id: u64, k: &Value, v: &Value, page: bool) -> Result<Value, EvalError> {
        let (kb, vb) = (self.step(k)?, self.step(v)?);
        let mut sequence = self
            .sequences
            .remove(&id)
            .ok_or_else(|| EvalError::Runtime("stale KV owner".into()))?;
        let pool = self.pool()?;
        let config = pool.config();
        let len = pool.len(&sequence).map_err(kernel)?;
        let result = if page && len >= TOKENS_PER_PAGE {
            Ok(err("PageFull"))
        } else {
            let step = |bytes| Bf16Step {
                heads: config.heads,
                head_dim: config.head_dim,
                bytes,
            };
            match pool.append(&mut sequence, len, step(&kb), step(&vb)) {
                Ok(()) => Ok(ok(Value::Tuple(vec![]))),
                Err(e) => kv_error(e),
            }
        };
        self.sequences.insert(id, sequence);
        result
    }
    fn begin(&mut self) -> Result<Result<u64, Value>, EvalError> {
        let pool = self.pool()?;
        match pool.begin() {
            Ok(s) => {
                let id = self.next_owner;
                self.next_owner += 1;
                self.sequences.insert(id, s);
                Ok(Ok(id))
            }
            Err(e) => Ok(Err(kv_error(e)?)),
        }
    }
    fn release(&mut self, id: u64) -> Result<Value, EvalError> {
        let s = self
            .sequences
            .remove(&id)
            .ok_or_else(|| EvalError::Runtime("KV owner released twice".into()))?;
        self.pool()?
            .release(s)
            .map_err(|f| EvalError::Kernel(format!("KV release: {}", f.error)))?;
        Ok(Value::Tuple(vec![]))
    }
    fn tokens(&mut self, id: u64) -> Result<Value, EvalError> {
        let s = self
            .sequences
            .get(&id)
            .ok_or_else(|| EvalError::Runtime("stale KV owner".into()))?;
        let pool = self
            .pool
            .as_ref()
            .ok_or_else(|| EvalError::Entry("no pool".into()))?;
        Ok(uint(pool.len(s).map_err(kernel)? as u64))
    }
    fn pool_op(&mut self, op: &str, a: &[Value]) -> Result<Value, EvalError> {
        match op {
            "begin_sequence" => Ok(match self.begin()? {
                Ok(id) => {
                    self.stats.sequences_begun += 1;
                    ok(data(ACTIVE_SEQ, ACTIVE_SEQ, vec![("seq", uint(id))]))
                }
                Err(e) => e,
            }),
            "lease_page" => Ok(match self.begin()? {
                Ok(id) => {
                    self.stats.pages_leased += 1;
                    ok(data(LEASE, LEASE, vec![("lease", uint(id))]))
                }
                Err(e) => e,
            }),
            "append_kv_step" => {
                let id = self.owner(&a[0], ACTIVE_SEQ, "seq")?;
                self.append(id, &a[1], &a[2], false)
            }
            "write_page" => {
                let id = self.owner(&a[0], LEASE, "lease")?;
                self.append(id, &a[1], &a[2], true)
            }
            "tokens" => {
                let id = self.owner(&a[0], ACTIVE_SEQ, "seq")?;
                self.tokens(id)
            }
            "page_tokens" => {
                let id = self.owner(&a[0], LEASE, "lease")?;
                self.tokens(id)
            }
            "release_sequence" => {
                let id = self.owner(&a[0], ACTIVE_SEQ, "seq")?;
                self.stats.sequences_released += 1;
                self.release(id)
            }
            "release_page" => {
                let id = self.owner(&a[0], LEASE, "lease")?;
                self.stats.pages_released += 1;
                self.release(id)
            }
            "attend" => {
                let id = self.owner(&a[0], ACTIVE_SEQ, "seq")?;
                let (s, q) = self.f32s(&a[1], op)?;
                let [query_heads, head_dim] = s[..] else {
                    return Err(EvalError::Shape("queries are [QHeads, HeadDim]".into()));
                };
                let pool = self
                    .pool
                    .as_ref()
                    .ok_or_else(|| EvalError::Entry("no pool".into()))?;
                let sequence = self
                    .sequences
                    .get(&id)
                    .ok_or_else(|| EvalError::Runtime("stale KV owner".into()))?;
                let tokens = pool.len(sequence).map_err(kernel)?;
                let shape = AttentionShape {
                    query_heads,
                    kv_heads: pool.config().heads,
                    head_dim,
                };
                let mut scores = vec![0.0; query_heads * tokens.max(1)];
                let mut out = vec![0.0; query_heads * head_dim];
                paged_gqa_attention(
                    pool,
                    sequence,
                    shape,
                    &q,
                    &mut scores,
                    &mut out,
                    AttentionMode::TwoPass,
                )
                .map_err(kernel)?;
                self.add(HostTensor::f32(s, out))
            }
            other => Err(EvalError::Runtime(format!("unknown KvCachePool::{other}"))),
        }
    }
}

/// RoPE frequency base used by `Tensor::rope` (the llama2.c value).
pub const ROPE_BASE: f32 = 10000.0;

fn dims_of(v: &Value) -> Result<Vec<usize>, EvalError> {
    let rank = field_u64(v, DIMS, "rank")? as usize;
    if rank > super::MAX_RANK {
        return Err(EvalError::Shape("rank exceeds the descriptor".into()));
    }
    (0..rank)
        .map(|i| {
            let d = field_u64(v, DIMS, &format!("d{i}"))?;
            usize::try_from(d)
                .ok()
                .filter(|d| *d > 0 && (*d as u64) <= super::dim::MAX_DIM)
                .ok_or_else(|| EvalError::Shape(format!("dimension {d} is out of range")))
        })
        .collect()
}

/// Right-aligned broadcasting over concrete shapes.
/// The right-aligned broadcast of two shapes.
pub fn broadcast_shape(sa: &[usize], sb: &[usize]) -> Result<Vec<usize>, String> {
    let n = sa.len().max(sb.len());
    let at = |s: &[usize], i: usize| (i + s.len()).checked_sub(n).map_or(1, |j| s[j]);
    (0..n)
        .map(|i| match (at(sa, i), at(sb, i)) {
            (x, y) if x == y || y == 1 => Ok(x),
            (1, y) => Ok(y),
            _ => Err(format!("{sa:?} and {sb:?} do not broadcast")),
        })
        .collect()
}

pub fn broadcast_apply(
    sa: &[usize],
    da: &[f32],
    sb: &[usize],
    db: &[f32],
    f: impl Fn(f32, f32) -> f32,
) -> Result<(Vec<usize>, Vec<f32>), String> {
    let shape = broadcast_shape(sa, sb)?;
    let n = shape.len();
    let pad = |s: &[usize]| {
        let mut v = vec![1; n - s.len()];
        v.extend_from_slice(s);
        v
    };
    let (pa, pb) = (pad(sa), pad(sb));
    let total: usize = shape.iter().product();
    let strides = |p: &[usize]| {
        let mut s = vec![0; n];
        let mut acc = 1;
        for i in (0..n).rev() {
            s[i] = if p[i] == 1 { 0 } else { acc };
            acc *= p[i];
        }
        s
    };
    let (ta, tb) = (strides(&pa), strides(&pb));
    let mut out = Vec::with_capacity(total);
    let mut index = vec![0usize; n];
    for _ in 0..total {
        let ia: usize = index.iter().zip(&ta).map(|(i, s)| i * s).sum();
        let ib: usize = index.iter().zip(&tb).map(|(i, s)| i * s).sum();
        out.push(f(da[ia], db[ib]));
        for i in (0..n).rev() {
            index[i] += 1;
            if index[i] < shape[i] {
                break;
            }
            index[i] = 0;
        }
    }
    Ok((shape, out))
}
