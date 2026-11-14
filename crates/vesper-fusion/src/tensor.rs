//! Tensor graph fusion (design §28.1 item 4 and §28.5).
//!
//! A [`TensorGraph`] is a DAG of vector operations over named inputs and
//! declared weight matrices. [`TensorGraph::evaluate`] is the unfused
//! reference: every node writes a whole intermediate vector, matrix-vector
//! products and RMSNorm go through `vesper-tensor`, and a quantized matrix is
//! dequantized into a full FP32 matrix before its product.
//! [`TensorGraph::fuse`] saturates the graph with [`RULES`] and extracts the
//! cheapest equivalent term, which [`FusedTensor::evaluate`] runs: a `Fuse`
//! node is one pass that computes its whole elementwise body per output lane,
//! with a matrix row dot product as a possible leaf.
//!
//! Exactness: every rule except `rmsnorm-prologue` keeps each lane's sequence
//! of FP32 (or checked `i32`) operations, so fused and unfused results are
//! bit-identical. `rmsnorm-prologue` factors the inverse RMS out of the dot
//! product, `inv * sum(W[c] * (w[c] * x[c]))` instead of
//! `sum(W[c] * (w[c] * (inv * x[c])))`; see [`rmsnorm_tolerance`].
use crate::engine::{self, Extracted};
use crate::{Error, MAX_ELEMENTS, MAX_NODES, next_owner, valid_name};
use std::collections::HashMap;
use std::fmt::Write as _;
use vesper_tensor::quant::{Matrix, WeightFormat};

/// The tensor rule set. `:cost` is the cost model: a node that writes a
/// whole intermediate vector costs 16 (one memory round trip), a materialized
/// dequantized matrix 64, and work inside a fused lane 1-2.
pub const RULES: &str = r#"
(sort M)
(sort T)
(sort E)
(constructor Mat (String) M)
(constructor Deq (String) M :cost 64)
(constructor QView (String) M)
(constructor In (String) T)
(constructor MatVec (M T) T :cost 16)
(constructor RmsNorm (T T i64) T :cost 16)
(constructor RmsMatVec (M T T i64) T :cost 14)
(constructor Act (i64 T) T :cost 16)
(constructor Add (T T) T :cost 16)
(constructor Mul (T T) T :cost 16)
(constructor Scale (i64 T) T :cost 16)
(constructor Fuse (E) T :cost 16)
(constructor Lane (T) E)
(constructor Dot (M T) E :cost 2)
(constructor EAct (i64 E) E)
(constructor EAdd (E E) E)
(constructor EMul (E E) E)
(constructor EScale (i64 E) E)
; ew-act / ew-add / ew-mul / ew-scale: an elementwise node is a one-op kernel.
(rewrite (Act a x) (Fuse (EAct a (Lane x))))
(rewrite (Add x y) (Fuse (EAdd (Lane x) (Lane y))))
(rewrite (Mul x y) (Fuse (EMul (Lane x) (Lane y))))
(rewrite (Scale c x) (Fuse (EScale c (Lane x))))
; ew-inline: reading lane i of a fused kernel is computing its body at i.
(rewrite (Lane (Fuse e)) e)
; matvec-epilogue: lane i of a matvec is the dot product of row i.
(rewrite (Lane (MatVec m x)) (Dot m x))
; dequant-into-matmul: every consumer of a matrix may dequantize it in place.
(rewrite (Deq q) (QView q))
; rmsnorm-prologue: the inverse RMS factors out of each row dot product.
(rewrite (MatVec m (RmsNorm x w e)) (RmsMatVec m x w e))
"#;

/// Elementwise activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// `max(v, 0)` (FP32 and `i32`).
    Relu,
    /// `v * sigmoid(v)` via `vesper_tensor::ops::silu` (FP32 only).
    Silu,
}

impl Activation {
    fn code(self) -> i64 {
        match self {
            Activation::Relu => 0,
            Activation::Silu => 1,
        }
    }
    fn from_code(code: i64) -> Result<Self, Error> {
        match code {
            0 => Ok(Activation::Relu),
            1 => Ok(Activation::Silu),
            _ => Err(Error::Extraction(format!("unknown activation {code}"))),
        }
    }
}

/// Element type of a vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    /// IEEE binary32, evaluated in program order without `mul_add`.
    F32,
    /// Checked two's-complement `i32`; overflow is [`Error::Overflow`].
    I32,
}

/// A vector value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// FP32 lanes.
    F32(Vec<f32>),
    /// `i32` lanes.
    I32(Vec<i32>),
}

impl Value {
    fn len(&self) -> usize {
        match self {
            Value::F32(v) => v.len(),
            Value::I32(v) => v.len(),
        }
    }
    fn f32(&self) -> Result<&[f32], Error> {
        match self {
            Value::F32(v) => Ok(v),
            Value::I32(_) => Err(Error::Shape("expected an FP32 vector")),
        }
    }
    fn lane(&self, i: usize) -> Lane {
        match self {
            Value::F32(v) => Lane::F(v[i]),
            Value::I32(v) => Lane::I(v[i]),
        }
    }
}

/// A node handle of one [`TensorGraph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tensor {
    owner: u64,
    index: u32,
}

/// A declared weight matrix of one [`TensorGraph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MatrixRef {
    owner: u64,
    index: u32,
}

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Input(String),
    MatVec(usize, usize),
    RmsNorm(usize, usize, usize),
    Act(Activation, usize),
    Add(usize, usize),
    Mul(usize, usize),
    Scale(usize, usize),
}

#[derive(Clone, Debug, PartialEq)]
struct NodeInfo {
    op: Op,
    dtype: DType,
    len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatrixDecl {
    name: String,
    rows: usize,
    cols: usize,
    format: WeightFormat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scalar {
    F32(f32),
    I32(i32),
}

/// Shared declarations a fused term is evaluated against.
#[derive(Clone, Debug, PartialEq)]
struct Decls {
    inputs: HashMap<String, (DType, usize)>,
    matrices: HashMap<String, MatrixDecl>,
    params: Vec<Scalar>,
}

/// Named input vectors and weight matrices.
#[derive(Clone, Debug, Default)]
pub struct TensorEnv {
    vectors: HashMap<String, Value>,
    matrices: HashMap<String, Matrix>,
}

impl TensorEnv {
    /// An empty environment.
    pub fn new() -> Self {
        Self::default()
    }
    /// Binds an FP32 input vector.
    pub fn vector_f32(&mut self, name: &str, data: Vec<f32>) -> &mut Self {
        self.vectors.insert(name.to_owned(), Value::F32(data));
        self
    }
    /// Binds an `i32` input vector.
    pub fn vector_i32(&mut self, name: &str, data: Vec<i32>) -> &mut Self {
        self.vectors.insert(name.to_owned(), Value::I32(data));
        self
    }
    /// Binds a weight matrix (FP32, Q8_0 or Q4_0).
    pub fn matrix(&mut self, name: &str, matrix: Matrix) -> &mut Self {
        self.matrices.insert(name.to_owned(), matrix);
        self
    }
}

/// A tensor dataflow graph.
#[derive(Clone, Debug)]
pub struct TensorGraph {
    owner: u64,
    nodes: Vec<NodeInfo>,
    matrices: Vec<MatrixDecl>,
    decls: Decls,
}

impl Default for TensorGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl TensorGraph {
    /// An empty graph.
    pub fn new() -> Self {
        Self {
            owner: next_owner(),
            nodes: Vec::new(),
            matrices: Vec::new(),
            decls: Decls {
                inputs: HashMap::new(),
                matrices: HashMap::new(),
                params: Vec::new(),
            },
        }
    }

    fn node(&self, t: Tensor) -> Result<&NodeInfo, Error> {
        if t.owner != self.owner {
            return Err(Error::InvalidNode);
        }
        self.nodes.get(t.index as usize).ok_or(Error::InvalidNode)
    }

    fn push(&mut self, op: Op, dtype: DType, len: usize) -> Result<Tensor, Error> {
        if self.nodes.len() >= MAX_NODES {
            return Err(Error::Limit("graph nodes"));
        }
        self.nodes.push(NodeInfo { op, dtype, len });
        Ok(Tensor {
            owner: self.owner,
            index: (self.nodes.len() - 1) as u32,
        })
    }

    fn param(&mut self, s: Scalar) -> Result<usize, Error> {
        if self.decls.params.len() >= MAX_NODES {
            return Err(Error::Limit("graph parameters"));
        }
        self.decls.params.push(s);
        Ok(self.decls.params.len() - 1)
    }

    /// Declares a named input vector.
    pub fn input(&mut self, name: &str, dtype: DType, len: usize) -> Result<Tensor, Error> {
        valid_name(name)?;
        if len == 0 || len > MAX_ELEMENTS {
            return Err(Error::Limit("vector length"));
        }
        if self.decls.inputs.contains_key(name) {
            return Err(Error::InvalidName(name.to_owned()));
        }
        self.decls.inputs.insert(name.to_owned(), (dtype, len));
        self.push(Op::Input(name.to_owned()), dtype, len)
    }

    /// Declares a named `[rows, cols]` weight matrix.
    pub fn matrix(
        &mut self,
        name: &str,
        rows: usize,
        cols: usize,
        format: WeightFormat,
    ) -> Result<MatrixRef, Error> {
        valid_name(name)?;
        if rows == 0 || cols == 0 || rows.checked_mul(cols).is_none_or(|n| n > MAX_ELEMENTS) {
            return Err(Error::Limit("matrix size"));
        }
        if format != WeightFormat::F32 && !cols.is_multiple_of(vesper_tensor::quant::BLOCK) {
            return Err(Error::Shape(
                "quantized matrix needs whole 32-element row blocks",
            ));
        }
        if self.decls.matrices.contains_key(name) || self.matrices.len() >= MAX_NODES {
            return Err(Error::InvalidName(name.to_owned()));
        }
        let decl = MatrixDecl {
            name: name.to_owned(),
            rows,
            cols,
            format,
        };
        self.decls.matrices.insert(name.to_owned(), decl.clone());
        self.matrices.push(decl);
        Ok(MatrixRef {
            owner: self.owner,
            index: (self.matrices.len() - 1) as u32,
        })
    }

    /// `out[r] = sum_c W[r, c] * x[c]` (FP32 activations).
    pub fn matvec(&mut self, m: MatrixRef, x: Tensor) -> Result<Tensor, Error> {
        if m.owner != self.owner {
            return Err(Error::InvalidNode);
        }
        let decl = self
            .matrices
            .get(m.index as usize)
            .ok_or(Error::InvalidNode)?
            .clone();
        let xi = self.node(x)?;
        if xi.dtype != DType::F32 {
            return Err(Error::Unsupported("matvec over i32 activations"));
        }
        if xi.len != decl.cols {
            return Err(Error::Shape("matvec inner dimension"));
        }
        self.push(
            Op::MatVec(m.index as usize, x.index as usize),
            DType::F32,
            decl.rows,
        )
    }

    /// `vesper_tensor::ops::rms_norm(x, weight, epsilon)`.
    pub fn rms_norm(&mut self, x: Tensor, weight: Tensor, epsilon: f32) -> Result<Tensor, Error> {
        let (xi, wi) = (self.node(x)?.clone(), self.node(weight)?.clone());
        if xi.dtype != DType::F32 || wi.dtype != DType::F32 {
            return Err(Error::Unsupported("rms_norm over i32"));
        }
        if xi.len != wi.len {
            return Err(Error::Shape("rms_norm weight length"));
        }
        if !(epsilon.is_finite() && epsilon > 0.0) {
            return Err(Error::Parameter("rms_norm epsilon"));
        }
        let e = self.param(Scalar::F32(epsilon))?;
        self.push(
            Op::RmsNorm(x.index as usize, weight.index as usize, e),
            DType::F32,
            xi.len,
        )
    }

    fn binary(&mut self, a: Tensor, b: Tensor, mul: bool) -> Result<Tensor, Error> {
        let (ai, bi) = (self.node(a)?.clone(), self.node(b)?.clone());
        if ai.dtype != bi.dtype {
            return Err(Error::Shape("elementwise operand types"));
        }
        if ai.len != bi.len {
            return Err(Error::Shape("elementwise operand lengths"));
        }
        let (a, b) = (a.index as usize, b.index as usize);
        self.push(
            if mul { Op::Mul(a, b) } else { Op::Add(a, b) },
            ai.dtype,
            ai.len,
        )
    }

    /// Elementwise `a + b`.
    pub fn add(&mut self, a: Tensor, b: Tensor) -> Result<Tensor, Error> {
        self.binary(a, b, false)
    }

    /// Elementwise `a * b`.
    pub fn mul(&mut self, a: Tensor, b: Tensor) -> Result<Tensor, Error> {
        self.binary(a, b, true)
    }

    /// Elementwise `c * x` for an FP32 vector.
    pub fn scale_f32(&mut self, c: f32, x: Tensor) -> Result<Tensor, Error> {
        let xi = self.node(x)?.clone();
        if xi.dtype != DType::F32 {
            return Err(Error::Shape("scale_f32 of an i32 vector"));
        }
        if !c.is_finite() {
            return Err(Error::NonFinite);
        }
        let p = self.param(Scalar::F32(c))?;
        self.push(Op::Scale(p, x.index as usize), DType::F32, xi.len)
    }

    /// Elementwise checked `c * x` for an `i32` vector.
    pub fn scale_i32(&mut self, c: i32, x: Tensor) -> Result<Tensor, Error> {
        let xi = self.node(x)?.clone();
        if xi.dtype != DType::I32 {
            return Err(Error::Shape("scale_i32 of an FP32 vector"));
        }
        let p = self.param(Scalar::I32(c))?;
        self.push(Op::Scale(p, x.index as usize), DType::I32, xi.len)
    }

    /// Elementwise activation.
    pub fn activation(&mut self, act: Activation, x: Tensor) -> Result<Tensor, Error> {
        let xi = self.node(x)?.clone();
        if act == Activation::Silu && xi.dtype == DType::I32 {
            return Err(Error::Unsupported("silu over i32"));
        }
        self.push(Op::Act(act, x.index as usize), xi.dtype, xi.len)
    }

    /// Element type and length of `t`.
    pub fn shape(&self, t: Tensor) -> Result<(DType, usize), Error> {
        let n = self.node(t)?;
        Ok((n.dtype, n.len))
    }

    fn reachable(&self, root: usize) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            match self.nodes[i].op {
                Op::Input(_) => {}
                Op::MatVec(_, x) | Op::Act(_, x) | Op::Scale(_, x) => stack.push(x),
                Op::Add(a, b) | Op::Mul(a, b) | Op::RmsNorm(a, b, _) => stack.extend([a, b]),
            }
        }
        seen
    }

    fn bindings(&self, root: usize) -> String {
        let live = self.reachable(root);
        let mut out = String::new();
        // Node operands always precede the node, so index order is a valid
        // definition order.
        for (i, n) in self.nodes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            let body = match &n.op {
                Op::Input(name) => format!("(In \"{name}\")"),
                Op::MatVec(m, x) => {
                    let d = &self.matrices[*m];
                    let head = if d.format == WeightFormat::F32 {
                        "Mat"
                    } else {
                        "Deq"
                    };
                    format!("(MatVec ({head} \"{}\") t{x})", d.name)
                }
                Op::RmsNorm(x, w, e) => format!("(RmsNorm t{x} t{w} {e})"),
                Op::Act(a, x) => format!("(Act {} t{x})", a.code()),
                Op::Add(a, b) => format!("(Add t{a} t{b})"),
                Op::Mul(a, b) => format!("(Mul t{a} t{b})"),
                Op::Scale(p, x) => format!("(Scale {p} t{x})"),
            };
            let _ = writeln!(out, "(let t{i} {body})");
        }
        out
    }

    /// Kernel count of the unfused graph rooted at `root`: every reachable
    /// non-input node writes one intermediate vector.
    pub fn kernels(&self, root: Tensor) -> Result<usize, Error> {
        self.node(root)?;
        let live = self.reachable(root.index as usize);
        Ok(self
            .nodes
            .iter()
            .zip(&live)
            .filter(|(n, live)| **live && !matches!(n.op, Op::Input(_)))
            .count())
    }

    /// Saturates the graph rooted at `root` with [`RULES`] and extracts the
    /// cheapest equivalent term.
    pub fn fuse(&self, root: Tensor) -> Result<FusedTensor, Error> {
        self.node(root)?;
        let r = root.index as usize;
        let bindings = self.bindings(r);
        let before = engine::run(RULES, &bindings, &format!("t{r}"), false)?;
        let term = engine::run(RULES, &bindings, &format!("t{r}"), true)?;
        let fused = FusedTensor {
            decls: self.decls.clone(),
            unfused_cost: before.cost,
            unfused_kernels: self.kernels(root)?,
            term,
        };
        // Reject a term outside the grammar now rather than at evaluation.
        fused.lengths()?;
        Ok(fused)
    }

    /// Unfused reference evaluation of `root`.
    pub fn evaluate(&self, root: Tensor, env: &TensorEnv) -> Result<Value, Error> {
        self.node(root)?;
        let live = self.reachable(root.index as usize);
        let mut values: Vec<Option<Value>> = vec![None; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            let get = |j: usize| values[j].as_ref().expect("operands precede nodes");
            let v = match &n.op {
                Op::Input(name) => input(&self.decls, env, name)?,
                Op::MatVec(m, x) => {
                    let matrix = matrix(&self.decls, env, &self.matrices[*m].name)?;
                    let dense = materialize(matrix)?;
                    let mut out = vec![0.0; dense.rows()];
                    dense.matvec(get(*x).f32()?, &mut out)?;
                    finite(Value::F32(out))?
                }
                Op::RmsNorm(x, w, e) => {
                    let x = get(*x).f32()?;
                    let mut out = vec![0.0; x.len()];
                    vesper_tensor::ops::rms_norm(
                        &mut out,
                        x,
                        get(*w).f32()?,
                        f32_param(&self.decls, *e)?,
                    )?;
                    finite(Value::F32(out))?
                }
                Op::Act(a, x) => map1(get(*x), |l| act(*a, l))?,
                Op::Scale(p, x) => {
                    let c = self.decls.params[*p];
                    map1(get(*x), |l| scale(c, l))?
                }
                Op::Add(a, b) => map2(get(*a), get(*b), add)?,
                Op::Mul(a, b) => map2(get(*a), get(*b), mul)?,
            };
            values[i] = Some(v);
        }
        Ok(values[root.index as usize].take().expect("root evaluated"))
    }
}

fn input(decls: &Decls, env: &TensorEnv, name: &str) -> Result<Value, Error> {
    let &(dtype, len) = decls
        .inputs
        .get(name)
        .ok_or_else(|| Error::Extraction(format!("undeclared input {name}")))?;
    let v = env
        .vectors
        .get(name)
        .ok_or_else(|| Error::Missing(name.to_owned()))?;
    match (dtype, v) {
        (DType::F32, Value::F32(data)) if data.len() == len => {
            if data.iter().any(|x| !x.is_finite()) {
                return Err(Error::NonFinite);
            }
        }
        (DType::I32, Value::I32(data)) if data.len() == len => {}
        _ => return Err(Error::Shape("input does not match its declaration")),
    }
    Ok(v.clone())
}

fn matrix<'a>(decls: &Decls, env: &'a TensorEnv, name: &str) -> Result<&'a Matrix, Error> {
    let d = decls
        .matrices
        .get(name)
        .ok_or_else(|| Error::Extraction(format!("undeclared matrix {name}")))?;
    let m = env
        .matrices
        .get(name)
        .ok_or_else(|| Error::Missing(name.to_owned()))?;
    if m.rows() != d.rows || m.cols() != d.cols || m.format() != d.format {
        return Err(Error::Shape("matrix does not match its declaration"));
    }
    Ok(m)
}

/// The unfused dequantization step: a whole FP32 copy of the matrix.
fn materialize(m: &Matrix) -> Result<Matrix, Error> {
    if m.format() == WeightFormat::F32 {
        return Ok(m.clone());
    }
    let (rows, cols) = (m.rows(), m.cols());
    let mut data = vec![0.0; rows * cols];
    for (r, row) in data.chunks_exact_mut(cols).enumerate() {
        m.dequantize_row(r, row)?;
    }
    Ok(Matrix::f32(rows, cols, data)?)
}

fn f32_param(decls: &Decls, p: usize) -> Result<f32, Error> {
    match decls.params.get(p) {
        Some(Scalar::F32(v)) => Ok(*v),
        _ => Err(Error::Extraction(format!("parameter {p} is not FP32"))),
    }
}

fn finite(v: Value) -> Result<Value, Error> {
    if let Value::F32(data) = &v
        && data.iter().any(|x| !x.is_finite())
    {
        return Err(Error::NonFinite);
    }
    Ok(v)
}

/// One lane of a vector.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Lane {
    F(f32),
    I(i32),
}

fn lane_f(v: f32) -> Result<Lane, Error> {
    if v.is_finite() {
        Ok(Lane::F(v))
    } else {
        Err(Error::NonFinite)
    }
}

// The scalar semantics shared by both evaluators: fusion changes which pass
// computes a lane, never the operation applied to it.
fn act(a: Activation, l: Lane) -> Result<Lane, Error> {
    match (a, l) {
        (Activation::Relu, Lane::F(v)) => Ok(Lane::F(if v > 0.0 { v } else { 0.0 })),
        (Activation::Relu, Lane::I(v)) => Ok(Lane::I(v.max(0))),
        (Activation::Silu, Lane::F(v)) => lane_f(vesper_tensor::ops::silu(v)),
        (Activation::Silu, Lane::I(_)) => Err(Error::Unsupported("silu over i32")),
    }
}
fn scale(c: Scalar, l: Lane) -> Result<Lane, Error> {
    match (c, l) {
        (Scalar::F32(c), Lane::F(v)) => lane_f(c * v),
        (Scalar::I32(c), Lane::I(v)) => c.checked_mul(v).map(Lane::I).ok_or(Error::Overflow),
        _ => Err(Error::Shape("scale parameter type")),
    }
}
fn add(a: Lane, b: Lane) -> Result<Lane, Error> {
    match (a, b) {
        (Lane::F(a), Lane::F(b)) => lane_f(a + b),
        (Lane::I(a), Lane::I(b)) => a.checked_add(b).map(Lane::I).ok_or(Error::Overflow),
        _ => Err(Error::Shape("elementwise operand types")),
    }
}
fn mul(a: Lane, b: Lane) -> Result<Lane, Error> {
    match (a, b) {
        (Lane::F(a), Lane::F(b)) => lane_f(a * b),
        (Lane::I(a), Lane::I(b)) => a.checked_mul(b).map(Lane::I).ok_or(Error::Overflow),
        _ => Err(Error::Shape("elementwise operand types")),
    }
}

fn collect(
    lanes: impl Iterator<Item = Result<Lane, Error>>,
    dtype_of: &Value,
) -> Result<Value, Error> {
    match dtype_of {
        Value::F32(_) => lanes
            .map(|l| match l? {
                Lane::F(v) => Ok(v),
                Lane::I(_) => Err(Error::Shape("lane type")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::F32),
        Value::I32(_) => lanes
            .map(|l| match l? {
                Lane::I(v) => Ok(v),
                Lane::F(_) => Err(Error::Shape("lane type")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::I32),
    }
}
fn map1(x: &Value, f: impl Fn(Lane) -> Result<Lane, Error>) -> Result<Value, Error> {
    collect((0..x.len()).map(|i| f(x.lane(i))), x)
}
fn map2(
    a: &Value,
    b: &Value,
    f: impl Fn(Lane, Lane) -> Result<Lane, Error>,
) -> Result<Value, Error> {
    if a.len() != b.len() {
        return Err(Error::Shape("elementwise operand lengths"));
    }
    collect((0..a.len()).map(|i| f(a.lane(i), b.lane(i))), a)
}

/// Fusion statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// egglog cost of the input graph.
    pub cost_before: u64,
    /// egglog cost of the extracted term.
    pub cost_after: u64,
    /// Passes that write a vector in the unfused graph.
    pub kernels_before: usize,
    /// Passes that write a vector in the fused term.
    pub kernels_after: usize,
    /// Dequantized FP32 matrices the fused term still materializes.
    pub materialized_dequant: usize,
}

/// The extracted fused term of a [`TensorGraph`].
#[derive(Clone, Debug)]
pub struct FusedTensor {
    decls: Decls,
    unfused_cost: u64,
    unfused_kernels: usize,
    term: Extracted,
}

impl FusedTensor {
    /// The fused term as an s-expression.
    pub fn term(&self) -> &str {
        &self.term.text
    }

    /// Number of `name` constructor nodes in the fused term.
    pub fn count(&self, name: &str) -> usize {
        self.term.count(name)
    }

    /// Fusion statistics.
    pub fn report(&self) -> Report {
        const PASSES: [&str; 8] = [
            "MatVec",
            "RmsNorm",
            "RmsMatVec",
            "Act",
            "Add",
            "Mul",
            "Scale",
            "Fuse",
        ];
        Report {
            cost_before: self.unfused_cost,
            cost_after: self.term.cost,
            kernels_before: self.unfused_kernels,
            kernels_after: PASSES.iter().map(|p| self.term.count(p)).sum(),
            materialized_dequant: self.term.count("Deq"),
        }
    }

    /// Length of every `T`/`E` node (and checks the grammar).
    fn lengths(&self) -> Result<Vec<Option<usize>>, Error> {
        let t = &self.term;
        let mut len: Vec<Option<usize>> = vec![None; t.nodes.len()];
        let rows = |m: usize| -> Result<(usize, usize), Error> {
            let (head, kids) = t.app(m)?;
            if !matches!(head, "Mat" | "Deq" | "QView") || kids.len() != 1 {
                return Err(Error::Extraction(format!("not a matrix: {head}")));
            }
            let d = self
                .decls
                .matrices
                .get(t.str(kids[0])?)
                .ok_or_else(|| Error::Extraction("undeclared matrix".into()))?;
            Ok((d.rows, d.cols))
        };
        // Children precede parents in the arena.
        for id in 0..t.nodes.len() {
            let crate::engine::Node::App(head, kids) = &t.nodes[id] else {
                continue;
            };
            let k = |i: usize| -> Result<usize, Error> {
                kids.get(i)
                    .and_then(|&c| len[c])
                    .ok_or_else(|| Error::Extraction(format!("malformed {head}")))
            };
            let same = |a: usize, b: usize| -> Result<usize, Error> {
                if a == b {
                    Ok(a)
                } else {
                    Err(Error::Shape("fused operand lengths"))
                }
            };
            len[id] = match head.as_str() {
                "Mat" | "Deq" | "QView" => None,
                "In" => Some(
                    self.decls
                        .inputs
                        .get(t.str(kids[0])?)
                        .ok_or_else(|| Error::Extraction("undeclared input".into()))?
                        .1,
                ),
                "MatVec" | "Dot" => {
                    let (r, c) = rows(kids[0])?;
                    same(k(1)?, c)?;
                    Some(r)
                }
                "RmsMatVec" => {
                    let (r, c) = rows(kids[0])?;
                    same(same(k(1)?, k(2)?)?, c)?;
                    Some(r)
                }
                "RmsNorm" => Some(same(k(0)?, k(1)?)?),
                "Act" | "Scale" | "EAct" | "EScale" => Some(k(1)?),
                "Add" | "Mul" | "EAdd" | "EMul" => Some(same(k(0)?, k(1)?)?),
                "Fuse" | "Lane" => Some(k(0)?),
                other => return Err(Error::Extraction(format!("unknown constructor {other}"))),
            };
        }
        if len[t.root].is_none() {
            return Err(Error::Extraction("root is not a tensor".into()));
        }
        Ok(len)
    }

    /// Evaluates the fused term.
    pub fn evaluate(&self, env: &TensorEnv) -> Result<Value, Error> {
        let len = self.lengths()?;
        let mut eval = FusedEval {
            fused: self,
            env,
            values: HashMap::new(),
            len,
            row: Vec::new(),
        };
        eval.tensor(self.term.root)
    }
}

struct FusedEval<'a> {
    fused: &'a FusedTensor,
    env: &'a TensorEnv,
    values: HashMap<usize, Value>,
    len: Vec<Option<usize>>,
    row: Vec<f32>,
}

impl<'a> FusedEval<'a> {
    fn matrix(&self, m: usize) -> Result<(&'a str, &'a Matrix), Error> {
        let (fused, env): (&'a FusedTensor, &'a TensorEnv) = (self.fused, self.env);
        let (head, kids) = fused.term.app(m)?;
        Ok((head, matrix(&fused.decls, env, fused.term.str(kids[0])?)?))
    }

    fn tensor(&mut self, id: usize) -> Result<Value, Error> {
        if let Some(v) = self.values.get(&id) {
            return Ok(v.clone());
        }
        let fused = self.fused;
        let t = &fused.term;
        let (head, kids) = t.app(id)?;
        let v = match head {
            "In" => input(&self.fused.decls, self.env, t.str(kids[0])?)?,
            "MatVec" => {
                let x = self.tensor(kids[1])?;
                let (kind, m) = self.matrix(kids[0])?;
                let mut out = vec![0.0; m.rows()];
                if kind == "Deq" {
                    materialize(m)?.matvec(x.f32()?, &mut out)?;
                } else {
                    // `Mat` or `QView`: dequantization happens inside the
                    // row loop, element by element.
                    m.matvec(x.f32()?, &mut out)?;
                }
                finite(Value::F32(out))?
            }
            "RmsNorm" => {
                let (x, w) = (self.tensor(kids[0])?, self.tensor(kids[1])?);
                let x = x.f32()?;
                let mut out = vec![0.0; x.len()];
                let e = f32_param(&self.fused.decls, param_index(t, kids[2])?)?;
                vesper_tensor::ops::rms_norm(&mut out, x, w.f32()?, e)?;
                finite(Value::F32(out))?
            }
            "RmsMatVec" => {
                let (x, w) = (self.tensor(kids[1])?, self.tensor(kids[2])?);
                let e = f32_param(&self.fused.decls, param_index(t, kids[3])?)?;
                let (_, m) = self.matrix(kids[0])?;
                finite(Value::F32(rms_matvec(
                    m,
                    x.f32()?,
                    w.f32()?,
                    e,
                    &mut self.row,
                )?))?
            }
            "Act" => {
                let a = Activation::from_code(t.int(kids[0])?)?;
                map1(&self.tensor(kids[1])?, |l| act(a, l))?
            }
            "Scale" => {
                let c = self.scalar(kids[0])?;
                map1(&self.tensor(kids[1])?, |l| scale(c, l))?
            }
            "Add" => map2(&self.tensor(kids[0])?, &self.tensor(kids[1])?, add)?,
            "Mul" => map2(&self.tensor(kids[0])?, &self.tensor(kids[1])?, mul)?,
            "Fuse" => {
                let n = self.len[id].ok_or_else(|| Error::Extraction("Fuse length".into()))?;
                self.prepare(kids[0])?;
                let mut lanes = Vec::with_capacity(n);
                for i in 0..n {
                    lanes.push(self.lane(kids[0], i)?);
                }
                match lanes.first() {
                    Some(Lane::I(_)) => {
                        collect(lanes.into_iter().map(Ok), &Value::I32(Vec::new()))?
                    }
                    _ => collect(lanes.into_iter().map(Ok), &Value::F32(Vec::new()))?,
                }
            }
            other => return Err(Error::Extraction(format!("{other} is not a tensor"))),
        };
        self.values.insert(id, v.clone());
        Ok(v)
    }

    fn scalar(&self, id: usize) -> Result<Scalar, Error> {
        let p = param_index(&self.fused.term, id)?;
        self.fused
            .decls
            .params
            .get(p)
            .copied()
            .ok_or_else(|| Error::Extraction(format!("unknown parameter {p}")))
    }

    /// Evaluates every tensor a fused body reads, once, before the lane loop.
    fn prepare(&mut self, e: usize) -> Result<(), Error> {
        let fused = self.fused;
        let t = &fused.term;
        let (head, kids) = t.app(e)?;
        match head {
            "Lane" => self.tensor(kids[0]).map(drop),
            "Dot" => self.tensor(kids[1]).map(drop),
            "EAct" | "EScale" => self.prepare(kids[1]),
            "EAdd" | "EMul" => {
                self.prepare(kids[0])?;
                self.prepare(kids[1])
            }
            other => Err(Error::Extraction(format!(
                "{other} is not a lane expression"
            ))),
        }
    }

    fn lane(&mut self, e: usize, i: usize) -> Result<Lane, Error> {
        let fused = self.fused;
        let t = &fused.term;
        let (head, kids) = t.app(e)?;
        match head {
            "Lane" => Ok(self.values[&kids[0]].lane(i)),
            "Dot" => {
                let (_, m) = self.matrix(kids[0])?;
                let x = &self.values[&kids[1]];
                // One row is dequantized into an L1 scratch row and consumed
                // immediately (`Mat`, `QView`; `Deq` never survives
                // extraction because `QView` is cheaper).
                self.row.resize(m.cols(), 0.0);
                m.dequantize_row(i, &mut self.row)?;
                let mut sum = 0.0f32;
                for (w, x) in self.row.iter().zip(x.f32()?) {
                    sum += w * x;
                }
                lane_f(sum)
            }
            "EAct" => {
                let a = Activation::from_code(t.int(kids[0])?)?;
                act(a, self.lane(kids[1], i)?)
            }
            "EScale" => {
                let c = self.scalar(kids[0])?;
                scale(c, self.lane(kids[1], i)?)
            }
            "EAdd" => add(self.lane(kids[0], i)?, self.lane(kids[1], i)?),
            "EMul" => mul(self.lane(kids[0], i)?, self.lane(kids[1], i)?),
            other => Err(Error::Extraction(format!(
                "{other} is not a lane expression"
            ))),
        }
    }
}

fn param_index(t: &Extracted, id: usize) -> Result<usize, Error> {
    usize::try_from(t.int(id)?).map_err(|_| Error::Extraction("negative parameter".into()))
}

/// The fused RMSNorm-prologue matvec:
/// `out[r] = inv * sum_c W[r, c] * (w[c] * x[c])` with
/// `inv = 1 / sqrt(sum(x^2) / n + eps)` computed exactly as
/// `vesper_tensor::ops::rms_norm` does.
fn rms_matvec(
    m: &Matrix,
    x: &[f32],
    w: &[f32],
    eps: f32,
    row: &mut Vec<f32>,
) -> Result<Vec<f32>, Error> {
    if x.is_empty() || x.len() != w.len() || x.len() != m.cols() {
        return Err(Error::Shape("rms_matvec lengths"));
    }
    if !(eps.is_finite() && eps > 0.0) {
        return Err(Error::Parameter("rms_norm epsilon"));
    }
    let mut ss = 0.0f32;
    for v in x {
        ss += v * v;
    }
    ss /= x.len() as f32;
    ss += eps;
    let inv = 1.0 / ss.sqrt();
    let wx: Vec<f32> = w.iter().zip(x).map(|(w, x)| w * x).collect();
    row.resize(m.cols(), 0.0);
    let mut out = Vec::with_capacity(m.rows());
    for r in 0..m.rows() {
        m.dequantize_row(r, row)?;
        let mut sum = 0.0f32;
        for (a, b) in row.iter().zip(&wx) {
            sum += a * b;
        }
        out.push(inv * sum);
    }
    Ok(out)
}

/// Error bound used by the tests for `rmsnorm-prologue`: for output row `r`,
/// `|fused - unfused| <= 4 (n + 4) u * inv * sum_c |W[r,c]| |w[c]| |x[c]|`
/// with `u = 2^-24` (FP32 unit roundoff) and `n` the row length. Both sides
/// are within `(n + 3) u` relative of the exact real value of that sum
/// (standard recursive-summation bound, Higham §3.1), so their difference is
/// within twice that; the factor 4 leaves slack for the rounded `inv`.
pub fn rmsnorm_tolerance(abs_sum: f32, inv: f32, n: usize) -> f32 {
    4.0 * (n as f32 + 4.0) * f32::EPSILON / 2.0 * inv * abs_sum
}
