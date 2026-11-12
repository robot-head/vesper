//! Shape-checked vector IR and scalar host executor.
//!
//! Q4_0 uses exactly 18 bytes/block: little-endian IEEE binary16 scale followed
//! by 16 packed bytes. Low nibbles encode columns 0..16, high nibbles 16..32;
//! each unsigned nibble is biased by 8. Rows have no partial quant blocks.
//!
//! Arithmetic is ordered FP32, without `mul_add`: RMS sum in increasing index
//! order, then mean, +epsilon, sqrt and reciprocal; norm element `(x*inv)*w`;
//! dequant `(scale*(nibble-8))`, product, then accumulator addition in increasing
//! column order. Fusion recomputes those same rounded norm elements per row,
//! eliminating an otherwise unused norm vector without reassociating operations.
//! Finite inputs and epsilon>0 are mandatory; any nonfinite intermediate errors.

use std::collections::BTreeMap;

use crate::{valid_name, Error};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    F32,
    Q4_0,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorSpec {
    pub dtype: DType,
    pub shape: Vec<usize>,
}
impl TensorSpec {
    pub fn vector(len: usize) -> Self {
        Self {
            dtype: DType::F32,
            shape: vec![len],
        }
    }
    pub fn q4_matrix(rows: usize, cols: usize) -> Self {
        Self {
            dtype: DType::Q4_0,
            shape: vec![rows, cols],
        }
    }
    fn elements(&self) -> Result<usize, Error> {
        self.shape
            .iter()
            .try_fold(1usize, |a, &b| a.checked_mul(b).ok_or(Error::Overflow))
    }
    fn bytes(&self) -> Result<usize, Error> {
        let elements = self.elements()?;
        match self.dtype {
            DType::F32 => elements.checked_mul(4).ok_or(Error::Overflow),
            DType::Q4_0 => (elements / 32).checked_mul(18).ok_or(Error::Overflow),
        }
    }
    fn validate(&self, limits: Limits) -> Result<(), Error> {
        match (self.dtype, self.shape.as_slice()) {
            (DType::F32, [n]) if *n > 0 && *n <= (1 << 24) => (),
            (DType::Q4_0, [rows, cols]) if *rows > 0 && *cols > 0 && cols % 32 == 0 => (),
            _ => return Err(Error::Shape("unsupported dtype/rank/zero/quant alignment")),
        }
        if self.elements()? > limits.max_values || self.bytes()? > limits.max_bytes {
            return Err(Error::Limit("tensor storage"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_depth: usize,
    /// Aggregate input logical values plus all reachable intermediate values.
    pub max_values: usize,
    /// Aggregate input bytes plus all reachable intermediate bytes.
    pub max_bytes: usize,
    /// Abstract scalar work units, including complete binding validation;
    /// not a count of CPU instructions or a wall-clock bound.
    pub max_work: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 256,
            max_depth: 64,
            max_values: 1_048_576,
            max_bytes: 8_388_608,
            max_work: 16_777_216,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_nodes == 0
            || self.max_nodes > 4096
            || self.max_depth == 0
            || self.max_depth > 128
            || self.max_values == 0
            || self.max_values > 16_777_216
            || self.max_bytes == 0
            || self.max_bytes > 134_217_728
            || self.max_work == 0
            || self.max_work > 1_000_000_000
        {
            return Err(Error::Limit("host configuration"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema(BTreeMap<String, TensorSpec>);
impl Schema {
    pub fn new(
        entries: impl IntoIterator<Item = (String, TensorSpec)>,
        limits: Limits,
    ) -> Result<Self, Error> {
        limits.validate()?;
        let mut specs = BTreeMap::new();
        let mut values = 0usize;
        let mut bytes = 0usize;
        for (name, spec) in entries {
            if specs.len() >= limits.max_nodes {
                return Err(Error::Limit("schema entries"));
            }
            if !valid_name(&name) {
                return Err(Error::InvalidName);
            }
            spec.validate(limits)?;
            values = values
                .checked_add(spec.elements()?)
                .ok_or(Error::Overflow)?;
            bytes = bytes.checked_add(spec.bytes()?).ok_or(Error::Overflow)?;
            if values > limits.max_values || bytes > limits.max_bytes {
                return Err(Error::Limit("schema storage"));
            }
            if specs.insert(name, spec).is_some() {
                return Err(Error::SchemaMismatch);
            }
        }
        Ok(Self(specs))
    }
    pub fn get(&self, name: &str) -> Result<&TensorSpec, Error> {
        self.0
            .get(name)
            .ok_or_else(|| Error::MissingTensor(name.into()))
    }
}

#[derive(Clone, Debug)]
pub enum Storage {
    F32(Vec<f32>),
    Q4Bytes(Vec<u8>),
}
#[derive(Clone, Debug)]
pub struct Tensor {
    pub spec: TensorSpec,
    pub storage: Storage,
}
impl Tensor {
    pub fn vector(values: Vec<f32>) -> Self {
        Self {
            spec: TensorSpec::vector(values.len()),
            storage: Storage::F32(values),
        }
    }
    pub fn q4_matrix(rows: usize, cols: usize, bytes: Vec<u8>) -> Self {
        Self {
            spec: TensorSpec::q4_matrix(rows, cols),
            storage: Storage::Q4Bytes(bytes),
        }
    }
}
pub type Bindings = BTreeMap<String, Tensor>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId(usize, u64);
#[derive(Clone, Debug)]
enum Op {
    Input(String),
    RmsNorm {
        x: NodeId,
        weight: String,
        eps: f32,
    },
    Q4MatVec {
        x: NodeId,
        weight: String,
    },
    Residual {
        a: NodeId,
        b: NodeId,
    },
    Fused {
        x: NodeId,
        norm_weight: String,
        matrix: String,
        eps: f32,
    },
}
impl Op {
    fn parents(&self) -> Vec<NodeId> {
        match self {
            Self::Input(_) => vec![],
            Self::RmsNorm { x, .. } | Self::Q4MatVec { x, .. } | Self::Fused { x, .. } => vec![*x],
            Self::Residual { a, b } => vec![*a, *b],
        }
    }
}
#[derive(Clone, Debug)]
struct Node {
    op: Op,
    spec: TensorSpec,
    depth: usize,
}

#[derive(Debug)]
pub struct Graph {
    owner: u64,
    schema: Schema,
    limits: Limits,
    nodes: Vec<Node>,
}
impl Graph {
    pub fn new(schema: Schema, limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        // A schema constructed with looser limits must still fit this graph.
        Schema::new(schema.0.clone(), limits)?;
        Ok(Self {
            owner: crate::next_owner()?,
            schema,
            limits,
            nodes: Vec::new(),
        })
    }
    fn node(&self, id: NodeId) -> Result<&Node, Error> {
        if id.1 != self.owner {
            return Err(Error::InvalidId);
        }
        self.nodes.get(id.0).ok_or(Error::InvalidId)
    }
    fn insert(&mut self, op: Op, spec: TensorSpec) -> Result<NodeId, Error> {
        if self.nodes.len() >= self.limits.max_nodes {
            return Err(Error::Limit("IR nodes"));
        }
        let mut depth = 1;
        for p in op.parents() {
            depth = depth.max(self.node(p)?.depth + 1);
        }
        if depth > self.limits.max_depth {
            return Err(Error::Limit("IR depth"));
        }
        let id = NodeId(self.nodes.len(), self.owner);
        self.nodes.push(Node { op, spec, depth });
        Ok(id)
    }
    pub fn input(&mut self, name: &str) -> Result<NodeId, Error> {
        let spec = self.schema.get(name)?.clone();
        if spec.dtype != DType::F32 {
            return Err(Error::Shape("input must be FP32 vector"));
        }
        self.insert(Op::Input(name.into()), spec)
    }
    pub fn rms_norm(&mut self, x: NodeId, weight: &str, epsilon: f32) -> Result<NodeId, Error> {
        if !epsilon.is_finite() || epsilon <= 0.0 {
            return Err(Error::InvalidEpsilon);
        }
        let spec = self.node(x)?.spec.clone();
        if self.schema.get(weight)? != &spec {
            return Err(Error::Shape("RMS weight mismatch"));
        }
        self.insert(
            Op::RmsNorm {
                x,
                weight: weight.into(),
                eps: epsilon,
            },
            spec,
        )
    }
    pub fn q4_matvec(&mut self, x: NodeId, weight: &str) -> Result<NodeId, Error> {
        let input = &self.node(x)?.spec;
        let matrix = self.schema.get(weight)?;
        if matrix.dtype != DType::Q4_0 || matrix.shape[1] != input.shape[0] {
            return Err(Error::Shape("matvec contraction/dtype mismatch"));
        }
        self.insert(
            Op::Q4MatVec {
                x,
                weight: weight.into(),
            },
            TensorSpec::vector(matrix.shape[0]),
        )
    }
    pub fn residual(&mut self, a: NodeId, b: NodeId) -> Result<NodeId, Error> {
        let spec = self.node(a)?.spec.clone();
        if self.node(b)?.spec != spec {
            return Err(Error::Shape("residual mismatch"));
        }
        self.insert(Op::Residual { a, b }, spec)
    }
    pub fn compile(&self, output: NodeId, fuse: bool) -> Result<Plan, Error> {
        self.node(output)?;
        let mut nodes = self.nodes.clone();
        if fuse {
            // Pure immutable schema weights; fusion does not move operations
            // across residuals, mutate inputs, or change sharing semantics.
            for i in 0..nodes.len() {
                if let Op::Q4MatVec { x, weight: matrix } = nodes[i].op.clone() {
                    if let Op::RmsNorm {
                        x,
                        weight: norm_weight,
                        eps,
                    } = nodes[x.0].op.clone()
                    {
                        nodes[i].op = Op::Fused {
                            x,
                            norm_weight,
                            matrix,
                            eps,
                        };
                    }
                }
            }
        }
        let mut reachable = vec![false; nodes.len()];
        reachable[output.0] = true;
        for i in (0..nodes.len()).rev() {
            if reachable[i] {
                for p in nodes[i].op.parents() {
                    reachable[p.0] = true;
                }
            }
        }
        let mut stats = PlanStats::default();
        for spec in self.schema.0.values() {
            stats.values = stats
                .values
                .checked_add(spec.elements()?)
                .ok_or(Error::Overflow)?;
            stats.bytes = stats
                .bytes
                .checked_add(spec.bytes()?)
                .ok_or(Error::Overflow)?;
            stats.work = stats
                .work
                .checked_add(spec.elements()? as u64)
                .ok_or(Error::Overflow)?;
        }
        for (i, node) in nodes.iter().enumerate() {
            if !reachable[i] {
                continue;
            }
            let n = node.spec.elements()?;
            stats.nodes += 1;
            stats.values = stats.values.checked_add(n).ok_or(Error::Overflow)?;
            stats.bytes = stats
                .bytes
                .checked_add(n.checked_mul(4).ok_or(Error::Overflow)?)
                .ok_or(Error::Overflow)?;
            let work = match &node.op {
                Op::Input(_) => n as u64,
                Op::Residual { .. } => n as u64,
                Op::RmsNorm { .. } => (n as u64).checked_mul(8).ok_or(Error::Overflow)?,
                Op::Q4MatVec { weight, .. } => (self.schema.get(weight)?.elements()? as u64)
                    .checked_mul(4)
                    .ok_or(Error::Overflow)?,
                Op::Fused { x, matrix, .. } => {
                    stats.fused_nodes += 1;
                    let reduce = (nodes[x.0].spec.elements()? as u64)
                        .checked_mul(4)
                        .ok_or(Error::Overflow)?;
                    let project = (self.schema.get(matrix)?.elements()? as u64)
                        .checked_mul(6)
                        .ok_or(Error::Overflow)?;
                    reduce.checked_add(project).ok_or(Error::Overflow)?
                }
            };
            stats.work = stats.work.checked_add(work).ok_or(Error::Overflow)?;
        }
        if stats.values > self.limits.max_values
            || stats.bytes > self.limits.max_bytes
            || stats.work > self.limits.max_work
        {
            return Err(Error::Limit("plan storage/work"));
        }
        Ok(Plan {
            schema: self.schema.clone(),
            nodes,
            reachable,
            output,
            stats,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanStats {
    pub nodes: usize,
    pub fused_nodes: usize,
    pub values: usize,
    pub bytes: usize,
    pub work: u64,
}
#[derive(Clone, Debug)]
pub struct Plan {
    schema: Schema,
    nodes: Vec<Node>,
    reachable: Vec<bool>,
    output: NodeId,
    stats: PlanStats,
}
impl Plan {
    pub fn stats(&self) -> PlanStats {
        self.stats
    }
    pub fn output_spec(&self) -> &TensorSpec {
        &self.nodes[self.output.0].spec
    }
    pub fn execute(&self, bindings: &Bindings) -> Result<Vec<f32>, Error> {
        self.validate_bindings(bindings)?;
        let mut values: Vec<Option<Vec<f32>>> = vec![None; self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate() {
            if !self.reachable[i] {
                continue;
            }
            let get = |id: NodeId| -> Result<&[f32], Error> {
                values[id.0].as_deref().ok_or(Error::InvalidId)
            };
            let result = match &node.op {
                Op::Input(name) => fp32(bindings, name)?.to_vec(),
                Op::RmsNorm { x, weight, eps } => {
                    let x = get(*x)?;
                    let w = fp32(bindings, weight)?;
                    let inv = rms_inverse(x, *eps)?;
                    x.iter()
                        .zip(w)
                        .map(|(&x, &w)| norm_element(x, w, inv))
                        .collect::<Result<_, _>>()?
                }
                Op::Q4MatVec { x, weight } => matvec(q4(bindings, weight)?, get(*x)?, None)?,
                Op::Residual { a, b } => get(*a)?
                    .iter()
                    .zip(get(*b)?)
                    .map(|(&a, &b)| finite(a + b))
                    .collect::<Result<_, _>>()?,
                Op::Fused {
                    x,
                    norm_weight,
                    matrix,
                    eps,
                } => {
                    let x = get(*x)?;
                    let inv = rms_inverse(x, *eps)?;
                    matvec(
                        q4(bindings, matrix)?,
                        x,
                        Some((fp32(bindings, norm_weight)?, inv)),
                    )?
                }
            };
            values[i] = Some(result);
        }
        values[self.output.0].take().ok_or(Error::InvalidId)
    }

    fn validate_bindings(&self, bindings: &Bindings) -> Result<(), Error> {
        if bindings.len() != self.schema.0.len() {
            return Err(Error::SchemaMismatch);
        }
        // Complete pass before the first arithmetic kernel executes.
        for (name, spec) in &self.schema.0 {
            let tensor = bindings
                .get(name)
                .ok_or_else(|| Error::MissingTensor(name.clone()))?;
            if tensor.spec != *spec {
                return Err(Error::SchemaMismatch);
            }
            match (&tensor.storage, spec.dtype) {
                (Storage::F32(values), DType::F32) => {
                    if values.len() != spec.elements()? {
                        return Err(Error::SchemaMismatch);
                    }
                    if values.iter().any(|x| !x.is_finite()) {
                        return Err(Error::NonFinite);
                    }
                }
                (Storage::Q4Bytes(bytes), DType::Q4_0) => {
                    if bytes.len() != spec.bytes()? {
                        return Err(Error::SchemaMismatch);
                    }
                    for block in bytes.as_chunks::<18>().0.iter() {
                        finite(decode_fp16(u16::from_le_bytes([block[0], block[1]])))?;
                    }
                }
                _ => return Err(Error::SchemaMismatch),
            }
        }
        Ok(())
    }
}

fn fp32<'a>(bindings: &'a Bindings, name: &str) -> Result<&'a [f32], Error> {
    match &bindings
        .get(name)
        .ok_or_else(|| Error::MissingTensor(name.into()))?
        .storage
    {
        Storage::F32(v) => Ok(v),
        _ => Err(Error::SchemaMismatch),
    }
}
fn q4<'a>(bindings: &'a Bindings, name: &str) -> Result<(&'a [u8], usize, usize), Error> {
    let tensor = bindings
        .get(name)
        .ok_or_else(|| Error::MissingTensor(name.into()))?;
    match &tensor.storage {
        Storage::Q4Bytes(v) => Ok((v, tensor.spec.shape[0], tensor.spec.shape[1])),
        _ => Err(Error::SchemaMismatch),
    }
}
fn finite(value: f32) -> Result<f32, Error> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::NonFinite)
    }
}
fn rms_inverse(x: &[f32], epsilon: f32) -> Result<f32, Error> {
    let mut sum = 0.0f32;
    for &x in x {
        sum = finite(sum + finite(x * x)?)?;
    }
    let mean = sum / x.len() as f32;
    let denom = finite(mean + epsilon)?.sqrt();
    finite(1.0 / denom)
}
fn norm_element(x: f32, weight: f32, inv: f32) -> Result<f32, Error> {
    finite(finite(x * inv)? * weight)
}
fn matvec(
    (bytes, rows, cols): (&[u8], usize, usize),
    x: &[f32],
    norm: Option<(&[f32], f32)>,
) -> Result<Vec<f32>, Error> {
    let blocks_per_row = cols / 32;
    let mut result = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut sum = 0.0f32;
        for col in 0..cols {
            let block_offset = (row * blocks_per_row + col / 32) * 18;
            let block = &bytes[block_offset..block_offset + 18];
            let scale = decode_fp16(u16::from_le_bytes([block[0], block[1]]));
            let within = col % 32;
            let packed = block[2 + within % 16];
            let nibble = if within < 16 {
                packed & 15
            } else {
                packed >> 4
            };
            let dequant = finite(scale * (i32::from(nibble) - 8) as f32)?;
            let activation = match norm {
                Some((w, inv)) => norm_element(x[col], w[col], inv)?,
                None => x[col],
            };
            sum = finite(sum + finite(dequant * activation)?)?;
        }
        result.push(sum);
    }
    Ok(result)
}

/// Exact IEEE binary16→binary32 conversion, including signed zero, subnormals,
/// infinities and NaNs. The executor rejects nonfinite scales after decoding.
/// NaNs keep payload bits shifted to binary32; no arithmetic is performed here.
pub fn decode_fp16(bits: u16) -> f32 {
    let sign = (u32::from(bits & 0x8000)) << 16;
    let exponent = (bits >> 10) & 31;
    let fraction = u32::from(bits & 1023);
    let out = match exponent {
        0 if fraction == 0 => sign,
        0 => {
            let shift = fraction.leading_zeros() - 21;
            let normalized = fraction << shift;
            sign | ((113 - shift) << 23) | ((normalized & 1023) << 13)
        }
        31 => sign | 0x7f80_0000 | (fraction << 13),
        _ => sign | ((u32::from(exponent) + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(out)
}
