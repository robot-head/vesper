//! Fixed-size arrays `[T; N]` and module constants `const NAME: T = e;`
//! (manuscript §5 and §2.2, P3b gaps lane 6, #1091).
//!
//! * **Arrays.** `[T; N]` is the homogeneous tuple of `N` elements of `T`
//!   (the parser spells it [`Type::Tuple`]), so an array is `copy` when `T`
//!   is, is laid out inline as `N` consecutive elements, nests in structs and
//!   tuples, and is read with the tuple's dynamic index `a[i]`: checked, with
//!   `Fail[RuntimeError]` (`IndexOutOfBounds`) unless the index is a constant
//!   below `N` or a dominating guard bounds it. The literals `[a, b, c]` and
//!   `[x; N]` are tuple literals. This module adds what a tuple lacks:
//!   `a.len()` (the constant `N`, a `u64`), and in-place element writes
//!   `a[i] = v`, `a[i] op= v` and `a.set(i, v)` on a mutable place. After
//!   checking, [`lower_world`] rewrites `len` to its constant and every
//!   write to `Assign("[]op", a, (i, v))`, which Place IR carries as
//!   `Op::Write { op: "[]op" }` on the array's place: the evaluator updates
//!   the element (`crate::runtime`) and native code stores it in place
//!   (`src/native_arrays.rs`).
//! * **Constants.** The parser turns `const NAME: T = e;` into the
//!   parameterless function [`CONST_PREFIX`]`NAME`, and [`lower_consts`]
//!   turns every use of `NAME` into a call of it before checking. The
//!   initializer must be a constant expression ([`check_world`]): literals,
//!   arithmetic on them, tuple, array and struct literals, and other
//!   constants. A use of a constant aggregate whose value is a tree of
//!   literals becomes a call of [`TABLE_PREFIX`]`NAME` ([`lower_world`]),
//!   which no pass inlines: natively it is a pointer to read-only data
//!   ([`constant_bytes`]), copied to the stack first unless every use only
//!   reads an element or a field (`TABLE[i]`).
use crate::ast::*;
use crate::check::World;
use crate::layout::{Layout, LayoutKind};
use std::collections::{BTreeMap, BTreeSet};

pub use crate::parser::{ARRAY_SPLAT_BINDING, CONST_PREFIX, MAX_ARRAY_LENGTH};

/// Prefix of the assignment operator of a lowered element write
/// (`"[]="`, `"[]+="`, ...).
pub const WRITE_PREFIX: &str = "[]";
/// Diagnostic: an array operation on a value that is not a mutable array
/// place, or with the wrong operands.
pub const ARRAY_OPERATION: &str = "E1540";
/// Diagnostic: a constant initializer that is not a constant expression.
pub const NOT_CONSTANT: &str = "E1541";
/// Diagnostic: a constant whose name is also a type, constructor, effect or
/// another constant.
pub const CONST_NAME: &str = "E1542";

/// The element type and length of an array type: a non-empty tuple whose
/// elements are all one type.
pub fn element(ty: &Type) -> Option<(&Type, usize)> {
    match ty {
        Type::Tuple(ts) if !ts.is_empty() && ts.iter().all(|t| t == &ts[0]) => {
            Some((&ts[0], ts.len()))
        }
        _ => None,
    }
}

/// Whether `name` is the function of a module constant.
pub fn is_const_function(name: &str) -> bool {
    // A package renames a constant of a module other than its root to
    // `{scope}__{package}__{module}__const$NAME` (`crate::packages`).
    name.starts_with(CONST_PREFIX) || name.contains(&format!("__{CONST_PREFIX}"))
}

/// The source name of a constant's function.
pub fn const_name(function: &str) -> &str {
    function
        .rsplit_once(CONST_PREFIX)
        .map_or(function, |(_, name)| name)
}

/// The module with every use of a constant turned into a call of its
/// function, or `None` when the module declares no constant. The parser
/// already does this ([`crate::parser::lower_const_uses`]); this covers
/// modules built or expanded after parsing.
pub fn lower_consts(module: &Module) -> Option<Module> {
    let mut module = module.clone();
    crate::parser::lower_const_uses(&mut module).then_some(module)
}

pub use crate::parser::walk_exprs as walk;

/// Rewrites the checked array operations of every function into the forms
/// Place IR carries (see the module docs). Runs before IR lowering.
pub fn lower_world(world: &mut World) {
    // Constant aggregates natively read from read-only data.
    let tables = world
        .functions
        .iter()
        .filter(|(name, f)| {
            is_const_function(name)
                && Layout::of(world, &f.result).is_ok_and(|l| l.aggregate())
                && constant_bytes(world, f).is_ok_and(|b| b.is_some())
        })
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    let mut functions = std::mem::take(&mut world.functions);
    for f in functions.values_mut() {
        if let Some(body) = &mut f.body {
            let types = &mut world.types;
            walk(body, &mut |e| {
                lower_expr(e, types);
                if let ExprKind::Call { callee, args, .. } = &mut e.kind
                    && args.is_empty()
                    && let ExprKind::Name(n) = &mut callee.kind
                    && tables.contains(n.as_str())
                {
                    *n = format!("{TABLE_PREFIX}{n}");
                }
            });
        }
    }
    world.functions = functions;
}

/// The callee of a use of a constant aggregate after [`lower_world`]:
/// `$table$` followed by the constant function's name. It is not a function
/// of the World, so no pass inlines it; the evaluator calls the constant's
/// function and native code reads the constant's read-only data.
pub const TABLE_PREFIX: &str = "$table$";

/// The constant function a table read (`$table$` + its name) stands for.
pub fn table_function(callee: &str) -> Option<String> {
    callee.strip_prefix(TABLE_PREFIX).map(str::to_owned)
}

fn type_at<'a>(types: &'a TypeMap, e: &Expr) -> Option<&'a Type> {
    types.get(&(e.span.start, e.span.end))
}

/// Whether evaluating `e` has no effect beyond reading places.
fn place_like(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Name(_) | ExprKind::Literal(_) => true,
        ExprKind::Field(b, _) => place_like(b),
        _ => false,
    }
}

fn lower_expr(e: &mut Expr, types: &mut TypeMap) {
    let span = e.span.clone();
    let pair = |types: &mut TypeMap, at: &Span, index: &Expr, element: &Type| {
        let index_ty = type_at(types, index)
            .map(crate::numeric::default_type)
            .unwrap_or_else(|| Type::named("u64"));
        types.insert(
            (at.start, at.end),
            Type::Tuple(vec![index_ty, element.clone()]),
        );
    };
    match &mut e.kind {
        ExprKind::Call { callee, args, .. } => {
            let ExprKind::Field(base, method) = &mut callee.kind else {
                return;
            };
            let Some((element_ty, n)) = type_at(types, base)
                .and_then(element)
                .map(|(t, n)| (t.clone(), n))
            else {
                return;
            };
            match (method.as_str(), args.len()) {
                ("len", 0) => {
                    let literal = Expr {
                        kind: ExprKind::Literal(Literal::Int(n as i128)),
                        span: span.clone(),
                    };
                    e.kind = if place_like(base) {
                        literal.kind
                    } else {
                        ExprKind::Block {
                            statements: vec![Statement::Expr((**base).clone())],
                            tail: Some(Box::new(literal)),
                        }
                    };
                }
                ("set", 2) => {
                    let base = (**base).clone();
                    let at = callee.span.clone();
                    pair(types, &at, &args[0].expr, &element_ty);
                    let operands = Expr {
                        kind: ExprKind::Tuple(vec![args[0].expr.clone(), args[1].expr.clone()]),
                        span: at,
                    };
                    e.kind = ExprKind::Assign(
                        format!("{WRITE_PREFIX}="),
                        Box::new(base),
                        Box::new(operands),
                    );
                }
                _ => {}
            }
        }
        ExprKind::Assign(op, l, r) if !op.starts_with(WRITE_PREFIX) => {
            let ExprKind::Index(base, index) = &l.kind else {
                return;
            };
            if matches!(index.kind, ExprKind::Range { .. }) {
                return;
            }
            let Some(element_ty) = type_at(types, base)
                .and_then(element)
                .map(|(t, _)| t.clone())
            else {
                return;
            };
            let at = l.span.clone();
            pair(types, &at, index, &element_ty);
            let operands = Expr {
                kind: ExprKind::Tuple(vec![(**index).clone(), (**r).clone()]),
                span: at,
            };
            let base = (**base).clone();
            e.kind = ExprKind::Assign(
                format!("{WRITE_PREFIX}{op}"),
                Box::new(base),
                Box::new(operands),
            );
        }
        _ => {}
    }
}

/// Constant diagnostics: initializers that are not constant expressions
/// and names that collide with another item.
pub fn check_world(world: &World) -> Vec<Diagnostic> {
    let mut out = vec![];
    for (name, f) in &world.functions {
        if !is_const_function(name) {
            continue;
        }
        let source = const_name(name);
        if name.starts_with(CONST_PREFIX)
            && (world.adts.contains_key(source)
                || world.effects.contains_key(source)
                || world.constructor(source).is_some())
        {
            out.push(Diagnostic::new(
                CONST_NAME,
                format!("constant `{source}` has the name of a type, constructor or effect"),
                f.span.clone(),
            ));
        }
        if let Some(body) = &f.body
            && let Some(at) = non_constant(world, body, &mut BTreeSet::new())
        {
            out.push(
                Diagnostic::new(
                    NOT_CONSTANT,
                    format!("the initializer of constant `{source}` is not a constant expression"),
                    at,
                )
                .with_help(
                    "a constant is built from literals, operators, tuple, array and struct \
                     literals, and other constants",
                ),
            );
        }
    }
    out
}

/// The span of the first part of `e` that is not constant, if any.
fn non_constant(world: &World, e: &Expr, bound: &mut BTreeSet<String>) -> Option<Span> {
    match &e.kind {
        ExprKind::Literal(_) => None,
        ExprKind::Name(n) if bound.contains(n) || world.constructor(n).is_some() => None,
        ExprKind::Tuple(es) => es.iter().find_map(|e| non_constant(world, e, bound)),
        ExprKind::Struct { fields, .. } => fields
            .iter()
            .find_map(|(_, e)| non_constant(world, e, bound)),
        ExprKind::Unary(_, v) => non_constant(world, v, bound),
        ExprKind::Binary(_, a, b) => {
            non_constant(world, a, bound).or_else(|| non_constant(world, b, bound))
        }
        ExprKind::Index(a, i) => {
            non_constant(world, a, bound).or_else(|| non_constant(world, i, bound))
        }
        ExprKind::Field(b, _) => non_constant(world, b, bound),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { pat, value, .. } => {
                        if let Some(at) = non_constant(world, value, bound) {
                            return Some(at);
                        }
                        match &pat.kind {
                            PatternKind::Bind(n) => {
                                bound.insert(n.clone());
                            }
                            _ => return Some(pat.span.clone()),
                        }
                    }
                    Statement::Expr(e) => return Some(e.span.clone()),
                }
            }
            tail.as_ref().and_then(|t| non_constant(world, t, bound))
        }
        ExprKind::Call { callee, args, .. } => match &callee.kind {
            // Another constant, or a constructor applied to constants.
            ExprKind::Name(n) if is_const_function(n) && args.is_empty() => None,
            ExprKind::Name(n) if world.constructor(n).is_some() => args
                .iter()
                .find_map(|a| non_constant(world, &a.expr, bound)),
            _ => Some(e.span.clone()),
        },
        _ => Some(e.span.clone()),
    }
}

/// The bytes of constant function `f`'s value in its native layout, when
/// the value is a tree of literals, tuples, arrays, structs and other such
/// constants; `None` for anything else (enums, operators, floats in an
/// `f32` field, ...), whose uses then call the function.
pub fn constant_bytes(world: &World, f: &Function) -> Result<Option<Vec<u8>>, String> {
    let layout = Layout::of(world, &f.result)?;
    let mut out = vec![0u8; layout.size as usize];
    let Some(body) = &f.body else {
        return Ok(None);
    };
    let ok = encode(world, body, &layout, &mut out, 0, &BTreeMap::new(), 0)?;
    Ok(ok.then_some(out))
}

const MAX_CONST_DEPTH: usize = 64;

fn encode(
    world: &World,
    e: &Expr,
    layout: &Layout,
    out: &mut [u8],
    at: usize,
    env: &BTreeMap<String, Expr>,
    depth: usize,
) -> Result<bool, String> {
    if depth > MAX_CONST_DEPTH {
        return Ok(false);
    }
    let word = |out: &mut [u8], w: u64| {
        out[at..at + 8].copy_from_slice(&w.to_ne_bytes());
    };
    match &e.kind {
        ExprKind::Block { statements, tail } => {
            let mut env = env.clone();
            for s in statements {
                match s {
                    Statement::Let {
                        pat:
                            Pattern {
                                kind: PatternKind::Bind(n),
                                ..
                            },
                        value,
                        ..
                    } => {
                        env.insert(n.clone(), value.clone());
                    }
                    _ => return Ok(false),
                }
            }
            match tail {
                Some(t) => encode(world, t, layout, out, at, &env, depth + 1),
                None => Ok(false),
            }
        }
        ExprKind::Name(n) => match env.get(n) {
            Some(v) => encode(world, v, layout, out, at, env, depth + 1),
            None => Ok(false),
        },
        ExprKind::Call { callee, args, .. } if args.is_empty() => match &callee.kind {
            ExprKind::Name(n) if is_const_function(n) || table_function(n).is_some() => match world
                .functions
                .get(&table_function(n).unwrap_or_else(|| n.clone()))
            {
                Some(Function { body: Some(b), .. }) => {
                    encode(world, b, layout, out, at, &BTreeMap::new(), depth + 1)
                }
                _ => Ok(false),
            },
            _ => Ok(false),
        },
        ExprKind::Tuple(es) => {
            let LayoutKind::Product(fields) = &layout.kind else {
                return Ok(false);
            };
            if fields.len() != es.len() || !matches!(layout.ty, Type::Tuple(_)) {
                return Ok(false);
            }
            for (f, e) in fields.iter().zip(es) {
                if !encode(
                    world,
                    e,
                    &f.layout,
                    out,
                    at + f.offset as usize,
                    env,
                    depth + 1,
                )? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ExprKind::Struct { fields: values, .. } => {
            let LayoutKind::Product(fields) = &layout.kind else {
                return Ok(false);
            };
            if fields.len() != values.len() {
                return Ok(false);
            }
            for f in fields {
                let Some((_, e)) = values.iter().find(|(n, _)| *n == f.name) else {
                    return Ok(false);
                };
                if !encode(
                    world,
                    e,
                    &f.layout,
                    out,
                    at + f.offset as usize,
                    env,
                    depth + 1,
                )? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ExprKind::Literal(_) | ExprKind::Unary(..) => {
            let Some(w) = scalar_word(e, &layout.ty) else {
                return Ok(false);
            };
            word(out, w);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The native word of a scalar constant of type `ty`: integers in their
/// canonical sign- or zero-extended form, `bool` as 0 or 1, `f64` bits.
fn scalar_word(e: &Expr, ty: &Type) -> Option<u64> {
    let (negative, literal) = match &e.kind {
        ExprKind::Literal(l) => (false, l),
        ExprKind::Unary(op, v) if op == "-" => match &v.kind {
            ExprKind::Literal(l) => (true, l),
            _ => return None,
        },
        _ => return None,
    };
    match (literal, ty.name()) {
        (Literal::Bool(b), "bool") if !negative => Some(u64::from(*b)),
        (Literal::Float(x), "f64") => Some(if negative { -*x } else { *x }.to_bits()),
        (Literal::Int(n), name) => {
            let kind = crate::numeric::IntegerKind::from_name(name)?;
            let n = if negative { -*n } else { *n };
            kind.fits(n).then_some(n as i64 as u64)
        }
        _ => None,
    }
}
