//! Drop imports of affine `.vsys` handles (§16.3, #1095 part 4).
//!
//! `affine extern type Probe drop aead_probe_release;` declares the import
//! that releases a `Probe` when source code drops one without consuming
//! it. The front end generates one source function per droppable type,
//! [`drop_function`] (`$drop$Probe`, and `$drop$Option$Probe` for the
//! `Option[Probe]` the library's constructors return), each calling the
//! import, so the drop is an ordinary foreign call that every backend and
//! the evaluator already run.
//!
//! [`insert`] then places a call of that function at each point the IR
//! drops such a value: the `Box[T]` drop elaboration
//! ([`crate::fip::handle_drop_plan`]) run with the handle types in place of
//! `Box`, so a handle is released exactly once on every path, loops and
//! early exits included. It runs right after [`crate::ir::lower_world`]
//! ([`lower_world`]) in the native backends and the evaluator. The call
//! performs `Ffi[Lib]`, so code that may drop a handle needs `Ffi[Lib]` in
//! its row ([`check_rows`]); a handle live across an abortable effect is
//! refused by the checker (`E0402`), as a linear one is, because an abort
//! would skip its drop.
use super::{FfiError, FfiErrorKind};
use crate::ast::*;
use crate::check::World;
use crate::fip::FipDrop;
use crate::ir::{Argument, FunctionIr, Instruction, Op, Program, ValueId};
use std::collections::{BTreeMap, BTreeSet};

/// The prefix of generated drop functions.
pub const DROP_PREFIX: &str = "$drop$";

/// The generated drop function of values of `ty`: `$drop$H` for a handle
/// `H`, `$drop$Option$H` for `Option[H]`.
pub fn drop_function(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(n, args) if args.is_empty() => Some(format!("{DROP_PREFIX}{n}")),
        Type::Named(o, args) if o == "Option" && args.len() == 1 => match &args[0] {
            Type::Named(n, inner) if inner.is_empty() => Some(format!("{DROP_PREFIX}Option${n}")),
            _ => None,
        },
        _ => None,
    }
}

/// The source functions that release droppable handles of `library`
/// (before the foreign-call rewrite: they call `Lib::f` like source code).
/// One past the largest source span end in `module`: generated functions
/// are parsed at offsets beyond it, so their expression spans (the keys of
/// the checker's type table) never meet a source expression's.
pub(super) fn span_base(module: &Module) -> usize {
    fn visit(v: &serde_json::Value, max: &mut usize) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(e) = m.get("end").and_then(serde_json::Value::as_u64) {
                    *max = (*max).max(e as usize);
                }
                m.values().for_each(|v| visit(v, max));
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| visit(v, max)),
            _ => {}
        }
    }
    let mut max = 0;
    if let Ok(v) = serde_json::to_value(module) {
        visit(&v, &mut max);
    }
    max + 1
}

/// The release functions of `library`'s droppable handles, parsed at
/// offset `base` and up; returns them and the offset after them.
pub(super) fn generated(
    library: &str,
    descriptor: &vesper_ffi::sys::LibraryDescriptor,
    mut base: usize,
) -> (Vec<Item>, usize) {
    let mut items = vec![];
    for t in descriptor.extern_types() {
        let Some(symbol) = t.drop_import() else {
            continue;
        };
        let h = t.name();
        let text = format!(
            "fn drop_handle(sink h: {h}) eff [Ffi[{library}]] {{ {library}::{symbol}(h) }}\n\
             fn drop_option(sink o: Option[{h}]) eff [Ffi[{library}]] {{\n\
                 match o {{ Some(h) => {library}::{symbol}(h), None => () }}\n\
             }}\n"
        );
        let text = format!("{}{text}", " ".repeat(base));
        base = text.len() + 1;
        let parsed = crate::parser::parse(&text);
        debug_assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        for (item, ty) in parsed.module.items.into_iter().zip([
            Type::Named(h.to_owned(), vec![]),
            Type::Named("Option".into(), vec![Type::Named(h.to_owned(), vec![])]),
        ]) {
            let Item::Function(mut f) = item else {
                continue;
            };
            f.name = drop_function(&ty).expect("handle drop name");
            items.push(Item::Function(f));
        }
    }
    (items, base)
}

/// The handle types with a drop import, by the effect of their library.
fn droppable(world: &World) -> BTreeMap<String, String> {
    world
        .functions
        .iter()
        .filter_map(|(name, f)| {
            let handle = name.strip_prefix(DROP_PREFIX)?;
            if handle.contains('$') {
                return None;
            }
            let effect = f.effects.instances.first()?.name.clone();
            Some((handle.to_owned(), effect))
        })
        .collect()
}

/// [`crate::ir::lower_world`] with the handle drops [`insert`]ed.
pub fn lower_world(world: &World) -> Program {
    let mut program = crate::ir::lower_world(world);
    // A refused drop is reported by `check_module` before any backend runs.
    let _ = insert(world, &mut program);
    program
}

/// One inserted drop: the graph, the handle type and its library effect.
#[derive(Debug)]
pub(super) struct DropSite {
    graph: String,
    handle: String,
    effect: String,
}

/// Insert a call of the generated drop function at each point a graph of
/// `program` drops a handle with a drop import, and rethread the state
/// tokens. Returns the sites, or an error for a drop it cannot place.
pub(super) fn insert(world: &World, program: &mut Program) -> Result<Vec<DropSite>, String> {
    let handles = droppable(world);
    if handles.is_empty() {
        return Ok(vec![]);
    }
    let names: BTreeSet<String> = handles.keys().cloned().collect();
    let mut sites = vec![];
    let mut error = None;
    for f in program.functions.values_mut() {
        if f.name.starts_with(DROP_PREFIX)
            || world.functions.get(&f.name).is_some_and(|src| src.fip)
        {
            continue;
        }
        release_wildcards(world, f, &handles, &mut sites);
        if let Err(e) = refuse_unplaced(world, f, &names) {
            error.get_or_insert(e);
            continue;
        }
        let Some(plan) = crate::fip::handle_drop_plan(world, f, &names) else {
            error.get_or_insert_with(|| {
                format!(
                    "the drops of the handles in '{}' do not reach a fixed point",
                    f.name
                )
            });
            continue;
        };
        if let Err(e) = apply(world, f, plan, &handles, &mut sites) {
            error.get_or_insert(e);
        }
    }
    crate::ir::thread_tokens(program);
    match error {
        Some(e) => Err(e),
        None => Ok(sites),
    }
}

/// Whether a value of `ty` holds a handle of `names`.
fn holds(world: &World, ty: &Type, names: &BTreeSet<String>, seen: &mut BTreeSet<String>) -> bool {
    match ty {
        Type::Named(n, args) => {
            if names.contains(n) || args.iter().any(|a| holds(world, a, names, seen)) {
                return true;
            }
            if !seen.insert(n.clone()) {
                return false;
            }
            let found = world.adts.get(n).is_some_and(|a| {
                let vars: BTreeMap<String, Type> = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                a.variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .any(|f| holds(world, &substitute(&f.ty, &vars), names, seen))
            });
            seen.remove(n);
            found
        }
        Type::Tuple(ts) => ts.iter().any(|t| holds(world, t, names, seen)),
        _ => false,
    }
}

/// `let _ = v` discards a handle (or an `Option` of one): the bind becomes
/// the release call, which consumes `v` as the bind did.
fn release_wildcards(
    world: &World,
    f: &mut FunctionIr,
    names: &BTreeMap<String, String>,
    sites: &mut Vec<DropSite>,
) {
    // A wildcard arm of a match on a borrowed place (§4.3) discards nothing:
    // the handle stays the caller's.
    let borrowed = crate::fip::borrowed_scrutinees(f);
    for b in &mut f.blocks {
        for inst in &mut b.instructions {
            let Op::Bind { pattern, value } = &inst.op else {
                continue;
            };
            if !matches!(pattern.kind, PatternKind::Wild) || borrowed.contains(value) {
                continue;
            }
            let Some(ty) = f.values.get(value) else {
                continue;
            };
            let handle = match ty {
                Type::Named(n, a) if a.is_empty() && names.contains_key(n) => n.clone(),
                Type::Named(o, a) if o == "Option" => match &a[..] {
                    [Type::Named(n, x)] if x.is_empty() && names.contains_key(n) => n.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            let Some(callee) = drop_function(ty) else {
                continue;
            };
            let Some(target) = world.functions.get(&callee) else {
                continue;
            };
            let value = *value;
            inst.op = Op::Call {
                callee,
                args: vec![Argument {
                    pass: Pass::Sink,
                    value,
                    place: None,
                }],
                types: vec![],
                capability: None,
            };
            inst.ty = target.result.clone();
            f.values.insert(inst.dest, target.result.clone());
            sites.push(DropSite {
                graph: f.name.clone(),
                effect: names[&handle].clone(),
                handle,
            });
        }
    }
}

/// Drops the elaboration cannot place: a handle discarded by a `_` inside
/// a destructuring pattern, and a handle moved into a closure (dropping an
/// unused closure does not release what it captured).
fn refuse_unplaced(world: &World, f: &FunctionIr, names: &BTreeSet<String>) -> Result<(), String> {
    fn wild_handle(world: &World, p: &Pattern, ty: &Type, names: &BTreeSet<String>) -> bool {
        match (&p.kind, ty) {
            (PatternKind::Wild, _) => holds(world, ty, names, &mut BTreeSet::new()),
            (PatternKind::Tuple(ps), Type::Tuple(ts)) => ps
                .iter()
                .zip(ts)
                .any(|(p, t)| wild_handle(world, p, t, names)),
            (PatternKind::Constructor { name, fields, .. }, Type::Named(_, args)) => {
                let Some((a, v)) = world.constructor(name) else {
                    return false;
                };
                let vars: BTreeMap<String, Type> = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                fields.iter().any(|(fname, sub)| {
                    v.fields
                        .iter()
                        .find(|f| &f.name == fname)
                        .is_some_and(|f| wild_handle(world, sub, &substitute(&f.ty, &vars), names))
                })
            }
            _ => false,
        }
    }
    // A borrowed match (§4.3) discards nothing: its `_`s leave the handle
    // with the caller.
    let borrowed = crate::fip::borrowed_scrutinees(f);
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &inst.op {
            Op::Bind { pattern, value } => {
                if !borrowed.contains(value)
                    && let Some(ty) = f.values.get(value)
                    && wild_handle(world, pattern, ty, names)
                {
                    return Err(format!(
                        "'{}' discards a handle with a drop import with `_` inside a pattern; bind it to a name so it is released",
                        f.name
                    ));
                }
            }
            Op::Closure { moves, .. } => {
                for m in moves {
                    let ty = f
                        .locals
                        .get(&m.root)
                        .or_else(|| f.params.iter().find(|p| p.name == m.root).map(|p| &p.ty))
                        .and_then(|t| {
                            if m.path.is_empty() {
                                Some(t.clone())
                            } else {
                                world.field(t, &m.path)
                            }
                        });
                    if ty.is_some_and(|t| holds(world, &t, names, &mut BTreeSet::new())) {
                        return Err(format!(
                            "'{}' moves a handle with a drop import into a closure; a dropped closure does not release it, so pass the handle to the closure's call instead",
                            f.name
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn apply(
    world: &World,
    f: &mut FunctionIr,
    plan: crate::fip::HandleDrops,
    handles: &BTreeMap<String, String>,
    sites: &mut Vec<DropSite>,
) -> Result<(), String> {
    if plan.before_inst.is_empty() && plan.after_inst.is_empty() && plan.before_term.is_empty() {
        return Ok(());
    }
    let mut next: ValueId = f
        .values
        .keys()
        .copied()
        .chain(f.blocks.iter().flat_map(|b| {
            b.params
                .iter()
                .copied()
                .chain(b.instructions.iter().map(|i| i.dest))
        }))
        .max()
        .map_or(0, |m| m + 1);
    let graph = f.name.clone();
    let mut emit = |f: &mut FunctionIr, drop: &FipDrop, out: &mut Vec<Instruction>| {
        let (value, ty) = match drop {
            FipDrop::Value(v, ty) => (Some(*v), ty.clone()),
            FipDrop::Place(_, ty) => (None, ty.clone()),
            FipDrop::OtherFields { ty, keep, .. } => {
                return Err(format!(
                    "'{graph}' drops the fields of a `{}` other than `{keep}`, and one holds a handle with a drop import; bind the value and move each handle out",
                    ty
                ));
            }
        };
        let handle = match &ty {
            Type::Named(n, args) if args.is_empty() && handles.contains_key(n) => n.clone(),
            Type::Named(o, args)
                if o == "Option"
                    && matches!(&args[..], [Type::Named(n, a)] if a.is_empty() && handles.contains_key(n)) =>
            {
                let Type::Named(n, _) = &args[0] else {
                    unreachable!()
                };
                n.clone()
            }
            other => {
                return Err(format!(
                    "'{graph}' drops a `{}` holding a handle with a drop import; only the handle itself or an `Option` of it is released implicitly, so move the handle out first",
                    other
                ));
            }
        };
        let callee = drop_function(&ty).expect("droppable type");
        let Some(target) = world.functions.get(&callee) else {
            return Err(format!("no drop function for `{}`", ty));
        };
        let value = match (value, drop) {
            (Some(v), _) => v,
            (None, FipDrop::Place(place, _)) => {
                let v = next;
                next += 1;
                f.values.insert(v, ty.clone());
                out.push(Instruction {
                    dest: v,
                    op: Op::Read {
                        root: place.root.clone(),
                        path: place.path.clone(),
                        pass: Pass::Sink,
                        projections: vec![],
                    },
                    ty: ty.clone(),
                    span: 0..0,
                    token: None,
                    fallible: false,
                });
                v
            }
            _ => unreachable!(),
        };
        let dest = next;
        next += 1;
        f.values.insert(dest, target.result.clone());
        out.push(Instruction {
            dest,
            op: Op::Call {
                callee,
                args: vec![Argument {
                    pass: Pass::Sink,
                    value,
                    place: None,
                }],
                types: vec![],
                capability: None,
            },
            ty: target.result.clone(),
            span: 0..0,
            token: None,
            fallible: false,
        });
        sites.push(DropSite {
            graph: graph.clone(),
            effect: handles[&handle].clone(),
            handle,
        });
        Ok(())
    };
    for b in 0..f.blocks.len() {
        let before = plan.before_inst.get(&b);
        let after = plan.after_inst.get(&b);
        let term = plan.before_term.get(&b);
        if before.is_none() && after.is_none() && term.is_none() {
            continue;
        }
        let old = std::mem::take(&mut f.blocks[b].instructions);
        let mut out = Vec::with_capacity(old.len() + 2);
        for (i, inst) in old.into_iter().enumerate() {
            for (_, d) in before.into_iter().flatten().filter(|(at, _)| *at == i) {
                emit(f, d, &mut out)?;
            }
            out.push(inst);
            for (_, d) in after.into_iter().flatten().filter(|(at, _)| *at == i) {
                emit(f, d, &mut out)?;
            }
        }
        for d in term.into_iter().flatten() {
            emit(f, d, &mut out)?;
        }
        f.blocks[b].instructions = out;
    }
    Ok(())
}

/// Lower `world` and check that every graph that drops a handle with a
/// drop import performs its library's `Ffi[Lib]` (the drop is a foreign
/// call), and that every drop could be placed.
pub(super) fn check_rows(world: &World) -> Result<(), FfiError> {
    let mut program = crate::ir::lower_world(world);
    let sites =
        insert(world, &mut program).map_err(|m| FfiError::new(FfiErrorKind::Check, m, None))?;
    let mut diagnostics = vec![];
    let mut seen = BTreeSet::new();
    for site in sites {
        let Some(f) = program.functions.get(&site.graph) else {
            continue;
        };
        let performs = f
            .effects
            .instances
            .iter()
            .any(|e| e.name == site.effect && e.label.is_none());
        if performs || !seen.insert((site.graph.clone(), site.effect.clone())) {
            continue;
        }
        let library = site
            .effect
            .strip_prefix(super::EFFECT_PREFIX)
            .unwrap_or(&site.effect);
        let span = world
            .functions
            .get(&site.graph)
            .map_or(0..0, |src| src.span.clone());
        diagnostics.push(Diagnostic {
            code: "E0401".into(),
            message: format!(
                "'{}' may drop a `{}`, which calls its drop import; add Ffi[{library}] to its effect row",
                site.graph, site.handle
            ),
            span,
            related: vec![],
            help: Some(format!(
                "An affine handle with a drop import is released by a foreign call wherever it is dropped, so dropping it performs Ffi[{library}]."
            )),
        });
    }
    if diagnostics.is_empty() {
        return Ok(());
    }
    let mut e = FfiError::new(
        FfiErrorKind::Check,
        format!("{} semantic error(s)", diagnostics.len()),
        None,
    );
    e.diagnostics = diagnostics;
    Err(e)
}

/// Whether a world function is a generated drop function.
pub fn is_drop_function(name: &str) -> bool {
    name.starts_with(DROP_PREFIX)
}

/// The drop functions the graphs of world function `name` call: its own
/// graph and the closures and handled bodies nested in its source span.
/// Native instance discovery walks source bodies, which never name them.
pub fn drop_callees(world: &World, program: &Program, name: &str) -> Vec<String> {
    let Some(src) = world.functions.get(name) else {
        return vec![];
    };
    let span = src.span.clone();
    let nested = |graph: &str| {
        let Some(rest) = graph.strip_prefix('$') else {
            return false;
        };
        let mut parts = rest.rsplitn(3, '_');
        let (Some(end), Some(start)) = (parts.next(), parts.next()) else {
            return false;
        };
        matches!((start.parse::<usize>(), end.parse::<usize>()), (Ok(s), Ok(e)) if s >= span.start && e <= span.end)
    };
    let mut out = vec![];
    for f in program.functions.values() {
        if f.name != name && !nested(&f.name) {
            continue;
        }
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Op::Call { callee, .. } = &i.op
                && callee.starts_with(DROP_PREFIX)
                && !out.contains(callee)
            {
                out.push(callee.clone());
            }
        }
    }
    out
}
