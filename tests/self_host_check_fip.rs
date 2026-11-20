//! Self-hosting SH2 lane 12 (#1184): the fip checks of `src/fip.rs`
//! (`fip::check_world`) in Vesper (`packages/vesper-check-fip`), run
//! differentially against Rust over the corpus of lane 4's harness, the fip
//! checker test sources (`packages/vesper-check-fip/tests/cases.txt`) and
//! mutants of both that make every diagnostic of the module fire. Every
//! diagnostic's code, span, message, help and labels are compared.
//!
//! The input is the harness encoding of the world followed by the fip task
//! section ([`fip_task`]), which `read_ast` in
//! `packages/vesper-check-fip/src/ast.ves` decodes.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::sync::OnceLock;
use vesper::ast::{Expr, ExprKind, Pass, Pattern, PatternKind, Statement, Type};
use vesper::check::World;

// ------------------------------------------------------------------ encoder

/// The fip task section: the pool of list words (each a tag and a value: 0
/// an index, 1 a name, 2 a type), the expression and pattern tables and the
/// functions of `world.functions` in name order (ast.ves documents the
/// node layout).
#[derive(Default)]
struct FipEnc {
    pool: Vec<(u64, u64)>,
    exprs: Vec<[u64; 8]>,
    pats: Vec<[u64; 6]>,
}

impl FipEnc {
    fn list(&mut self, words: &[(u64, u64)]) -> u64 {
        let at = self.pool.len() as u64;
        self.pool.extend_from_slice(words);
        at
    }

    fn idx(i: u64) -> (u64, u64) {
        (0, i)
    }

    fn pat(&mut self, enc: &mut Enc, p: &Pattern) -> u64 {
        let (kind, a, b, c) = match &p.kind {
            PatternKind::Wild => (1, 0, 0, 0),
            PatternKind::Bind(n) => (2, enc.name(n), 0, 0),
            PatternKind::Literal(_) => (3, 0, 0, 0),
            PatternKind::Tuple(ps) => {
                let items: Vec<_> = ps.iter().map(|q| Self::idx(self.pat(enc, q))).collect();
                (4, self.list(&items), items.len() as u64, 0)
            }
            PatternKind::Constructor { name, fields, .. } => {
                let mut items = vec![];
                for (f, q) in fields {
                    let fname = enc.name(f);
                    let sub = self.pat(enc, q);
                    items.push((1, fname));
                    items.push(Self::idx(sub));
                }
                let name = enc.name(name);
                (5, name, self.list(&items), fields.len() as u64)
            }
        };
        let i = self.pats.len() as u64;
        self.pats
            .push([kind, p.span.start as u64, p.span.end as u64, a, b, c]);
        i
    }

    fn exprs(&mut self, enc: &mut Enc, es: &[&Expr]) -> Vec<(u64, u64)> {
        es.iter().map(|e| Self::idx(self.expr(enc, e))).collect()
    }

    fn expr(&mut self, enc: &mut Enc, e: &Expr) -> u64 {
        let opt = |x: Option<u64>| x.map_or(0, |i| i + 1);
        let pass = |p: Pass| pass_word(p);
        let node: [u64; 6] = match &e.kind {
            ExprKind::Literal(_) => [1, 0, 0, 0, 0, 0],
            ExprKind::Name(n) => [2, enc.name(n), 0, 0, 0, 0],
            ExprKind::Tuple(es) => {
                let items = self.exprs(enc, &es.iter().collect::<Vec<_>>());
                [3, self.list(&items), items.len() as u64, 0, 0, 0]
            }
            ExprKind::Struct { fields, .. } => {
                let items = self.exprs(enc, &fields.iter().map(|(_, e)| e).collect::<Vec<_>>());
                [4, self.list(&items), items.len() as u64, 0, 0, 0]
            }
            ExprKind::Block { statements, tail } => {
                let mut items = vec![];
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            let v = self.expr(enc, value);
                            let p = self.pat(enc, pat);
                            let t = enc.ty(ty);
                            items.extend([Self::idx(p + 1), (2, t), Self::idx(v)]);
                        }
                        Statement::Expr(x) => {
                            let v = self.expr(enc, x);
                            // A statement's type word is unused: any type.
                            let t = enc.ty(&Type::Infer);
                            items.extend([Self::idx(0), (2, t), Self::idx(v)]);
                        }
                    }
                }
                let t = tail.as_deref().map(|t| self.expr(enc, t));
                [5, self.list(&items), statements.len() as u64, opt(t), 0, 0]
            }
            ExprKind::Call { callee, args, .. } => {
                let c = self.expr(enc, callee);
                let mut items = vec![];
                for a in args {
                    let x = self.expr(enc, &a.expr);
                    items.extend([Self::idx(pass(a.pass)), Self::idx(x)]);
                }
                [6, c, self.list(&items), args.len() as u64, 0, 0]
            }
            ExprKind::Field(b, n) => {
                let x = self.expr(enc, b);
                [7, x, enc.name(n), 0, 0, 0]
            }
            ExprKind::Index(a, b) => {
                let x = self.expr(enc, a);
                let y = self.expr(enc, b);
                [8, x, y, 0, 0, 0]
            }
            ExprKind::Try(a) => [9, self.expr(enc, a), 0, 0, 0, 0],
            ExprKind::Unary(_, a) => [10, self.expr(enc, a), 0, 0, 0, 0],
            ExprKind::Binary(op, a, b) => {
                let x = self.expr(enc, a);
                let y = self.expr(enc, b);
                [11, x, y, matches!(op.as_str(), "&&" | "||") as u64, 0, 0]
            }
            ExprKind::Assign(_, a, b) => {
                let x = self.expr(enc, a);
                let y = self.expr(enc, b);
                [12, x, y, 0, 0, 0]
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(enc, condition);
                let t = self.expr(enc, then);
                let o = otherwise.as_deref().map(|o| self.expr(enc, o));
                [13, c, t, opt(o), 0, 0]
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(enc, value);
                let mut items = vec![];
                for (p, body) in arms {
                    let pi = self.pat(enc, p);
                    let bi = self.expr(enc, body);
                    items.extend([Self::idx(pi), Self::idx(bi)]);
                }
                [14, v, self.list(&items), arms.len() as u64, 0, 0]
            }
            ExprKind::Closure { body, .. } => [15, self.expr(enc, body), 0, 0, 0, 0],
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let (sp, sv) = match state {
                    Some((p, v)) => {
                        let pi = self.pat(enc, p);
                        (pi + 1, self.expr(enc, v))
                    }
                    None => (0, 0),
                };
                let b = self.expr(enc, body);
                let mut items = vec![Self::idx(1 + ops.len() as u64)];
                for c in std::iter::once(&**ret).chain(ops) {
                    let ci = self.expr(enc, &c.body);
                    let ps: Vec<_> = c.pats.iter().map(|p| Self::idx(self.pat(enc, p))).collect();
                    items.push(Self::idx(ci));
                    items.push(Self::idx(ps.len() as u64));
                    items.extend(ps);
                }
                [16, enc.name(&effect.name), sp, sv, b, self.list(&items)]
            }
            k => {
                let kind = match k {
                    ExprKind::Loop(_) => 17,
                    ExprKind::For { .. } => 18,
                    ExprKind::Range { .. } => 19,
                    ExprKind::Break(_) => 20,
                    ExprKind::Return(_) => 21,
                    _ => 22,
                };
                let kids = k.control_children().unwrap_or_default();
                let items = self.exprs(enc, &kids);
                [kind, self.list(&items), items.len() as u64, 0, 0, 0]
            }
        };
        let i = self.exprs.len() as u64;
        self.exprs.push([
            node[0],
            e.span.start as u64,
            e.span.end as u64,
            node[1],
            node[2],
            node[3],
            node[4],
            node[5],
        ]);
        i
    }
}

/// Writes the fip task section of `world` to `tail`.
fn fip_task(enc: &mut Enc, tail: &mut Vec<u64>, world: &World) {
    let mut fe = FipEnc::default();
    let mut fns = vec![];
    for f in world.functions.values() {
        let mut words = vec![enc.name(&f.name), f.fip as u64];
        words.extend([f.span.start as u64, f.span.end as u64]);
        words.push(f.generics.len() as u64);
        for g in &f.generics {
            words.extend([enc.name(&g.name), g.row as u64]);
        }
        words.push(f.params.len() as u64);
        for p in &f.params {
            words.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
            words.extend([p.span.start as u64, p.span.end as u64]);
        }
        words.push(enc.ty(&f.result));
        words.push(enc.ty(&Type::Row(f.effects.clone())));
        words.push(f.body.as_ref().map_or(0, |b| fe.expr(enc, b) + 1));
        fns.push(words);
    }
    tail.push(fe.pool.len() as u64);
    for (tag, x) in &fe.pool {
        tail.extend([*tag, *x]);
    }
    tail.push(fe.exprs.len() as u64);
    for e in &fe.exprs {
        tail.extend(e);
    }
    tail.push(fe.pats.len() as u64);
    for p in &fe.pats {
        tail.extend(p);
    }
    tail.push(fns.len() as u64);
    for f in fns {
        tail.extend(f);
    }
}

pub fn fip_input(world: &World) -> Vec<u8> {
    encode(world, |enc, tail| fip_task(enc, tail, world))
}

// ------------------------------------------------------------------ texts

/// `fip::check_world` as the Vesper module prints it: one line per
/// diagnostic, then its help and related labels (the fip checks have none,
/// so a difference there is a Rust change the port must follow).
fn rust_text(world: &World) -> String {
    let mut out = String::new();
    for d in vesper::fip::check_world(world) {
        out += &format!("{} {} {} {}\n", d.code, d.span.start, d.span.end, d.message);
        if let Some(h) = &d.help {
            out += &format!("  help {h}\n");
        }
        for l in &d.related {
            out += &format!("  related {} {} {}\n", l.span.start, l.span.end, l.message);
        }
    }
    out
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-fip"))
}

fn entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("check_fip", optimize))
}

fn vesper_text(world: &World, optimize: bool) -> String {
    entry(optimize).text(&fip_input(world))
}

// ------------------------------------------------------------------ cases

/// The fip checker test sources, checked (with their diagnostics: the
/// worlds still hold every function and expression type).
fn checker_cases() -> &'static [(String, World)] {
    static CASES: OnceLock<Vec<(String, World)>> = OnceLock::new();
    CASES.get_or_init(|| {
        let text =
            std::fs::read_to_string(repo().join("packages/vesper-check-fip/tests/cases.txt"))
                .unwrap();
        let mut out = vec![];
        let mut name = String::new();
        let mut body = String::new();
        let flush = |name: &str, body: &str, out: &mut Vec<(String, World)>| {
            if name.is_empty() {
                return;
            }
            let parsed = vesper::parser::parse(body);
            if !parsed.diagnostics.is_empty() {
                return;
            }
            out.push((name.to_string(), check::check(&parsed.module).world));
        };
        for line in text.lines() {
            if let Some(n) = line.strip_prefix("==== ") {
                flush(&name, &body, &mut out);
                name = n.to_string();
                body.clear();
            } else if !name.is_empty() {
                body += line;
                body.push('\n');
            }
        }
        flush(&name, &body, &mut out);
        out
    })
}

use vesper::check;

/// The mutants: 1 every function `@fip`; 2 every function `@fip` with
/// every parameter `sink`; 3 every `@fip` function plain (so its
/// `Box::new_in_caller` calls are E0802).
pub const MUTANTS: usize = 3;

fn mutant(world: &World, m: usize) -> World {
    let mut w = world.clone();
    for f in w.functions.values_mut() {
        match m {
            1 => f.fip = true,
            2 => {
                f.fip = true;
                for p in &mut f.params {
                    p.pass = Pass::Sink;
                }
            }
            _ => f.fip = !f.fip,
        }
    }
    w
}

/// Every (name, world) the differential runs: the corpus, the checker
/// cases, and each one's mutants.
fn all_cases(mutants: bool) -> Vec<(String, World)> {
    let mut out = vec![];
    let base: Vec<(String, &World)> = corpus()
        .iter()
        .map(|c| (c.name.clone(), &c.world))
        .chain(checker_cases().iter().map(|(n, w)| (n.clone(), w)))
        .collect();
    for (name, w) in base {
        out.push((name.clone(), w.clone()));
        if mutants {
            for m in 1..=MUTANTS {
                out.push((format!("{name}#m{m}"), mutant(w, m)));
            }
        }
    }
    out
}

fn run(label: &str, cases: &[(String, World)], optimize: bool) -> (usize, usize, Vec<String>) {
    let mut bad = 0;
    let mut diags = 0;
    let mut codes = std::collections::BTreeMap::<String, usize>::new();
    for (name, w) in cases {
        let r = rust_text(w);
        let v = vesper_text(w, optimize);
        diags += r.lines().count();
        for l in r.lines() {
            *codes
                .entry(l.split(' ').next().unwrap_or("").to_string())
                .or_default() += 1;
        }
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 5 {
                eprintln!("{label}: {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "{label}: {bad} of {} worlds differ ({diags} diagnostics: {codes:?})",
        cases.len()
    );
    (bad, diags, codes.keys().cloned().collect())
}

// ------------------------------------------------------------------ tests

#[test]
fn checker_cases_match_rust() {
    let cases: Vec<_> = checker_cases().to_vec();
    assert!(
        cases.len() >= 30,
        "only {} checker cases parse",
        cases.len()
    );
    let (bad, diags, codes) = run("fip cases", &cases, true);
    assert_eq!(bad, 0);
    assert!(diags > 0);
    // Every diagnostic of the module fires.
    for code in [
        "E0801", "E0802", "E0804", "E0805", "E0806", "E0807", "E0808", "E0809",
    ] {
        assert!(codes.iter().any(|c| c == code), "{code} never fires");
    }
}

#[test]
fn corpus_and_mutants_match_rust() {
    let cases = all_cases(true);
    let (bad, diags, _) = run("fip corpus", &cases, true);
    assert_eq!(bad, 0);
    assert!(diags > 0);
}

#[test]
fn unoptimized_jit_agrees() {
    // The JIT with the optimizer off on the checker cases and their mutants.
    let mut cases = vec![];
    for (n, w) in checker_cases() {
        cases.push((n.clone(), w.clone()));
        for m in 1..=MUTANTS {
            cases.push((format!("{n}#m{m}"), mutant(w, m)));
        }
    }
    let (bad, _, _) = run("fip unoptimized", &cases, false);
    assert_eq!(bad, 0);
}

/// ABBA timing of the checks alone (CHECK_HARNESS_SPEED=1): Rust's
/// `check_world` against the Vesper entry on pre-encoded inputs, and the
/// decode-only entry as the encoding baseline.
#[test]
#[ignore]
fn abba_check_fip() {
    if !speed() {
        return;
    }
    for mutants in [false, true] {
        abba_cases(
            &all_cases(mutants),
            if mutants {
                "with mutants"
            } else {
                "corpus and cases"
            },
        );
    }
}

fn abba_cases(cases: &[(String, World)], label: &str) {
    let inputs: Vec<Vec<u8>> = cases.iter().map(|(_, w)| fip_input(w)).collect();
    let count = package().entry("check_fip_count", true);
    let decode = package().entry("check_fip_decode", true);
    let sink = std::cell::Cell::new(0u64);
    let (r, v) = abba(
        &format!("fip check_world, {label}"),
        &mut || {
            time(|| {
                for (_, w) in cases {
                    sink.set(
                        sink.get()
                            .wrapping_add(vesper::fip::check_world(w).len() as u64),
                    );
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    sink.set(sink.get().wrapping_add(count.word(i)));
                }
            })
        },
    );
    let (_, d) = abba(&format!("fip decode only, {label}"), &mut || r, &mut || {
        time(|| {
            for i in &inputs {
                sink.set(sink.get().wrapping_add(decode.word(i)));
            }
        })
    });
    eprintln!(
        "fip checks without decoding, {label}: vesper {:?} ratio {:.2}",
        v.saturating_sub(d),
        v.saturating_sub(d).as_secs_f64() / r.as_secs_f64()
    );
    let enc = time(|| {
        for (_, w) in cases {
            sink.set(sink.get().wrapping_add(fip_input(w).len() as u64));
        }
    });
    eprintln!("encoding all inputs, {label}: {enc:?} ({})", sink.get());
}

/// One side once, for callgrind (`CHECK_FIP_SIDE=rust|vesper|none`, with
/// `CHECK_HARNESS_STRIDE` to shrink the corpus): the inputs are encoded and
/// the entry compiled before the side runs, so `none` is the baseline both
/// sides subtract.
#[test]
#[ignore]
fn callgrind_side() {
    let Ok(side) = std::env::var("CHECK_FIP_SIDE") else {
        return;
    };
    let cases = all_cases(true);
    let inputs: Vec<Vec<u8>> = cases.iter().map(|(_, w)| fip_input(w)).collect();
    let count = package().entry("check_fip_count", true);
    let mut n = 0u64;
    match side.as_str() {
        "rust" => {
            for (_, w) in &cases {
                n += vesper::fip::check_world(w).len() as u64;
            }
        }
        "vesper" => {
            for i in &inputs {
                n += count.word(i);
            }
        }
        _ => {}
    }
    eprintln!(
        "callgrind side {side}: {} worlds, {n} diagnostics",
        cases.len()
    );
}
