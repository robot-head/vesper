//! Zero-RC functional-but-in-place (`@fip`) constructor reuse (§12.4).
//!
//! # Surface
//!
//! * `@fip fn name(...) -> T eff [...]` asserts that the function performs zero
//!   heap allocations and zero deallocations on all execution paths.
//! * `Box[T]` is the built-in owning heap-indirection type (`affine` by default,
//!   or `linear` when `T` is linear), activated conditionally when a module uses
//!   `@fip` or `Box` without declaring its own `Box` type.
//! * `Box::new(value)` constructs a `Box[T]` (reusing a compile-time `ReuseSlot`
//!   when called inside `@fip`).
//! * `Box::new_in_caller(value)` constructs the `@fip` function's returned
//!   `Box[T]` directly into the caller-supplied return slot.
//! * `Box(pat)` destructures a `Box[T]` in `match` or `let` patterns, yielding a
//!   compile-time `ReuseSlot` for `T` when unpacked from an owned value inside
//!   `@fip`.

use crate::{
    ast::*,
    check::{Place, World},
    ir::ValueId,
    layout::Layout,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod aot_runtime;
pub mod native_runtime;

pub const BOX_TYPE: &str = "Box";
pub const BOX_NEW: &str = "Box::new";
pub const BOX_NEW_IN_CALLER: &str = "Box::new_in_caller";
pub const ALLOC_EFFECT: &str = "Alloc";

/// Place-IR operations lowered from `Box::new` and `Box::new_in_caller` and
/// rewritten by Stage 4 `egglog` constructor-reuse optimization (§12.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum FipOp {
    /// Allocate a `Box[T]` on the heap (non-`@fip`), or an unpaired `@fip`
    /// allocation prior to Stage 4 reuse matching.
    BoxNew,
    /// Write `T` directly into the caller-supplied `$caller_slot` buffer.
    BoxNewInCaller,
    /// Zero-allocation in-place reuse of the pointer unpacked from `Bind`
    /// instruction `bind_dest` at preorder slot `slot_index`.
    BoxReuse {
        bind_dest: ValueId,
        slot_index: usize,
        size: u32,
    },
}

/// Caller-slot source chosen for a call to an `@fip` function returning via
/// `Box::new_in_caller`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum CallSlot {
    /// Forward the enclosing `@fip` function's `$caller_slot` in tail position.
    CallerSlot,
    /// Pass the unpacked pointer from an earlier `Op::Bind` `ReuseSlot`.
    ReuseSlot {
        bind_dest: ValueId,
        slot_index: usize,
        size: u32,
    },
}

/// Selected `ReuseSlot` pointer merged across incoming CFG edges at `join_block`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FipJoinSlot {
    pub dest_slot: (ValueId, usize),
    pub incoming: Vec<(usize, ValueId, usize)>,
}

/// Ownership-driven `Box[T]` reclamation emitted for non-`@fip` functions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum FipDrop {
    Value(ValueId, Type),
    Place(Place, Type),
    OtherFields {
        value: ValueId,
        ty: Type,
        keep: String,
    },
}

/// Whether the module needs the built-in `Box[T]` prelude: it uses `@fip` or
/// `Box` without declaring its own `struct Box` / `enum Box`.
pub fn needs_prelude(module: &Module) -> bool {
    let declares_box = module
        .items
        .iter()
        .any(|item| matches!(item, Item::Adt(a) if a.name == BOX_TYPE));
    if declares_box {
        return false;
    }
    module.items.iter().any(|item| match item {
        Item::Function(f) => f.fip || function_uses_box(f),
        Item::Impl { methods, .. } => methods.iter().any(|f| f.fip || function_uses_box(f)),
        Item::Adt(a) => a
            .variants
            .iter()
            .flat_map(|v| &v.fields)
            .any(|f| type_mentions_box(&f.ty)),
        Item::Effect(e) => e.ops.iter().any(function_uses_box),
        Item::Trait(t) => t.methods.iter().any(|m| m.fip || function_uses_box(m)),
        Item::Conformance(c) => c.methods.iter().any(|m| m.fip || function_uses_box(m)),
    })
}

/// Built-in `Box[T]` ADT definition injected when [`needs_prelude`] is true.
pub fn prelude() -> Vec<Item> {
    vec![Item::Adt(Adt {
        name: BOX_TYPE.into(),
        mode: Mode::Affine,
        generics: vec![Generic {
            name: "T".into(),
            bound: Mode::Linear,
            row: false,
        }],
        variants: vec![Variant {
            name: BOX_TYPE.into(),
            fields: vec![Field {
                name: "0".into(),
                ty: Type::Named("T".into(), vec![]),
                modality: None,
                temperature: None,
            }],
            named: false,
        }],
        is_enum: false,
        tile_width: None,
        span: 0..0,
    })]
}

/// Whether the World uses the built-in `Box[T]` prelude (rather than a user
/// `struct Box[T]`).
pub fn active(world: &World) -> bool {
    world.adts.get(BOX_TYPE).is_some_and(|a| {
        !a.is_enum
            && a.mode == Mode::Affine
            && a.span == (0..0)
            && a.generics.len() == 1
            && a.variants.len() == 1
            && !a.variants[0].named
            && a.variants[0].name == BOX_TYPE
            && a.variants[0].fields.len() == 1
            && a.variants[0].fields[0].name == "0"
    })
}

thread_local! {
    /// While [`handle_drop_plan`] runs: the `.vsys` handle types whose
    /// drops the elaboration plans in place of `Box[T]`.
    static DROPPED_HANDLES: std::cell::RefCell<Option<BTreeSet<String>>> =
        const { std::cell::RefCell::new(None) };
}
fn dropped_handles<R>(f: impl FnOnce(Option<&BTreeSet<String>>) -> R) -> R {
    DROPPED_HANDLES.with(|h| f(h.borrow().as_ref()))
}

/// Whether `ty` is the built-in `Box[T]`.
pub fn is_box_type(world: &World, ty: &Type) -> bool {
    if let Some(handle) = dropped_handles(|h| {
        h.map(|h| matches!(ty, Type::Named(n, args) if args.is_empty() && h.contains(n)))
    }) {
        return handle;
    }
    active(world) && matches!(ty, Type::Named(n, args) if n == BOX_TYPE && args.len() == 1)
}

/// Inner type `T` when `ty` is built-in `Box[T]`.
pub fn box_inner<'a>(world: &World, ty: &'a Type) -> Option<&'a Type> {
    if !active(world) || dropped_handles(|h| h.is_some()) {
        return None;
    }
    match ty {
        Type::Named(n, args) if n == BOX_TYPE && args.len() == 1 => Some(&args[0]),
        _ => None,
    }
}

/// Whether a pattern unpacks the built-in `Box(...)` constructor.
pub fn is_box_pattern(world: &World, pat: &Pattern) -> bool {
    active(world)
        && !dropped_handles(|h| h.is_some())
        && matches!(
            &pat.kind,
            PatternKind::Constructor { name, fields, .. }
                if matches!(name.as_str(), "Box" | "Box::Box")
                    && fields.len() == 1
                    && fields[0].0 == "0"
        )
}

/// Whether `ty` holds a built-in `Box[T]` anywhere (native backends check
/// [`loop_drops_elaborated`] for such a graph with loops or early exits).
pub fn mentions_box(world: &World, ty: &Type) -> bool {
    active(world) && type_mentions_box(ty)
}

fn type_mentions_box(t: &Type) -> bool {
    match t {
        Type::Named(n, args) => n == BOX_TYPE || args.iter().any(type_mentions_box),
        Type::Tuple(ts) => ts.iter().any(type_mentions_box),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| type_mentions_box(t))
                || type_mentions_box(result)
                || effects
                    .instances
                    .iter()
                    .any(|e| e.args.iter().any(type_mentions_box))
        }
        Type::Row(e) => e
            .instances
            .iter()
            .any(|e| e.args.iter().any(type_mentions_box)),
        Type::IntLiteral(_) | Type::Infer => false,
    }
}

fn pattern_mentions_box(p: &Pattern) -> bool {
    match &p.kind {
        PatternKind::Tuple(ps) => ps.iter().any(pattern_mentions_box),
        PatternKind::Constructor { name, fields, .. } => {
            matches!(name.as_str(), "Box" | "Box::Box")
                || fields.iter().any(|(_, sub)| pattern_mentions_box(sub))
        }
        PatternKind::Wild | PatternKind::Bind(_) | PatternKind::Literal(_) => false,
    }
}

fn expr_uses_box(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) => false,
        ExprKind::Name(n) => matches!(n.as_str(), "Box" | "Box::new" | "Box::new_in_caller"),
        ExprKind::Tuple(es) => es.iter().any(expr_uses_box),
        ExprKind::Struct { name, fields } => {
            name == BOX_TYPE || fields.iter().any(|(_, e)| expr_uses_box(e))
        }
        ExprKind::Field(b, _) | ExprKind::Unary(_, b) | ExprKind::Try(b) => expr_uses_box(b),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            expr_uses_box(a) || expr_uses_box(b)
        }
        ExprKind::Block { statements, tail } => {
            statements.iter().any(|s| match s {
                Statement::Let { pat, ty, value, .. } => {
                    pattern_mentions_box(pat) || type_mentions_box(ty) || expr_uses_box(value)
                }
                Statement::Expr(e) => expr_uses_box(e),
            }) || tail.as_deref().is_some_and(expr_uses_box)
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            expr_uses_box(callee)
                || types.iter().any(type_mentions_box)
                || args.iter().any(|a| expr_uses_box(&a.expr))
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            expr_uses_box(condition)
                || expr_uses_box(then)
                || otherwise.as_deref().is_some_and(expr_uses_box)
        }
        ExprKind::Match { value, arms } => {
            expr_uses_box(value)
                || arms
                    .iter()
                    .any(|(p, e)| pattern_mentions_box(p) || expr_uses_box(e))
        }
        k @ (ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Continue
        | ExprKind::Return(_)) => k
            .control_children()
            .into_iter()
            .flatten()
            .any(expr_uses_box),
        ExprKind::Closure {
            params,
            result,
            body,
            ..
        } => {
            params.iter().any(|p| type_mentions_box(&p.ty))
                || type_mentions_box(result)
                || expr_uses_box(body)
        }
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            state
                .as_ref()
                .is_some_and(|(p, e)| pattern_mentions_box(p) || expr_uses_box(e))
                || expr_uses_box(body)
                || ret.pats.iter().any(pattern_mentions_box)
                || expr_uses_box(&ret.body)
                || ops
                    .iter()
                    .any(|c| c.pats.iter().any(pattern_mentions_box) || expr_uses_box(&c.body))
        }
    }
}

fn function_uses_box(f: &Function) -> bool {
    type_mentions_box(&f.result)
        || f.params.iter().any(|p| type_mentions_box(&p.ty))
        || f.body.as_ref().is_some_and(expr_uses_box)
}

/// Whether `ty` is `Box[T]` or structurally contains a `Box[T]` field/element.
pub fn type_contains_box(world: &World, ty: &Type) -> bool {
    fn visit(world: &World, ty: &Type, seen: &mut BTreeSet<String>) -> bool {
        if is_box_type(world, ty) {
            return true;
        }
        match ty {
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if args.iter().any(|a| visit(world, a, seen)) {
                    return true;
                }
                let Some(a) = world.adts.get(n) else {
                    return false;
                };
                if !seen.insert(n.clone()) {
                    return false;
                }
                let vars: BTreeMap<String, Type> = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                let found = a.variants.iter().any(|v| {
                    v.fields
                        .iter()
                        .any(|f| visit(world, &substitute(&f.ty, &vars), seen))
                });
                seen.remove(n);
                found
            }
            Type::Function { .. } | Type::Row(_) | Type::IntLiteral(_) | Type::Infer => false,
        }
    }
    visit(world, ty, &mut BTreeSet::new())
}

/// Preorder list of `(field_path, inner_type)` for every `Box(...)` constructor
/// unpacked inside `pat` at scrutinised type `ty`.
///
/// The path entries match `crate::ir::Op::Bind` / `native::bind_ir` field
/// components so runtime/native codegen and Stage 4 `egglog` slot indices match
/// 1-to-1.
pub fn pattern_box_slots(world: &World, pat: &Pattern, ty: &Type) -> Vec<(Vec<String>, Type)> {
    fn walk(
        world: &World,
        pat: &Pattern,
        ty: &Type,
        path: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, Type)>,
    ) {
        match &pat.kind {
            PatternKind::Tuple(ps) => {
                let ts = match ty {
                    Type::Tuple(ts) => ts.as_slice(),
                    _ => &[],
                };
                for (i, p) in ps.iter().enumerate() {
                    if let Some(t) = ts.get(i) {
                        path.push(i.to_string());
                        walk(world, p, t, path, out);
                        path.pop();
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if is_box_pattern(world, pat) {
                    let inner = box_inner(world, ty).cloned().unwrap_or(Type::Infer);
                    path.push("0".into());
                    out.push((path.clone(), inner.clone()));
                    walk(world, &fields[0].1, &inner, path, out);
                    path.pop();
                    return;
                }
                let Some((a, v)) = world.constructor(name) else {
                    return;
                };
                let vars: BTreeMap<String, Type> = match ty {
                    Type::Named(_, args) => a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect(),
                    _ => BTreeMap::new(),
                };
                for (field_name, sub) in fields {
                    if let Some(f) = v.fields.iter().find(|f| &f.name == field_name) {
                        let ft = substitute(&f.ty, &vars);
                        path.push(field_name.clone());
                        walk(world, sub, &ft, path, out);
                        path.pop();
                    }
                }
            }
            PatternKind::Wild | PatternKind::Bind(_) | PatternKind::Literal(_) => {}
        }
    }
    let mut out = vec![];
    let mut path = vec![];
    walk(world, pat, ty, &mut path, &mut out);
    out
}

/// Set of `@fip` functions that consume `$caller_slot` (directly via
/// `Box::new_in_caller` or indirectly by tail-forwarding to another such `@fip`
/// function).
pub fn caller_slot_functions(world: &World) -> BTreeSet<String> {
    fn mentions_new_in_caller(e: &Expr) -> bool {
        let mut found = false;
        crate::regions::visit_expr(e, &mut |sub| {
            if let ExprKind::Call { callee, .. } = &sub.kind
                && matches!(&callee.kind, ExprKind::Name(n) if n == BOX_NEW_IN_CALLER)
            {
                found = true;
            }
        });
        found
    }
    fn tail_calls<'a>(e: &'a Expr, out: &mut Vec<&'a str>) {
        match &e.kind {
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Name(n) = &callee.kind {
                    out.push(n.as_str());
                }
            }
            ExprKind::Block {
                tail: Some(tail), ..
            } => tail_calls(tail, out),
            ExprKind::If {
                then, otherwise, ..
            } => {
                tail_calls(then, out);
                if let Some(o) = otherwise {
                    tail_calls(o, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for (_, body) in arms {
                    tail_calls(body, out);
                }
            }
            _ => {}
        }
    }

    let mut set: BTreeSet<String> = world
        .functions
        .values()
        .filter(|f| f.fip && f.body.as_ref().is_some_and(mentions_new_in_caller))
        .map(|f| f.name.clone())
        .collect();
    loop {
        let mut changed = false;
        for f in world.functions.values() {
            if !f.fip || set.contains(&f.name) || !is_box_type(world, &f.result) {
                continue;
            }
            if let Some(body) = &f.body {
                let mut calls = vec![];
                tail_calls(body, &mut calls);
                if calls.into_iter().any(|c| set.contains(c)) {
                    set.insert(f.name.clone());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    set
}

/// Whether `@fip` function `f` expects a `$caller_slot` parameter in native ABI.
pub fn uses_caller_slot(world: &World, f: &Function) -> bool {
    f.fip && caller_slot_functions(world).contains(&f.name)
}

#[derive(Clone, Debug)]
struct BindingState {
    ty: Type,
    span: Span,
    moved_paths: Vec<Vec<String>>,
}

#[derive(Clone, Debug)]
struct FipState {
    slots: Vec<(Type, Span)>,
    caller_slot_used: bool,
    bindings: BTreeMap<String, BindingState>,
    diverged: bool,
}

impl FipState {
    fn mark_moved(&mut self, place: &Place) {
        if let Some(b) = self.bindings.get_mut(&place.root)
            && !b.moved_paths.contains(&place.path)
        {
            b.moved_paths.push(place.path.clone());
        }
    }
}

/// Canonicalizes generic type parameters to positional names so generic slot
/// matching compares structurally, and uses concrete `Layout::of` size when
/// neither type contains type parameters.
fn slots_match(world: &World, generics: &[Generic], a: &Type, b: &Type) -> Result<bool, String> {
    fn has_generic(t: &Type, generics: &[Generic]) -> bool {
        match t {
            Type::Named(n, args) => {
                generics.iter().any(|g| !g.row && &g.name == n)
                    || args.iter().any(|x| has_generic(x, generics))
            }
            Type::Tuple(ts) => ts.iter().any(|x| has_generic(x, generics)),
            Type::Function { params, result, .. } => {
                params.iter().any(|(_, t)| has_generic(t, generics))
                    || has_generic(result, generics)
            }
            Type::Row(_) | Type::IntLiteral(_) | Type::Infer => false,
        }
    }
    let a_norm = crate::numeric::default_type(a);
    let b_norm = crate::numeric::default_type(b);
    if !has_generic(&a_norm, generics) && !has_generic(&b_norm, generics) {
        let la = Layout::of(world, &a_norm)?;
        let lb = Layout::of(world, &b_norm)?;
        Ok(la.size == lb.size)
    } else {
        Ok(a_norm == b_norm)
    }
}

fn slot_size_label(world: &World, generics: &[Generic], t: &Type) -> String {
    let norm = crate::numeric::default_type(t);
    let has_gen = match &norm {
        Type::Named(n, args) => {
            generics.iter().any(|g| !g.row && &g.name == n)
                || args
                    .iter()
                    .any(|x| slot_size_label(world, generics, x).starts_with("type "))
        }
        _ => false,
    };
    if !has_gen && let Ok(l) = Layout::of(world, &norm) {
        format!("size {}", l.size)
    } else {
        format!("type {norm}")
    }
}

/// Slot-matching key `(key, concrete_byte_size)` used by Stage 4 `egglog`
/// constructor-reuse optimization. Uses exact byte size for concrete types and
/// canonical structural type key when generic parameters are still free.
pub(crate) fn slot_key(
    world: &World,
    generics: &[Generic],
    t: &Type,
) -> Result<(String, u32), String> {
    fn has_generic(t: &Type, generics: &[Generic]) -> bool {
        match t {
            Type::Named(n, args) => {
                generics.iter().any(|g| !g.row && &g.name == n)
                    || args.iter().any(|x| has_generic(x, generics))
            }
            Type::Tuple(ts) => ts.iter().any(|x| has_generic(x, generics)),
            Type::Function { params, result, .. } => {
                params.iter().any(|(_, t)| has_generic(t, generics))
                    || has_generic(result, generics)
            }
            Type::Row(_) | Type::IntLiteral(_) | Type::Infer => false,
        }
    }
    let norm = crate::numeric::default_type(t);
    if !has_generic(&norm, generics) {
        let layout = Layout::of(world, &norm)?;
        Ok((format!("size:{}", layout.size), layout.size))
    } else {
        Ok((format!("type:{norm}"), 0))
    }
}

fn remaining_box_paths(world: &World, ty: &Type, moved: &[Vec<String>]) -> Vec<Vec<String>> {
    fn is_covered(path: &[String], moved: &[Vec<String>]) -> bool {
        moved.iter().any(|m| path.starts_with(m))
    }
    fn walk(
        world: &World,
        ty: &Type,
        path: &mut Vec<String>,
        moved: &[Vec<String>],
        out: &mut Vec<Vec<String>>,
    ) {
        if is_covered(path, moved) {
            return;
        }
        if !type_contains_box(world, ty) {
            return;
        }
        if is_box_type(world, ty) {
            out.push(path.clone());
            return;
        }
        match ty {
            Type::Tuple(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    path.push(i.to_string());
                    walk(world, t, path, moved, out);
                    path.pop();
                }
            }
            Type::Named(n, args) => {
                if let Some(a) = world.adts.get(n)
                    && !a.is_enum
                    && a.variants.len() == 1
                {
                    let vars: BTreeMap<String, Type> = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    for f in &a.variants[0].fields {
                        let ft = substitute(&f.ty, &vars);
                        path.push(f.name.clone());
                        walk(world, &ft, path, moved, out);
                        path.pop();
                    }
                } else {
                    out.push(path.clone());
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    let mut path = vec![];
    walk(world, ty, &mut path, moved, &mut out);
    out
}

fn pattern_has_box_pattern(world: &World, p: &Pattern) -> bool {
    if is_box_pattern(world, p) {
        return true;
    }
    match &p.kind {
        PatternKind::Tuple(ps) => ps.iter().any(|sub| pattern_has_box_pattern(world, sub)),
        PatternKind::Constructor { fields, .. } => fields
            .iter()
            .any(|(_, sub)| pattern_has_box_pattern(world, sub)),
        PatternKind::Wild | PatternKind::Bind(_) | PatternKind::Literal(_) => false,
    }
}

fn unsafe_non_fip_functions(world: &World, caller_slot_fns: &BTreeSet<String>) -> BTreeSet<String> {
    let mut unsafe_fns = BTreeSet::new();
    let mut call_edges: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for f in world.functions.values() {
        if f.fip {
            continue;
        }
        let mut is_unsafe = f
            .effects
            .instances
            .iter()
            .any(|eff| eff.name == ALLOC_EFFECT)
            || type_contains_box(world, &f.result)
            || f.params
                .iter()
                .any(|p| p.pass == Pass::Sink && type_contains_box(world, &p.ty))
            || f.body.is_none();

        let mut callees = Vec::new();
        if !is_unsafe && let Some(body) = &f.body {
            crate::regions::visit_expr(body, &mut |e| {
                if is_unsafe {
                    return;
                }
                let e_ty = world
                    .types
                    .get(&(e.span.start, e.span.end))
                    .cloned()
                    .unwrap_or(Type::Infer);
                match &e.kind {
                    ExprKind::Block { statements, .. } => {
                        for stmt in statements {
                            if let Statement::Let { pat, .. } = stmt
                                && pattern_has_box_pattern(world, pat)
                            {
                                is_unsafe = true;
                                return;
                            }
                        }
                    }
                    ExprKind::Match { arms, .. } => {
                        for (pat, _) in arms {
                            if pattern_has_box_pattern(world, pat) {
                                is_unsafe = true;
                                return;
                            }
                        }
                    }
                    ExprKind::Handle {
                        effect,
                        state,
                        ret,
                        ops,
                        ..
                    } => {
                        if effect.name == ALLOC_EFFECT
                            || state
                                .as_ref()
                                .is_some_and(|(p, _)| pattern_has_box_pattern(world, p))
                            || ret.pats.iter().any(|p| pattern_has_box_pattern(world, p))
                            || ops
                                .iter()
                                .any(|c| c.pats.iter().any(|p| pattern_has_box_pattern(world, p)))
                        {
                            is_unsafe = true;
                        }
                    }
                    ExprKind::Call { callee, .. } => {
                        if crate::seq::fip_allocation(world, callee).is_some() {
                            is_unsafe = true;
                            return;
                        }
                        if let ExprKind::Name(name) = &callee.kind {
                            if name == BOX_NEW
                                || name == BOX_NEW_IN_CALLER
                                || caller_slot_fns.contains(name)
                            {
                                is_unsafe = true;
                                return;
                            }
                            if let Some(Type::Function { .. }) =
                                world.types.get(&(callee.span.start, callee.span.end))
                            {
                                is_unsafe = true;
                                return;
                            }
                            if let Some(callee_fn) = world.functions.get(name) {
                                if !callee_fn.fip {
                                    callees.push(name.clone());
                                }
                            } else if name.contains("::") && type_contains_box(world, &e_ty) {
                                is_unsafe = true;
                            }
                        } else if let ExprKind::Field(base, method) = &callee.kind {
                            let base_ty = world
                                .types
                                .get(&(base.span.start, base.span.end))
                                .cloned()
                                .unwrap_or(Type::Infer);
                            let method_name = format!("{}::{method}", base_ty.name());
                            if caller_slot_fns.contains(&method_name) {
                                is_unsafe = true;
                                return;
                            }
                            if let Some(callee_fn) = world.functions.get(&method_name) {
                                if !callee_fn.fip {
                                    callees.push(method_name);
                                }
                            } else {
                                is_unsafe = true;
                            }
                        } else {
                            is_unsafe = true;
                        }
                    }
                    _ => {}
                }
            });
        }

        if is_unsafe {
            unsafe_fns.insert(f.name.clone());
        } else if !callees.is_empty() {
            call_edges.insert(f.name.clone(), callees);
        }
    }

    loop {
        let mut changed = false;
        for (caller, callees) in &call_edges {
            if unsafe_fns.contains(caller) {
                continue;
            }
            if callees.iter().any(|callee| unsafe_fns.contains(callee)) {
                unsafe_fns.insert(caller.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    unsafe_fns
}

struct FipChecker<'a> {
    world: &'a World,
    caller_slot_fns: BTreeSet<String>,
    unsafe_non_fip: BTreeSet<String>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> FipChecker<'a> {
    fn expr_type(&self, e: &Expr) -> Type {
        self.world
            .types
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or(Type::Infer)
    }

    fn check_non_fip(&mut self, f: &Function) {
        if let Some(body) = &f.body {
            crate::regions::visit_expr(body, &mut |e| {
                if let ExprKind::Call { callee, .. } = &e.kind
                    && matches!(&callee.kind, ExprKind::Name(n) if n == BOX_NEW_IN_CALLER)
                {
                    self.diagnostics.push(Diagnostic::new(
                        "E0802",
                        "Box::new_in_caller is only permitted inside @fip functions",
                        e.span.clone(),
                    ));
                }
            });
        }
    }

    fn check_fip_fn(&mut self, f: &Function) {
        if f.effects.instances.iter().any(|e| e.name == ALLOC_EFFECT) {
            self.diagnostics.push(Diagnostic::new(
                "E0801",
                "@fip function cannot declare the 'Alloc' effect",
                f.span.clone(),
            ));
        }
        let Some(body) = &f.body else {
            return;
        };
        let mut state = FipState {
            slots: vec![],
            caller_slot_used: false,
            bindings: BTreeMap::new(),
            diverged: false,
        };
        for p in &f.params {
            if p.pass == Pass::Sink && type_contains_box(self.world, &p.ty) {
                state.bindings.insert(
                    p.name.clone(),
                    BindingState {
                        ty: p.ty.clone(),
                        span: p.span.clone(),
                        moved_paths: vec![],
                    },
                );
            }
        }
        self.check_expr(f, body, &mut state, true, true);
        if !state.diverged {
            for (_, slot_span) in &state.slots {
                self.diagnostics.push(Diagnostic::new(
                    "E0805",
                    "@fip function leaves an unconsumed Box ReuseSlot (would drop/deallocate)",
                    slot_span.clone(),
                ));
            }
            for (name, b) in &state.bindings {
                let rem = remaining_box_paths(self.world, &b.ty, &b.moved_paths);
                if !rem.is_empty() {
                    self.diagnostics.push(Diagnostic::new(
                        "E0806",
                        format!("@fip function cannot drop owned Box in '{name}' without unpacking or moving it"),
                        b.span.clone(),
                    ));
                }
            }
        }
    }

    fn bind_pattern(&mut self, pat: &Pattern, ty: &Type, state: &mut FipState) {
        match &pat.kind {
            PatternKind::Wild => {
                if type_contains_box(self.world, ty) {
                    self.diagnostics.push(Diagnostic::new(
                        "E0806",
                        "@fip function cannot discard Box value with '_' pattern",
                        pat.span.clone(),
                    ));
                }
            }
            PatternKind::Bind(name) => {
                if type_contains_box(self.world, ty) {
                    state.bindings.insert(
                        name.clone(),
                        BindingState {
                            ty: ty.clone(),
                            span: pat.span.clone(),
                            moved_paths: vec![],
                        },
                    );
                }
            }
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = ty {
                    for (p, t) in ps.iter().zip(ts) {
                        self.bind_pattern(p, t, state);
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if is_box_pattern(self.world, pat) {
                    let inner = box_inner(self.world, ty).cloned().unwrap_or(Type::Infer);
                    state.slots.push((inner.clone(), pat.span.clone()));
                    self.bind_pattern(&fields[0].1, &inner, state);
                    return;
                }
                if let Some((a, v)) = self.world.constructor(name) {
                    let vars: BTreeMap<String, Type> = match ty {
                        Type::Named(_, args) => a
                            .generics
                            .iter()
                            .zip(args)
                            .map(|(g, t)| (g.name.clone(), t.clone()))
                            .collect(),
                        _ => BTreeMap::new(),
                    };
                    for (fnm, sub) in fields {
                        if let Some(f) = v.fields.iter().find(|f| &f.name == fnm) {
                            self.bind_pattern(sub, &substitute(&f.ty, &vars), state);
                        }
                    }
                }
            }
            PatternKind::Literal(_) => {}
        }
    }

    fn pop_matching_slot(&mut self, f: &Function, needed: &Type, span: Span, state: &mut FipState) {
        let mut idx = None;
        for (i, (slot_ty, _)) in state.slots.iter().enumerate() {
            if slots_match(self.world, &f.generics, slot_ty, needed).unwrap_or(false) {
                idx = Some(i);
                break;
            }
        }
        if let Some(i) = idx {
            state.slots.remove(i);
        } else {
            let need_lbl = slot_size_label(self.world, &f.generics, needed);
            let have = state
                .slots
                .iter()
                .map(|(t, _)| slot_size_label(self.world, &f.generics, t))
                .collect::<Vec<_>>();
            let detail = if have.is_empty() {
                "none available".into()
            } else {
                format!("available: [{}]", have.join(", "))
            };
            self.diagnostics.push(Diagnostic::new(
                "E0804",
                format!("@fip allocation requires a ReuseSlot with {need_lbl} ({detail})"),
                span,
            ));
        }
    }

    fn consume_caller_slot(
        &mut self,
        f: &Function,
        produced: &Type,
        span: Span,
        state: &mut FipState,
    ) {
        let Some(ret_inner) = box_inner(self.world, &f.result) else {
            self.diagnostics.push(Diagnostic::new(
                "E0807",
                "Box::new_in_caller requires @fip function to return Box[T]",
                span,
            ));
            return;
        };
        if !slots_match(self.world, &f.generics, produced, ret_inner).unwrap_or(false) {
            self.diagnostics.push(Diagnostic::new(
                "E0807",
                format!(
                    "Box::new_in_caller type {} does not match function return slot {}",
                    slot_size_label(self.world, &f.generics, produced),
                    slot_size_label(self.world, &f.generics, ret_inner)
                ),
                span.clone(),
            ));
        }
        if state.caller_slot_used {
            self.diagnostics.push(Diagnostic::new(
                "E0808",
                "caller ReuseSlot already consumed on this execution path",
                span,
            ));
        }
        state.caller_slot_used = true;
    }

    fn merge_states(&mut self, f: &Function, branches: Vec<FipState>, span: Span) -> FipState {
        let live: Vec<FipState> = branches.into_iter().filter(|b| !b.diverged).collect();
        let Some(first) = live.first() else {
            return FipState {
                slots: vec![],
                caller_slot_used: false,
                bindings: BTreeMap::new(),
                diverged: true,
            };
        };
        let mut merged = first.clone();
        for branch in live.iter().skip(1) {
            if branch.caller_slot_used != merged.caller_slot_used {
                self.diagnostics.push(Diagnostic::new(
                    "E0808",
                    "all non-diverging paths of an @fip function must agree on consuming caller ReuseSlot",
                    span.clone(),
                ));
            }
            if !self.slot_multisets_match(f, &merged.slots, &branch.slots) {
                self.diagnostics.push(Diagnostic::new(
                    "E0809",
                    "branches leave incompatible remaining Box ReuseSlots",
                    span.clone(),
                ));
            }
            for (name, b) in &mut merged.bindings {
                if let Some(other) = branch.bindings.get(name) {
                    b.moved_paths
                        .retain(|p| other.moved_paths.iter().any(|op| op == p));
                }
            }
        }
        merged
    }

    fn slot_multisets_match(&self, f: &Function, a: &[(Type, Span)], b: &[(Type, Span)]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut used = vec![false; b.len()];
        for (ta, _) in a {
            let mut found = false;
            for (j, (tb, _)) in b.iter().enumerate() {
                if !used[j] && slots_match(self.world, &f.generics, ta, tb).unwrap_or(false) {
                    used[j] = true;
                    found = true;
                    break;
                }
            }
            if !found {
                return false;
            }
        }
        true
    }

    fn check_expr(
        &mut self,
        f: &Function,
        e: &Expr,
        state: &mut FipState,
        owned: bool,
        in_tail: bool,
    ) {
        if owned
            && let Some(place) = Place::from_expr(e)
            && state.bindings.contains_key(&place.root)
        {
            state.mark_moved(&place);
            return;
        }
        let e_ty = self.expr_type(e);
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(es) => {
                for item in es {
                    self.check_expr(f, item, state, owned, false);
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, item) in fields {
                    self.check_expr(f, item, state, owned, false);
                }
            }
            ExprKind::Field(base, _) | ExprKind::Unary(_, base) | ExprKind::Try(base) => {
                self.check_expr(f, base, state, false, false);
            }
            ExprKind::Index(base, idx) => {
                self.check_expr(f, base, state, false, false);
                self.check_expr(f, idx, state, true, false);
            }
            ExprKind::Assign(_, lhs, rhs) => {
                if let Some(p) = Place::from_expr(lhs)
                    && let Some(b) = state.bindings.get(&p.root)
                {
                    let sub_ty = if p.path.is_empty() {
                        Some(b.ty.clone())
                    } else {
                        self.world.field(&b.ty, &p.path)
                    };
                    if sub_ty.is_some_and(|t| type_contains_box(self.world, &t)) {
                        self.diagnostics.push(Diagnostic::new(
                            "E0806",
                            "@fip function cannot overwrite an owned Box without unpacking or moving it first",
                            lhs.span.clone(),
                        ));
                    }
                }
                self.check_expr(f, rhs, state, true, false);
            }
            ExprKind::Binary(op, l, r) => {
                self.check_expr(f, l, state, true, false);
                if matches!(op.as_str(), "&&" | "||") {
                    let mut rhs_state = state.clone();
                    self.check_expr(f, r, &mut rhs_state, true, false);
                    *state = self.merge_states(f, vec![state.clone(), rhs_state], e.span.clone());
                } else {
                    self.check_expr(f, r, state, true, false);
                }
            }
            ExprKind::Block { statements, tail } => {
                let outer_keys: BTreeMap<String, BindingState> = state.bindings.clone();
                for stmt in statements {
                    if state.diverged {
                        break;
                    }
                    match stmt {
                        Statement::Let { pat, ty, value, .. } => {
                            let val_ty = {
                                let inferred = self.expr_type(value);
                                if inferred != Type::Infer {
                                    inferred
                                } else {
                                    ty.clone()
                                }
                            };
                            self.check_expr(f, value, state, true, false);
                            if !state.diverged {
                                self.bind_pattern(pat, &val_ty, state);
                            }
                        }
                        Statement::Expr(se) => {
                            let se_ty = self.expr_type(se);
                            self.check_expr(f, se, state, true, false);
                            if type_contains_box(self.world, &se_ty) {
                                self.diagnostics.push(Diagnostic::new(
                                    "E0806",
                                    "@fip function drops temporary Box value in statement",
                                    se.span.clone(),
                                ));
                            }
                        }
                    }
                }
                if !state.diverged
                    && let Some(tail) = tail
                {
                    self.check_expr(f, tail, state, owned, in_tail);
                }
                let local_names: Vec<String> = state
                    .bindings
                    .iter()
                    .filter(|(k, v)| outer_keys.get(*k).is_none_or(|ov| ov.span != v.span))
                    .map(|(k, _)| k.clone())
                    .collect();
                if !state.diverged {
                    for name in &local_names {
                        if let Some(b) = state.bindings.get(name) {
                            let rem = remaining_box_paths(self.world, &b.ty, &b.moved_paths);
                            if !rem.is_empty() {
                                self.diagnostics.push(Diagnostic::new(
                                    "E0806",
                                    format!("@fip function cannot drop owned Box in '{name}' without unpacking or moving it"),
                                    b.span.clone(),
                                ));
                            }
                        }
                    }
                }
                for name in local_names {
                    state.bindings.remove(&name);
                    if let Some(old) = outer_keys.get(&name) {
                        state.bindings.insert(name, old.clone());
                    }
                }
            }
            ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_) => {
                // Loops and early exits leave the FIP ownership model (P0 lane 4).
                self.diagnostics.push(Diagnostic::new(
                    "E0806",
                    "@fip function cannot use loop, for, break or return; write the iteration as tail recursion",
                    e.span.clone(),
                ));
                state.diverged = true;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.check_expr(f, condition, state, true, false);
                let mut left = state.clone();
                self.check_expr(f, then, &mut left, owned, in_tail);
                let mut right = state.clone();
                if let Some(o) = otherwise {
                    self.check_expr(f, o, &mut right, owned, in_tail);
                }
                *state = self.merge_states(f, vec![left, right], e.span.clone());
            }
            ExprKind::Match { value, arms } => {
                let scrutinee_ty = self.expr_type(value);
                self.check_expr(f, value, state, true, false);
                let mut arm_states = vec![];
                for (pat, body) in arms {
                    let mut arm = state.clone();
                    let arm_outer = arm.bindings.clone();
                    self.bind_pattern(pat, &scrutinee_ty, &mut arm);
                    self.check_expr(f, body, &mut arm, owned, in_tail);
                    let arm_locals: Vec<String> = arm
                        .bindings
                        .iter()
                        .filter(|(k, v)| arm_outer.get(*k).is_none_or(|ov| ov.span != v.span))
                        .map(|(k, _)| k.clone())
                        .collect();
                    if !arm.diverged {
                        for name in &arm_locals {
                            if let Some(b) = arm.bindings.get(name) {
                                let rem = remaining_box_paths(self.world, &b.ty, &b.moved_paths);
                                if !rem.is_empty() {
                                    self.diagnostics.push(Diagnostic::new(
                                        "E0806",
                                        format!("@fip function cannot drop owned Box in '{name}' without unpacking or moving it"),
                                        b.span.clone(),
                                    ));
                                }
                            }
                        }
                    }
                    for name in arm_locals {
                        arm.bindings.remove(&name);
                        if let Some(old) = arm_outer.get(&name) {
                            arm.bindings.insert(name, old.clone());
                        }
                    }
                    arm_states.push(arm);
                }
                *state = self.merge_states(f, arm_states, e.span.clone());
            }
            ExprKind::Call { callee, args, .. } => {
                if let Some(op) = crate::seq::fip_allocation(self.world, callee) {
                    self.diagnostics.push(Diagnostic::new(
                        "E0801",
                        format!("@fip function cannot allocate through ChunkSeq::{op}"),
                        e.span.clone(),
                    ));
                }
                if let ExprKind::Name(name) = &callee.kind {
                    if name == BOX_NEW {
                        for a in args {
                            self.check_expr(f, &a.expr, state, true, false);
                        }
                        if let Some(inner) = box_inner(self.world, &e_ty) {
                            self.pop_matching_slot(f, inner, e.span.clone(), state);
                        }
                        return;
                    }
                    if name == BOX_NEW_IN_CALLER {
                        for a in args {
                            self.check_expr(f, &a.expr, state, true, false);
                        }
                        if let Some(inner) = box_inner(self.world, &e_ty) {
                            self.consume_caller_slot(f, inner, e.span.clone(), state);
                        }
                        return;
                    }
                    if self.world.constructor(name).is_some() {
                        for a in args {
                            self.check_expr(f, &a.expr, state, true, false);
                        }
                        return;
                    }
                    if matches!(self.expr_type(callee), Type::Function { .. }) {
                        for a in args {
                            let pass_own = a.pass == Pass::Sink;
                            self.check_expr(f, &a.expr, state, pass_own, false);
                        }
                        self.diagnostics.push(Diagnostic::new(
                            "E0801",
                            "@fip function cannot perform indirect calls to unverified closures",
                            e.span.clone(),
                        ));
                        return;
                    }
                    if let Some(callee_fn) = self.world.functions.get(name) {
                        if callee_fn
                            .effects
                            .instances
                            .iter()
                            .any(|eff| eff.name == ALLOC_EFFECT)
                        {
                            self.diagnostics.push(Diagnostic::new(
                                "E0801",
                                format!("@fip function cannot call '{name}' with 'Alloc' effect"),
                                e.span.clone(),
                            ));
                        }
                        if !callee_fn.fip
                            && (type_contains_box(self.world, &callee_fn.result)
                                || callee_fn.params.iter().any(|p| {
                                    p.pass == Pass::Sink && type_contains_box(self.world, &p.ty)
                                })
                                || self.unsafe_non_fip.contains(name))
                        {
                            self.diagnostics.push(Diagnostic::new(
                                "E0801",
                                format!("@fip function cannot call non-@fip function '{name}' that uses Box"),
                                e.span.clone(),
                            ));
                        }
                        for (arg, param) in args.iter().zip(&callee_fn.params) {
                            let arg_owned = param.pass == Pass::Sink;
                            let arg_ty = self.expr_type(&arg.expr);
                            self.check_expr(f, &arg.expr, state, arg_owned, false);
                            if !arg_owned
                                && Place::from_expr(&arg.expr).is_none()
                                && type_contains_box(self.world, &arg_ty)
                            {
                                self.diagnostics.push(Diagnostic::new(
                                    "E0806",
                                    "@fip function drops temporary Box passed by borrow",
                                    arg.expr.span.clone(),
                                ));
                            }
                        }
                        if self.caller_slot_fns.contains(name)
                            && let Some(inner) = box_inner(self.world, &e_ty)
                        {
                            let can_forward = in_tail
                                && !state.caller_slot_used
                                && box_inner(self.world, &f.result).is_some_and(|ret_inner| {
                                    slots_match(self.world, &f.generics, inner, ret_inner)
                                        .unwrap_or(false)
                                });
                            if can_forward {
                                self.consume_caller_slot(f, inner, e.span.clone(), state);
                            } else {
                                self.pop_matching_slot(f, inner, e.span.clone(), state);
                            }
                        }
                        return;
                    }
                    // Builtin vector/tiled/arithmetic intrinsic or effect op
                    if name.contains("::") && type_contains_box(self.world, &e_ty) {
                        self.diagnostics.push(Diagnostic::new(
                            "E0801",
                            format!(
                                "@fip function cannot call effect operation '{name}' producing Box"
                            ),
                            e.span.clone(),
                        ));
                    }
                    for a in args {
                        let pass_own = a.pass == Pass::Sink;
                        self.check_expr(f, &a.expr, state, pass_own, false);
                    }
                    return;
                }
                if let ExprKind::Field(base, method) = &callee.kind {
                    let base_ty = self.expr_type(base);
                    let method_name = format!("{}::{method}", base_ty.name());
                    if let Some(callee_fn) = self.world.functions.get(&method_name) {
                        if callee_fn
                            .effects
                            .instances
                            .iter()
                            .any(|eff| eff.name == ALLOC_EFFECT)
                        {
                            self.diagnostics.push(Diagnostic::new(
                                "E0801",
                                format!(
                                    "@fip function cannot call '{method_name}' with 'Alloc' effect"
                                ),
                                e.span.clone(),
                            ));
                        }
                        if !callee_fn.fip
                            && (type_contains_box(self.world, &callee_fn.result)
                                || callee_fn.params.iter().any(|p| {
                                    p.pass == Pass::Sink && type_contains_box(self.world, &p.ty)
                                })
                                || self.unsafe_non_fip.contains(&method_name))
                        {
                            self.diagnostics.push(Diagnostic::new(
                                "E0801",
                                format!("@fip function cannot call non-@fip function '{method_name}' that uses Box"),
                                e.span.clone(),
                            ));
                        }
                        let recv_param = callee_fn.params.first();
                        let recv_owned = recv_param.is_some_and(|p| p.pass == Pass::Sink);
                        self.check_expr(f, base, state, recv_owned, false);
                        if !recv_owned
                            && Place::from_expr(base).is_none()
                            && type_contains_box(self.world, &base_ty)
                        {
                            self.diagnostics.push(Diagnostic::new(
                                "E0806",
                                "@fip function drops temporary Box passed as receiver by borrow",
                                base.span.clone(),
                            ));
                        }
                        for (arg, param) in args.iter().zip(callee_fn.params.iter().skip(1)) {
                            let arg_owned = param.pass == Pass::Sink;
                            let arg_ty = self.expr_type(&arg.expr);
                            self.check_expr(f, &arg.expr, state, arg_owned, false);
                            if !arg_owned
                                && Place::from_expr(&arg.expr).is_none()
                                && type_contains_box(self.world, &arg_ty)
                            {
                                self.diagnostics.push(Diagnostic::new(
                                    "E0806",
                                    "@fip function drops temporary Box passed by borrow",
                                    arg.expr.span.clone(),
                                ));
                            }
                        }
                        if self.caller_slot_fns.contains(&method_name)
                            && let Some(inner) = box_inner(self.world, &e_ty)
                        {
                            let can_forward = in_tail
                                && !state.caller_slot_used
                                && box_inner(self.world, &f.result).is_some_and(|ret_inner| {
                                    slots_match(self.world, &f.generics, inner, ret_inner)
                                        .unwrap_or(false)
                                });
                            if can_forward {
                                self.consume_caller_slot(f, inner, e.span.clone(), state);
                            } else {
                                self.pop_matching_slot(f, inner, e.span.clone(), state);
                            }
                        }
                        return;
                    }
                }
                self.check_expr(f, callee, state, false, false);
                for a in args {
                    self.check_expr(f, &a.expr, state, true, false);
                }
                self.diagnostics.push(Diagnostic::new(
                    "E0801",
                    "@fip function cannot perform indirect calls to unverified closures",
                    e.span.clone(),
                ));
            }
            ExprKind::Closure { .. } => {}
            ExprKind::Handle {
                effect,
                state: h_state,
                body,
                ..
            } => {
                if effect.name == ALLOC_EFFECT {
                    self.diagnostics.push(Diagnostic::new(
                        "E0801",
                        "@fip function cannot handle the 'Alloc' effect",
                        e.span.clone(),
                    ));
                }
                if let Some((p, init)) = h_state {
                    let it = self.expr_type(init);
                    self.check_expr(f, init, state, true, false);
                    self.bind_pattern(p, &it, state);
                }
                self.check_expr(f, body, state, owned, in_tail);
            }
        }
        if e_ty.name() == "Never" {
            state.diverged = true;
        }
    }
}

/// Static verification of `@fip` functions, `Box::new` reuse pairing,
/// `Box::new_in_caller`, and forbidden `Alloc` effects.
pub fn check_world(world: &World) -> Vec<Diagnostic> {
    let caller_slot_fns = caller_slot_functions(world);
    let unsafe_non_fip = unsafe_non_fip_functions(world, &caller_slot_fns);
    let mut checker = FipChecker {
        world,
        caller_slot_fns,
        unsafe_non_fip,
        diagnostics: vec![],
    };
    for f in world.functions.values() {
        if f.fip {
            checker.check_fip_fn(f);
        } else {
            checker.check_non_fip(f);
        }
    }
    checker.diagnostics
}

#[derive(Clone, Debug, PartialEq)]
struct DropSlotState {
    ty: Type,
    pass: Pass,
    moved: Vec<Vec<String>>,
}

fn path_is_covered(path: &[String], moved: &[Vec<String>]) -> bool {
    moved.iter().any(|m| path.starts_with(m))
}

/// The `match` scrutinees of `f` that are borrows (§4.3): by-value reads
/// of a non-copy place rooted at an `in`/`inout` parameter, or at a name
/// that an earlier borrowed `match` bound. The checker types such a `match`
/// as a borrow, so its pattern binds `in` borrows: no `Box` it unpacks is
/// freed and nothing it binds is dropped.
pub fn borrowed_scrutinees(f: &crate::ir::FunctionIr) -> BTreeSet<ValueId> {
    use crate::ir::{Op, Terminator};
    let matched: BTreeSet<ValueId> = f
        .blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            Terminator::Match { value, .. } => Some(*value),
            _ => None,
        })
        .collect();
    let mut names: BTreeSet<String> = f
        .params
        .iter()
        .filter(|p| p.pass != Pass::Sink)
        .map(|p| p.name.clone())
        .collect();
    let mut out = BTreeSet::new();
    loop {
        let before = (names.len(), out.len());
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            match &i.op {
                Op::Read { root, .. } if matched.contains(&i.dest) && names.contains(root) => {
                    out.insert(i.dest);
                }
                Op::Bind { pattern, value } if out.contains(value) => {
                    pattern_bound_names(pattern, &mut names);
                }
                _ => {}
            }
        }
        if (names.len(), out.len()) == before {
            return out;
        }
    }
}

fn pattern_bound_names(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| pattern_bound_names(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| pattern_bound_names(p, out));
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

fn bind_drop_slots(
    world: &World,
    pat: &Pattern,
    ty: &Type,
    slots: &mut BTreeMap<String, DropSlotState>,
    pass: Pass,
) {
    match &pat.kind {
        PatternKind::Bind(name) => {
            slots.insert(
                name.clone(),
                DropSlotState {
                    ty: ty.clone(),
                    pass,
                    moved: vec![],
                },
            );
        }
        PatternKind::Tuple(ps) => {
            if let Type::Tuple(ts) = ty {
                for (p, t) in ps.iter().zip(ts) {
                    bind_drop_slots(world, p, t, slots, pass);
                }
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            if is_box_pattern(world, pat) {
                let inner = box_inner(world, ty).cloned().unwrap_or(Type::Infer);
                bind_drop_slots(world, &fields[0].1, &inner, slots, pass);
                return;
            }
            if let Some((a, v)) = world.constructor(name) {
                let vars: BTreeMap<String, Type> = match ty {
                    Type::Named(_, args) => a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect(),
                    _ => BTreeMap::new(),
                };
                for (fnm, sub) in fields {
                    if let Some(f) = v.fields.iter().find(|f| &f.name == fnm) {
                        bind_drop_slots(world, sub, &substitute(&f.ty, &vars), slots, pass);
                    }
                }
            }
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

pub(crate) fn op_reads_value(op: &crate::ir::Op, target: ValueId) -> bool {
    use crate::ir::Op;
    match op {
        Op::Bind { value, .. }
        | Op::Write { value, .. }
        | Op::Unary(_, value)
        | Op::Project(value, _)
        | Op::Exit { value, .. }
        | Op::Try(value) => *value == target,
        Op::Binary(_, a, b) | Op::Index(a, b) => *a == target || *b == target,
        Op::Tuple(vs) | Op::Vector { args: vs, .. } => vs.contains(&target),
        Op::Data(_, fs) => fs.iter().any(|(_, v)| *v == target),
        Op::Handle {
            state_value: Some(value),
            ..
        } => *value == target,
        Op::Call { args, .. }
        | Op::Tiled { args, .. }
        | Op::Seq { args, .. }
        | Op::Fip { args, .. } => args.iter().any(|a| a.value == target),
        Op::Indirect { callee, args } => {
            *callee == target || args.iter().any(|a| a.value == target)
        }
        Op::Constant(_)
        | Op::Unit
        | Op::Read { .. }
        | Op::Closure { .. }
        | Op::Handle { .. }
        | Op::Loop => false,
    }
}

fn value_used_after_in_block(block: &crate::ir::Block, after_inst: usize, target: ValueId) -> bool {
    use crate::ir::Terminator;
    if block
        .instructions
        .iter()
        .skip(after_inst + 1)
        .any(|inst| op_reads_value(&inst.op, target))
    {
        return true;
    }
    match &block.terminator {
        Terminator::Return(v) => *v == target,
        Terminator::Jump { args, .. } => args.contains(&target),
        Terminator::Branch { condition, .. } => *condition == target,
        Terminator::Match { value, .. } => *value == target,
        Terminator::Unreachable => false,
    }
}

fn resolve_place_type(world: &World, root_ty: &Type, path: &[String]) -> Option<Type> {
    if path.is_empty() {
        Some(root_ty.clone())
    } else {
        world.field(root_ty, path)
    }
}

/// Inserts ownership-driven `Box[T]` drop metadata (`box_drops_*`) on every
/// non-`@fip` function in `program` so unpacked, overwritten, discarded, and
/// out-of-scope affine `Box[T]` allocations are freed without waiting for process
/// exit (or JIT teardown).
///
/// Loops and early exits (§7.3, #1064) take the same analysis: the slot
/// states at each block entry are solved to a fixed point over the graph's
/// back edges, a `return` (`Op::Exit`) drops what the function still owns,
/// and a drop that belongs to one edge of a branch is placed in a block
/// split onto that edge ([`split_edge`]), so each box is dropped exactly
/// once on every path without a runtime drop flag.
pub fn elaborate_non_fip_drops(world: &World, program: &mut crate::ir::Program) {
    if !active(world) {
        return;
    }
    for f in program.functions.values_mut() {
        if world.functions.get(&f.name).is_some_and(|src| src.fip) {
            continue;
        }
        f.box_drops_before_inst.clear();
        f.box_drops_after_inst.clear();
        f.box_drops_before_term.clear();
        let Some(e) = elaborate_function(world, f) else {
            continue;
        };
        if !edges_placeable(f, &e) {
            continue;
        }
        f.box_drops_before_inst = e.before_inst;
        f.box_drops_after_inst = e.after_inst;
        f.box_drops_before_term = e.before_term;
        for ((p, s), drops) in e.edges {
            if drops.is_empty() {
                continue;
            }
            let at = if matches!(f.blocks[p].terminator, crate::ir::Terminator::Jump { .. }) {
                p
            } else {
                split_edge(f, p, s)
            };
            f.box_drops_before_term.entry(at).or_default().extend(drops);
        }
    }
}

/// Whether every `Box` a function with loops or early exits owns is
/// dropped by [`elaborate_non_fip_drops`]: its slot states reach a fixed
/// point and each edge drop can be placed. Native backends refuse the
/// function otherwise ([`crate::loops::BOX_REFUSAL`]).
pub fn loop_drops_elaborated(world: &World, f: &crate::ir::FunctionIr) -> bool {
    // An `@fip` function drops nothing (`E0805`/`E0806`) and is not
    // elaborated; with a loop it may only borrow boxes.
    if world.functions.get(&f.name).is_some_and(|src| src.fip) {
        return f.locals.iter().all(|(name, t)| {
            !type_contains_box(world, t)
                || f.params
                    .iter()
                    .any(|p| &p.name == name && p.pass == Pass::In)
        });
    }
    elaborate_function(world, f).is_some_and(|e| edges_placeable(f, &e))
}

/// The drops [`elaborate_function`] plans for one graph. `edges` holds the
/// drops that belong to one control-flow edge `(from, to)` only.
struct Elaboration {
    before_inst: BTreeMap<usize, Vec<(usize, FipDrop)>>,
    after_inst: BTreeMap<usize, Vec<(usize, FipDrop)>>,
    before_term: BTreeMap<usize, Vec<FipDrop>>,
    edges: BTreeMap<(usize, usize), Vec<FipDrop>>,
    /// What the graph owns while an instruction that can abort runs, by
    /// (block, instruction): dropped when a `Fail` leaves the graph there
    /// ([`fail_exit_drops`]).
    at_abort: BTreeMap<(usize, usize), Vec<FipDrop>>,
}

/// The boxes a graph owns at each instruction that can abort it (a call, an
/// indirect call, `?`, a handled body, a sequence or tiled operation, a
/// partial primitive), by (block, instruction): what a `Fail` leaving the
/// graph there must drop. Arguments the instruction takes `sink` are the
/// callee's to drop. Empty when the graph owns no box or its drops are not
/// elaborated ([`elaborate_non_fip_drops`]).
pub fn fail_exit_drops(
    world: &World,
    f: &crate::ir::FunctionIr,
) -> BTreeMap<(usize, usize), Vec<FipDrop>> {
    if !active(world)
        || world.functions.get(&f.name).is_some_and(|src| src.fip)
        || !f
            .values
            .values()
            .chain(f.locals.values())
            .chain(f.params.iter().map(|p| &p.ty))
            .any(|t| type_contains_box(world, t))
    {
        return BTreeMap::new();
    }
    elaborate_function(world, f)
        .filter(|e| edges_placeable(f, e))
        .map(|e| e.at_abort)
        .unwrap_or_default()
}

/// Where one graph drops the `.vsys` handles of `handles` (affine extern
/// types with a drop import, §16.3): the `Box` drop elaboration of
/// [`elaborate_non_fip_drops`] run with those handle types standing in for
/// `Box[T]`, so a handle is dropped exactly where a box in its place would
/// be. Edge drops are placed with [`split_edge`] into `f`. `None` when the
/// slot states reach no fixed point.
pub fn handle_drop_plan(
    world: &World,
    f: &mut crate::ir::FunctionIr,
    handles: &BTreeSet<String>,
) -> Option<HandleDrops> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            DROPPED_HANDLES.with(|h| *h.borrow_mut() = None);
        }
    }
    DROPPED_HANDLES.with(|h| *h.borrow_mut() = Some(handles.clone()));
    let _reset = Reset;
    let mentions = f
        .values
        .values()
        .chain(f.locals.values())
        .chain(f.params.iter().map(|p| &p.ty))
        .any(|t| type_contains_box(world, t));
    if !mentions {
        return Some(HandleDrops::default());
    }
    let e = elaborate_function(world, f)?;
    let mut plan = HandleDrops {
        before_inst: e.before_inst,
        after_inst: e.after_inst,
        before_term: e.before_term,
    };
    for ((p, s), drops) in e.edges {
        if drops.is_empty() {
            continue;
        }
        let at = if matches!(f.blocks[p].terminator, crate::ir::Terminator::Jump { .. }) {
            p
        } else {
            split_edge(f, p, s)
        };
        plan.before_term.entry(at).or_default().extend(drops);
    }
    Some(plan)
}

/// The handle drops of one graph ([`handle_drop_plan`]), keyed as the
/// `box_drops_*` fields of [`crate::ir::FunctionIr`].
#[derive(Debug, Default)]
pub struct HandleDrops {
    pub before_inst: BTreeMap<usize, Vec<(usize, FipDrop)>>,
    pub after_inst: BTreeMap<usize, Vec<(usize, FipDrop)>>,
    pub before_term: BTreeMap<usize, Vec<FipDrop>>,
}

/// An edge drop is placed before its source's `Jump`, or in a block split
/// onto a branch or match edge. A target carrying `@fip` reuse slots
/// ([`FipJoinSlot`], keyed by predecessor) is not split.
fn edges_placeable(f: &crate::ir::FunctionIr, e: &Elaboration) -> bool {
    e.edges.iter().all(|((p, s), drops)| {
        drops.is_empty()
            || matches!(f.blocks[*p].terminator, crate::ir::Terminator::Jump { .. })
            || !f.fip_join_slots.contains_key(s)
    })
}

/// Splits the edge `from -> to` of a branch or match with a new empty block
/// that jumps to `to`, and returns the new block. Branch and match targets
/// take no parameters, so the jump passes none.
fn split_edge(f: &mut crate::ir::FunctionIr, from: usize, to: usize) -> usize {
    use crate::ir::{Block, Terminator};
    let at = f.blocks.len();
    let token = f.blocks[from].outgoing_token;
    f.blocks.push(Block {
        entry_token: token,
        incoming_tokens: vec![(from, token)],
        outgoing_token: token,
        params: vec![],
        instructions: vec![],
        terminator: Terminator::Jump {
            block: to,
            args: vec![],
        },
    });
    match &mut f.blocks[from].terminator {
        Terminator::Branch { yes, no, .. } => {
            for t in [yes, no] {
                if *t == to {
                    *t = at;
                }
            }
        }
        Terminator::Match { arms, .. } => {
            for (_, t) in arms {
                if *t == to {
                    *t = at;
                }
            }
        }
        _ => unreachable!("only branch and match edges are split"),
    }
    for (p, _) in &mut f.blocks[to].incoming_tokens {
        if *p == from {
            *p = at;
        }
    }
    at
}

/// Passes over a graph before its slot states are taken as unsolved.
const DROP_FIXPOINT_LIMIT: usize = 64;

/// Plans the `Box` drops of one graph, or `None` when its slot states do
/// not reach a fixed point within [`DROP_FIXPOINT_LIMIT`] passes.
///
/// Blocks are visited in topological order when the graph is acyclic and
/// in reverse postorder otherwise. A block's entry state joins its
/// predecessors' exit states: a slot exists when it exists on every
/// incoming edge, and a path is moved when any edge moved it; an edge on
/// which the slot (or a path of it) is still owned drops it there. A back
/// edge not yet visited in the first pass is left out of its join. Every
/// pass recomputes every drop, so the plan of the pass that changes no
/// state is the plan of the solution. The loop checker (`E0312`) keeps the
/// moved paths reaching a loop header from a back edge within those
/// reaching it from outside the loop, so a box live at the header is
/// owned on every edge that enters it.
fn elaborate_function(world: &World, f: &crate::ir::FunctionIr) -> Option<Elaboration> {
    use crate::ir::Terminator;
    let n_blocks = f.blocks.len();
    let mut plan = Elaboration {
        before_inst: BTreeMap::new(),
        after_inst: BTreeMap::new(),
        before_term: BTreeMap::new(),
        edges: BTreeMap::new(),
        at_abort: BTreeMap::new(),
    };
    if n_blocks == 0 {
        return Some(plan);
    }
    let mut preds = vec![vec![]; n_blocks];
    let mut succs = vec![vec![]; n_blocks];
    let mut indegree = vec![0usize; n_blocks];
    for (id, b) in f.blocks.iter().enumerate() {
        let targets = match &b.terminator {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, t)| *t).collect(),
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        };
        for t in targets {
            if !succs[id].contains(&t) {
                succs[id].push(t);
                preds[t].push(id);
                indegree[t] += 1;
            }
        }
    }
    let mut reachable = vec![false; n_blocks];
    let mut stack = vec![0usize];
    while let Some(b) = stack.pop() {
        if reachable[b] {
            continue;
        }
        reachable[b] = true;
        for &s in &succs[b] {
            stack.push(s);
        }
    }
    let mut order = Vec::with_capacity(n_blocks);
    let mut queue = std::collections::VecDeque::new();
    for (b, &deg) in indegree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(b);
        }
    }
    while let Some(b) = queue.pop_front() {
        order.push(b);
        for &s in &succs[b] {
            indegree[s] -= 1;
            if indegree[s] == 0 {
                queue.push_back(s);
            }
        }
    }
    let acyclic = order.len() == n_blocks;
    // The entry block's state is the parameters; a back edge into it is
    // not planned.
    if !preds[0].is_empty() {
        return None;
    }
    if !acyclic {
        // Reverse postorder from the entry block.
        let mut visited = vec![false; n_blocks];
        let mut post = Vec::with_capacity(n_blocks);
        let mut stack = vec![(0usize, 0usize)];
        visited[0] = true;
        while let Some((b, next)) = stack.pop() {
            if let Some(&s) = succs[b].get(next) {
                stack.push((b, next + 1));
                if !visited[s] {
                    visited[s] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
            }
        }
        post.reverse();
        order = post;
    }

    let mut exit_slots: Vec<Option<BTreeMap<String, DropSlotState>>> = vec![None; n_blocks];
    let mut diverged = vec![false; n_blocks];
    for _ in 0..DROP_FIXPOINT_LIMIT {
        plan.before_inst.clear();
        plan.after_inst.clear();
        plan.before_term.clear();
        plan.edges.clear();
        plan.at_abort.clear();
        let mut changed = false;
        for &b in &order {
            if !reachable[b] {
                continue;
            }
            let r_preds: Vec<usize> = preds[b]
                .iter()
                .copied()
                .filter(|&p| reachable[p] && !diverged[p] && exit_slots[p].is_some())
                .collect();
            let slots = entry_slots(world, f, b, &r_preds, &exit_slots, &mut plan.edges);
            let (slots, block_diverges) = block_drops(world, f, b, slots, &mut plan);
            if diverged[b] != block_diverges || exit_slots[b].as_ref() != Some(&slots) {
                changed = true;
            }
            diverged[b] = block_diverges;
            exit_slots[b] = Some(slots);
        }
        if !changed || acyclic {
            return Some(plan);
        }
    }
    None
}

/// The slot state entering block `b` from its visited predecessors
/// `r_preds`, recording in `edges` what each incoming edge drops.
fn entry_slots(
    world: &World,
    f: &crate::ir::FunctionIr,
    b: usize,
    r_preds: &[usize],
    exit_slots: &[Option<BTreeMap<String, DropSlotState>>],
    edges: &mut BTreeMap<(usize, usize), Vec<FipDrop>>,
) -> BTreeMap<String, DropSlotState> {
    use crate::ir::Terminator;
    if b == 0 && r_preds.is_empty() {
        return f
            .params
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    DropSlotState {
                        ty: p.ty.clone(),
                        pass: p.pass,
                        moved: vec![],
                    },
                )
            })
            .collect();
    }
    let exit = |p: usize| exit_slots[p].as_ref().expect("visited predecessor");
    let non_never: Vec<usize> = r_preds
        .iter()
        .copied()
        .filter(|&p| {
            !matches!(
                &f.blocks[p].terminator,
                Terminator::Jump { block, args }
                    if *block == b
                        && args.iter().any(|id| {
                            f.values.get(id).is_some_and(|t| t.name() == "Never")
                        })
            )
        })
        .collect();
    let active_preds = if non_never.is_empty() {
        r_preds
    } else {
        &non_never
    };
    if active_preds.is_empty() {
        return BTreeMap::new();
    }
    if active_preds.len() == 1 {
        return exit(active_preds[0]).clone();
    }
    // Build union of moved paths for every slot present in all active predecessors.
    let first = exit(active_preds[0]);
    let mut common = BTreeMap::new();
    for (name, first_slot) in first {
        if active_preds[1..]
            .iter()
            .all(|&p| exit(p).contains_key(name))
        {
            let mut union_moved = Vec::new();
            for &p in active_preds {
                for m in &exit(p)[name].moved {
                    if !union_moved.contains(m) {
                        union_moved.push(m.clone());
                    }
                }
            }
            common.insert(
                name.clone(),
                DropSlotState {
                    ty: first_slot.ty.clone(),
                    pass: first_slot.pass,
                    moved: union_moved,
                },
            );
        }
    }
    // On each active predecessor edge, drop slots local to that predecessor
    // or unmoved paths that another branch moved.
    for &p in active_preds {
        let edge_drops = edges.entry((p, b)).or_default();
        for (name, p_slot) in exit(p) {
            if p_slot.pass != Pass::Sink
                || f.external_captures.contains(name)
                || !type_contains_box(world, &p_slot.ty)
            {
                continue;
            }
            let rem = remaining_box_paths(world, &p_slot.ty, &p_slot.moved);
            let joined = common.get(name);
            for path in rem {
                if joined.is_none_or(|j| path_is_covered(&path, &j.moved))
                    && let Some(drop_ty) = resolve_place_type(world, &p_slot.ty, &path)
                    && type_contains_box(world, &drop_ty)
                {
                    edge_drops.push(FipDrop::Place(
                        Place {
                            root: name.clone(),
                            path,
                        },
                        drop_ty,
                    ));
                }
            }
        }
    }
    common
}

/// The drops slot `slots` still owns when the graph is left: every
/// remaining `Box` path of a `sink` slot that is not an external capture.
fn owned_slot_drops(
    world: &World,
    f: &crate::ir::FunctionIr,
    slots: &BTreeMap<String, DropSlotState>,
) -> Vec<FipDrop> {
    let mut drops = vec![];
    for (name, slot) in slots {
        if slot.pass != Pass::Sink
            || f.external_captures.contains(name)
            || !type_contains_box(world, &slot.ty)
        {
            continue;
        }
        for path in remaining_box_paths(world, &slot.ty, &slot.moved) {
            if let Some(drop_ty) = resolve_place_type(world, &slot.ty, &path)
                && type_contains_box(world, &drop_ty)
            {
                drops.push(FipDrop::Place(
                    Place {
                        root: name.clone(),
                        path,
                    },
                    drop_ty,
                ));
            }
        }
    }
    drops
}

/// Runs block `b` from entry state `slots`, recording its drops in `plan`;
/// returns its exit state and whether it diverges.
fn block_drops(
    world: &World,
    f: &crate::ir::FunctionIr,
    b: usize,
    mut slots: BTreeMap<String, DropSlotState>,
    plan: &mut Elaboration,
) -> (BTreeMap<String, DropSlotState>, bool) {
    use crate::ir::{Op, Terminator};
    let borrowed = borrowed_scrutinees(f);
    let mut owned_values: BTreeMap<ValueId, Type> = BTreeMap::new();
    for &param_id in &f.blocks[b].params {
        if let Some(ty) = f.values.get(&param_id)
            && type_contains_box(world, ty)
        {
            owned_values.insert(param_id, ty.clone());
        }
    }
    let blk = &f.blocks[b];
    let mut block_div = false;
    for (inst_idx, inst) in blk.instructions.iter().enumerate() {
        // `return v` leaves the graph: what it still owns is dropped first.
        if let Op::Exit { value, .. } = &inst.op {
            owned_values.remove(value);
            let before = plan.before_inst.entry(b).or_default();
            for (val_id, val_ty) in std::mem::take(&mut owned_values) {
                before.push((inst_idx, FipDrop::Value(val_id, val_ty)));
            }
            for drop in owned_slot_drops(world, f, &slots) {
                before.push((inst_idx, drop));
            }
            block_div = true;
            break;
        }
        if may_abort(inst) {
            let taken: BTreeSet<ValueId> = match &inst.op {
                Op::Call { args, .. }
                | Op::Indirect { args, .. }
                | Op::Tiled { args, .. }
                | Op::Seq { args, .. }
                | Op::Fip { args, .. } => args
                    .iter()
                    .filter(|a| a.pass == Pass::Sink)
                    .map(|a| a.value)
                    .collect(),
                Op::Try(v) => BTreeSet::from([*v]),
                _ => BTreeSet::new(),
            };
            let mut drops: Vec<FipDrop> = owned_values
                .iter()
                .filter(|(v, _)| !taken.contains(v))
                .map(|(v, t)| FipDrop::Value(*v, t.clone()))
                .collect();
            drops.extend(owned_slot_drops(world, f, &slots));
            if !drops.is_empty() {
                plan.at_abort.insert((b, inst_idx), drops);
            }
        }
        if inst.ty.name() == "Never" {
            block_div = true;
            break;
        }
        match &inst.op {
            Op::Read {
                root,
                path,
                pass,
                projections,
            } => {
                if *pass == Pass::Sink && slots.get(root).is_none_or(|s| s.pass == Pass::Sink) {
                    if let Some(slot) = slots.get_mut(root) {
                        if projections.is_empty() {
                            if !slot.moved.contains(path) {
                                slot.moved.push(path.clone());
                            }
                        } else {
                            for proj in projections {
                                let mut full = path.clone();
                                full.extend(proj.clone());
                                if !slot.moved.contains(&full) {
                                    slot.moved.push(full);
                                }
                            }
                        }
                    }
                    if type_contains_box(world, &inst.ty) {
                        owned_values.insert(inst.dest, inst.ty.clone());
                    }
                }
            }
            Op::Bind { pattern, value } => {
                owned_values.remove(value);
                let val_ty = f.values.get(value).cloned().unwrap_or(Type::Infer);
                // A `match` of a borrowed place binds borrows: the place's
                // owner frees the payloads.
                let pass = if borrowed.contains(value) {
                    Pass::In
                } else {
                    Pass::Sink
                };
                bind_drop_slots(world, pattern, &val_ty, &mut slots, pass);
            }
            Op::Write { place, value, .. } => {
                owned_values.remove(value);
                let mut parts = place.split('.');
                let root = parts.next().unwrap_or("");
                let path: Vec<String> = parts.map(str::to_owned).collect();
                if let Some(slot) = slots.get_mut(root) {
                    if type_contains_box(world, &slot.ty)
                        && let Some(sub_ty) = resolve_place_type(world, &slot.ty, &path)
                        && type_contains_box(world, &sub_ty)
                    {
                        let sub_moved: Vec<Vec<String>> = slot
                            .moved
                            .iter()
                            .filter_map(|m| {
                                if path.starts_with(m) {
                                    Some(vec![])
                                } else if m.starts_with(&path) {
                                    Some(m[path.len()..].to_vec())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        let before = plan.before_inst.entry(b).or_default();
                        for rem_sub in remaining_box_paths(world, &sub_ty, &sub_moved) {
                            let mut full_path = path.clone();
                            full_path.extend(rem_sub);
                            if let Some(drop_ty) = resolve_place_type(world, &slot.ty, &full_path)
                                && type_contains_box(world, &drop_ty)
                            {
                                before.push((
                                    inst_idx,
                                    FipDrop::Place(
                                        Place {
                                            root: root.to_owned(),
                                            path: full_path,
                                        },
                                        drop_ty,
                                    ),
                                ));
                            }
                        }
                    }
                    slot.moved.retain(|m| !m.starts_with(&path));
                }
            }
            Op::Tuple(vs) => {
                for v in vs {
                    owned_values.remove(v);
                }
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            Op::Data(_, fs) => {
                for (_, v) in fs {
                    owned_values.remove(v);
                }
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            Op::Project(base, field) => {
                if let Some(base_ty) = owned_values.remove(base)
                    && type_contains_box(world, &base_ty)
                {
                    plan.after_inst.entry(b).or_default().push((
                        inst_idx,
                        FipDrop::OtherFields {
                            value: *base,
                            ty: base_ty,
                            keep: field.clone(),
                        },
                    ));
                }
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            // A sequence element read in place is lent, never owned: the
            // sequence keeps it (boxed objects, §7.3).
            Op::Seq {
                op: crate::seq::SeqOp::Get | crate::seq::SeqOp::SliceGet,
                ..
            } => {}
            Op::Call { args, .. }
            | Op::Tiled { args, .. }
            | Op::Seq { args, .. }
            | Op::Fip { args, .. } => {
                for a in args {
                    if a.pass == Pass::Sink {
                        owned_values.remove(&a.value);
                    }
                }
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            Op::Indirect { callee, args } => {
                owned_values.remove(callee);
                for a in args {
                    if a.pass == Pass::Sink {
                        owned_values.remove(&a.value);
                    }
                }
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            Op::Try(v) => {
                owned_values.remove(v);
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            Op::Exit { .. } | Op::Loop => {}
            Op::Closure { moves, .. } | Op::Handle { moves, .. } => {
                for p in moves {
                    if let Some(slot) = slots.get_mut(&p.root)
                        && !slot.moved.contains(&p.path)
                    {
                        slot.moved.push(p.path.clone());
                    }
                }
                if let Op::Handle {
                    state_value: Some(sv),
                    ..
                } = &inst.op
                {
                    owned_values.remove(sv);
                }
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
            Op::Constant(_)
            | Op::Unit
            | Op::Unary(..)
            | Op::Binary(..)
            | Op::Index(..)
            | Op::Vector { .. } => {
                if type_contains_box(world, &inst.ty) {
                    owned_values.insert(inst.dest, inst.ty.clone());
                }
            }
        }

        // A temporary used in a later block (an argument evaluated before
        // a branch in a later argument) is that use's to consume: dropping
        // it here would hand the use a freed box or a released handle.
        if owned_values.contains_key(&inst.dest) && used_outside_block(f, b, inst.dest) {
            owned_values.remove(&inst.dest);
        }
        let dead_temps: Vec<ValueId> = owned_values
            .keys()
            .copied()
            .filter(|&v| !value_used_after_in_block(blk, inst_idx, v))
            .collect();
        for v in dead_temps {
            if let Some(v_ty) = owned_values.remove(&v) {
                plan.after_inst
                    .entry(b)
                    .or_default()
                    .push((inst_idx, FipDrop::Value(v, v_ty)));
            }
        }
    }

    if !block_div {
        match &blk.terminator {
            Terminator::Return(v) => {
                owned_values.remove(v);
                let drops = plan.before_term.entry(b).or_default();
                for (val_id, val_ty) in owned_values {
                    drops.push(FipDrop::Value(val_id, val_ty));
                }
                drops.extend(owned_slot_drops(world, f, &slots));
            }
            Terminator::Jump { args, .. } => {
                for a in args {
                    owned_values.remove(a);
                }
                if !owned_values.is_empty() {
                    let drops = plan.before_term.entry(b).or_default();
                    for (val_id, val_ty) in owned_values {
                        drops.push(FipDrop::Value(val_id, val_ty));
                    }
                }
            }
            Terminator::Branch { .. } => {
                if !owned_values.is_empty() {
                    let drops = plan.before_term.entry(b).or_default();
                    for (val_id, val_ty) in owned_values {
                        drops.push(FipDrop::Value(val_id, val_ty));
                    }
                }
            }
            Terminator::Match { value, .. } => {
                owned_values.remove(value);
                if !owned_values.is_empty() {
                    let drops = plan.before_term.entry(b).or_default();
                    for (val_id, val_ty) in owned_values {
                        drops.push(FipDrop::Value(val_id, val_ty));
                    }
                }
            }
            Terminator::Unreachable => {
                block_div = true;
            }
        }
    }
    (slots, block_div)
}
/// Whether value `v` (defined in block `b`) is used by another block.
fn used_outside_block(f: &crate::ir::FunctionIr, b: usize, v: ValueId) -> bool {
    use crate::ir::Terminator;
    f.blocks.iter().enumerate().any(|(i, blk)| {
        i != b
            && (blk
                .instructions
                .iter()
                .any(|inst| crate::ir_inline::operands(&inst.op).contains(&v))
                || match &blk.terminator {
                    Terminator::Jump { args, .. } => args.contains(&v),
                    Terminator::Branch { condition, .. } => *condition == v,
                    Terminator::Match { value, .. } => *value == v,
                    Terminator::Return(r) => *r == v,
                    Terminator::Unreachable => false,
                })
    })
}
/// Whether `inst` can abort its graph with a `Fail` (its own or a callee's).
fn may_abort(inst: &crate::ir::Instruction) -> bool {
    use crate::ir::Op;
    inst.fallible
        || matches!(
            inst.op,
            Op::Call { .. }
                | Op::Indirect { .. }
                | Op::Try(_)
                | Op::Handle { .. }
                | Op::Seq { .. }
                | Op::Tiled { .. }
        )
}
/// What a native sequence drop frees ([`seq_drop_plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeqDrop {
    /// A `ChunkSeq[T]` handle: its header, the buffer it owns and the
    /// elements still in it ([`needs_drop`]).
    Seq,
    /// An inline `String` value: the buffer it owns (a literal owns none).
    Str,
    /// A by-value (`sink`) aggregate parameter: the sequences and strings
    /// inside it ([`needs_drop`]).
    Owned,
    /// A `let`-bound struct value: what its fields own (nested structs
    /// included). Native code plans it only when every sequence sits in a
    /// product field (not an enum payload).
    Fields,
}

/// Whether dropping a value of `ty` frees memory that native code owns: a
/// `ChunkSeq`, a `String`, or a tuple, struct or enum holding one in a
/// field (§13). A `Box` is not: Box drops are elaborated by
/// [`elaborate_non_fip_drops`]; only the elements of a dropped sequence free
/// the boxes they hold. Views, `Str`, tiled sequences and region handles
/// own nothing.
pub fn needs_drop(world: &World, ty: &Type) -> bool {
    drop_visit(world, ty, &mut BTreeSet::new(), false)
}

/// [`needs_drop`], counting a type it cannot see into (a generic parameter
/// of an instance not yet substituted) as one that may own something.
pub fn may_need_drop(world: &World, ty: &Type) -> bool {
    drop_visit(world, ty, &mut BTreeSet::new(), true)
}

fn drop_visit(world: &World, ty: &Type, seen: &mut BTreeSet<String>, unknown: bool) -> bool {
    {
        let visit = |world: &World, ty: &Type, seen: &mut BTreeSet<String>| {
            drop_visit(world, ty, seen, unknown)
        };
        let ty = crate::numeric::default_type(ty);
        if crate::seq::is_seq_type(&ty) || crate::strings::is_string_type(&ty) {
            return true;
        }
        // A trait object (or a `dyn` parameter's hidden type) may own
        // anything: its vtable's drop decides.
        if crate::dyn_trait::is_object(&ty)
            || matches!(&ty, Type::Named(n, a) if a.is_empty() && crate::dyn_trait::is_dyn_generic(n))
        {
            return true;
        }
        match &ty {
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if is_box_type(world, &ty)
                    || crate::seq::is_slice_type(&ty)
                    || crate::tiled::is_seq_type(&ty)
                    || crate::regions::is_handle_type(&ty)
                {
                    return false;
                }
                let Some(a) = world.adts.get(n) else {
                    // Not a data type: a scalar, a vector, `Str`, or a
                    // generic parameter not yet substituted.
                    return unknown
                        && !crate::numeric::is_scalar_name(n)
                        && n != "Never"
                        && !crate::strings::is_text(&ty)
                        && crate::vector::layout_words(&ty).is_none();
                };
                if a.generics.len() != args.len() {
                    return unknown;
                }
                let key = ty.to_string();
                if !seen.insert(key.clone()) {
                    return false;
                }
                let vars: BTreeMap<String, Type> = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                let found = a.variants.iter().any(|v| {
                    v.fields
                        .iter()
                        .any(|f| visit(world, &substitute(&f.ty, &vars), seen))
                });
                seen.remove(&key);
                found
            }
            Type::Infer => unknown,
            _ => false,
        }
    }
}

/// Whether an element of a `ChunkSeq[ty]` owns something its sequence
/// frees when it is dropped, cleared, truncated or overwritten: what
/// [`needs_drop`] names, and boxes (no Box drop tracks a sequence buffer).
pub fn elem_needs_drop(world: &World, ty: &Type) -> bool {
    needs_drop(world, ty) || type_contains_box(world, &crate::numeric::default_type(ty))
}

/// The function-local `ChunkSeq` and `String` slots that native code frees
/// itself (§13), with what each frees: `let` bindings and by-value (`sink`)
/// parameters that the function owns. `in` and `inout` parameters belong to
/// the caller.
///
/// Native code keeps the drop state in the slot. A slot starts empty; a
/// sequence moved out of it (a `sink` read: returned, passed by value,
/// stored into an aggregate or bound elsewhere) empties it; binding or
/// assigning it frees the sequence it held; every exit of the function
/// (return, `return` from a nested block, a `Fail` exit) frees what it
/// holds. A sequence is therefore freed once, by its last owner.
///
/// A string slot cannot be emptied that way (its value lives inline in the
/// frame), so a string is only planned when its slot is never moved from
/// or captured (and, for a parameter, never assigned): its last owner is
/// then the function. A `let`-bound string has its own storage, which an
/// assignment overwrites after freeing what it held. A dropped
/// sequence also drops the elements still in it ([`elem_needs_drop`]).
///
/// Bindings inside a destructuring `let` own what they bind, as a plain
/// binding does. A by-value (`sink`) parameter of an aggregate type that
/// [`needs_drop`] is planned as [`SeqDrop::Owned`] unless part of it is moved
/// out; a `let`-bound struct holding a sequence is planned as
/// [`SeqDrop::Fields`] (#972). Both free everything their value owns, and a
/// field of either may be replaced ([`replace_drops`]).
///
/// Slots a closure names (by alias or move) or a handler captures by move,
/// external captures, partial moves, match-arm bindings and bindings under a
/// `Box` pattern are never planned; they keep today's behavior (freed at JIT
/// teardown, never in executables). `@fip` functions are never planned.
pub fn seq_drop_plan(world: &World, f: &crate::ir::FunctionIr) -> BTreeMap<String, SeqDrop> {
    use crate::ir::Op;
    let mut plan = BTreeMap::new();
    if world.functions.get(&f.name).is_some_and(|src| src.fip) {
        return plan;
    }
    for (name, ty) in &f.locals {
        let param = f.params.iter().find(|p| &p.name == name);
        let kind = if crate::seq::is_seq_type(ty) {
            SeqDrop::Seq
        } else if crate::strings::is_string_type(ty) {
            SeqDrop::Str
        } else if param.is_some() && needs_drop(world, ty) {
            SeqDrop::Owned
        } else if matches!(ty, Type::Named(n, _) if world.adts.contains_key(n))
            && crate::seq::holds_sequence(world, ty)
            && param.is_none()
        {
            SeqDrop::Fields
        } else {
            continue;
        };
        if f.external_captures.contains(name) {
            continue;
        }
        if param.is_some_and(|p| p.pass != Pass::Sink) {
            continue;
        }
        plan.insert(name.clone(), kind);
    }
    if plan.is_empty() {
        return plan;
    }
    let mut refused = BTreeSet::new();
    let defs: BTreeMap<ValueId, &Op> = f
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .map(|i| (i.dest, &i.op))
        .collect();
    fn pattern_has_box(world: &World, p: &Pattern) -> bool {
        is_box_pattern(world, p)
            || match &p.kind {
                PatternKind::Tuple(ps) => ps.iter().any(|p| pattern_has_box(world, p)),
                PatternKind::Constructor { fields, .. } => {
                    fields.iter().any(|(_, p)| pattern_has_box(world, p))
                }
                _ => false,
            }
    }
    fn pattern_names(p: &Pattern, out: &mut Vec<String>) {
        match &p.kind {
            PatternKind::Bind(n) => out.push(n.clone()),
            PatternKind::Tuple(ps) => ps.iter().for_each(|p| pattern_names(p, out)),
            PatternKind::Constructor { fields, .. } => {
                fields.iter().for_each(|(_, p)| pattern_names(p, out));
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
    }
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &inst.op {
            Op::Read {
                root, path, pass, ..
            } if *pass == Pass::Sink => {
                // Reading a field that owns nothing (such as `w.k`) copies
                // it out and leaves the struct's drops alone.
                if (!path.is_empty() && may_need_drop(world, &inst.ty))
                    || plan.get(root) == Some(&SeqDrop::Str)
                {
                    refused.insert(root.clone());
                }
            }
            Op::Write { place, op, .. } => {
                // A field of an owned aggregate may be replaced: the write
                // frees the value it replaces ([`replace_drops`]). A
                // `let`-bound string is assigned into its own storage; a
                // string parameter has none.
                let root = place.split('.').next().unwrap_or("");
                if (place != root
                    && !matches!(plan.get(root), Some(SeqDrop::Owned | SeqDrop::Fields)))
                    || op != "="
                    || (plan.get(root) == Some(&SeqDrop::Str)
                        && f.params.iter().any(|p| p.name == root))
                {
                    refused.insert(root.to_owned());
                }
            }
            Op::Bind { pattern, value } => {
                let mut names = vec![];
                pattern_names(pattern, &mut names);
                // A binding owns its value unless the value is a borrow; a
                // `Box` pattern frees its box as it binds.
                if matches!(defs.get(value), Some(Op::Read { pass, .. }) if *pass != Pass::Sink)
                    || (!matches!(pattern.kind, PatternKind::Bind(_))
                        && pattern_has_box(world, pattern))
                {
                    refused.extend(names);
                }
            }
            Op::Closure { aliases, moves, .. } => {
                refused.extend(aliases.keys().cloned());
                refused.extend(aliases.values().cloned());
                refused.extend(moves.iter().map(|p| p.root.clone()));
            }
            // A handled block and its clauses are graphs emitted inside this
            // function that name its slots through aliases; their sequence
            // moves and assignments update the same slots. A string they
            // name, and places moved into the handler, are not tracked.
            Op::Handle { aliases, moves, .. } => {
                refused.extend(moves.iter().map(|p| p.root.clone()));
                refused.extend(
                    aliases
                        .values()
                        .filter(|slot| plan.get(*slot) == Some(&SeqDrop::Str))
                        .cloned(),
                );
            }
            _ => {}
        }
    }
    // A match arm binds through its pattern, not a `let`.
    for b in &f.blocks {
        if let crate::ir::Terminator::Match { arms, .. } = &b.terminator {
            for (p, _) in arms {
                let mut names = vec![];
                pattern_names(p, &mut names);
                refused.extend(names);
            }
        }
    }
    plan.retain(|name, _| !refused.contains(name));
    plan
}

/// Where a native graph drops an owned temporary ([`temporary_drops`]).
pub const AT_BLOCK_ENTRY: usize = usize::MAX;

/// Owned values that a native graph never binds, moves or returns, keyed by
/// where they are dropped: after the instruction of their last use, after
/// their definition when nothing uses them, or at the entry of the block
/// whose parameter receives them ([`AT_BLOCK_ENTRY`]) when that parameter
/// is unused (§13, port gaps lane 2). A value is owned when a call
/// (`Call`, `Indirect`) or a sequence constructor produces it, when it
/// moves a [`seq_drop_plan`] slot out (`Read` by value: the slot is then
/// empty), or when it is a block parameter that every incoming jump passes
/// such a value used nowhere else (the result of an inlined call). It is a
/// temporary when every other use is an `in` or `inout` argument of a call
/// in its own block: `f(in make())`, or a discarded `make();`. Callers keep
/// only the values whose type [`needs_drop`]. Any other use (a binding, a
/// sink argument, a builtin operand, a projection, a terminator) owns or
/// forwards the value.
pub fn temporary_drops(
    world: &World,
    f: &crate::ir::FunctionIr,
) -> BTreeMap<(usize, usize), Vec<ValueId>> {
    use crate::ir::{Op, Terminator};
    use crate::seq::SeqOp;
    #[derive(Default, Clone)]
    struct Uses {
        /// Call borrows, as (block, instruction).
        borrows: Vec<(usize, usize)>,
        /// Jumps that pass the value, as (target block, parameter index).
        jumps: Vec<(usize, usize)>,
        /// Any other use.
        other: bool,
    }
    let plan = seq_drop_plan(world, f);
    // Locals bound to a borrow own nothing.
    let value_defs: BTreeMap<ValueId, &Op> = f
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .map(|i| (i.dest, &i.op))
        .collect();
    let mut borrow_bound = BTreeSet::new();
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Bind { pattern, value } = &inst.op
            && let PatternKind::Bind(n) = &pattern.kind
            && matches!(value_defs.get(value), Some(Op::Read { pass, .. }) if *pass != Pass::Sink)
        {
            borrow_bound.insert(n.clone());
        }
    }
    let mut uses: BTreeMap<ValueId, Uses> = BTreeMap::new();
    // Where each value is defined: (block, instruction), or the block entry.
    let mut defs: BTreeMap<ValueId, (usize, usize)> = BTreeMap::new();
    let mut produced = BTreeSet::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for p in &block.params {
            defs.insert(*p, (b, AT_BLOCK_ENTRY));
        }
        for (i, inst) in block.instructions.iter().enumerate() {
            defs.insert(inst.dest, (b, i));
            let owned = match &inst.op {
                Op::Call { .. } | Op::Indirect { .. } => !f.fip_call_slots.contains_key(&inst.dest),
                Op::Seq { op, .. } => matches!(op, SeqOp::New | SeqOp::WithCapacity),
                Op::Read {
                    root, path, pass, ..
                } => {
                    *pass == Pass::Sink
                        && path.is_empty()
                        && match plan.get(root) {
                            // A move out of an aggregate copies it and
                            // empties the source.
                            Some(SeqDrop::Seq | SeqDrop::Owned | SeqDrop::Fields) => true,
                            // A string slot is never planned once it is
                            // moved from; the move hands its buffer on.
                            None => {
                                f.locals
                                    .get(root)
                                    .is_some_and(crate::strings::is_string_type)
                                    && !f.params.iter().any(|p| &p.name == root)
                                    && !borrow_bound.contains(root)
                            }
                            _ => false,
                        }
                }
                _ => false,
            };
            if owned {
                produced.insert(inst.dest);
            }
            let borrows: Vec<ValueId> = match &inst.op {
                Op::Call { args, .. } | Op::Indirect { args, .. } => args
                    .iter()
                    .filter(|a| a.pass != Pass::Sink && a.place.is_none())
                    .map(|a| a.value)
                    .collect(),
                _ => vec![],
            };
            let mut owning = crate::ir_inline::operands(&inst.op);
            for v in &borrows {
                if let Some(at) = owning.iter().position(|o| o == v) {
                    owning.remove(at);
                }
            }
            for v in borrows {
                uses.entry(v).or_default().borrows.push((b, i));
            }
            for v in owning {
                uses.entry(v).or_default().other = true;
            }
        }
        match &block.terminator {
            Terminator::Jump {
                block: target,
                args,
            } => {
                for (k, v) in args.iter().enumerate() {
                    uses.entry(*v).or_default().jumps.push((*target, k));
                }
            }
            t => {
                for v in crate::ir_inline::terminator_operands(t) {
                    uses.entry(v).or_default().other = true;
                }
            }
        }
    }
    // Block parameters that receive only owned values used nowhere else.
    let mut incoming: BTreeMap<(usize, usize), Vec<ValueId>> = BTreeMap::new();
    for (v, u) in &uses {
        for &j in &u.jumps {
            incoming.entry(j).or_default().push(*v);
        }
    }
    let sole_jump = |u: Option<&Uses>| {
        u.is_some_and(|u| !u.other && u.borrows.is_empty() && u.jumps.len() == 1)
    };
    loop {
        let mut grew = false;
        for (b, block) in f.blocks.iter().enumerate() {
            for (k, p) in block.params.iter().enumerate() {
                if produced.contains(p) {
                    continue;
                }
                let Some(senders) = incoming.get(&(b, k)) else {
                    continue;
                };
                if senders
                    .iter()
                    .all(|v| produced.contains(v) && sole_jump(uses.get(v)))
                {
                    produced.insert(*p);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    // A value forwarded to an owned parameter is dropped there instead.
    let forwarded: BTreeSet<ValueId> = f
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(b, block)| {
            block
                .params
                .iter()
                .enumerate()
                .filter(|(_, p)| produced.contains(*p))
                .map(move |(k, _)| (b, k))
        })
        .filter_map(|j: (usize, usize)| incoming.get(&j))
        .flatten()
        .copied()
        .collect();
    let mut drops: BTreeMap<(usize, usize), Vec<ValueId>> = BTreeMap::new();
    for v in &produced {
        if forwarded.contains(v) {
            continue;
        }
        let Some(&(b, i)) = defs.get(v) else {
            continue;
        };
        let u = uses.get(v).cloned().unwrap_or_default();
        if u.other
            || !u.jumps.is_empty()
            || u.borrows
                .iter()
                .any(|&(ub, ui)| ub != b || (i != AT_BLOCK_ENTRY && ui <= i))
        {
            continue;
        }
        let last = u.borrows.iter().map(|&(_, ui)| ui).max().unwrap_or(i);
        drops.entry((b, last)).or_default().push(*v);
    }
    drops
}

/// The writes in graph `f` that free the value they replace, as (block,
/// instruction) positions (§13, port gaps lane 2): `h.s = v` frees the old
/// `h.s`, and `h = v` frees the old `h`, when the place holds a value it
/// owns. Through an `inout` parameter it always does: nothing can be moved
/// out through one. A local or a `sink` parameter does when no closure or
/// handler names it and, on every path to the write, nothing of it was
/// moved out since it was last bound or assigned whole (a forward "maybe
/// moved" analysis over the graph's blocks). `fresh`, for a graph nested in
/// another (a handled body), limits the locals to those it declares itself;
/// the enclosing function's parameters are then out of reach. Callers apply
/// it only to places whose type [`needs_drop`], and leave whole writes of
/// [`seq_drop_plan`] slots to that plan.
pub fn replace_drops(
    world: &World,
    f: &crate::ir::FunctionIr,
    fresh: Option<&BTreeSet<String>>,
) -> BTreeSet<(usize, usize)> {
    use crate::ir::{Op, Terminator};
    let mut out = BTreeSet::new();
    if world.functions.get(&f.name).is_some_and(|src| src.fip) {
        return out;
    }
    let mut borrowed = BTreeSet::new();
    let mut candidates = BTreeSet::new();
    // Only a place that may own something can free what it holds; leaving
    // scalars out keeps the dataflow below small on large graphs.
    for (name, ty) in &f.locals {
        if f.external_captures.contains(name) || !may_need_drop(world, ty) {
            continue;
        }
        match f.params.iter().find(|p| &p.name == name) {
            Some(p) if p.pass == Pass::Inout && fresh.is_none() => {
                borrowed.insert(name.clone());
            }
            Some(p) if p.pass == Pass::Sink && fresh.is_none() => {
                candidates.insert(name.clone());
            }
            Some(_) => {}
            None if fresh.is_none_or(|names| names.contains(name)) => {
                candidates.insert(name.clone());
            }
            None => {}
        }
    }
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Closure { aliases, moves, .. } | Op::Handle { aliases, moves, .. } = &inst.op {
            for name in aliases
                .keys()
                .chain(aliases.values())
                .chain(moves.iter().map(|p| &p.root))
            {
                candidates.remove(name);
            }
        }
    }
    if candidates.is_empty() && borrowed.is_empty() {
        return out;
    }
    // What one instruction does to the set of maybe-moved roots.
    let step = |op: &Op, moved: &mut BTreeSet<String>| match op {
        Op::Read {
            root, path, pass, ..
        } if *pass == Pass::Sink && candidates.contains(root) => {
            moved.insert(root.clone());
        }
        Op::Write { place, op, .. } if op == "=" && !place.contains('.') => {
            moved.remove(place);
        }
        Op::Bind { pattern, .. } => {
            let mut names = vec![];
            fn collect(p: &Pattern, out: &mut Vec<String>) {
                match &p.kind {
                    PatternKind::Bind(n) => out.push(n.clone()),
                    PatternKind::Tuple(ps) => ps.iter().for_each(|p| collect(p, out)),
                    PatternKind::Constructor { fields, .. } => {
                        fields.iter().for_each(|(_, p)| collect(p, out));
                    }
                    PatternKind::Wild | PatternKind::Literal(_) => {}
                }
            }
            collect(pattern, &mut names);
            for n in names {
                moved.remove(&n);
            }
        }
        _ => {}
    };
    // A read of a field that owns nothing moves nothing out.
    let moves = |inst: &crate::ir::Instruction| match &inst.op {
        Op::Read { path, pass, .. } if *pass == Pass::Sink && !path.is_empty() => {
            may_need_drop(world, &inst.ty)
        }
        _ => true,
    };
    let n = f.blocks.len();
    let mut preds: Vec<Vec<usize>> = vec![vec![]; n];
    for (b, block) in f.blocks.iter().enumerate() {
        let succs: Vec<usize> = match &block.terminator {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, t)| *t).collect(),
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        };
        for t in succs {
            if t < n {
                preds[t].push(b);
            }
        }
    }
    let mut exit: Vec<Option<BTreeSet<String>>> = vec![None; n];
    let entry_state = |b: usize, exit: &Vec<Option<BTreeSet<String>>>| {
        let mut state = BTreeSet::new();
        for &p in &preds[b] {
            if let Some(s) = &exit[p] {
                state.extend(s.iter().cloned());
            }
        }
        state
    };
    loop {
        let mut changed = false;
        for (b, block) in f.blocks.iter().enumerate() {
            let mut state = entry_state(b, &exit);
            for inst in &block.instructions {
                if moves(inst) {
                    step(&inst.op, &mut state);
                }
            }
            if exit[b].as_ref() != Some(&state) {
                exit[b] = Some(state);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (b, block) in f.blocks.iter().enumerate() {
        let mut state = entry_state(b, &exit);
        for (i, inst) in block.instructions.iter().enumerate() {
            if let Op::Write { place, op, .. } = &inst.op
                && op == "="
            {
                let root = place.split('.').next().unwrap_or("");
                if borrowed.contains(root) || (candidates.contains(root) && !state.contains(root)) {
                    out.insert((b, i));
                }
            }
            if moves(inst) {
                step(&inst.op, &mut state);
            }
        }
    }
    out
}
