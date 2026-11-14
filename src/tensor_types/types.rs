//! The tensor-aware view of source types, generic kinds and type erasure.
use super::dim::{Dim, DimError};
use super::{
    ACTIVE_SEQ, BYTE_ARRAY, DIM_PRODUCT, KV_EFFECT, LEASE, SHAPE_TYPE, TENSOR, ZERO_PAD,
    wire_integer,
};
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// What a generic parameter ranges over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A tensor dimension (`N: dim`), passed at run time as a hidden `u64`.
    Dim,
    /// A tensor element type, erased (the host tensor carries it).
    DType,
    /// An ordinary type parameter, left to the core checker.
    Type,
}
impl Kind {
    /// Tensor-only parameters are erased from the core program.
    pub fn erased(self) -> bool {
        self != Self::Type
    }
}

/// A tensor element type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DType {
    F32,
    Bf16,
    /// A quantized block given by a checked wire struct.
    Block(String),
    /// A dtype parameter (rigid in its item, `?n` during a call).
    Var(String),
}
impl fmt::Display for DType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::F32 => write!(f, "F32"),
            Self::Bf16 => write!(f, "Bf16"),
            Self::Block(n) | Self::Var(n) => write!(f, "{n}"),
        }
    }
}

/// A source type as the tensor pass sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Tensor {
        dtype: DType,
        shape: Vec<Dim>,
    },
    /// `KvPageLease[KvHeads, HeadDim]`.
    Lease {
        kv: Dim,
        hd: Dim,
    },
    /// `ActiveKvSeq[KvHeads, HeadDim]`.
    Seq {
        kv: Dim,
        hd: Dim,
    },
    /// A user ADT or `Option`/`Result`, arguments in declaration order.
    Adt(String, Vec<Arg>),
    Tuple(Vec<Ty>),
    /// An ordinary type parameter.
    Var(String),
    /// A type without tensor content that the core checker owns.
    Base(String),
    /// Not tracked by this pass (closures, unknown names, ...).
    Unknown,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Ty(Ty),
    Dim(Dim),
    DType(DType),
}
impl Ty {
    /// Whether the type carries any shape or dtype information.
    pub fn has_tensor(&self) -> bool {
        match self {
            Self::Tensor { .. } | Self::Lease { .. } | Self::Seq { .. } => true,
            Self::Adt(_, args) => args.iter().any(|a| match a {
                Arg::Dim(_) | Arg::DType(_) => true,
                Arg::Ty(t) => t.has_tensor(),
            }),
            Self::Tuple(ts) => ts.iter().any(Ty::has_tensor),
            _ => false,
        }
    }
    pub fn unit() -> Self {
        Self::Tuple(vec![])
    }
}
impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tensor { dtype, shape } => {
                write!(f, "Tensor[{dtype}, {}]", super::dim::display_shape(shape))
            }
            Self::Lease { kv, hd } => write!(f, "{LEASE}[{kv}, {hd}]"),
            Self::Seq { kv, hd } => write!(f, "{ACTIVE_SEQ}[{kv}, {hd}]"),
            Self::Adt(n, args) if args.is_empty() => write!(f, "{n}"),
            Self::Adt(n, args) => write!(
                f,
                "{n}[{}]",
                args.iter()
                    .map(|a| match a {
                        Arg::Ty(t) => t.to_string(),
                        Arg::Dim(d) => d.to_string(),
                        Arg::DType(d) => d.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Tuple(ts) => write!(
                f,
                "({})",
                ts.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Var(n) | Self::Base(n) => write!(f, "{n}"),
            Self::Unknown => write!(f, "_"),
        }
    }
}

/// Declared kinds of each user ADT's generics, in order.
pub type AdtKinds = BTreeMap<String, Vec<Kind>>;

fn is_dim_name(n: &str) -> bool {
    n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty()
}

/// Record the kinds at which each name of `generics` is used in `ty`.
pub fn collect_kinds(
    ty: &Type,
    generics: &BTreeSet<String>,
    adts: &AdtKinds,
    out: &mut BTreeMap<String, BTreeSet<Kind>>,
) {
    let dims = |t: &Type, out: &mut BTreeMap<String, BTreeSet<Kind>>| {
        dim_names(t, &mut |n| {
            if generics.contains(n) {
                out.entry(n.to_string()).or_default().insert(Kind::Dim);
            }
        })
    };
    match ty {
        Type::Named(n, args) if n == TENSOR && args.len() == 2 => {
            if let Type::Named(d, a) = &args[0]
                && a.is_empty()
                && generics.contains(d)
            {
                out.entry(d.clone()).or_default().insert(Kind::DType);
            } else {
                collect_kinds(&args[0], generics, adts, out);
            }
            dims(&args[1], out);
        }
        Type::Named(n, args) if n == LEASE || n == ACTIVE_SEQ || n == SHAPE_TYPE => {
            for a in args {
                dims(a, out);
            }
        }
        Type::Named(n, args) if args.is_empty() && generics.contains(n) => {
            out.entry(n.clone()).or_default().insert(Kind::Type);
        }
        Type::Named(n, args) => {
            let kinds = adts.get(n);
            for (i, a) in args.iter().enumerate() {
                match kinds.and_then(|k| k.get(i)) {
                    Some(Kind::Dim) => dims(a, out),
                    Some(Kind::DType) => {
                        if let Type::Named(d, x) = a
                            && x.is_empty()
                            && generics.contains(d)
                        {
                            out.entry(d.clone()).or_default().insert(Kind::DType);
                        }
                    }
                    _ => collect_kinds(a, generics, adts, out),
                }
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_kinds(t, generics, adts, out);
            }
        }
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            for (_, t) in params {
                collect_kinds(t, generics, adts, out);
            }
            collect_kinds(result, generics, adts, out);
            collect_row_kinds(effects, generics, adts, out);
        }
        Type::Row(effects) => collect_row_kinds(effects, generics, adts, out),
        Type::IntLiteral(_) | Type::Infer => {}
    }
}
pub fn collect_row_kinds(
    effects: &Effects,
    generics: &BTreeSet<String>,
    adts: &AdtKinds,
    out: &mut BTreeMap<String, BTreeSet<Kind>>,
) {
    for e in &effects.instances {
        for a in &e.args {
            if e.name == KV_EFFECT {
                dim_names(a, &mut |n| {
                    if generics.contains(n) {
                        out.entry(n.to_string()).or_default().insert(Kind::Dim);
                    }
                });
            } else {
                collect_kinds(a, generics, adts, out);
            }
        }
    }
}
fn dim_names(t: &Type, f: &mut dyn FnMut(&str)) {
    match t {
        Type::Named(n, args) if n == DIM_PRODUCT || n == SHAPE_TYPE => {
            for a in args {
                dim_names(a, f);
            }
        }
        Type::Named(n, args) if args.is_empty() => f(n),
        _ => {}
    }
}

/// Resolve the kinds of `generics` from their uses; a parameter used at two
/// kinds is an error.
pub fn resolve_kinds(
    generics: &[Generic],
    uses: &BTreeMap<String, BTreeSet<Kind>>,
) -> Result<Vec<Kind>, String> {
    generics
        .iter()
        .map(|g| match uses.get(&g.name) {
            None => Ok(Kind::Type),
            Some(k) if k.len() == 1 => Ok(*k.iter().next().unwrap_or(&Kind::Type)),
            Some(_) => Err(format!(
                "generic '{}' is used both as a tensor dimension or dtype and as a type",
                g.name
            )),
        })
        .collect()
}

/// Kinds of every user ADT, computed to a fixed point over field types.
pub fn adt_kinds(module: &Module) -> (AdtKinds, Vec<(String, Span)>) {
    let mut kinds: AdtKinds = BTreeMap::new();
    let mut errors = vec![];
    for _ in 0..=module.items.len() {
        let mut changed = false;
        errors.clear();
        for item in &module.items {
            let Item::Adt(a) = item else { continue };
            let names: BTreeSet<String> = a.generics.iter().map(|g| g.name.clone()).collect();
            let mut uses = BTreeMap::new();
            for v in &a.variants {
                for f in &v.fields {
                    collect_kinds(&f.ty, &names, &kinds, &mut uses);
                }
            }
            let resolved = resolve_kinds(&a.generics, &uses).unwrap_or_else(|e| {
                errors.push((e, a.span.clone()));
                vec![Kind::Type; a.generics.len()]
            });
            if kinds.get(&a.name) != Some(&resolved) {
                kinds.insert(a.name.clone(), resolved);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (kinds, errors)
}

/// Converts surface types for one item, whose generics have known kinds.
pub struct Scope<'a> {
    pub kinds: BTreeMap<String, Kind>,
    pub adts: &'a AdtKinds,
    /// Wire structs usable as quantized block dtypes.
    pub blocks: &'a BTreeSet<String>,
}
impl Scope<'_> {
    pub fn ty(&self, t: &Type) -> Result<Ty, String> {
        Ok(match t {
            Type::Named(n, args) if n == TENSOR => {
                let [dtype, shape] = args.as_slice() else {
                    return Err("a tensor type is written Tensor[DType, [d, ...]]".into());
                };
                Ty::Tensor {
                    dtype: self.dtype(dtype)?,
                    shape: self.shape(shape)?,
                }
            }
            Type::Named(n, args) if n == LEASE || n == ACTIVE_SEQ => {
                let [kv, hd] = args.as_slice() else {
                    return Err(format!("{n} takes two dimensions: {n}[KvHeads, HeadDim]"));
                };
                let (kv, hd) = (self.dim(kv)?, self.dim(hd)?);
                if n == LEASE {
                    Ty::Lease { kv, hd }
                } else {
                    Ty::Seq { kv, hd }
                }
            }
            Type::Named(n, _) if n == SHAPE_TYPE => {
                return Err("a shape literal is only valid as a tensor's second argument".into());
            }
            Type::Named(n, args) if args.is_empty() && self.kinds.contains_key(n) => {
                match self.kinds[n] {
                    Kind::Type => Ty::Var(n.clone()),
                    Kind::DType => {
                        return Err(format!("dtype parameter '{n}' is used as a type"));
                    }
                    Kind::Dim => return Err(format!("dimension '{n}' is used as a type")),
                }
            }
            Type::Named(n, args) if is_dim_name(n) && args.is_empty() => {
                return Err(format!("'{n}' is a dimension, not a type"));
            }
            Type::Named(n, args) if self.adts.contains_key(n) || n == "Option" || n == "Result" => {
                let kinds = self
                    .adts
                    .get(n)
                    .cloned()
                    .unwrap_or_else(|| vec![Kind::Type; args.len()]);
                if kinds.len() != args.len() {
                    // The core checker reports the arity error.
                    return Ok(Ty::Unknown);
                }
                let mut out = vec![];
                for (k, a) in kinds.iter().zip(args) {
                    out.push(match k {
                        Kind::Dim => Arg::Dim(self.dim(a)?),
                        Kind::DType => Arg::DType(self.dtype(a)?),
                        Kind::Type => Arg::Ty(self.ty(a)?),
                    });
                }
                Ty::Adt(n.clone(), out)
            }
            Type::Named(n, args) => {
                if wire_integer(n).is_some() || n == BYTE_ARRAY || n == ZERO_PAD {
                    return Err(format!("'{n}' is only valid as a wire struct field type"));
                }
                for a in args {
                    if self.ty(a)?.has_tensor() {
                        return Err(format!(
                            "a tensor type cannot instantiate the parameters of '{n}'"
                        ));
                    }
                }
                Ty::Base(n.clone())
            }
            Type::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.ty(t)).collect::<Result<_, _>>()?),
            Type::Function { params, result, .. } => {
                for (_, p) in params {
                    self.ty(p)?;
                }
                self.ty(result)?;
                Ty::Unknown
            }
            Type::IntLiteral(_) | Type::Infer | Type::Row(_) => Ty::Unknown,
        })
    }
    pub fn dtype(&self, t: &Type) -> Result<DType, String> {
        match t {
            Type::Named(n, a) if a.is_empty() && n == "F32" => Ok(DType::F32),
            Type::Named(n, a) if a.is_empty() && n == "Bf16" => Ok(DType::Bf16),
            Type::Named(n, a) if a.is_empty() && self.kinds.get(n) == Some(&Kind::DType) => {
                Ok(DType::Var(n.clone()))
            }
            Type::Named(n, a) if a.is_empty() && self.blocks.contains(n) => {
                Ok(DType::Block(n.clone()))
            }
            other => Err(format!(
                "'{}' is not a tensor dtype: use F32, Bf16, a quantized block wire struct or a dtype parameter",
                display_type(other)
            )),
        }
    }
    pub fn shape(&self, t: &Type) -> Result<Vec<Dim>, String> {
        match t {
            Type::Named(n, dims) if n == SHAPE_TYPE => {
                if dims.len() > super::MAX_RANK {
                    return Err(format!(
                        "tensor rank {} exceeds the supported rank {}",
                        dims.len(),
                        super::MAX_RANK
                    ));
                }
                dims.iter().map(|d| self.dim(d)).collect()
            }
            other => Err(format!(
                "expected a shape literal [d, ...], found '{}'",
                display_type(other)
            )),
        }
    }
    pub fn dim(&self, t: &Type) -> Result<Dim, String> {
        let e = |e: DimError| e.to_string();
        match t {
            Type::Named(n, args) if n == DIM_PRODUCT && args.len() == 2 => {
                self.dim(&args[0])?.mul(&self.dim(&args[1])?).map_err(e)
            }
            Type::Named(n, args) if args.is_empty() && is_dim_name(n) => {
                let value: u64 = n.parse().map_err(|_| DimError::Overflow).map_err(e)?;
                Dim::constant(value).map_err(e)
            }
            Type::Named(n, args) if args.is_empty() => match self.kinds.get(n) {
                Some(Kind::Dim) => Ok(Dim::var(n)),
                _ => Err(format!(
                    "unknown dimension '{n}': declare it as a parameter `{n}: dim`"
                )),
            },
            other => Err(format!(
                "expected a dimension, found '{}'",
                display_type(other)
            )),
        }
    }
}

pub fn display_type(t: &Type) -> String {
    match t {
        Type::Named(n, a) if n == SHAPE_TYPE => format!(
            "[{}]",
            a.iter().map(display_type).collect::<Vec<_>>().join(", ")
        ),
        Type::Named(n, a) if n == DIM_PRODUCT && a.len() == 2 => {
            format!("{} * {}", display_type(&a[0]), display_type(&a[1]))
        }
        Type::Named(n, a) if !a.is_empty() => format!(
            "{n}[{}]",
            a.iter().map(display_type).collect::<Vec<_>>().join(", ")
        ),
        other => other.to_string(),
    }
}

/// Erase tensor information for the core checker: tensor, lease and sequence
/// types lose their arguments, tensor-only ADT arguments are dropped and wire
/// field types become their value carriers.
pub fn erase(t: &Type, adts: &AdtKinds) -> Type {
    match t {
        Type::Named(n, _) if n == TENSOR || n == LEASE || n == ACTIVE_SEQ => {
            Type::Named(n.clone(), vec![])
        }
        Type::Named(n, a) if a.is_empty() && wire_integer(n).is_some() => Type::named("u64"),
        Type::Named(n, _) if n == BYTE_ARRAY || n == ZERO_PAD => Type::unit(),
        Type::Named(n, args) => {
            let kinds = adts.get(n);
            Type::Named(
                n.clone(),
                args.iter()
                    .enumerate()
                    .filter(|(i, _)| kinds.and_then(|k| k.get(*i)).is_none_or(|k| !k.erased()))
                    .map(|(_, a)| erase(a, adts))
                    .collect(),
            )
        }
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| erase(t, adts)).collect()),
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => Type::Function {
            params: params.iter().map(|(p, t)| (*p, erase(t, adts))).collect(),
            result: Box::new(erase(result, adts)),
            effects: erase_row(effects, adts),
            mode: *mode,
        },
        Type::Row(e) => Type::Row(erase_row(e, adts)),
        other => other.clone(),
    }
}
pub fn erase_row(e: &Effects, adts: &AdtKinds) -> Effects {
    Effects {
        instances: e
            .instances
            .iter()
            .map(|i| Effect {
                label: i.label.clone(),
                name: i.name.clone(),
                args: if i.name == KV_EFFECT {
                    vec![]
                } else {
                    i.args.iter().map(|a| erase(a, adts)).collect()
                },
            })
            .collect(),
        tail: e.tail.clone(),
    }
}

/// The diagnostic code of a type-conversion error: dimension range and rank
/// errors are [`super::codes::DIM`], the rest [`super::codes::KIND`].
pub fn error_code(message: &str) -> &'static str {
    let dim = [
        DimError::Overflow.to_string(),
        DimError::Zero.to_string(),
        DimError::Factors.to_string(),
    ];
    if dim.iter().any(|d| message.contains(d.as_str()))
        || message.contains("exceeds the supported rank")
    {
        super::codes::DIM
    } else {
        super::codes::KIND
    }
}
