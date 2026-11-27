//! The input of `@vesper/check-core-typing`'s expression typing (SH2 lane
//! 7, #1179): the Rust encoder of `exprs_input` over a checked world and
//! the readers of both sides' expression types. Shared by
//! tests/self_host_check_core_typing_exprs.rs and
//! tests/self_host_check_world.rs (#2024 slice 3), which builds the same
//! stream in Vesper.
#![allow(dead_code)]

use crate::check_harness::*;
use std::collections::{BTreeMap, BTreeSet};
use vesper::ast::{
    Expr, ExprKind, Function, Item, Literal, Module, Pass, Pattern, PatternKind, Statement,
};
use vesper::check::World;

// Expression kinds (`E_*` in exprs.ves).
pub const E_OTHER: u64 = 0;
pub const E_INT: u64 = 1;
pub const E_FLOAT: u64 = 2;
pub const E_BOOL: u64 = 3;
pub const E_STRING: u64 = 4;
pub const E_NAME: u64 = 5;
pub const E_TUPLE: u64 = 6;
pub const E_STRUCT: u64 = 7;
pub const E_BLOCK: u64 = 8;
pub const E_CALL: u64 = 9;
pub const E_FIELD: u64 = 10;
pub const E_UNARY: u64 = 11;
pub const E_BINARY: u64 = 12;
pub const E_IF: u64 = 13;
pub const E_MATCH: u64 = 14;
pub const E_RETURN: u64 = 15;
pub const E_ASSIGN: u64 = 16;
pub const E_FOR: u64 = 17;
pub const E_RANGE: u64 = 18;
pub const E_LOOP: u64 = 19;
pub const E_BREAK: u64 = 20;
pub const E_CONTINUE: u64 = 21;
pub const E_HANDLE: u64 = 22;
pub const E_TRY: u64 = 23;
pub const E_INDEX: u64 = 40;

// Literal pattern kinds (`LIT_*` in exprs.ves).
pub const LIT_INT: u64 = 0;
pub const LIT_FLOAT: u64 = 1;
pub const LIT_BOOL: u64 = 2;
pub const LIT_WIDE_INT: u64 = 3;
pub const LIT_TYPED_INT: u64 = 4;
pub const LIT_STRING: u64 = 5;

fn unary_code(op: &str) -> u64 {
    match op {
        "!" => 1,
        "-" => 2,
        "~" => 3,
        _ => 0,
    }
}

fn binary_code(op: &str) -> u64 {
    match op {
        "+" => 1,
        "-" => 2,
        "*" => 3,
        "/" => 4,
        "%" => 5,
        "==" => 6,
        "!=" => 7,
        "<" => 8,
        "<=" => 9,
        ">" => 10,
        ">=" => 11,
        "&&" => 12,
        "||" => 13,
        "&" => 14,
        "|" => 15,
        "^" => 16,
        "<<" => 17,
        ">>" => 18,
        _ => 0,
    }
}

fn pass_code(p: Pass) -> u64 {
    pass_word(p)
}

/// A name and its constructor reading: (full name, qualifier + 1 or 0, or
/// u64::MAX when `World::constructor` refuses the path; short name).
fn name_words(e: &mut Enc, n: &str, out: &mut Vec<u64>) {
    out.push(e.name(n));
    let qual = if n.matches("::").count() > 1 {
        u64::MAX
    } else if let Some((q, _)) = n.rsplit_once("::") {
        e.name(q) + 1
    } else {
        0
    };
    out.push(qual);
    out.push(e.name(n.rsplit("::").next().unwrap_or(n)));
}

/// A pattern in pre-order: kind (0 wild, 1 bind + name, 2 true, 3 false,
/// 4 other literal, 5 tuple, 6 constructor), start, end, then the payload.
fn encode_pattern(e: &mut Enc, p: &Pattern, out: &mut Vec<u64>) {
    let (s, t) = (p.span.start as u64, p.span.end as u64);
    match &p.kind {
        PatternKind::Wild => out.extend([0, s, t]),
        PatternKind::Bind(n) => {
            out.extend([1, s, t]);
            out.push(e.name(n));
        }
        PatternKind::Literal(Literal::Bool(true)) => out.extend([2, s, t]),
        PatternKind::Literal(Literal::Bool(false)) => out.extend([3, s, t]),
        PatternKind::Literal(l) => {
            out.extend([4, s, t]);
            if let Some(kind) = p.integer_kind {
                out.extend([LIT_TYPED_INT, e.name(kind.name())]);
                return;
            }
            // The literal's type (`literal_type`), for `bind_mode`.
            // An integer past `u64::MAX` in magnitude is not ported yet (as
            // in `encode_expr`): `LIT_WIDE_INT` skips the function.
            out.push(match l {
                Literal::Int(n) if fits_literal(*n) => LIT_INT,
                Literal::Float(_) => LIT_FLOAT,
                Literal::Bool(_) => LIT_BOOL,
                Literal::Int(_) | Literal::WideInt(_) => LIT_WIDE_INT,
                Literal::String(_) => LIT_STRING,
            });
            if let Literal::Int(n) = l
                && fits_literal(*n)
            {
                out.extend([(*n < 0) as u64, n.unsigned_abs() as u64]);
            }
        }
        PatternKind::Tuple(ps) => {
            out.extend([5, s, t, ps.len() as u64]);
            for p in ps {
                encode_pattern(e, p, out);
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            out.extend([6, s, t]);
            name_words(e, name, out);
            out.push(fields.len() as u64);
            for (f, p) in fields {
                out.push(e.name(f));
                encode_pattern(e, p, out);
            }
        }
    }
}

/// Whether `call` routes a call of `name` somewhere other than a
/// constructor or a world function before it looks those up.
fn special_call(w: &World, name: &str) -> bool {
    vesper::numeric::wrapping_intrinsic(name).is_some()
        || (vesper::fip::active(w)
            && matches!(name, vesper::fip::BOX_NEW | vesper::fip::BOX_NEW_IN_CALLER))
}

fn fits_literal(n: i128) -> bool {
    n.unsigned_abs() <= u64::MAX as u128
}

/// Whether an integer literal at `span` is an array's `len()` that
/// `arrays::lower_world` wrote after checking: the checker typed the call,
/// whose array receiver it recorded inside the span. Such a literal is
/// encoded as `E_OTHER`, so its function is skipped.
fn lowered_len(w: &World, span: &vesper::ast::Span) -> bool {
    w.types
        .range((span.start, span.start)..(span.start, span.end))
        .any(|(_, ty)| vesper::arrays::element(ty).is_some())
}

/// An expression in pre-order: kind, start, end, then the payload. Kinds
/// the Vesper checker does not cover yet are `E_OTHER` with no payload.
fn encode_expr(w: &World, e: &mut Enc, x: &Expr, out: &mut Vec<u64>) {
    let (s, t) = (x.span.start as u64, x.span.end as u64);
    match &x.kind {
        ExprKind::Literal(Literal::Int(n)) if fits_literal(*n) && !lowered_len(w, &x.span) => {
            out.extend([E_INT, s, t, (*n < 0) as u64, n.unsigned_abs() as u64]);
        }
        ExprKind::Literal(Literal::Float(_)) => out.extend([E_FLOAT, s, t]),
        ExprKind::Literal(Literal::Bool(b)) => out.extend([E_BOOL, s, t, *b as u64]),
        ExprKind::Literal(Literal::String(_)) => out.extend([E_STRING, s, t]),
        ExprKind::Name(n) => {
            out.extend([E_NAME, s, t]);
            name_words(e, n, out);
        }
        ExprKind::Tuple(es) => {
            out.extend([E_TUPLE, s, t, es.len() as u64]);
            for x in es {
                encode_expr(w, e, x, out);
            }
        }
        ExprKind::Struct { name, fields } => {
            out.extend([E_STRUCT, s, t]);
            name_words(e, name, out);
            out.push(fields.len() as u64);
            for (f, x) in fields {
                out.push(e.name(f));
                encode_expr(w, e, x, out);
            }
        }
        ExprKind::Block { statements, tail } => {
            out.extend([E_BLOCK, s, t, statements.len() as u64]);
            for st in statements {
                match st {
                    Statement::Let {
                        pat,
                        ty,
                        value,
                        mutable,
                    } => {
                        out.push(0);
                        encode_pattern(e, pat, out);
                        out.push(e.ty(ty));
                        out.push(*mutable as u64);
                        encode_expr(w, e, value, out);
                    }
                    Statement::Expr(x) => {
                        out.push(1);
                        encode_expr(w, e, x, out);
                    }
                }
            }
            out.push(tail.is_some() as u64);
            if let Some(x) = tail {
                encode_expr(w, e, x, out);
            }
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            out.extend([E_CALL, s, t]);
            let special = matches!(&callee.kind, ExprKind::Name(n) if special_call(w, n));
            out.push(special as u64);
            encode_expr(w, e, callee, out);
            out.push(types.len() as u64);
            for ty in types {
                out.push(e.ty(ty));
            }
            out.push(args.len() as u64);
            for a in args {
                out.push(pass_code(a.pass));
                out.push(a.explicit as u64);
                encode_expr(w, e, &a.expr, out);
            }
        }
        ExprKind::Field(base, f) => {
            out.extend([E_FIELD, s, t]);
            out.push(e.name(f));
            encode_expr(w, e, base, out);
        }
        ExprKind::Unary(op, v) if unary_code(op) != 0 => {
            out.extend([E_UNARY, s, t, unary_code(op)]);
            encode_expr(w, e, v, out);
        }
        ExprKind::Binary(op, l, r) if binary_code(op) != 0 => {
            out.extend([E_BINARY, s, t, binary_code(op)]);
            encode_expr(w, e, l, out);
            encode_expr(w, e, r, out);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            out.extend([E_IF, s, t]);
            encode_expr(w, e, condition, out);
            encode_expr(w, e, then, out);
            out.push(otherwise.is_some() as u64);
            if let Some(x) = otherwise {
                encode_expr(w, e, x, out);
            }
        }
        // Element writes (`a[i] = v`, lowered to `"[]="`) belong to the
        // arrays lane.
        ExprKind::Assign(op, place, value) if !op.starts_with(vesper::arrays::WRITE_PREFIX) => {
            out.extend([E_ASSIGN, s, t, (op != "=") as u64]);
            encode_expr(w, e, place, out);
            encode_expr(w, e, value, out);
        }
        ExprKind::For { pat, iter, body } => {
            out.extend([E_FOR, s, t]);
            encode_pattern(e, pat, out);
            encode_expr(w, e, iter, out);
            encode_expr(w, e, body, out);
        }
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => {
            out.extend([E_RANGE, s, t, *inclusive as u64]);
            encode_expr(w, e, start, out);
            encode_expr(w, e, end, out);
        }
        ExprKind::Loop(body) => {
            out.extend([E_LOOP, s, t]);
            encode_expr(w, e, body, out);
        }
        ExprKind::Break(value) => {
            out.extend([E_BREAK, s, t, value.is_some() as u64]);
            if let Some(x) = value {
                encode_expr(w, e, x, out);
            }
        }
        ExprKind::Continue => out.extend([E_CONTINUE, s, t]),
        ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } => {
            out.extend([E_HANDLE, s, t]);
            out.push(e.name(&effect.name));
            out.push(effect.args.len() as u64);
            for a in &effect.args {
                out.push(e.ty(a));
            }
            // The handler's inferred effects (its continuation's row), as
            // the row of an empty function type + 1; 0 before inference.
            out.push(
                w.handler_effects
                    .get(&(x.span.start, x.span.end))
                    .map_or(0, |effects| {
                        e.ty(&vesper::ast::Type::Function {
                            params: vec![],
                            result: Box::new(vesper::ast::Type::unit()),
                            effects: effects.clone(),
                            mode: vesper::ast::Mode::Copy,
                        }) + 1
                    }),
            );
            out.push(state.is_some() as u64);
            if let Some((p, value)) = state {
                encode_pattern(e, p, out);
                encode_expr(w, e, value, out);
            }
            encode_expr(w, e, body, out);
            out.push(1 + ops.len() as u64);
            for c in std::iter::once(ret.as_ref()).chain(ops) {
                out.push(e.name(&c.name));
                out.push(c.pats.len() as u64);
                for p in &c.pats {
                    encode_pattern(e, p, out);
                }
                encode_expr(w, e, &c.body, out);
            }
        }
        ExprKind::Try(value) => {
            out.extend([E_TRY, s, t]);
            encode_expr(w, e, value, out);
        }
        ExprKind::Return(value) => {
            out.extend([E_RETURN, s, t, value.is_some() as u64]);
            if let Some(x) = value {
                encode_expr(w, e, x, out);
            }
        }
        ExprKind::Match { value, arms } => {
            out.extend([E_MATCH, s, t]);
            encode_expr(w, e, value, out);
            out.push(arms.len() as u64);
            for (p, x) in arms {
                encode_pattern(e, p, out);
                encode_expr(w, e, x, out);
            }
        }
        ExprKind::Index(base, index) => {
            out.extend([E_INDEX, s, t]);
            encode_expr(w, e, base, out);
            match &index.kind {
                ExprKind::Binary(op, lo, hi) if op == ".." => {
                    out.push(1);
                    encode_expr(w, e, lo, out);
                    encode_expr(w, e, hi, out);
                }
                _ => {
                    out.push(0);
                    encode_expr(w, e, index, out);
                }
            }
        }
        _ => out.extend([E_OTHER, s, t]),
    }
}

/// The functions the differential checks: those with a body whose span no
/// other function shares (generated preludes reuse spans).
pub fn checked_functions(w: &World) -> Vec<&Function> {
    let mut spans: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for f in w.functions.values() {
        *spans.entry((f.span.start, f.span.end)).or_default() += 1;
    }
    w.functions
        .values()
        .filter(|f| {
            f.body.is_some() && f.span.start < f.span.end && spans[&(f.span.start, f.span.end)] == 1
        })
        .collect()
}

/// The function table (every world function, in name order: name,
/// generics, parameters, result, effects, the inferred result) and the
/// method names the module's traits declare, and the checked functions'
/// bodies.
pub fn exprs_input(w: &World, module: &Module) -> Vec<u8> {
    encode(w, |e, tail| {
        tail.push(w.functions.len() as u64);
        for f in w.functions.values() {
            tail.push(e.name(&f.name));
            tail.push(f.generics.len() as u64);
            for g in &f.generics {
                tail.push(e.name(&g.name));
                tail.push(mode_word(g.bound));
                tail.push(g.row as u64);
            }
            tail.push(f.params.len() as u64);
            for p in &f.params {
                tail.push(pass_code(p.pass));
                tail.push(e.name(&p.name));
                tail.push(e.ty(&p.ty));
                tail.push(p.projections.len() as u64);
                tail.extend([p.span.start as u64, p.span.end as u64]);
            }
            tail.push(e.ty(&f.result));
            let effectful = !f.effects.instances.is_empty() || f.effects.tail.is_some();
            tail.push(effectful as u64);
            tail.push(f.effects.instances.len() as u64);
            for inst in &f.effects.instances {
                tail.push(e.name(&inst.name));
                tail.push(e.opt_name(&inst.label));
                tail.push(inst.args.len() as u64);
                for a in &inst.args {
                    tail.push(e.ty(a));
                }
            }
            tail.push(e.opt_name(&f.effects.tail));
            let result = w.function_results.get(&f.name).unwrap_or(&f.result);
            tail.push(e.ty(result));
        }
        // Effect declarations: generics, then each operation's signature
        // under its `Eff::op` name.
        tail.push(w.effects.len() as u64);
        for (name, decl) in &w.effects {
            tail.push(e.name(name));
            tail.push(decl.generics.len() as u64);
            for g in &decl.generics {
                tail.push(e.name(&g.name));
                tail.push(mode_word(g.bound));
                tail.push(g.row as u64);
            }
            tail.push(decl.ops.len() as u64);
            for op in &decl.ops {
                tail.push(e.name(&format!("{name}::{}", op.name)));
                tail.push(op.generics.len() as u64);
                for g in &op.generics {
                    tail.push(e.name(&g.name));
                    tail.push(mode_word(g.bound));
                    tail.push(g.row as u64);
                }
                tail.push(op.params.len() as u64);
                for p in &op.params {
                    tail.push(pass_code(p.pass));
                    tail.push(e.name(&p.name));
                    tail.push(e.ty(&p.ty));
                    tail.push(p.projections.len() as u64);
                    tail.extend([p.span.start as u64, p.span.end as u64]);
                }
                tail.push(e.ty(&op.result));
            }
        }
        let methods: BTreeSet<&str> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Trait(t) => Some(t.methods.iter().map(|m| m.name.as_str())),
                _ => None,
            })
            .flatten()
            .collect();
        tail.push(methods.len() as u64);
        for m in methods {
            tail.push(e.name(m));
        }
        let fs = checked_functions(w);
        tail.push(fs.len() as u64);
        for f in fs {
            tail.push(e.name(&f.name));
            tail.extend([f.span.start as u64, f.span.end as u64]);
            encode_expr(w, e, f.body.as_ref().unwrap(), tail);
        }
    })
}

/// Rust's side for the functions Vesper checked: every recorded expression
/// type inside the function's span, sorted by span.
pub fn rust_types(w: &World, checked: &[(usize, usize)]) -> String {
    let mut out = String::new();
    for &(a, b) in checked {
        out.push_str(&format!("F {a} {b}\n"));
        for ((s, t), ty) in w.types.range((a, 0)..) {
            if *s > b {
                break;
            }
            if *t <= b {
                out.push_str(&format!("T {s} {t} {ty:?}\n"));
            }
        }
    }
    out
}

/// Vesper's text: `F start end` then `T start end type` rows in insertion
/// order (a later row for a span replaces an earlier one, as
/// `types.insert` does), `S start end` for a skipped function and `D code
/// start end` per diagnostic. Returns the sorted text and the checked
/// functions' spans.
pub fn vesper_types(text: &str) -> (String, Vec<(usize, usize)>, usize) {
    let mut out = String::new();
    let mut checked = vec![];
    let mut skipped = 0;
    let mut rows: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut current: Option<(usize, usize)> = None;
    let flush = |out: &mut String, cur: Option<(usize, usize)>, rows: &mut BTreeMap<_, _>| {
        if let Some((a, b)) = cur {
            out.push_str(&format!("F {a} {b}\n"));
            // Only rows inside the function's span, as `rust_types` reads
            // them: sugar-lowered code keeps its prelude spans.
            for ((s, t), ty) in std::mem::take(rows) {
                if (a..=b).contains(&s) && t <= b {
                    out.push_str(&format!("T {s} {t} {ty}\n"));
                }
            }
        }
    };
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(4, ' ').collect();
        let num = |i: usize| parts[i].parse::<usize>().unwrap();
        match parts[0] {
            "F" => {
                flush(&mut out, current.take(), &mut rows);
                current = Some((num(1), num(2)));
                checked.push((num(1), num(2)));
            }
            "T" => {
                rows.insert((num(1), num(2)), parts[3].to_string());
            }
            "S" => skipped += 1,
            "D" => out.push_str(&format!("{line}\n")),
            _ => panic!("unexpected line {line}"),
        }
    }
    flush(&mut out, current.take(), &mut rows);
    (out, checked, skipped)
}
