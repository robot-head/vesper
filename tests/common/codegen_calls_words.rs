//! The input of `@vesper/codegen-calls` (packages/vesper-codegen-calls):
//! check-data's names, types and world, place-ir's program section, then
//! the calls section (data.ves). Shared by tests/self_host_codegen_calls.rs
//! and tests/self_host_native_walk_frame.rs, which declare `check_words`
//! and `ir_words` next to this module (as does vesper-hybrid).
#![allow(dead_code)]

use super::check_words::{Enc, encodable, pass_word};
use super::ir_words::{self, Payload, Tables};
use vesper::ast::{Expr, ExprKind, Function, Statement, Type};
use vesper::check::World;
use vesper::ir::Program;

// ------------------------------------------------------------------ input

/// `ExprKind` tags, in declaration order (`E_*` of data.ves).
fn expr_kind(k: &ExprKind) -> u64 {
    match k {
        ExprKind::Literal(_) => 0,
        ExprKind::Name(_) => 1,
        ExprKind::Tuple(_) => 2,
        ExprKind::Struct { .. } => 3,
        ExprKind::Block { .. } => 4,
        ExprKind::Call { .. } => 5,
        ExprKind::Field(..) => 6,
        ExprKind::Index(..) => 7,
        ExprKind::Try(_) => 8,
        ExprKind::Unary(..) => 9,
        ExprKind::Binary(..) => 10,
        ExprKind::Assign(..) => 11,
        ExprKind::If { .. } => 12,
        ExprKind::Match { .. } => 13,
        ExprKind::Closure { .. } => 14,
        ExprKind::Handle { .. } => 15,
        ExprKind::Loop(_) => 16,
        ExprKind::For { .. } => 17,
        ExprKind::Range { .. } => 18,
        ExprKind::Break(_) => 19,
        ExprKind::Return(_) => 20,
        ExprKind::Continue => 21,
    }
}

/// The direct subexpressions of `e`, in evaluation order: a call's callee
/// then its arguments, a block's `let` values and expression statements
/// then its tail, a handler's state, body, return clause and operation
/// clauses. Patterns and types are not expressions.
fn children(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => vec![],
        ExprKind::Tuple(es) => es.iter().collect(),
        ExprKind::Struct { fields, .. } => fields.iter().map(|(_, e)| e).collect(),
        ExprKind::Block { statements, tail } => statements
            .iter()
            .map(|s| match s {
                Statement::Let { value, .. } => value,
                Statement::Expr(e) => e,
            })
            .chain(tail.as_deref())
            .collect(),
        ExprKind::Call { callee, args, .. } => std::iter::once(&**callee)
            .chain(args.iter().map(|a| &a.expr))
            .collect(),
        ExprKind::Unary(_, e) | ExprKind::Field(e, _) | ExprKind::Try(e) => vec![e],
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            vec![a, b]
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => [&**condition, &**then]
            .into_iter()
            .chain(otherwise.as_deref())
            .collect(),
        ExprKind::Match { value, arms } => std::iter::once(&**value)
            .chain(arms.iter().map(|(_, e)| e))
            .collect(),
        ExprKind::Closure { body, .. } => vec![body],
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => state
            .iter()
            .map(|(_, e)| &**e)
            .chain([&**body, &ret.body])
            .chain(ops.iter().map(|c| &c.body))
            .collect(),
        k => k.control_children().unwrap_or_default(),
    }
}

/// Pre-order rows `(kind, start, end, symbol + 1, children, size)`.
fn put_expr(e: &Expr, enc: &mut Enc, rows: &mut Vec<[u64; 6]>) {
    let at = rows.len();
    let sym = match &e.kind {
        ExprKind::Name(n) | ExprKind::Field(_, n) => enc.name(n) + 1,
        _ => 0,
    };
    let kids = children(e);
    rows.push([
        expr_kind(&e.kind),
        e.span.start as u64,
        e.span.end as u64,
        sym,
        kids.len() as u64,
        0,
    ]);
    for c in kids {
        put_expr(c, enc, rows);
    }
    rows[at][5] = (rows.len() - at) as u64;
}

fn function_types(f: &Function) -> Vec<Type> {
    let mut out: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
    out.push(f.result.clone());
    out.push(Type::Row(f.effects.clone()));
    out
}

/// Whether every type the calls section writes fits the decoder.
fn calls_encodable(w: &World) -> bool {
    w.functions
        .values()
        .all(|f| function_types(f).iter().all(encodable))
        && w.calls.values().flatten().all(|(_, t)| encodable(t))
        && w.dyn_objects
            .params
            .values()
            .flatten()
            .all(|(_, t)| encodable(t))
        && w.dyn_objects.vtables.iter().all(|v| {
            encodable(&v.object)
                && encodable(&v.self_ty)
                && v.methods
                    .iter()
                    .flat_map(|(_, vs)| vs.values())
                    .all(encodable)
        })
}

/// The calls section (packages/vesper-codegen-calls/src/data.ves).
fn calls_section(e: &mut Enc, w: &World, tasks: &[(String, u64)]) -> Vec<u64> {
    let mut out = vec![vesper::trace_effects::active(w) as u64];
    let mut exprs = vec![];
    out.push(w.functions.len() as u64);
    for f in w.functions.values() {
        out.push(e.name(&f.name));
        out.push(f.fip as u64);
        out.extend([f.span.start as u64, f.span.end as u64]);
        out.push(f.generics.len() as u64);
        for g in &f.generics {
            out.push(e.name(&g.name));
        }
        out.push(f.params.len() as u64);
        for p in &f.params {
            out.push(e.name(&p.name));
            out.push(pass_word(p.pass));
            out.push(e.ty(&p.ty));
        }
        out.push(e.ty(&f.result));
        out.push(e.ty(&Type::Row(f.effects.clone())));
        match &f.body {
            Some(b) => {
                out.push(exprs.len() as u64 + 1);
                put_expr(b, e, &mut exprs);
            }
            None => out.push(0),
        }
    }
    out.push(exprs.len() as u64);
    for r in &exprs {
        out.extend(r);
    }
    let pairs = |e: &mut Enc, vs: &std::collections::BTreeMap<String, Type>, out: &mut Vec<u64>| {
        out.push(vs.len() as u64);
        for (n, t) in vs {
            out.push(e.name(n));
            out.push(e.ty(t));
        }
    };
    out.push(w.calls.len() as u64);
    for ((s, t), vs) in &w.calls {
        out.extend([*s as u64, *t as u64]);
        pairs(e, vs, &mut out);
    }
    let d = &w.dyn_objects;
    out.push(d.params.len() as u64);
    for (f, vs) in &d.params {
        out.push(e.name(f));
        pairs(e, vs, &mut out);
    }
    out.push(d.stubs.len() as u64);
    for s in d.stubs.keys() {
        out.push(e.name(s));
    }
    out.push(d.vtables.len() as u64);
    for v in &d.vtables {
        out.push(e.ty(&v.object));
        out.push(e.ty(&v.self_ty));
        out.push(v.methods.len() as u64);
        for (m, vs) in &v.methods {
            out.push(e.name(m));
            pairs(e, vs, &mut out);
        }
    }
    let slots = vesper::fip::caller_slot_functions(w);
    out.push(slots.len() as u64);
    for s in &slots {
        out.push(e.name(s));
    }
    out.push(tasks.len() as u64);
    for (entry, flags) in tasks {
        out.push(e.name(entry));
        out.push(*flags);
    }
    out
}

/// `Payload::label` of tests/common/ir_words.rs: what `program_input`
/// interns for each pool entry before writing the program section.
fn pool_label(p: &Payload) -> String {
    match p {
        Payload::Seq(x) => format!("{x:?}"),
        Payload::Vector(x) => format!("{x:?}"),
        Payload::Tiled(x) => format!("{x:?}"),
        Payload::Fip(x) => format!("{x:?}"),
        Payload::Expr(_) => String::new(),
    }
}

/// The package input: check-data's names, types and world, the program
/// section of `program`, then the calls section. `None` when a type does
/// not fit the decoder.
pub fn calls_input(w: &World, program: &Program, tasks: &[(String, bool)]) -> Option<Vec<u8>> {
    let flags: Vec<(String, u64)> = tasks.iter().map(|(e, o)| (e.clone(), *o as u64)).collect();
    calls_input_flags(w, program, &flags)
}

/// [`calls_input`] with each task's flags word: 1 optimizes, 2 asks for a
/// Beacon trace host (`EntryEffects::TraceHost`).
pub fn calls_input_flags(w: &World, program: &Program, tasks: &[(String, u64)]) -> Option<Vec<u8>> {
    if !ir_words::encodable(w, program) || !calls_encodable(w) {
        return None;
    }
    let mut t = Tables::of(program);
    let labels: Vec<String> = t.pool.items.iter().map(pool_label).collect();
    for l in &labels {
        t.enc.name(l);
    }
    let mut tail = t.section();
    let world = t.enc.world(w);
    tail.extend(calls_section(&mut t.enc, w, tasks));
    Some(t.enc.finish(&world, &tail))
}
