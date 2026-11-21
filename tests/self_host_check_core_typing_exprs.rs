//! Self-hosting SH2 lane 7 (#1179), part 2: expression typing in Vesper
//! (`packages/vesper-check-core-typing/src/exprs.ves`) against the Rust
//! checker's expression types (`World::types`), over every checked corpus
//! world (lane 4's harness).
//!
//! The Vesper checker covers a growing subset of `Checker::expr_expected`.
//! It reports a function it cannot check yet as skipped, and the test
//! prints how many functions it checked. Every checked function must give
//! exactly the types the Rust checker recorded for its expressions.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper::ast::{Expr, ExprKind, Function, Literal, Pass, Pattern, PatternKind, Statement};
use vesper::check::World;

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-core-typing"))
}

// Expression kinds (`E_*` in exprs.ves).
const E_OTHER: u64 = 0;
const E_INT: u64 = 1;
const E_FLOAT: u64 = 2;
const E_BOOL: u64 = 3;
const E_STRING: u64 = 4;
const E_NAME: u64 = 5;
const E_TUPLE: u64 = 6;
const E_STRUCT: u64 = 7;
const E_BLOCK: u64 = 8;
const E_CALL: u64 = 9;
const E_FIELD: u64 = 10;
const E_UNARY: u64 = 11;
const E_BINARY: u64 = 12;
const E_IF: u64 = 13;
const E_MATCH: u64 = 14;

fn unary_code(op: &str) -> u64 {
    match op {
        "!" => 1,
        "-" => 2,
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
            // The literal's type (`literal_type`), for `bind_mode`.
            out.push(match l {
                Literal::Int(_) => 0,
                Literal::Float(_) => 1,
                Literal::Bool(_) => 2,
                Literal::String(_) => 3,
            });
            if let Literal::Int(n) = l {
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

/// An expression in pre-order: kind, start, end, then the payload. Kinds
/// the Vesper checker does not cover yet are `E_OTHER` with no payload.
fn encode_expr(w: &World, e: &mut Enc, x: &Expr, out: &mut Vec<u64>) {
    let (s, t) = (x.span.start as u64, x.span.end as u64);
    match &x.kind {
        ExprKind::Literal(Literal::Int(n)) if fits_literal(*n) => {
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
        ExprKind::Match { value, arms } => {
            out.extend([E_MATCH, s, t]);
            encode_expr(w, e, value, out);
            out.push(arms.len() as u64);
            for (p, x) in arms {
                encode_pattern(e, p, out);
                encode_expr(w, e, x, out);
            }
        }
        _ => out.extend([E_OTHER, s, t]),
    }
}

/// The functions the differential checks: those with a body whose span no
/// other function shares (generated preludes reuse spans).
fn checked_functions(w: &World) -> Vec<&Function> {
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
/// checked functions' bodies.
fn exprs_input(w: &World) -> Vec<u8> {
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
            let result = w.function_results.get(&f.name).unwrap_or(&f.result);
            tail.push(e.ty(result));
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
fn rust_types(w: &World, checked: &[(usize, usize)]) -> String {
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
fn vesper_types(text: &str) -> (String, Vec<(usize, usize)>, usize) {
    let mut out = String::new();
    let mut checked = vec![];
    let mut skipped = 0;
    let mut rows: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut current: Option<(usize, usize)> = None;
    let flush = |out: &mut String, cur: Option<(usize, usize)>, rows: &mut BTreeMap<_, _>| {
        if let Some((a, b)) = cur {
            out.push_str(&format!("F {a} {b}\n"));
            for ((s, t), ty) in std::mem::take(rows) {
                out.push_str(&format!("T {s} {t} {ty}\n"));
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

#[test]
fn expression_types_match_rust() {
    let f = package().entry("core_typing_exprs", true);
    let (mut total, mut skipped, mut rows) = (0, 0, 0);
    let mut bad = 0;
    let mut shown = 0;
    for c in corpus() {
        let text = f.text(&exprs_input(&c.world));
        let (v, checked, s) = vesper_types(&text);
        total += checked.len();
        skipped += s;
        let r = rust_types(&c.world, &checked);
        rows += r.lines().filter(|l| l.starts_with('T')).count();
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if shown < 8 {
                shown += 1;
                eprintln!("exprs: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!(
        "exprs: {bad} of {} worlds differ; {total} functions checked, {skipped} skipped, \
         {rows} expression types",
        corpus().len()
    );
    assert_eq!(bad, 0);
    // Coverage floor: a checker that skipped everything would agree too.
    // Relative, because the corpus is smaller under Bazel (its data globs)
    // than under Cargo: a quarter of the functions, with types in each.
    assert!(
        total * 4 > total + skipped && rows > total,
        "{total} functions checked, {skipped} skipped, {rows} types"
    );
}
