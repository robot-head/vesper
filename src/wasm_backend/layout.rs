//! The uniform value representation: every Vesper value is one Wasm `i64`.
//!
//! Integers are zero-extended (`u8`, `u32`) or stored as their bits (`u64`,
//! `i64`), `bool` is 0 or 1, `Unit`/`Never` are 0, `f64` is its IEEE bits and
//! every aggregate is a pointer into linear memory. An aggregate block is a
//! sequence of 8-byte slots: tuples and structs store their fields in
//! declaration order; an enum stores its variant index in slot 0 and the
//! variant's fields after it. A `String` is a pointer to its byte length
//! (one slot) followed by its UTF-8 bytes; literals live in static data. Aggregates are immutable values: a field write
//! copies the block, so `let b = a; b.x = 1` leaves `a` unchanged.
use super::{Unsupported, WasmError};
#[allow(unused_imports)]
use super::Unsupported as _;
use crate::{ast::*, check::World};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scalar {
    I64,
    U64,
    U32,
    U8,
    Bool,
    F64,
    Unit,
    Never,
}

#[derive(Clone, Debug)]
pub(crate) struct VariantShape {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub named: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Scalar(Scalar),
    Tuple(Vec<Type>),
    Struct {
        name: String,
        fields: Vec<(String, Type)>,
    },
    Enum {
        variants: Vec<VariantShape>,
    },
    /// `String`: an immutable byte block `[len, bytes...]`.
    Str,
    /// A generic parameter or inferred type: the value is carried uniformly
    /// but no type-directed operation may be applied to it.
    Opaque,
    Function,
}

impl Shape {
    pub(crate) fn scalar(&self) -> Option<Scalar> {
        if let Shape::Scalar(s) = self {
            Some(*s)
        } else {
            None
        }
    }
}

pub(crate) fn shape(world: &World, ty: &Type) -> Result<Shape, WasmError> {
    let ty = crate::numeric::default_type(ty);
    Ok(match &ty {
        Type::Named(n, args) => match n.as_str() {
            "i64" => Shape::Scalar(Scalar::I64),
            "u64" => Shape::Scalar(Scalar::U64),
            "u32" => Shape::Scalar(Scalar::U32),
            "u8" => Shape::Scalar(Scalar::U8),
            "bool" => Shape::Scalar(Scalar::Bool),
            "f64" => Shape::Scalar(Scalar::F64),
            "Unit" => Shape::Scalar(Scalar::Unit),
            "Never" => Shape::Scalar(Scalar::Never),
            "String" => Shape::Str,
            _ => {
                let Some(adt) = world.adts.get(n) else {
                    return Ok(Shape::Opaque);
                };
                let vars: BTreeMap<String, Type> = adt
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                let fields = |v: &Variant| {
                    v.fields
                        .iter()
                        .map(|f| (f.name.clone(), substitute(&f.ty, &vars)))
                        .collect::<Vec<_>>()
                };
                if adt.is_enum {
                    Shape::Enum {
                        variants: adt
                            .variants
                            .iter()
                            .map(|v| VariantShape {
                                name: v.name.clone(),
                                fields: fields(v),
                                named: v.named,
                            })
                            .collect(),
                    }
                } else {
                    Shape::Struct {
                        name: n.clone(),
                        fields: adt.variants.first().map(fields).unwrap_or_default(),
                    }
                }
            }
        },
        Type::Tuple(ts) if ts.is_empty() => Shape::Scalar(Scalar::Unit),
        Type::Tuple(ts) => Shape::Tuple(ts.clone()),
        Type::Function { .. } => Shape::Function,
        Type::IntLiteral(_) => Shape::Scalar(Scalar::I64),
        Type::Infer | Type::Row(_) => Shape::Opaque,
    })
}

/// Byte offset and type of field `name` of a tuple or struct value.
pub(crate) fn field(world: &World, ty: &Type, name: &str) -> Result<(u64, Type), WasmError> {
    match shape(world, ty)? {
        Shape::Tuple(ts) => {
            let i: usize = name.parse().map_err(|_| {
                WasmError::Invalid(format!("tuple field '{name}' of {ty} is not an index"))
            })?;
            let t = ts
                .get(i)
                .ok_or_else(|| WasmError::Invalid(format!("tuple {ty} has no field {i}")))?;
            Ok((8 * i as u64, t.clone()))
        }
        Shape::Struct { fields, .. } => fields
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| (8 * i as u64, fields[i].1.clone()))
            .ok_or_else(|| WasmError::Invalid(format!("struct {ty} has no field '{name}'"))),
        _ => Err(WasmError::unsupported(
            Unsupported::Projection,
            "",
            format!("field '{name}' of {ty}"),
        )),
    }
}

/// Number of 8-byte slots of a struct or tuple block.
pub(crate) fn slots(world: &World, ty: &Type) -> Result<usize, WasmError> {
    Ok(match shape(world, ty)? {
        Shape::Tuple(ts) => ts.len(),
        Shape::Struct { fields, .. } => fields.len(),
        _ => return Err(WasmError::Invalid(format!("{ty} is not a product type"))),
    })
}

/// The variant of `constructor` in the enum or struct `ty`: (index, shape,
/// whether the type is an enum).
pub(crate) fn variant(
    world: &World,
    ty: &Type,
    constructor: &str,
) -> Result<(usize, VariantShape, bool), WasmError> {
    let short = constructor.rsplit("::").next().unwrap_or(constructor);
    match shape(world, ty)? {
        Shape::Enum { variants, .. } => variants
            .iter()
            .position(|v| v.name == short)
            .map(|i| (i, variants[i].clone(), true))
            .ok_or_else(|| WasmError::Invalid(format!("{ty} has no variant '{constructor}'"))),
        Shape::Struct { name, fields } => Ok((
            0,
            VariantShape {
                name,
                fields,
                named: true,
            },
            false,
        )),
        _ => Err(WasmError::Invalid(format!(
            "constructor '{constructor}' does not build {ty}"
        ))),
    }
}

/// Byte offset of field `index` of a variant block.
pub(crate) fn variant_offset(is_enum: bool, index: usize) -> u64 {
    8 * (index as u64 + u64::from(is_enum))
}
