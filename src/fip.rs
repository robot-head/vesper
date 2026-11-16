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
                modality: Mode::Linear,
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

/// Whether `ty` is the built-in `Box[T]`.
pub fn is_box_type(world: &World, ty: &Type) -> bool {
    active(world) && matches!(ty, Type::Named(n, args) if n == BOX_TYPE && args.len() == 1)
}

/// Inner type `T` when `ty` is built-in `Box[T]`.
pub fn box_inner<'a>(world: &World, ty: &'a Type) -> Option<&'a Type> {
    if !active(world) {
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
        && matches!(
            &pat.kind,
            PatternKind::Constructor { name, fields, .. }
                if matches!(name.as_str(), "Box" | "Box::Box")
                    && fields.len() == 1
                    && fields[0].0 == "0"
        )
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
                Statement::Let { pat, ty, value } => {
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

struct FipChecker<'a> {
    world: &'a World,
    caller_slot_fns: BTreeSet<String>,
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
                        Statement::Let { pat, ty, value } => {
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
                                }))
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
                                }))
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
    let mut checker = FipChecker {
        world,
        caller_slot_fns: caller_slot_functions(world),
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
