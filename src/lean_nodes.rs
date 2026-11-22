//! Shared structured syntax lowering for both Lean backends.
//! No helper accepts or parses serialized Lean fragments.

use crate::{
    ast::{Literal, Pass, Type},
    check::{Place, World},
    numeric::IntegerKind,
};
use std::collections::BTreeSet;
use vesper_lean_syntax::{BinaryOp, Ident, Name, Projection, Term, UnaryOp};

pub(crate) fn ident(value: &str) -> Result<Ident, String> {
    Ident::new(value).map_err(|e| format!("unsupported Lean identifier {value:?}: {e}"))
}

pub(crate) fn source_name(value: &str) -> Result<Name, String> {
    Name::single(value).map_err(|e| format!("unsupported Lean name {value:?}: {e}"))
}

pub(crate) fn source_ref(value: &str) -> Result<Term, String> {
    Ok(Term::Ref(source_name(value)?))
}

/// A trusted, fixed standard-library path, kept separate from source identifiers.
pub(crate) fn global(path: &[&str]) -> Term {
    Term::Ref(
        Name::qualified(
            path.iter()
                .map(|s| Ident::new(*s).expect("fixed Lean path component")),
        )
        .expect("nonempty fixed Lean path")
        .rooted(),
    )
}

pub(crate) fn unit() -> Term {
    global(&["Unit", "unit"])
}
pub(crate) fn product(mut terms: Vec<Term>) -> Term {
    match terms.len() {
        0 => global(&["Unit"]),
        1 => terms.pop().expect("one element"),
        _ => Term::ProductType(terms),
    }
}
pub(crate) fn tuple(mut terms: Vec<Term>) -> Term {
    match terms.len() {
        0 => unit(),
        1 => terms.pop().expect("one element"),
        _ => Term::Tuple(terms),
    }
}

pub(crate) fn ty_with_fip(value: &Type, erase_box: bool) -> Result<Term, String> {
    typed(value, None, erase_box)
}
pub(crate) fn ty_monad_with_fip(
    value: &Type,
    monad: &Term,
    erase_box: bool,
) -> Result<Term, String> {
    typed(value, Some(monad), erase_box)
}

fn typed(value: &Type, monad: Option<&Term>, erase_box: bool) -> Result<Term, String> {
    Ok(match value {
        Type::IntLiteral(n) => typed(&IntegerKind::default_for(*n).ty(), monad, erase_box)?,
        Type::Named(name, arguments)
            if erase_box && name == crate::fip::BOX_TYPE && arguments.len() == 1 =>
        {
            typed(&arguments[0], monad, erase_box)?
        }
        Type::Named(name, arguments) => {
            let constructor = match name.as_str() {
                "u8" => global(&["UInt8"]),
                "u16" => global(&["UInt16"]),
                "u32" => global(&["UInt32"]),
                // `usize`/`isize` are 64 bits on every target
                // (`vesper.arith.checked/1`), not Lean's platform `USize`.
                "u64" | "usize" => global(&["UInt64"]),
                "i8" => global(&["Int8"]),
                "i16" => global(&["Int16"]),
                "i32" => global(&["Int32"]),
                "i64" | "isize" => global(&["Int64"]),
                // Not modelled yet (SH2 lane 2, #1174): refused, like the
                // Wasm backend refuses every kind it does not lower.
                "i128" | "u128" => {
                    return Err(format!("Lean extraction does not model {name} yet"));
                }
                "f64" => global(&["Float"]),
                // IEEE binary32, like `Float` for binary64
                // (`vesper.float.ieee754/1` basic operations).
                "f32" => global(&["Float32"]),
                "bool" => global(&["Bool"]),
                "Unit" => global(&["Unit"]),
                "Never" => global(&["Empty"]),
                "String" => global(&["String"]),
                "Option" => global(&["Option"]),
                "Result" => global(&["Except"]),
                "TiledSeq" | "ChunkSeq" | "Slice" => global(&["Array"]),
                _ => source_ref(name)?,
            };
            let ordered = if name == "Result" && arguments.len() == 2 {
                vec![&arguments[1], &arguments[0]]
            } else if name == "TiledSeq" && !arguments.is_empty() {
                vec![&arguments[0]]
            } else {
                arguments.iter().collect()
            };
            if ordered.is_empty() {
                constructor
            } else {
                Term::app(
                    constructor,
                    ordered
                        .into_iter()
                        .map(|t| typed(t, monad, erase_box))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
        }
        Type::Tuple(terms) => product(
            terms
                .iter()
                .map(|t| typed(t, monad, erase_box))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            let mut results = vec![typed(result, monad, erase_box)?];
            results.extend(
                params
                    .iter()
                    .filter(|(pass, _)| *pass == Pass::Inout)
                    .map(|(_, t)| typed(t, monad, erase_box))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let result = product(results);
            let mut result = if !effects.instances.is_empty() || effects.tail.is_some() {
                Term::app(
                    monad
                        .ok_or("effectful function type needs an ambient Lean monad")?
                        .clone(),
                    [result],
                )
            } else {
                result
            };
            if params.is_empty() {
                result = Term::Arrow(Box::new(global(&["Unit"])), Box::new(result));
            }
            for (_, parameter) in params.iter().rev() {
                result = Term::Arrow(
                    Box::new(typed(parameter, monad, erase_box)?),
                    Box::new(result),
                );
            }
            result
        }
        Type::Infer => Term::Infer,
        Type::Row(_) => return Err("effect rows are not first-class Lean values".into()),
    })
}

pub(crate) fn recursive_structs(world: &World) -> BTreeSet<String> {
    let fip_active = crate::fip::active(world);
    let mut graph = petgraph::graphmap::DiGraphMap::<&str, ()>::new();
    for a in world.adts.values().filter(|a| {
        a.name != "Option" && a.name != "Result" && (!fip_active || a.name != crate::fip::BOX_TYPE)
    }) {
        graph.add_node(&a.name);
    }
    fn deps<'a>(t: &'a Type, names: &mut Vec<&'a str>) {
        match t {
            Type::Named(n, ts) => {
                names.push(n);
                for t in ts {
                    deps(t, names);
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    deps(t, names);
                }
            }
            Type::Function { params, result, .. } => {
                for (_, t) in params {
                    deps(t, names);
                }
                deps(result, names);
            }
            _ => {}
        }
    }
    for a in world.adts.values() {
        if !graph.contains_node(a.name.as_str()) {
            continue;
        }
        for field in a.variants.iter().flat_map(|v| &v.fields) {
            let mut names = vec![];
            deps(&field.ty, &mut names);
            for n in names {
                if graph.contains_node(n) {
                    graph.add_edge(a.name.as_str(), n, ());
                }
            }
        }
    }
    let mut rec = BTreeSet::new();
    for group in petgraph::algo::tarjan_scc(&graph) {
        let is_cycle =
            group.len() > 1 || (group.len() == 1 && graph.contains_edge(group[0], group[0]));
        if is_cycle {
            for n in group {
                if !world.adts[n].is_enum {
                    rec.insert(n.to_owned());
                }
            }
        }
    }
    rec
}

pub(crate) fn effectful_arrow(value: &Type) -> bool {
    match value {
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            !effects.instances.is_empty()
                || effects.tail.is_some()
                || params.iter().any(|(_, t)| effectful_arrow(t))
                || effectful_arrow(result)
        }
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(effectful_arrow),
        _ => false,
    }
}

/// [`literal`] at the checked type `ty`: an f32 float literal is the f32
/// nearest its f64 value (`vesper.float.ieee754/1`).
pub(crate) fn typed_literal(value: &Literal, ty: &Type) -> Term {
    match value {
        Literal::Float(n) if ty.name() == "f32" => Term::app(
            global(&["Float32", "ofBits"]),
            [Term::Nat(u64::from((*n as f32).to_bits()).into())],
        ),
        _ => literal(value),
    }
}
pub(crate) fn literal(value: &Literal) -> Term {
    match value {
        Literal::Int(n) => {
            let magnitude = Term::Nat(n.unsigned_abs().into());
            if *n < 0 {
                unary(UnaryOp::Neg, magnitude)
            } else {
                magnitude
            }
        }
        Literal::WideInt(n) => Term::Nat((*n).into()),
        Literal::Float(n) => Term::app(
            global(&["Float", "ofBits"]),
            [Term::Nat(n.to_bits().into())],
        ),
        Literal::Bool(b) => Term::Bool(*b),
        Literal::String(s) => Term::String(s.clone()),
    }
}

fn unary(op: UnaryOp, value: Term) -> Term {
    Term::Unary {
        op,
        value: Box::new(value),
    }
}
fn nat(value: u128) -> Term {
    Term::Nat(value.into())
}
fn negative(value: u128) -> Term {
    unary(UnaryOp::Neg, nat(value))
}
pub(crate) fn project(value: Term, field: &str) -> Term {
    Term::Project {
        base: Box::new(value),
        projection: Projection::Field(Ident::new(field).expect("fixed numeric method")),
    }
}
fn decide(proposition: Term) -> Term {
    Term::app(global(&["Decidable", "decide"]), [proposition])
}

pub(crate) fn in_range(op: &str, kind: IntegerKind, a: Term, b: Term) -> Option<Term> {
    Some(if kind.unsigned() {
        match op {
            "+" | "*" => decide(Term::binary(
                BinaryOp::Le,
                Term::binary(
                    if op == "+" {
                        BinaryOp::Add
                    } else {
                        BinaryOp::Mul
                    },
                    project(a, "toNat"),
                    project(b, "toNat"),
                ),
                nat(kind.max() as u128),
            )),
            "-" => decide(Term::binary(BinaryOp::Le, b, a)),
            "neg" => Term::binary(BinaryOp::BoolEq, a, nat(0)),
            _ => return None,
        }
    } else {
        let min = negative(kind.min().unsigned_abs());
        match op {
            "+" | "-" | "*" => {
                let exact = Term::binary(
                    match op {
                        "+" => BinaryOp::Add,
                        "-" => BinaryOp::Sub,
                        _ => BinaryOp::Mul,
                    },
                    project(a, "toInt"),
                    project(b, "toInt"),
                );
                if kind.bits() == 64 {
                    Term::app(source_ref("inI64").expect("fixed prelude name"), [exact])
                } else {
                    decide(Term::binary(
                        BinaryOp::And,
                        Term::binary(BinaryOp::Le, min, exact.clone()),
                        Term::binary(BinaryOp::Le, exact, nat(kind.max() as u128)),
                    ))
                }
            }
            "/" => unary(
                UnaryOp::BoolNot,
                Term::binary(
                    BinaryOp::BoolAnd,
                    Term::binary(BinaryOp::BoolEq, a, min),
                    Term::binary(BinaryOp::BoolEq, b, negative(1)),
                ),
            ),
            "neg" => Term::binary(BinaryOp::BoolNe, a, min),
            _ => return None,
        }
    })
}

pub(crate) fn arith_term(
    op: &str,
    kind: Option<IntegerKind>,
    a: Term,
    b: Term,
) -> Result<Term, String> {
    let operator = match op {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" => BinaryOp::Div,
        "%" => BinaryOp::Mod,
        "==" => BinaryOp::BoolEq,
        "!=" => BinaryOp::BoolNe,
        "<" => BinaryOp::Lt,
        "<=" => BinaryOp::Le,
        ">" => BinaryOp::Gt,
        ">=" => BinaryOp::Ge,
        "&&" => BinaryOp::BoolAnd,
        "||" => BinaryOp::BoolOr,
        _ => return Err(format!("unsupported Lean binary operator {op:?}")),
    };
    if op == "%" && kind.is_some_and(IntegerKind::signed) {
        Ok(Term::If {
            condition: Box::new(Term::binary(BinaryOp::BoolEq, b.clone(), negative(1))),
            yes: Box::new(nat(0)),
            no: Box::new(Term::binary(operator, a, b)),
        })
    } else {
        Ok(Term::binary(operator, a, b))
    }
}

pub(crate) fn update_place_typed(
    world: &World,
    place: &Place,
    value: Term,
    root_ty: Option<&Type>,
    recursive_structs: &BTreeSet<String>,
) -> Result<Term, String> {
    fn update(
        world: &World,
        base: Term,
        ty: Option<&Type>,
        path: &[String],
        value: Term,
        recursive_structs: &BTreeSet<String>,
    ) -> Result<Term, String> {
        let Some((field, rest)) = path.split_first() else {
            return Ok(value);
        };
        let field_ty = ty.and_then(|t| world.field(t, std::slice::from_ref(field)));
        let field_id = ident(field)?;
        let next = Term::Project {
            base: Box::new(base.clone()),
            projection: Projection::Field(field_id.clone()),
        };
        let updated = update(
            world,
            next,
            field_ty.as_ref(),
            rest,
            value,
            recursive_structs,
        )?;
        if let Some(Type::Named(adt_name, args)) = ty
            && recursive_structs.contains(adt_name)
            && let Some(adt) = world.adts.get(adt_name)
        {
            let _ = args;
            let ctor =
                Name::qualified([ident(&adt.name)?, ident("mk")?]).map_err(|e| e.to_string())?;
            let fields = adt.variants[0]
                .fields
                .iter()
                .map(|fld| {
                    if fld.name == *field {
                        Ok(updated.clone())
                    } else {
                        Ok(project(base.clone(), &fld.name))
                    }
                })
                .collect::<Result<Vec<_>, String>>()?;
            return Ok(Term::app(Term::Ref(ctor), fields));
        }
        Ok(Term::Update {
            base: Box::new(base),
            fields: vec![(field_id, updated)],
        })
    }
    update(
        world,
        source_ref(&place.root)?,
        root_ty,
        &place.path,
        value,
        recursive_structs,
    )
}

pub(crate) fn tiled_op_term(
    op: &crate::tiled::TiledOp,
    args: &[Term],
) -> Result<(bool, Term), String> {
    use crate::tiled::TiledOp;
    Ok(match op {
        TiledOp::New { .. } => (
            false,
            Term::app(
                global(&["Array", "emptyWithCapacity"]),
                [Term::Nat(0u64.into())],
            ),
        ),
        TiledOp::WithCapacity { .. } | TiledOp::WithRegion { .. } => (
            false,
            Term::app(
                global(&["Array", "emptyWithCapacity"]),
                [project(args[0].clone(), "toNat")],
            ),
        ),
        TiledOp::Len => (
            false,
            Term::app(
                global(&["Nat", "toUInt64"]),
                [Term::app(global(&["Array", "size"]), [args[0].clone()])],
            ),
        ),
        TiledOp::TileCount { tile_width } => {
            let w = u64::from(*tile_width);
            let size = Term::app(global(&["Array", "size"]), [args[0].clone()]);
            let num = Term::binary(BinaryOp::Add, size, Term::Nat((w - 1).into()));
            let tiles = Term::binary(BinaryOp::Div, num, Term::Nat(w.into()));
            (false, Term::app(global(&["Nat", "toUInt64"]), [tiles]))
        }
        TiledOp::Get { .. } => (
            false,
            Term::app(
                global(&["GetElem?", "getElem!"]),
                [args[0].clone(), project(args[1].clone(), "toNat")],
            ),
        ),
        TiledOp::ColGet { field, .. } => {
            let row = Term::app(
                global(&["GetElem?", "getElem!"]),
                [args[0].clone(), project(args[1].clone(), "toNat")],
            );
            (
                false,
                Term::Project {
                    base: Box::new(row),
                    projection: Projection::Field(ident(field)?),
                },
            )
        }
        TiledOp::Push { .. } => (
            true,
            Term::app(
                global(&["Array", "push"]),
                [args[0].clone(), args[1].clone()],
            ),
        ),
        TiledOp::Set { .. } => (
            true,
            Term::app(
                global(&["Array", "setIfInBounds"]),
                [
                    args[0].clone(),
                    project(args[1].clone(), "toNat"),
                    args[2].clone(),
                ],
            ),
        ),
        TiledOp::ColSet { field, .. } => {
            let idx = project(args[1].clone(), "toNat");
            let cur = Term::app(
                global(&["GetElem?", "getElem!"]),
                [args[0].clone(), idx.clone()],
            );
            let updated = Term::Update {
                base: Box::new(cur),
                fields: vec![(ident(field)?, args[2].clone())],
            };
            (
                true,
                Term::app(
                    global(&["Array", "setIfInBounds"]),
                    [args[0].clone(), idx, updated],
                ),
            )
        }
        TiledOp::LoadTileVec { .. } => {
            return Err("Lean extraction does not lower SIMD load_tile_vec".into());
        }
    })
}
