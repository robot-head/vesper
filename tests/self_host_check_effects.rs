//! Self-hosting SH2 lane 8 (#1180): `@vesper/check-effects`, the capability
//! effect rules of `src/capability_effects.rs` in Vesper, against the Rust
//! `check_world` over the corpus, the capability checker fixtures, and
//! mutants that turn the capabilities on for every function so that each
//! diagnostic of the module fires.
//!
//! Both sides write one text line per diagnostic (code, span, message, help
//! and related labels) and the texts must be equal. The task words are the
//! world's functions with their bodies, its effect declarations and its
//! call instantiations (`encode_task`; the decoder is `read_prog` in
//! `packages/vesper-check-effects/src/prog.ves`).
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::OnceLock;
use vesper::ast::{
    Arg, Clause, Diagnostic, Effect, Effects, Expr, ExprKind, Literal, Param, Pattern, PatternKind,
    Statement, Type,
};
use vesper::check::World;

// ------------------------------------------------------------------ encoder

const NONE: u64 = u32::MAX as u64;

/// The AST tables of one world: expressions (8 words each), patterns (6
/// words each) and the list pool, laid out as `prog.ves` reads them.
#[derive(Default)]
struct Ast {
    exprs: Vec<u64>,
    expr_count: u64,
    pats: Vec<u64>,
    pat_count: u64,
    pool: Vec<u64>,
}

impl Ast {
    fn node(&mut self, kind: u64, e: &Expr, f: [u64; 5]) -> u64 {
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64]);
        self.exprs.extend(f);
        self.expr_count += 1;
        self.expr_count - 1
    }

    fn list(&mut self, words: Vec<u64>) -> u64 {
        let first = self.pool.len() as u64;
        self.pool.extend(words);
        first
    }

    fn opt(&mut self, enc: &mut Enc, e: &Option<Box<Expr>>) -> u64 {
        e.as_ref().map_or(NONE, |e| self.expr(enc, e))
    }

    fn pat(&mut self, enc: &mut Enc, p: &Pattern) -> u64 {
        let (kind, a, b, c) = match &p.kind {
            PatternKind::Wild => (1, 0, 0, 0),
            PatternKind::Bind(n) => (2, enc.name(n), 0, 0),
            PatternKind::Literal(_) => (3, 0, 0, 0),
            PatternKind::Tuple(ps) => {
                let ids: Vec<u64> = ps.iter().map(|p| self.pat(enc, p)).collect();
                let n = ids.len() as u64;
                (4, self.list(ids), n, 0)
            }
            PatternKind::Constructor { name, fields, .. } => {
                let mut words = vec![];
                for (f, sub) in fields {
                    let s = self.pat(enc, sub);
                    words.extend([enc.name(f), s]);
                }
                let n = fields.len() as u64;
                let name = enc.name(name);
                (5, name, self.list(words), n)
            }
        };
        self.pats
            .extend([kind, p.span.start as u64, p.span.end as u64, a, b, c]);
        self.pat_count += 1;
        self.pat_count - 1
    }

    fn row(enc: &mut Enc, e: &Effects) -> u64 {
        enc.ty(&Type::Row(e.clone()))
    }

    fn params(&mut self, enc: &mut Enc, ps: &[Param]) -> (u64, u64) {
        let mut words = vec![];
        for p in ps {
            words.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
        }
        (self.list(words), ps.len() as u64)
    }

    fn clause(&mut self, enc: &mut Enc, c: &Clause) -> [u64; 4] {
        let ids: Vec<u64> = c.pats.iter().map(|p| self.pat(enc, p)).collect();
        let n = ids.len() as u64;
        let first = self.list(ids);
        let body = self.expr(enc, &c.body);
        [enc.name(&c.name), first, n, body]
    }

    fn expr(&mut self, enc: &mut Enc, e: &Expr) -> u64 {
        match &e.kind {
            ExprKind::Literal(l) => {
                let k = match l {
                    Literal::Int(_) | Literal::WideInt(_) => 1,
                    Literal::Float(_) => 2,
                    Literal::Bool(_) => 3,
                    Literal::String(_) => 4,
                };
                self.node(1, e, [k, 0, 0, 0, 0])
            }
            ExprKind::Name(n) => {
                let n = enc.name(n);
                self.node(2, e, [n, 0, 0, 0, 0])
            }
            ExprKind::Tuple(es) => {
                let ids: Vec<u64> = es.iter().map(|x| self.expr(enc, x)).collect();
                let n = ids.len() as u64;
                let first = self.list(ids);
                self.node(3, e, [first, n, 0, 0, 0])
            }
            ExprKind::Struct { name, fields } => {
                let mut words = vec![];
                for (f, v) in fields {
                    let v = self.expr(enc, v);
                    words.extend([enc.name(f), v]);
                }
                let n = fields.len() as u64;
                let first = self.list(words);
                let name = enc.name(name);
                self.node(4, e, [name, first, n, 0, 0])
            }
            ExprKind::Block { statements, tail } => {
                let mut words = vec![];
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            let v = self.expr(enc, value);
                            let p = self.pat(enc, pat);
                            words.extend([1, p, enc.ty(ty), v]);
                        }
                        Statement::Expr(x) => {
                            let v = self.expr(enc, x);
                            words.extend([2, NONE, NONE, v]);
                        }
                    }
                }
                let n = statements.len() as u64;
                let first = self.list(words);
                let t = self.opt(enc, tail);
                self.node(5, e, [first, n, t, 0, 0])
            }
            ExprKind::Call { callee, args, .. } => {
                let c = self.expr(enc, callee);
                let words = args_words(self, enc, args);
                let n = args.len() as u64;
                let first = self.list(words);
                self.node(6, e, [c, first, n, 0, 0])
            }
            ExprKind::Field(base, name) => {
                let b = self.expr(enc, base);
                let n = enc.name(name);
                self.node(7, e, [b, n, 0, 0, 0])
            }
            ExprKind::Index(a, b) => {
                let a = self.expr(enc, a);
                let b = self.expr(enc, b);
                self.node(8, e, [a, b, 0, 0, 0])
            }
            ExprKind::Try(v) => {
                let v = self.expr(enc, v);
                self.node(9, e, [v, 0, 0, 0, 0])
            }
            ExprKind::Unary(op, v) => {
                let v = self.expr(enc, v);
                let op = enc.name(op);
                self.node(10, e, [op, v, 0, 0, 0])
            }
            ExprKind::Binary(op, l, r) => {
                let l = self.expr(enc, l);
                let r = self.expr(enc, r);
                let op = enc.name(op);
                self.node(11, e, [op, l, r, 0, 0])
            }
            ExprKind::Assign(op, l, r) => {
                let l = self.expr(enc, l);
                let r = self.expr(enc, r);
                let op = enc.name(op);
                self.node(12, e, [op, l, r, 0, 0])
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(enc, condition);
                let t = self.expr(enc, then);
                let o = self.opt(enc, otherwise);
                self.node(13, e, [c, t, o, 0, 0])
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(enc, value);
                let mut words = vec![];
                for (p, body) in arms {
                    let p = self.pat(enc, p);
                    let b = self.expr(enc, body);
                    words.extend([p, b]);
                }
                let n = arms.len() as u64;
                let first = self.list(words);
                self.node(14, e, [v, first, n, 0, 0])
            }
            ExprKind::Closure {
                params,
                result,
                effects,
                body,
                ..
            } => {
                let (first, n) = self.params(enc, params);
                let r = enc.ty(result);
                let row = Self::row(enc, effects);
                let b = self.expr(enc, body);
                self.node(15, e, [first, n, r, row, b])
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let (sp, sv) = match state {
                    Some((p, v)) => {
                        let v = self.expr(enc, v);
                        (self.pat(enc, p), v)
                    }
                    None => (NONE, NONE),
                };
                let b = self.expr(enc, body);
                let mut words = vec![1 + ops.len() as u64];
                words.extend(self.clause(enc, ret));
                for op in ops {
                    words.extend(self.clause(enc, op));
                }
                let first = self.list(words);
                let name = enc.name(&effect.name);
                self.node(16, e, [name, sp, sv, b, first])
            }
            ExprKind::Loop(body) => {
                let b = self.expr(enc, body);
                self.node(17, e, [b, 0, 0, 0, 0])
            }
            ExprKind::For { pat, iter, body } => {
                let p = self.pat(enc, pat);
                let i = self.expr(enc, iter);
                let b = self.expr(enc, body);
                self.node(18, e, [p, i, b, 0, 0])
            }
            ExprKind::Range { start, end, .. } => {
                let a = self.expr(enc, start);
                let b = self.expr(enc, end);
                self.node(19, e, [a, b, 0, 0, 0])
            }
            ExprKind::Break(v) => {
                let v = self.opt(enc, v);
                self.node(20, e, [v, 0, 0, 0, 0])
            }
            ExprKind::Return(v) => {
                let v = self.opt(enc, v);
                self.node(21, e, [v, 0, 0, 0, 0])
            }
            ExprKind::Continue => self.node(22, e, [0; 5]),
        }
    }
}

fn args_words(ast: &mut Ast, enc: &mut Enc, args: &[Arg]) -> Vec<u64> {
    let mut words = vec![];
    for a in args {
        let x = ast.expr(enc, &a.expr);
        words.extend([pass_word(a.pass), x]);
    }
    words
}

/// The words of one function (a `Fun` row of prog.ves) and its parameters.
fn function_words(
    ast: &mut Ast,
    enc: &mut Enc,
    world: &World,
    key: &str,
    f: &vesper::ast::Function,
    out: &mut Vec<u64>,
) {
    let carried = vesper::float_math::carried_effects(world, &f.name, &f.effects);
    let body = f.body.as_ref().map_or(NONE, |b| ast.expr(enc, b));
    out.extend([
        enc.name(key),
        enc.name(&f.name),
        f.span.start as u64,
        f.span.end as u64,
        enc.ty(&f.result),
        Ast::row(enc, &f.effects),
        Ast::row(enc, &carried),
        body,
        f.params.len() as u64,
    ]);
    for p in &f.params {
        out.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
    }
}

/// The task words: the AST tables, then the functions, the effect
/// declarations and the call instantiations.
fn encode_task(enc: &mut Enc, tail: &mut Vec<u64>, world: &World) {
    let mut ast = Ast::default();
    let mut funs = vec![world.functions.len() as u64];
    for (key, f) in &world.functions {
        function_words(&mut ast, enc, world, key, f, &mut funs);
    }
    let mut effs = vec![world.effects.len() as u64];
    for (key, d) in &world.effects {
        effs.extend([
            enc.name(key),
            d.linear as u64,
            d.span.start as u64,
            d.span.end as u64,
            d.generics.len() as u64,
        ]);
        for g in &d.generics {
            effs.push(enc.name(&g.name));
        }
        effs.push(d.ops.len() as u64);
        for op in &d.ops {
            function_words(&mut ast, enc, world, &op.name, op, &mut effs);
        }
    }
    let mut calls = vec![world.calls.len() as u64];
    for ((a, b), vars) in &world.calls {
        calls.extend([*a as u64, *b as u64, vars.len() as u64]);
        for (n, t) in vars {
            calls.extend([enc.name(n), enc.ty(t)]);
        }
    }
    tail.push(ast.expr_count);
    tail.extend(&ast.exprs);
    tail.push(ast.pat_count);
    tail.extend(&ast.pats);
    tail.push(ast.pool.len() as u64);
    tail.extend(&ast.pool);
    tail.extend(funs);
    tail.extend(effs);
    tail.extend(calls);
}

fn input(world: &World) -> Vec<u8> {
    encode(world, |enc, tail| encode_task(enc, tail, world))
}

// ------------------------------------------------------------------ texts

fn diag_line(out: &mut String, d: &Diagnostic) {
    let _ = writeln!(
        out,
        "{} {}..{} {} | {} | {}",
        d.code,
        d.span.start,
        d.span.end,
        d.message,
        d.help.as_deref().unwrap_or("-"),
        d.related.len()
    );
    for l in &d.related {
        let _ = writeln!(out, "  {}..{} {}", l.span.start, l.span.end, l.message);
    }
}

fn rust_text(world: &World) -> String {
    let mut out = String::new();
    for d in vesper::capability_effects::check_world(world) {
        diag_line(&mut out, &d);
    }
    out
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-effects"))
}

fn entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("check_effects", optimize))
}

fn vesper_text(world: &World, optimize: bool) -> String {
    entry(optimize).text(&input(world))
}

// ------------------------------------------------------------------ cases

/// The capability checker's fixtures and every other fixture that parses,
/// checked whatever their diagnostics (the corpus keeps only clean worlds,
/// and the rejected fixtures are what make the module's diagnostics fire).
fn fixture_worlds() -> &'static [(String, World)] {
    static CASES: OnceLock<Vec<(String, World)>> = OnceLock::new();
    CASES.get_or_init(|| {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|e| e == "ves") {
                    out.push(p);
                }
            }
        }
        let mut files = vec![];
        for d in [
            "capability_effects",
            "capability_verifiers",
            "floats",
            "vector",
        ] {
            walk(&repo().join("tests/fixtures").join(d), &mut files);
        }
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut out = vec![];
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let r = std::panic::catch_unwind(|| {
                let parsed = vesper::parser::parse(&src);
                parsed
                    .diagnostics
                    .is_empty()
                    .then(|| vesper::check::check(&parsed.module).world)
            });
            if let Ok(Some(w)) = r {
                out.push((
                    f.strip_prefix(repo()).unwrap_or(&f).display().to_string(),
                    w,
                ));
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

/// The capability mutants: every function and effect operation of the
/// world carries `caps` in its row (after its own effects).
fn mutant(world: &World, caps: &[&str]) -> World {
    let mut w = world.clone();
    let add = |e: &mut Effects| {
        for c in caps {
            if !e.instances.iter().any(|i| i.name == *c) {
                e.instances.push(Effect {
                    label: None,
                    name: c.to_string(),
                    args: vec![],
                });
            }
        }
    };
    for f in w.functions.values_mut() {
        add(&mut f.effects);
    }
    w
}

pub const MUTANTS: [&[&str]; 5] = [
    &["ConstTime"],
    &["Realtime"],
    &["DetFloat"],
    &["DetFloat", "ConstDenorm"],
    &["ConstTime", "Realtime", "DetFloat", "ConstDenorm"],
];

/// Counts the diagnostic codes of a text.
fn codes(text: &str, out: &mut BTreeMap<String, usize>) {
    for l in text.lines() {
        if let Some(code) = l.split(' ').next()
            && code.starts_with('E')
        {
            *out.entry(code.to_string()).or_default() += 1;
        }
    }
}

fn compare(label: &str, cases: &[(String, &World)], optimize: bool) -> usize {
    let mut bad = 0;
    let mut lines = 0;
    let mut seen = BTreeMap::new();
    for (name, w) in cases {
        let r = rust_text(w);
        let v = vesper_text(w, optimize);
        lines += r.lines().count();
        codes(&r, &mut seen);
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 5 {
                eprintln!("{label}: {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "{label}: {bad} of {} worlds differ ({lines} diagnostics: {seen:?})",
        cases.len()
    );
    bad
}

// ------------------------------------------------------------------ tests

#[test]
fn effects_match_rust_on_the_corpus() {
    let cases: Vec<(String, &World)> = corpus()
        .iter()
        .map(|c| (c.name.clone(), &c.world))
        .collect();
    assert!(cases.len() > 100, "{} corpus worlds", cases.len());
    assert_eq!(compare("corpus", &cases, true), 0);
}

#[test]
fn effects_match_rust_on_the_checker_fixtures() {
    let cases: Vec<(String, &World)> = fixture_worlds()
        .iter()
        .map(|(n, w)| (n.clone(), w))
        .collect();
    assert!(cases.len() > 30, "{} fixture worlds", cases.len());
    let mut seen = BTreeMap::new();
    for (_, w) in &cases {
        codes(&rust_text(w), &mut seen);
    }
    // The rejected fixtures make the module's own diagnostics fire.
    for code in [
        "E0700", "E0702", "E0703", "E0704", "E0705", "E0706", "E0707", "E0708", "E0720", "E0721",
        "E0722", "E0723", "E0730", "E0731",
    ] {
        assert!(seen.contains_key(code), "no fixture fires {code}: {seen:?}");
    }
    assert_eq!(compare("fixtures", &cases, true), 0);
}

#[test]
fn effects_match_rust_on_capability_mutants() {
    let stride = std::env::var("CHECK_EFFECTS_MUTANT_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1usize)
        .max(1);
    let base: Vec<(String, &World)> = corpus()
        .iter()
        .map(|c| (c.name.clone(), &c.world))
        .chain(fixture_worlds().iter().map(|(n, w)| (n.clone(), w)))
        .step_by(stride)
        .collect();
    let mut bad = 0;
    for (m, caps) in MUTANTS.iter().enumerate() {
        let worlds: Vec<(String, World)> = base
            .iter()
            .map(|(n, w)| (format!("{n}#m{}", m + 1), mutant(w, caps)))
            .collect();
        let cases: Vec<(String, &World)> = worlds.iter().map(|(n, w)| (n.clone(), w)).collect();
        bad += compare(&format!("mutant {caps:?}"), &cases, true);
    }
    assert_eq!(bad, 0);
}

#[test]
fn jit_without_the_optimizer_agrees() {
    let cases: Vec<(String, &World)> = fixture_worlds()
        .iter()
        .map(|(n, w)| (n.clone(), w))
        .collect();
    assert_eq!(compare("fixtures (optimizer off)", &cases, false), 0);
}

#[test]
#[ignore = "ABBA timing: CHECK_HARNESS_SPEED=1 cargo test --release -- --ignored"]
fn effects_abba() {
    if !speed() {
        return;
    }
    let worlds: Vec<World> = corpus()
        .iter()
        .map(|c| c.world.clone())
        .chain(fixture_worlds().iter().map(|(_, w)| w.clone()))
        .collect();
    let all = mutant_set(&worlds);
    let inputs: Vec<Vec<u8>> = all.iter().map(input).collect();
    let f = entry(true);
    abba(
        "check_world (corpus, fixtures, mutant 5)",
        &mut || {
            time(|| {
                for w in &all {
                    std::hint::black_box(vesper::capability_effects::check_world(w));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.words(i));
                }
            })
        },
    );
    let t = time(|| {
        for w in &all {
            std::hint::black_box(input(w));
        }
    });
    eprintln!("encoding the inputs: {t:?}");
}

fn mutant_set(worlds: &[World]) -> Vec<World> {
    worlds
        .iter()
        .cloned()
        .chain(worlds.iter().map(|w| mutant(w, MUTANTS[4])))
        .collect()
}

/// One side of the differential for callgrind: `CHECK_EFFECTS_SIDE=rust`,
/// `vesper` or `none` (the baseline: corpus, inputs and the compiled entry
/// only). Instructions of a side are its run minus the baseline's.
#[test]
#[ignore = "callgrind: CHECK_EFFECTS_SIDE=rust|vesper|none CHECK_HARNESS_STRIDE=4"]
fn effects_callgrind_side() {
    let Ok(side) = std::env::var("CHECK_EFFECTS_SIDE") else {
        return;
    };
    let worlds: Vec<World> = corpus()
        .iter()
        .map(|c| c.world.clone())
        .chain(fixture_worlds().iter().map(|(_, w)| w.clone()))
        .collect();
    let all = mutant_set(&worlds);
    let inputs: Vec<Vec<u8>> = all.iter().map(input).collect();
    let f = entry(true);
    match side.as_str() {
        "rust" => {
            for w in &all {
                std::hint::black_box(vesper::capability_effects::check_world(w));
            }
        }
        "vesper" => {
            for i in &inputs {
                std::hint::black_box(f.words(i));
            }
        }
        _ => {}
    }
    eprintln!("callgrind side {side}: {} worlds", all.len());
}
