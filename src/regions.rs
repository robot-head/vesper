//! Source Strata regions: lexical `region` blocks, region-scoped allocation
//! and the §15.2 lifetime and alias rules (design §15).
//!
//! # Surface
//!
//! ```text
//! region r { body }          // default bound: 1 MiB of region memory
//! region r(4096) { body }    // explicit bound in bytes (1 B ..= 1 GiB)
//!
//! r.box(value)      -> RegionBox[r, T]   allocate `value` in region r
//! r.get(b)          -> T                 read a copy payload
//! r.set(b, value)   -> Unit              overwrite a copy payload in place
//! r.take(b)         -> T                 move an affine/linear payload out
//! r.checkpoint()    -> Watermark[r]      linear watermark
//! r.rewind(mark)    -> Unit              discard every allocation after mark
//! r.commit(mark)    -> Unit              drop a watermark without rewinding
//! r.used()          -> u64               bytes currently allocated in r
//!
//! fn f(x: i64) -> RegionBox[r, i64] eff [r: Region] { r.box(x) }
//! ```
//!
//! A region block is the source form of `Region::scoped(λr. body)`. It binds
//! the lexical capability `r : Cap_linear[Region]` in Σ for its body and is
//! encoded, without new expression or IR schema, as a handler of the built-in
//! linear effect [`REGION_EFFECT`] labelled `r` whose return clause binds the
//! reserved [`REGION_MARKER`] (see `vesper_syntax::parser`). Functions that
//! allocate in a caller's region name it in their row, `eff [r: Region]`,
//! exactly like any other lexical capability; no `'a` lifetime appears.
//!
//! # Typing (checked by `check.rs` hooks into this module)
//!
//! * `RegionBox[r, T]` is represented as `RegionBox[eff [r: Region], T]`: its
//!   first argument is the region capability itself, so every existing
//!   capability rule applies to it. **[T-RegionScope]**: a region block whose
//!   result type mentions its own capability is rejected (E0791); this covers
//!   boxes, watermarks, closures that capture `r`, and tuples or results that
//!   contain any of them. A box therefore never outlives its block, and once
//!   the block ends no expression can name `r`, so no box can be read after
//!   its region ends.
//! * **[T-RegionAlloc]**: `r.box(e)` requires `r` in Σ and records `r` in the
//!   effect row; a function that allocates in `r` must declare `eff [r:
//!   Region]`, and the block discharges `r` from the enclosing row.
//! * Every access goes through the capability: a box of region `a` is
//!   rejected by an operation of region `b` (E0792), including through
//!   function parameters, whose region labels are instantiated per call.
//! * Modes (§15.2 "Substructural Modal Preservation"): `RegionBox[r, T]` has
//!   the mode of `T`. Copy payloads give copy handles that may alias freely
//!   (DAGs inside the region) and are read with `get` and written with `set`;
//!   affine and linear payloads give unique handles that are consumed by
//!   `take` (E0793, E0794). A linear payload must be taken before the region
//!   ends, by the ordinary linear exhaustion rule (E0302), and `rewind` is
//!   rejected while a linear box of the same region is live (E0795).
//! * Watermarks are linear and region-branded; they must be rewound or
//!   committed. A box bound after `let m = r.checkpoint();` in the same
//!   block cannot be used after `r.rewind(m);` (E0796).
//!
//! # Runtime
//!
//! Each region block owns a [`vesper_alloc::Region`] bounded by its capacity.
//! The evaluator stores payload values in it and the native JIT stores scalar
//! payloads through the `vesper_region_*` runtime calls in [`native_runtime`].
//! Both close the region when the block returns. Handles that outlive their
//! allocation by any path the static rules do not cover are caught by the
//! allocator's region/serial checks and reported as runtime errors, never as
//! reads of reclaimed memory.
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};

pub use vesper_syntax::parser::{MAX_REGION_CAPACITY, REGION_EFFECT, REGION_MARKER};

/// `RegionBox[r, T]`.
pub const BOX_TYPE: &str = "RegionBox";
/// `Watermark[r]`.
pub const WATERMARK_TYPE: &str = "Watermark";
/// Bound of a `region r { ... }` block without an explicit capacity.
pub const DEFAULT_CAPACITY: usize = 1 << 20;
/// Region operations, in declaration order.
pub const OPERATIONS: [&str; 8] = [
    "box",
    "get",
    "set",
    "take",
    "checkpoint",
    "rewind",
    "commit",
    "used",
];

/// Declaration of the built-in effect, added to a module's World when the
/// module uses regions. Its operation signatures fix argument passing modes
/// for the Place IR; their types are placeholders that the checker replaces
/// with the generic region typing rules.
const PRELUDE: &str = "linear effect Region { fn box(sink value: Unit) -> Unit; fn get(in boxed: Unit) -> Unit; fn set(in boxed: Unit, sink value: Unit) -> Unit; fn take(sink boxed: Unit) -> Unit; fn checkpoint() -> Unit; fn rewind(sink mark: Unit) -> Unit; fn commit(sink mark: Unit) -> Unit; fn used() -> u64; }";

/// Whether the World's `Region` effect is the built-in one (a module that
/// declares its own `effect Region` and uses no region syntax keeps it).
pub fn active(world: &crate::check::World) -> bool {
    world.effects.get(REGION_EFFECT).is_some_and(|d| {
        d.linear && d.generics.is_empty() && d.ops.iter().map(|f| f.name.as_str()).eq(OPERATIONS)
    })
}

/// Whether the module needs the built-in region prelude: it uses region
/// syntax or region types, or names `Region` in a row without declaring an
/// effect of that name itself.
pub fn needs_prelude(module: &Module) -> bool {
    let declares = module
        .items
        .iter()
        .any(|i| matches!(i, Item::Effect(e) if e.name == REGION_EFFECT));
    if !declares {
        return uses_regions(module);
    }
    module.items.iter().any(|item| match item {
        Item::Function(f) => f.body.as_ref().is_some_and(syntax_uses) || signature_types_use(f),
        Item::Impl { methods, .. } => methods
            .iter()
            .any(|f| f.body.as_ref().is_some_and(syntax_uses) || signature_types_use(f)),
        _ => false,
    })
}
fn signature_types_use(f: &Function) -> bool {
    fn handle_types(t: &Type) -> bool {
        match t {
            Type::Named(n, args) => is_handle_name(n) || args.iter().any(handle_types),
            Type::Tuple(ts) => ts.iter().any(handle_types),
            Type::Function { params, result, .. } => {
                params.iter().any(|(_, t)| handle_types(t)) || handle_types(result)
            }
            _ => false,
        }
    }
    handle_types(&f.result) || f.params.iter().any(|p| handle_types(&p.ty))
}
/// Region blocks in an expression.
fn syntax_uses(e: &Expr) -> bool {
    let mut found = false;
    visit_expr(e, &mut |e| {
        if let ExprKind::Handle { effect, ret, .. } = &e.kind {
            found |= block_capacity(effect, ret).is_some();
        }
    });
    found
}

/// Items of the built-in region prelude.
pub fn prelude() -> Vec<Item> {
    crate::parser::parse(PRELUDE).module.items
}

/// The capacity of a region block, or `None` when the handler is not one.
pub fn block_capacity(effect: &Effect, ret: &Clause) -> Option<usize> {
    if effect.name != REGION_EFFECT {
        return None;
    }
    let [
        Pattern {
            kind: PatternKind::Bind(name),
            ..
        },
    ] = ret.pats.as_slice()
    else {
        return None;
    };
    let rest = name.strip_prefix(REGION_MARKER)?;
    if rest.is_empty() {
        return Some(DEFAULT_CAPACITY);
    }
    rest.strip_prefix(':')?.parse().ok()
}

/// Whether the module mentions regions anywhere, so that the built-in
/// [`REGION_EFFECT`] must be declared in its World.
pub fn uses_regions(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(f) => function_uses(f),
        Item::Impl { methods, .. } => methods.iter().any(function_uses),
        Item::Adt(a) => a
            .variants
            .iter()
            .flat_map(|v| &v.fields)
            .any(|f| type_uses(&f.ty)),
        Item::Effect(e) => e.ops.iter().any(function_uses),
        Item::Trait(_) | Item::Conformance(_) => false,
    })
}
fn function_uses(f: &Function) -> bool {
    effects_use(&f.effects)
        || type_uses(&f.result)
        || f.params.iter().any(|p| type_uses(&p.ty))
        || f.body.as_ref().is_some_and(expr_uses)
}
fn effects_use(e: &Effects) -> bool {
    e.instances
        .iter()
        .any(|e| e.name == REGION_EFFECT || e.args.iter().any(type_uses))
}
fn type_uses(t: &Type) -> bool {
    match t {
        Type::Named(n, args) => n == BOX_TYPE || n == WATERMARK_TYPE || args.iter().any(type_uses),
        Type::Tuple(ts) => ts.iter().any(type_uses),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => params.iter().any(|(_, t)| type_uses(t)) || type_uses(result) || effects_use(effects),
        Type::Row(e) => effects_use(e),
        Type::IntLiteral(_) | Type::Infer => false,
    }
}
fn expr_uses(e: &Expr) -> bool {
    let mut found = false;
    visit_expr(e, &mut |e| {
        found |= match &e.kind {
            ExprKind::Handle { effect, .. } => effect.name == REGION_EFFECT,
            ExprKind::Closure {
                params,
                result,
                effects,
                ..
            } => {
                effects_use(effects) || type_uses(result) || params.iter().any(|p| type_uses(&p.ty))
            }
            ExprKind::Block { statements, .. } => statements
                .iter()
                .any(|s| matches!(s, Statement::Let { ty, .. } if type_uses(ty))),
            ExprKind::Call { types, .. } => types.iter().any(type_uses),
            _ => false,
        };
    });
    found
}

/// Calls `f` on `e` and every nested expression, outermost first.
pub fn visit_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es.iter().for_each(|e| visit_expr(e, f)),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, e)| visit_expr(e, f)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => visit_expr(value, f),
                    Statement::Expr(e) => visit_expr(e, f),
                }
            }
            if let Some(t) = tail {
                visit_expr(t, f);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            visit_expr(callee, f);
            args.iter().for_each(|a| visit_expr(&a.expr, f));
        }
        ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => visit_expr(e, f),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            visit_expr(a, f);
            visit_expr(b, f);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            visit_expr(condition, f);
            visit_expr(then, f);
            if let Some(o) = otherwise {
                visit_expr(o, f);
            }
        }
        ExprKind::Match { value, arms } => {
            visit_expr(value, f);
            arms.iter().for_each(|(_, e)| visit_expr(e, f));
        }
        ExprKind::Closure { body, .. } => visit_expr(body, f),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                visit_expr(s, f);
            }
            visit_expr(body, f);
            visit_expr(&ret.body, f);
            ops.iter().for_each(|c| visit_expr(&c.body, f));
        }
    }
}

// ----- type representation ---------------------------------------------------

/// The capability argument `eff [label: Region]` of a region type.
pub fn region_row(label: &str) -> Type {
    Type::Row(Effects {
        instances: vec![Effect {
            label: Some(label.into()),
            name: REGION_EFFECT.into(),
            args: vec![],
        }],
        tail: None,
    })
}
/// `RegionBox[label, payload]`.
pub fn box_type(label: &str, payload: Type) -> Type {
    Type::Named(BOX_TYPE.into(), vec![region_row(label), payload])
}
/// `Watermark[label]`.
pub fn watermark_type(label: &str) -> Type {
    Type::Named(WATERMARK_TYPE.into(), vec![region_row(label)])
}
/// The capability label of a normalized region argument.
pub fn row_label(t: &Type) -> Option<&str> {
    match t {
        Type::Row(Effects {
            instances,
            tail: None,
        }) => match instances.as_slice() {
            [
                Effect {
                    label: Some(l),
                    name,
                    args,
                },
            ] if name == REGION_EFFECT && args.is_empty() => Some(l),
            _ => None,
        },
        _ => None,
    }
}
/// `(region label, payload)` of a normalized `RegionBox`.
pub fn box_parts(t: &Type) -> Option<(&str, &Type)> {
    match t {
        Type::Named(n, args) if n == BOX_TYPE => match args.as_slice() {
            [r, payload] => Some((row_label(r)?, payload)),
            _ => None,
        },
        _ => None,
    }
}
/// The region label of a normalized `Watermark`.
pub fn watermark_label(t: &Type) -> Option<&str> {
    match t {
        Type::Named(n, args) if n == WATERMARK_TYPE => match args.as_slice() {
            [r] => row_label(r),
            _ => None,
        },
        _ => None,
    }
}
/// Region handle types are one machine word in every backend.
pub fn is_handle_type(t: &Type) -> bool {
    matches!(t, Type::Named(n, _) if is_handle_name(n))
}
pub fn is_handle_name(n: &str) -> bool {
    n == BOX_TYPE || n == WATERMARK_TYPE
}

/// Rewrites source region arguments (`RegionBox[r, T]`, `Watermark[r]`,
/// where `r` is a lexical capability label) to their capability form, using
/// `labels` from source labels to capability identities. Other types are
/// unchanged; an unknown label is left in source form and rejected by
/// [`validate_type`].
pub fn normalize(t: &Type, labels: &BTreeMap<String, String>) -> Type {
    match t {
        Type::Named(n, args) if is_handle_name(n) => {
            let mut args: Vec<Type> = args.iter().map(|t| normalize(t, labels)).collect();
            if let Some(cap) = args
                .first()
                .and_then(source_name)
                .and_then(|l| labels.get(l))
            {
                args[0] = region_row(cap);
            }
            Type::Named(n.clone(), args)
        }
        Type::Named(n, args) => Type::Named(
            n.clone(),
            args.iter().map(|t| normalize(t, labels)).collect(),
        ),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| normalize(t, labels)).collect()),
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => Type::Function {
            params: params
                .iter()
                .map(|(p, t)| (*p, normalize(t, labels)))
                .collect(),
            result: Box::new(normalize(result, labels)),
            effects: effects.clone(),
            mode: *mode,
        },
        _ => t.clone(),
    }
}

/// A region argument as written in source: the parser reads the lower-case
/// `r` of `RegionBox[r, T]` as a row variable (or, in other positions, as a
/// type name).
fn source_name(t: &Type) -> Option<&str> {
    match t {
        Type::Named(l, a) if a.is_empty() => Some(l),
        Type::Row(Effects {
            instances,
            tail: Some(l),
        }) if instances.is_empty() => Some(l),
        _ => None,
    }
}

/// Validates a region type; `None` when `t` is not one.
pub fn validate_type(t: &Type, span: Span) -> Option<Vec<Diagnostic>> {
    let Type::Named(n, args) = t else {
        return None;
    };
    if !is_handle_name(n) {
        return None;
    }
    let arity = if n == BOX_TYPE { 2 } else { 1 };
    let mut out = vec![];
    if args.len() != arity {
        out.push(
            Diagnostic::new(
                "E0797",
                format!("{n} expects {arity} type arguments, found {}", args.len()),
                span,
            )
            .with_help(if n == BOX_TYPE {
                "write RegionBox[r, T] for a T allocated in region r"
            } else {
                "write Watermark[r] for a checkpoint of region r"
            }),
        );
    } else if row_label(&args[0]).is_none() {
        out.push(
            Diagnostic::new(
                "E0797",
                format!(
                    "the first argument of {n} must name a region capability in scope, found {}",
                    display(&args[0])
                ),
                span,
            )
            .with_help("bind one with `region r { ... }` or declare `eff [r: Region]`"),
        );
    }
    Some(out)
}

/// Readable rendering of a type with region capabilities.
pub fn display(t: &Type) -> String {
    if let Some(label) = row_label(t) {
        return source_label(label);
    }
    if let Some(name) = source_name(t).filter(|_| matches!(t, Type::Row(_))) {
        return name.into();
    }
    match t {
        Type::Named(n, args) if !args.is_empty() => format!(
            "{n}[{}]",
            args.iter().map(display).collect::<Vec<_>>().join(", ")
        ),
        Type::Tuple(ts) => format!(
            "({})",
            ts.iter().map(display).collect::<Vec<_>>().join(", ")
        ),
        _ => t.to_string(),
    }
}
/// Human-readable name of a capability identity.
fn source_label(label: &str) -> String {
    if let Some(start) = label.strip_prefix("$cap") {
        format!("region@{start}")
    } else if let Some(rest) = label.strip_prefix("$fncap:") {
        let (function, _) = rest.rsplit_once(':').unwrap_or((rest, ""));
        format!("region of {function}")
    } else if label.starts_with('$') {
        "region".into()
    } else {
        label.into()
    }
}

/// Whether either side of a type mismatch involves regions, with a
/// region-specific diagnostic when it does.
pub fn mismatch(expected: &Type, actual: &Type, span: Span) -> Option<Diagnostic> {
    fn mentions(t: &Type) -> bool {
        match t {
            Type::Named(n, args) => is_handle_name(n) || args.iter().any(mentions),
            Type::Tuple(ts) => ts.iter().any(mentions),
            _ => false,
        }
    }
    if !mentions(expected) && !mentions(actual) {
        return None;
    }
    let cross = matches!(
        (box_parts(expected), box_parts(actual)),
        (Some((a, _)), Some((b, _))) if a != b
    ) || matches!(
        (watermark_label(expected), watermark_label(actual)),
        (Some(a), Some(b)) if a != b
    );
    Some(if cross {
        Diagnostic::new(
            "E0792",
            format!(
                "expected {}, found {}: a region reference belongs to the region that allocated it",
                display(expected),
                display(actual)
            ),
            span,
        )
    } else {
        Diagnostic::new(
            "E0200",
            format!("expected {}, found {}", display(expected), display(actual)),
            span,
        )
    })
}

// ----- post-check flow rule ---------------------------------------------------

/// Rules checked on the typed World after the core checker: use of a box
/// after a rewind past its allocation (E0796).
pub fn check_world(world: &crate::check::World) -> Vec<Diagnostic> {
    if !active(world) {
        return vec![];
    }
    let mut out = vec![];
    for f in world.functions.values() {
        if let Some(body) = &f.body {
            visit_expr(body, &mut |e| {
                if let ExprKind::Block { statements, tail } = &e.kind {
                    rewind_flow(world, statements, tail.as_deref(), &mut out);
                }
            });
        }
    }
    out
}
fn ty_of<'a>(world: &'a crate::check::World, e: &Expr) -> Option<&'a Type> {
    world.types.get(&(e.span.start, e.span.end))
}
/// Straight-line rewind analysis of one block: the boxes of region `c` bound
/// by `let` after `let m = r.checkpoint();` are dead after `r.rewind(m);`.
fn rewind_flow(
    world: &crate::check::World,
    statements: &[Statement],
    tail: Option<&Expr>,
    out: &mut Vec<Diagnostic>,
) {
    // mark name -> (region capability, boxes bound since)
    let mut marks: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    // dead box name -> span of the rewind that invalidated it
    let mut dead: BTreeMap<String, Span> = BTreeMap::new();
    let report = |e: &Expr, dead: &BTreeMap<String, Span>, out: &mut Vec<Diagnostic>| {
        let used = crate::check::free_names(e);
        for (name, rewind) in dead {
            if used.contains(name) {
                out.push(
                    Diagnostic::new(
                        "E0796",
                        format!(
                            "region box '{name}' is used after a rewind discarded its allocation"
                        ),
                        e.span.clone(),
                    )
                    .with_related(rewind.clone(), "rewound past the allocation here")
                    .with_help(
                        "read the value before rewinding, or allocate it before the checkpoint",
                    ),
                );
            }
        }
    };
    for s in statements {
        match s {
            Statement::Let { pat, value, .. } => {
                report(value, &dead, out);
                let names = pattern_binds(pat);
                for n in &names {
                    dead.remove(n);
                    marks.remove(n);
                    for (_, boxes) in marks.values_mut() {
                        boxes.retain(|b| b != n);
                    }
                }
                let ty = ty_of(world, value);
                if let [name] = names.as_slice() {
                    if let Some(label) = ty.and_then(watermark_label) {
                        marks.insert(name.clone(), (label.to_owned(), vec![]));
                    } else if let Some((label, _)) = ty.and_then(box_parts) {
                        for (region, boxes) in marks.values_mut() {
                            if region == label {
                                boxes.push(name.clone());
                            }
                        }
                    }
                }
            }
            Statement::Expr(e) => {
                report(e, &dead, out);
                if let ExprKind::Call { callee, args, .. } = &e.kind
                    && let ExprKind::Field(_, op) = &callee.kind
                    && op == "rewind"
                    && let [arg] = args.as_slice()
                    && let ExprKind::Name(mark) = &arg.expr.kind
                    && let Some((_, boxes)) = marks.remove(mark)
                {
                    for b in boxes {
                        dead.insert(b, e.span.clone());
                    }
                }
            }
        }
    }
    if let Some(t) = tail {
        report(t, &dead, out);
    }
}
fn pattern_binds(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatternKind::Bind(n) => vec![n.clone()],
        PatternKind::Tuple(ps) => ps.iter().flat_map(pattern_binds).collect(),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().flat_map(|(_, p)| pattern_binds(p)).collect()
        }
        PatternKind::Wild | PatternKind::Literal(_) => vec![],
    }
}

/// Source labels of the region capabilities a function signature declares.
pub fn signature_labels(f: &Function) -> BTreeSet<String> {
    f.effects
        .instances
        .iter()
        .filter(|e| e.name == REGION_EFFECT)
        .filter_map(|e| e.label.clone())
        .collect()
}

pub mod native_runtime;
pub mod store;
