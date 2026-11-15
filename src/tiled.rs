//! `tiled(N) struct` (AoSoA + hot/cold segregation) and `TiledSeq[T]` (§12.5).
//!
//! A `tiled(N) struct` declares a tile width `N` in `{8, 16, 32, 64}` and
//! partitions every field as `hot` or `cold`. `TiledSeq[T]` stores records in
//! Structure-of-Arrays tiles of `N` lanes for `hot` fields—each column aligned
//! to 64 bytes (`CACHE_LINE_BYTES`)—and segregates `cold` fields into a
//! separate side slab untouched by hot column scans or SIMD loads.
//!
//! Disjoint column projections (`in index.{col}: TiledSeq[T]`,
//! `inout index.{a}: TiledSeq[T]`) compose with Vesper's `[T-CallProj]` checker
//! (`check.rs`), `load_tile_vec` feeds `Vec[Elem, N]` directly under `Isa[P]`,
//! and Lean 4 extraction (`src/lean_nodes.rs`) models `TiledSeq[T]` as `Array T`.
use crate::{
    ast::*,
    check::World,
    layout::Layout,
    vector::{Elem, Shape},
};

#[path = "tiled/native_runtime.rs"]
pub mod native_runtime;

/// Built-in AoSoA sequence type `TiledSeq[T]`.
pub const TILED_SEQ_TYPE: &str = "TiledSeq";
/// Valid tile widths `N` in `tiled(N) struct`.
pub const SUPPORTED_WIDTHS: [u32; 4] = [8, 16, 32, 64];
/// Cache-line alignment for each hot column inside a tile.
pub const CACHE_LINE_BYTES: usize = 64;

/// Compiled AoSoA layout of one `tiled(N) struct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiledLayout {
    pub adt: String,
    pub tile_width: u32,
    pub hot_columns: Vec<HotColumnLayout>,
    pub tile_stride_bytes: usize,
    pub cold_fields: Vec<ColdFieldLayout>,
    pub cold_words_per_row: usize,
}

/// One `hot` column within a tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotColumnLayout {
    pub name: String,
    pub ty: Type,
    pub elem_bytes: usize,
    pub aligned_bytes: usize,
    pub tile_offset: usize,
}

/// One `cold` field in the segregated side slab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdFieldLayout {
    pub name: String,
    pub ty: Type,
    pub word_offset: usize,
    pub words: usize,
}

/// A `TiledSeq` operation lowered into Place IR (`Op::Tiled`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum TiledOp {
    New {
        adt: String,
    },
    WithCapacity {
        adt: String,
    },
    WithRegion {
        adt: String,
        region_label: String,
    },
    Len,
    TileCount {
        tile_width: u32,
    },
    Push {
        adt: String,
    },
    Get {
        adt: String,
    },
    Set {
        adt: String,
    },
    ColGet {
        adt: String,
        field: String,
    },
    ColSet {
        adt: String,
        field: String,
    },
    LoadTileVec {
        adt: String,
        field: String,
        shape: Shape,
    },
}

pub fn is_valid_width(width: u32) -> bool {
    SUPPORTED_WIDTHS.contains(&width)
}

pub fn is_seq_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n, a) if n == TILED_SEQ_TYPE && matches!(a.len(), 1 | 2))
}

pub fn seq_elem(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Named(n, args) if n == TILED_SEQ_TYPE && matches!(args.len(), 1 | 2) => {
            Some(&args[0])
        }
        _ => None,
    }
}

pub fn seq_region_label(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named(n, args) if n == TILED_SEQ_TYPE && args.len() == 2 => {
            crate::regions::row_label(&args[1])
        }
        _ => None,
    }
}

pub fn seq_adt<'a>(world: &'a World, ty: &Type) -> Option<(&'a Adt, u32)> {
    let elem = seq_elem(ty)?;
    let Type::Named(name, args) = elem else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let adt = world.adts.get(name)?;
    let width = adt.tile_width?;
    Some((adt, width))
}

pub fn hot_elem_bytes(ty: &Type) -> Option<usize> {
    match ty {
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            "u8" | "bool" => Some(1),
            "u32" => Some(4),
            "u64" | "i64" | "f64" => Some(8),
            _ => None,
        },
        _ => None,
    }
}

pub fn hot_vector_shape(elem_ty: &Type, tile_width: u32) -> Option<Shape> {
    let elem = Elem::from_type(elem_ty)?;
    let lanes = u16::try_from(tile_width).ok()?;
    Shape::new(elem, lanes)
}

pub fn round_up_64(bytes: usize) -> usize {
    (bytes + (CACHE_LINE_BYTES - 1)) & !(CACHE_LINE_BYTES - 1)
}

/// Derive the 64-byte cache-line AoSoA layout and segregated cold slab layout
/// for a `tiled(N) struct`.
pub fn layout_of_adt(world: &World, adt: &Adt) -> Result<TiledLayout, String> {
    let tile_width = adt
        .tile_width
        .ok_or_else(|| format!("'{}' is not a tiled(N) struct", adt.name))?;
    if !is_valid_width(tile_width) {
        return Err(format!(
            "tiled struct '{}' has unsupported tile width {tile_width}",
            adt.name
        ));
    }
    if adt.is_enum || adt.variants.is_empty() {
        return Err(format!("'{}' is not a tiled struct", adt.name));
    }
    let mut hot_columns = Vec::new();
    let mut tile_offset = 0usize;
    let mut cold_fields = Vec::new();
    let mut cold_word_offset = 0usize;

    for field in &adt.variants[0].fields {
        match field.temperature {
            Some(FieldTemperature::Hot) => {
                let elem_bytes = hot_elem_bytes(&field.ty).ok_or_else(|| {
                    format!(
                        "hot field '{}' of tiled struct '{}' must be a Copy scalar",
                        field.name, adt.name
                    )
                })?;
                let unpadded = (tile_width as usize)
                    .checked_mul(elem_bytes)
                    .ok_or_else(|| "tiled column size overflow".to_string())?;
                let aligned_bytes = round_up_64(unpadded);
                hot_columns.push(HotColumnLayout {
                    name: field.name.clone(),
                    ty: field.ty.clone(),
                    elem_bytes,
                    aligned_bytes,
                    tile_offset,
                });
                tile_offset = tile_offset
                    .checked_add(aligned_bytes)
                    .ok_or_else(|| "tile stride overflow".to_string())?;
            }
            Some(FieldTemperature::Cold) => {
                let field_layout = Layout::of(world, &field.ty)?;
                let words = (field_layout.size as usize).div_ceil(8).max(1);
                cold_fields.push(ColdFieldLayout {
                    name: field.name.clone(),
                    ty: field.ty.clone(),
                    word_offset: cold_word_offset,
                    words,
                });
                cold_word_offset = cold_word_offset
                    .checked_add(words)
                    .ok_or_else(|| "cold record size overflow".to_string())?;
            }
            None => {
                return Err(format!(
                    "field '{}' of tiled struct '{}' lacks a 'hot' or 'cold' partition",
                    field.name, adt.name
                ));
            }
        }
    }
    if hot_columns.is_empty() {
        return Err(format!(
            "tiled struct '{}' must declare at least one 'hot' field",
            adt.name
        ));
    }

    Ok(TiledLayout {
        adt: adt.name.clone(),
        tile_width,
        hot_columns,
        tile_stride_bytes: tile_offset,
        cold_fields,
        cold_words_per_row: cold_word_offset,
    })
}

/// Validates all `tiled(N) struct` declarations and misplaced `hot`/`cold`
/// annotations in `world`.
pub(crate) fn check_world_adts(world: &World) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for adt in world.adts.values() {
        if let Some(width) = adt.tile_width {
            if adt.is_enum {
                diagnostics.push(Diagnostic::new(
                    "E1501",
                    format!(
                        "tiled({width}) may only annotate a struct, not enum '{}'",
                        adt.name
                    ),
                    adt.span.clone(),
                ));
                continue;
            }
            if !is_valid_width(width) {
                diagnostics.push(
                    Diagnostic::new(
                        "E1501",
                        format!(
                            "tiled(N) struct '{}' tile width must be 8, 16, 32, or 64, found {width}",
                            adt.name
                        ),
                        adt.span.clone(),
                    )
                    .with_help("use tiled(8), tiled(16), tiled(32), or tiled(64)"),
                );
            }
            if !adt.generics.is_empty() {
                diagnostics.push(Diagnostic::new(
                    "E1501",
                    format!(
                        "tiled(N) struct '{}' cannot declare generic parameters yet",
                        adt.name
                    ),
                    adt.span.clone(),
                ));
            }
            let fields = adt
                .variants
                .first()
                .map(|v| v.fields.as_slice())
                .unwrap_or(&[]);
            let mut has_hot = false;
            for f in fields {
                match f.temperature {
                    Some(FieldTemperature::Hot) => {
                        has_hot = true;
                        if hot_elem_bytes(&f.ty).is_none() {
                            diagnostics.push(
                                Diagnostic::new(
                                    "E1503",
                                    format!(
                                        "hot field '{}' in tiled struct '{}' must have a Copy scalar type (u8, u32, u64, i64, f64, bool), found {}",
                                        f.name, adt.name, f.ty
                                    ),
                                    adt.span.clone(),
                                )
                                .with_help("mark non-scalar or aggregate fields as 'cold'"),
                            );
                        }
                    }
                    Some(FieldTemperature::Cold) => {}
                    None => {
                        diagnostics.push(
                            Diagnostic::new(
                                "E1502",
                                format!(
                                    "field '{}' of tiled struct '{}' must be annotated 'hot' or 'cold'",
                                    f.name, adt.name
                                ),
                                adt.span.clone(),
                            )
                            .with_help("prefix every field in a tiled(N) struct with 'hot' or 'cold'"),
                        );
                    }
                }
            }
            if !has_hot {
                diagnostics.push(Diagnostic::new(
                    "E1502",
                    format!(
                        "tiled struct '{}' must declare at least one 'hot' field",
                        adt.name
                    ),
                    adt.span.clone(),
                ));
            }
        } else {
            for f in adt.variants.iter().flat_map(|v| &v.fields) {
                if f.temperature.is_some() {
                    diagnostics.push(Diagnostic::new(
                        "E1502",
                        format!(
                            "'hot'/'cold' field annotation on '{}.{}' is only valid inside a tiled(N) struct",
                            adt.name, f.name
                        ),
                        adt.span.clone(),
                    ));
                }
            }
        }
    }
    diagnostics
}

/// Checks if a call targets `TiledSeq` (`TiledSeq::op` or receiver `.op` on
/// `TiledSeq[...]`) and resolves it to a [`TiledOp`] plus whether its first
/// argument is a receiver (`false` for `New`/`WithCapacity`/`WithRegion`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_ir_call(
    world: &World,
    types: &crate::ast::TypeMap,
    capabilities: &std::collections::BTreeMap<String, String>,
    callee_name: &str,
    receiver: Option<&Expr>,
    type_args: &[Type],
    args: &[Arg],
    call_expr: &Expr,
) -> Option<(TiledOp, Option<String>)> {
    let result_ty = types
        .get(&(call_expr.span.start, call_expr.span.end))
        .cloned()
        .unwrap_or(Type::Infer);
    let ty_of = |e: &Expr| {
        types
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or(Type::Infer)
    };
    let (op_name, receiver_ty) = if let Some(base) = receiver {
        let base_ty = ty_of(base);
        if !is_seq_type(&base_ty) {
            return None;
        }
        let method = callee_name.rsplit("::").next().unwrap_or(callee_name);
        (method, Some(base_ty))
    } else {
        let rest = callee_name.strip_prefix("TiledSeq::")?;
        let recv_ty = args.first().map(|a| ty_of(&a.expr)).filter(is_seq_type);
        (rest, recv_ty)
    };

    match op_name {
        "new" if receiver.is_none() => {
            let elem = seq_elem(&result_ty).or_else(|| type_args.first())?;
            let adt = world.adts.get(elem.name())?.name.clone();
            Some((TiledOp::New { adt }, None))
        }
        "with_capacity" if receiver.is_none() => {
            let elem = seq_elem(&result_ty).or_else(|| type_args.first())?;
            let adt = world.adts.get(elem.name())?.name.clone();
            Some((TiledOp::WithCapacity { adt }, None))
        }
        "with_region" if receiver.is_none() => {
            let elem = seq_elem(&result_ty).or_else(|| type_args.first())?;
            let adt = world.adts.get(elem.name())?.name.clone();
            let region_label = match &args.first()?.expr.kind {
                ExprKind::Name(n) => capabilities
                    .get(n)
                    .map(|_| n.clone())
                    .unwrap_or_else(|| n.clone()),
                _ => return None,
            };
            Some((
                TiledOp::WithRegion {
                    adt,
                    region_label: region_label.clone(),
                },
                Some(region_label),
            ))
        }
        "len" => Some((TiledOp::Len, None)),
        "tile_count" => {
            let seq_ty = receiver_ty?;
            let (_, width) = seq_adt(world, &seq_ty)?;
            Some((TiledOp::TileCount { tile_width: width }, None))
        }
        "push" => {
            let seq_ty = receiver_ty?;
            let (adt, _) = seq_adt(world, &seq_ty)?;
            Some((
                TiledOp::Push {
                    adt: adt.name.clone(),
                },
                None,
            ))
        }
        "get" => {
            let seq_ty = receiver_ty?;
            let (adt, _) = seq_adt(world, &seq_ty)?;
            Some((
                TiledOp::Get {
                    adt: adt.name.clone(),
                },
                None,
            ))
        }
        "set" => {
            let seq_ty = receiver_ty?;
            let (adt, _) = seq_adt(world, &seq_ty)?;
            Some((
                TiledOp::Set {
                    adt: adt.name.clone(),
                },
                None,
            ))
        }
        "col_get" | "col_set" => {
            let seq_ty = receiver_ty?;
            let (adt, _) = seq_adt(world, &seq_ty)?;
            let field = col_name_from_type_args(type_args, receiver.is_none())?.to_string();
            Some((
                if op_name == "col_get" {
                    TiledOp::ColGet {
                        adt: adt.name.clone(),
                        field,
                    }
                } else {
                    TiledOp::ColSet {
                        adt: adt.name.clone(),
                        field,
                    }
                },
                None,
            ))
        }
        "load_tile_vec" => {
            let seq_ty = receiver_ty?;
            let (adt, width) = seq_adt(world, &seq_ty)?;
            let field = load_vec_col_name(type_args, receiver.is_none())?.to_string();
            let f = adt.variants[0].fields.iter().find(|f| f.name == field)?;
            let shape = hot_vector_shape(&f.ty, width)?;
            Some((
                TiledOp::LoadTileVec {
                    adt: adt.name.clone(),
                    field,
                    shape,
                },
                None,
            ))
        }
        _ => None,
    }
}

pub(crate) fn field_arg_name(t: &Type) -> Option<&str> {
    match t {
        Type::Row(e) if e.instances.is_empty() => e.tail.as_deref(),
        Type::Named(name, a) if a.is_empty() => Some(name.as_str()),
        _ => None,
    }
}

fn col_name_from_type_args(types: &[Type], is_static: bool) -> Option<&str> {
    let idx = if is_static && types.len() == 2 { 1 } else { 0 };
    types.get(idx).and_then(field_arg_name)
}

fn load_vec_col_name(types: &[Type], is_static: bool) -> Option<&str> {
    let sliced = if is_static && !types.is_empty() && types.len() >= 2 {
        // Static `TiledSeq::load_tile_vec[T, col]` or `[T, P, col]`
        &types[1..]
    } else {
        types
    };
    match sliced {
        [col] => field_arg_name(col),
        [_, col] => field_arg_name(col),
        _ => None,
    }
}
