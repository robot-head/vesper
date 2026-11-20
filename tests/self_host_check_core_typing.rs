//! Self-hosting SH2 lane 7 (#1179): the core typing of `src/check.rs` in
//! Vesper (`packages/vesper-check-core-typing`) against the Rust code it
//! ports, over every checked corpus world (lane 4's harness).
//!
//! Part 1 covers the type relations (`compatible`, `join_types`,
//! `refine_annotation`, `unresolved`, `infer`) and exhaustiveness
//! (`is_uninhabited`, `matrix_covered`, match coverage and `irrefutable`).
//! The corpus only has exhaustive matches, so each match also runs with one
//! arm dropped at a time and with a constructor pattern missing a field:
//! those mutants are what make E0212 and the refutable-pattern checks fire.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper::ast::{Expr, ExprKind, Literal, Pattern, PatternKind, Statement, Type};
use vesper::check::World;
use vesper::check::core_typing_probe as probe;

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-core-typing"))
}

/// Sorts the lines of each `-`-terminated block: `infer` bindings are a
/// `BTreeMap` in Rust and in insertion order in Vesper.
fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut block: Vec<&str> = vec![];
    for line in text.lines() {
        if line == "-" {
            block.sort_unstable();
            for l in block.drain(..) {
                out.push_str(l);
                out.push('\n');
            }
            out.push_str("-\n");
        } else if line.starts_with("V ") {
            block.push(line);
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn bit(b: bool) -> char {
    if b { '1' } else { '0' }
}

// ------------------------------------------------------------- relations

/// Integer literal values around every kind's bounds.
fn literal_values() -> Vec<i128> {
    let mut v = vec![
        0, 1, -1, 3, 127, 128, 255, 256, -128, -129, 32767, 65535, -32769,
    ];
    v.extend([
        (1i128 << 31) - 1,
        1i128 << 31,
        (1i128 << 32) - 1,
        1i128 << 32,
        -(1i128 << 31) - 1,
        i64::MAX as i128,
        i64::MAX as i128 + 1,
        u64::MAX as i128,
        i64::MIN as i128,
        i64::MIN as i128 - 1,
        -(u64::MAX as i128),
    ]);
    v
}

const INTEGER_NAMES: [&str; 10] = [
    "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
];

/// The type pairs of a world: each type with itself and its next four in
/// `Debug` order (so most pairs share a constructor and recurse), with its
/// mirror in that order, and every literal against the literals, the
/// integer kinds and a sample of the world's types.
fn relation_pairs(w: &World) -> Vec<(Type, Type)> {
    let mut ts = world_types(w);
    ts.sort_by_cached_key(|t| format!("{t:?}"));
    let n = ts.len();
    let mut pairs = vec![];
    for i in 0..n {
        for j in i..(i + 5).min(n) {
            pairs.push((ts[i].clone(), ts[j].clone()));
        }
        pairs.push((ts[i].clone(), ts[n - 1 - i].clone()));
    }
    let lits: Vec<Type> = literal_values().into_iter().map(Type::IntLiteral).collect();
    let mut others: Vec<Type> = INTEGER_NAMES.iter().map(|s| Type::named(s)).collect();
    others.extend([
        Type::named("f64"),
        Type::named("bool"),
        Type::Infer,
        Type::named("Never"),
    ]);
    others.extend(ts.iter().step_by(17).take(8).cloned());
    for a in &lits {
        for b in lits.iter().chain(&others) {
            pairs.push((a.clone(), b.clone()));
        }
    }
    pairs
}

/// One `infer` case: the generics and the (expected, actual) pairs.
type InferCase = (Vec<vesper::ast::Generic>, Vec<(Type, Type)>);

/// `infer` cases of a world: for each generic function, its parameters (and
/// result) against (a) themselves with the generics bound to world types,
/// (b) unrelated world types, (c) integer literals on bare generic
/// parameters, which merge into one representative literal.
fn infer_cases(w: &World) -> Vec<InferCase> {
    let mut ts = world_types(w);
    ts.sort_by_cached_key(|t| format!("{t:?}"));
    let mut cases = vec![];
    if ts.is_empty() {
        return cases;
    }
    for (h, f) in w.functions.values().enumerate() {
        if f.generics.is_empty() {
            continue;
        }
        let expected: Vec<&Type> = f.params.iter().map(|p| &p.ty).chain([&f.result]).collect();
        let vars: BTreeMap<String, Type> = f
            .generics
            .iter()
            .enumerate()
            .filter(|(_, g)| !g.row)
            .map(|(i, g)| (g.name.clone(), ts[(h * 31 + i * 7) % ts.len()].clone()))
            .collect();
        cases.push((
            f.generics.clone(),
            expected
                .iter()
                .map(|t| ((*t).clone(), vesper::ast::substitute(t, &vars)))
                .collect(),
        ));
        cases.push((
            f.generics.clone(),
            expected
                .iter()
                .enumerate()
                .map(|(i, t)| ((*t).clone(), ts[(h * 13 + i * 5 + 1) % ts.len()].clone()))
                .collect(),
        ));
        let mut lits = vec![];
        for t in &expected {
            if let Type::Named(n, a) = t
                && a.is_empty()
                && f.generics.iter().any(|g| &g.name == n)
            {
                for v in [3, -1, i64::MAX as i128 + 5, -7] {
                    lits.push(((*t).clone(), Type::IntLiteral(v)));
                }
                lits.push(((*t).clone(), Type::named("i32")));
            }
        }
        if !lits.is_empty() {
            cases.push((f.generics.clone(), lits));
        }
    }
    cases
}

fn rust_relations(w: &World) -> String {
    normalize(&relations_text(&relation_pairs(w), &infer_cases(w)))
}

fn relations_text(pairs: &[(Type, Type)], cases: &[InferCase]) -> String {
    let mut out = String::new();
    for (a, b) in pairs {
        let (j, errors) = probe::join_types(a, b);
        out.push_str(&format!(
            "P {} {} {}\nJ {j:?} {errors}\nR {:?}\n",
            bit(vesper::check::compatible(a, b)),
            bit(vesper::check::compatible(b, a)),
            bit(probe::unresolved(a)),
            probe::refine_annotation(a, b),
        ));
    }
    for (generics, pairs) in cases {
        let mut vars = BTreeMap::new();
        for (e, a) in pairs {
            probe::infer(e, a, generics, &mut vars);
        }
        for (k, v) in &vars {
            out.push_str(&format!("V {k} {v:?}\n"));
        }
        out.push_str("-\n");
    }
    out
}

fn relations_input(w: &World) -> Vec<u8> {
    encode(w, |e, tail| {
        let pairs = relation_pairs(w);
        tail.push(pairs.len() as u64);
        for (a, b) in &pairs {
            tail.push(e.ty(a));
            tail.push(e.ty(b));
        }
        let cases = infer_cases(w);
        tail.push(cases.len() as u64);
        for (generics, pairs) in &cases {
            tail.push(generics.len() as u64);
            for g in generics {
                tail.push(e.name(&g.name));
            }
            tail.push(pairs.len() as u64);
            for (x, y) in pairs {
                tail.push(e.ty(x));
                tail.push(e.ty(y));
            }
        }
    })
}

#[test]
fn relations_match_rust() {
    let f = package().entry("core_typing_relations", true);
    let bad = differential(
        "relations",
        |c| rust_relations(&c.world),
        |c| normalize(&f.text(&relations_input(&c.world))),
    );
    assert_eq!(bad, 0);
}

// ------------------------------------------------------------- patterns

/// The patterns of a world's functions with their types: every `match`
/// (its arms; then the arms with one dropped, for the first six; then with
/// a constructor pattern missing its first field) and every destructuring
/// `let`, typed by the checker's type of the scrutinee or value.
fn pattern_cases(w: &World) -> Vec<(Type, Vec<Pattern>)> {
    let mut cases = vec![];
    let ty = |e: &Expr| w.types.get(&(e.span.start, e.span.end)).cloned();
    for f in w.functions.values() {
        let Some(body) = &f.body else { continue };
        vesper::regions::visit_expr(body, &mut |e| match &e.kind {
            ExprKind::Match { value, arms } => {
                let Some(t) = ty(value) else { return };
                let pats: Vec<Pattern> = arms.iter().map(|(p, _)| p.clone()).collect();
                cases.push((t.clone(), pats.clone()));
                for i in 0..pats.len().min(6) {
                    let mut fewer = pats.clone();
                    fewer.remove(i);
                    cases.push((t.clone(), fewer));
                }
                if let Some(i) = pats.iter().position(
                    |p| matches!(&p.kind, PatternKind::Constructor { fields, .. } if !fields.is_empty()),
                ) {
                    let mut missing = pats.clone();
                    if let PatternKind::Constructor { fields, .. } = &mut missing[i].kind {
                        fields.remove(0);
                    }
                    cases.push((t, missing));
                }
            }
            ExprKind::Block { statements, .. } => {
                for s in statements {
                    if let Statement::Let { pat, value, .. } = s
                        && !matches!(pat.kind, PatternKind::Bind(_) | PatternKind::Wild)
                        && let Some(t) = ty(value)
                    {
                        cases.push((t, vec![pat.clone()]));
                    }
                }
            }
            _ => {}
        });
    }
    cases
}

fn rust_patterns(w: &World) -> String {
    patterns_text(w, &world_types(w), &pattern_cases(w))
}

fn patterns_text(w: &World, ts: &[Type], cases: &[(Type, Vec<Pattern>)]) -> String {
    let mut out = String::new();
    for t in ts {
        out.push_str(&format!("U {}\n", bit(probe::is_uninhabited(w, t))));
    }
    for (t, pats) in cases {
        let rows: Vec<Vec<Pattern>> = pats.iter().map(|p| vec![p.clone()]).collect();
        let types = std::slice::from_ref(t);
        out.push_str(&format!(
            "C {}\n",
            bit(probe::matrix_covered(w, &rows, types))
        ));
        for p in pats {
            let row = [vec![p.clone()]];
            out.push_str(&format!(
                "I {}\n",
                bit(probe::matrix_covered(w, &row, types))
            ));
        }
    }
    out
}

/// A pattern tree in the task's pre-order encoding (`read_pat` in lib.ves).
fn encode_pattern(e: &mut Enc, p: &Pattern, out: &mut Vec<u64>) {
    match &p.kind {
        PatternKind::Wild => out.push(0),
        PatternKind::Bind(_) => out.push(1),
        PatternKind::Literal(Literal::Bool(true)) => out.push(2),
        PatternKind::Literal(Literal::Bool(false)) => out.push(3),
        PatternKind::Literal(_) => out.push(4),
        PatternKind::Tuple(ps) => {
            out.extend([5, ps.len() as u64]);
            for p in ps {
                encode_pattern(e, p, out);
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            let short = name.rsplit("::").next().unwrap_or(name);
            out.extend([6, e.name(short), fields.len() as u64]);
            for (f, p) in fields {
                out.push(e.name(f));
                encode_pattern(e, p, out);
            }
        }
    }
}

fn patterns_input(w: &World) -> Vec<u8> {
    encode(w, |e, tail| {
        let ts = world_types(w);
        tail.push(ts.len() as u64);
        for t in &ts {
            tail.push(e.ty(t));
        }
        let cases = pattern_cases(w);
        tail.push(cases.len() as u64);
        for (t, pats) in &cases {
            tail.push(e.ty(t));
            tail.push(pats.len() as u64);
            for p in pats {
                encode_pattern(e, p, tail);
            }
        }
    })
}

#[test]
fn patterns_match_rust() {
    let f = package().entry("core_typing_patterns", true);
    let bad = differential(
        "patterns",
        |c| rust_patterns(&c.world),
        |c| f.text(&patterns_input(&c.world)),
    );
    assert_eq!(bad, 0);
}

/// The mutants make the diagnostics fire: some matches stop covering their
/// type (E0212) and some patterns are refutable.
#[test]
fn mutants_make_coverage_fail() {
    let (mut uncovered, mut refutable, mut cases) = (0, 0, 0);
    for c in corpus() {
        let text = rust_patterns(&c.world);
        cases += text.lines().filter(|l| l.starts_with("C ")).count();
        uncovered += text.lines().filter(|l| *l == "C 0").count();
        refutable += text.lines().filter(|l| *l == "I 0").count();
    }
    eprintln!("patterns: {cases} cases, {uncovered} not covered, {refutable} refutable arms");
    assert!(uncovered > 0 && refutable > 0);
}

/// The JIT with the optimizer off gives the same texts on a sample.
#[test]
fn unoptimized_agrees() {
    let rel = package().entry("core_typing_relations", false);
    let pat = package().entry("core_typing_patterns", false);
    for c in corpus().iter().step_by(23) {
        assert_eq!(
            normalize(&rel.text(&relations_input(&c.world))),
            rust_relations(&c.world),
            "{}",
            c.name
        );
        assert_eq!(
            pat.text(&patterns_input(&c.world)),
            rust_patterns(&c.world),
            "{}",
            c.name
        );
    }
}

/// ABBA wall time of both tasks over the corpus. Both sides get their cases
/// built up front (Rust: the type and pattern lists; Vesper: the encoded
/// input, whose decoding it pays) and both produce the same text.
/// `CHECK_HARNESS_SPEED=1`.
#[test]
#[ignore]
fn speed_relations_and_patterns() {
    if !speed() {
        return;
    }
    let rel = package().entry("core_typing_relations", true);
    let pat = package().entry("core_typing_patterns", true);
    let rel_in: Vec<Vec<u8>> = corpus().iter().map(|c| relations_input(&c.world)).collect();
    let pat_in: Vec<Vec<u8>> = corpus().iter().map(|c| patterns_input(&c.world)).collect();
    let rel_cases: Vec<_> = corpus()
        .iter()
        .map(|c| (relation_pairs(&c.world), infer_cases(&c.world)))
        .collect();
    let pat_cases: Vec<_> = corpus()
        .iter()
        .map(|c| (world_types(&c.world), pattern_cases(&c.world)))
        .collect();
    abba(
        "relations",
        &mut || {
            time(|| {
                for (pairs, cases) in &rel_cases {
                    std::hint::black_box(relations_text(pairs, cases));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &rel_in {
                    std::hint::black_box(rel.words(i));
                }
            })
        },
    );
    abba(
        "patterns",
        &mut || {
            time(|| {
                for (c, (ts, cases)) in corpus().iter().zip(&pat_cases) {
                    std::hint::black_box(patterns_text(&c.world, ts, cases));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &pat_in {
                    std::hint::black_box(pat.words(i));
                }
            })
        },
    );
}
