//! Self-hosting SH3 lane 10 (#1249): the certificate checker and candidate
//! builder of `src/optimize/witness.rs` in Vesper (`packages/vesper-optimize`)
//! against the Rust code they port.
//!
//! The test installs a recording table at `src/optimize.rs`'s swap point,
//! so every proposal the Rust optimizer makes reaches it with the producer's
//! remaining budget: over every checked corpus world, the optimizer's own
//! test fixtures and a generated corpus of integer islands. Each proposal,
//! and mutants of it (certificate, candidate, limits and budget), is checked
//! by `witness::verify_spent` and by the Vesper checker; the two must give
//! the same result, the same `WitnessError` and the same spent budget
//! counters. The Vesper candidate builder must rebuild each genuine
//! candidate exactly.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper::ast::{Arg, Expr, ExprKind, Literal, Pass, Type, TypeMap};
use vesper::numeric::IntegerKind;
use vesper::optimize::witness::{
    self, Equality, IntOp, Reason, RewriteCertificate, Spent, Substitution, Term, TermNode,
    WitnessError, WitnessLimits,
};
use vesper::optimize::{self, OptimizeTable};

// ------------------------------------------------------------ recording

/// One check: what `verify_spent` was given, and what it answered.
#[derive(Clone)]
struct Check {
    source: Expr,
    candidate: Expr,
    types: TypeMap,
    certificate: RewriteCertificate,
    /// The certificate's vector capacities (a clone's may differ).
    terms_cap: usize,
    proofs_cap: usize,
    limits: WitnessLimits,
    spent: Spent,
}

struct Recorded {
    check: Check,
    result: Result<(), WitnessError>,
    after: Spent,
}

thread_local! {
    /// This thread's proposals since the last [`take_records`]: each corpus
    /// worker records its own, so the merged order is the corpus order.
    static RECORDS: std::cell::RefCell<Vec<Recorded>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn take_records() -> Vec<Recorded> {
    RECORDS.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

/// The spans of every node of `e`.
fn spans(e: &Expr, out: &mut Vec<(usize, usize)>) {
    out.push((e.span.start, e.span.end));
    match &e.kind {
        ExprKind::Binary(_, a, b) => {
            spans(a, out);
            spans(b, out);
        }
        ExprKind::Call { callee, args, .. } => {
            spans(callee, out);
            for a in args {
                spans(&a.expr, out);
            }
        }
        _ => {}
    }
}

/// The entries of `types` the checker can look up for `source` and
/// `candidate` (it only asks for their nodes' spans).
fn relevant_types(source: &Expr, candidate: &Expr, types: &TypeMap) -> TypeMap {
    let mut s = vec![];
    spans(source, &mut s);
    spans(candidate, &mut s);
    s.into_iter()
        .filter_map(|k| types.get(&k).map(|t| (k, t.clone())))
        .collect()
}

fn recording_verify(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    certificate: &RewriteCertificate,
    limits: WitnessLimits,
    spent: &mut Spent,
) -> Result<(), WitnessError> {
    let check = Check {
        source: source.clone(),
        candidate: candidate.clone(),
        types: relevant_types(source, candidate, types),
        certificate: certificate.clone(),
        terms_cap: certificate.terms.capacity(),
        proofs_cap: certificate.proofs.capacity(),
        limits,
        spent: *spent,
    };
    let result = witness::verify_spent(source, candidate, types, certificate, limits, spent);
    let record = Recorded {
        check,
        result,
        after: *spent,
    };
    RECORDS.with(|r| r.borrow_mut().push(record));
    result
}

fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        optimize::install_optimize(OptimizeTable {
            verify: recording_verify,
            propose: None,
        })
        .unwrap();
    });
}

// ------------------------------------------------------------ corpus

/// Integer island fixtures of `src/optimize/{certificate,adversarial_tests}.rs`.
const FIXTURES: &[&str] = &[
    "fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { x + 0 }",
    "fn f(x: u8) -> u8 { wrapping_mul(x, 0) }",
    "fn f(x: u32) -> u32 { wrapping_mul(x, 0) }",
    "fn f(x: u64) -> u64 { wrapping_mul(x, 0) }",
    "fn f(x: i64) -> i64 { wrapping_mul(x, 0) }",
    "fn f(x: u8, y: u8) -> u8 { wrapping_sub(wrapping_add(x, y), y) }",
    "fn f(x: i64, y: i64) -> i64 { wrapping_sub(wrapping_add(x, y), y) }",
    "fn f() -> u8 eff [Fail[ArithOverflow]] { 4 + 5 }",
    "fn f() -> u8 eff [Fail[ArithOverflow]] { 4 - 3 }",
    "fn f() -> u8 eff [Fail[ArithOverflow]] { 3 * 5 }",
    "fn f() -> u8 { wrapping_add(250, 9) }",
    "fn f() -> u8 { wrapping_sub(0, 1) }",
    "fn f() -> u8 { wrapping_mul(16, 16) }",
    "fn f() -> u64 { wrapping_add(9223372036854775807, 2) }",
    "fn f() -> u64 { wrapping_sub(18446744073709551615, 18446744073709551615) }",
    "fn f() -> u8 eff [Fail[ArithOverflow]] { 255 + 1 }",
    "fn f() -> u64 { wrapping_add(wrapping_add(1, 2), 3) }",
    "fn f(x: u8) -> u8 eff [Fail[ArithOverflow]] { (x + 0) * 0 }",
    "fn f(x: u8) -> u8 eff [Fail[ArithOverflow]] { (1 + 2) + x }",
    "fn f(x: u64, y: u64) -> u64 { wrapping_mul(wrapping_add(x, 0), y) }",
    "fn f()->i64{2+3}",
    "fn f(x:i64)->i64{wrapping_add(x,0)}",
    "fn f()->u64{18446744073709551615-1}",
    "fn f()->u64{wrapping_mul(18446744073709551615,0)}",
];

/// A small deterministic generator (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const KINDS: [(&str, &[&str]); 4] = [
    ("u8", &["0", "1", "2", "7", "255"]),
    ("u32", &["0", "1", "3", "4294967295"]),
    ("u64", &["0", "1", "5", "18446744073709551615"]),
    ("i64", &["0", "1", "6", "9223372036854775807"]),
];

/// A random pure integer expression over `x`, `y`, `z` and the kind's
/// constants: checked operators when `checked`, else `wrapping_*` calls.
fn island(rng: &mut Rng, constants: &[&str], checked: bool, depth: u32) -> String {
    if depth == 0 || rng.below(4) == 0 {
        return match rng.below(5) {
            0 => "x".into(),
            1 => "y".into(),
            2 => "z".into(),
            _ => constants[rng.below(constants.len() as u64) as usize].into(),
        };
    }
    let a = island(rng, constants, checked, depth - 1);
    let b = if rng.below(4) == 0 {
        a.clone()
    } else {
        island(rng, constants, checked, depth - 1)
    };
    let op = rng.below(3) as usize;
    if checked && rng.below(3) != 0 {
        format!("({a} {} {b})", ["+", "-", "*"][op])
    } else {
        format!(
            "{}({a}, {b})",
            ["wrapping_add", "wrapping_sub", "wrapping_mul"][op]
        )
    }
}

fn generated() -> Vec<String> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = vec![];
    for i in 0..400 {
        let (kind, constants) = KINDS[i % 4];
        let checked = i % 3 != 0;
        let body = island(&mut rng, constants, checked, 1 + (i % 5) as u32);
        let effect = if checked {
            " eff [Fail[ArithOverflow]]"
        } else {
            ""
        };
        out.push(format!(
            "fn f(x: {kind}, y: {kind}, z: {kind}) -> {kind}{effect} {{ {body} }}"
        ));
    }
    out
}

/// Runs the Rust optimizer over a module, recording its proposals.
fn optimize_module(module: &vesper::ast::Module) -> usize {
    let checked = vesper::check::check(module);
    if !checked.diagnostics.is_empty() {
        return 0;
    }
    let before = RECORDS.with(|r| r.borrow().len());
    let mut m = module.clone();
    optimize::optimize(&mut m, &checked.expression_types);
    RECORDS.with(|r| r.borrow().len()) - before
}

fn optimize_text(text: &str) -> usize {
    let parsed = vesper::parser::parse(text);
    if !parsed.diagnostics.is_empty() {
        return 0;
    }
    optimize_module(&parsed.module)
}

/// Every proposal of the corpus, the fixtures and the generated islands,
/// recorded once per test binary.
fn proposals() -> &'static (Vec<Recorded>, [usize; 3]) {
    static ALL: OnceLock<(Vec<Recorded>, [usize; 3])> = OnceLock::new();
    ALL.get_or_init(|| {
        install();
        let start = std::time::Instant::now();
        // `OPTIMIZE_SKIP_CORPUS=1` (callgrind runs) keeps the fixtures and
        // the generated islands only.
        // Each world is parsed and checked once, on a few threads.
        let mut all: Vec<Recorded> = if std::env::var_os("OPTIMIZE_SKIP_CORPUS").is_some() {
            vec![]
        } else {
            map_checked_corpus(|mut module, types| {
                take_records();
                optimize::optimize(&mut module, types);
                take_records()
            })
            .into_iter()
            .flatten()
            .collect()
        };
        let corpus_count = all.len();
        let fixture_count: usize = FIXTURES.iter().map(|t| optimize_text(t)).sum();
        let generated_count: usize = generated().iter().map(|t| optimize_text(t)).sum();
        all.extend(take_records());
        eprintln!("recorded the proposals in {:?}", start.elapsed());
        (all, [corpus_count, fixture_count, generated_count])
    })
}

// ------------------------------------------------------------ encoding

#[derive(Default)]
struct Words {
    names: Vec<String>,
    index: BTreeMap<String, u64>,
    exprs: Vec<u64>,
    count: u64,
}

impl Words {
    fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.index.get(s) {
            return *i;
        }
        let i = self.names.len() as u64;
        self.names.push(s.to_owned());
        self.index.insert(s.to_owned(), i);
        i
    }

    /// Appends `e` children first; returns its input index.
    fn expr(&mut self, e: &Expr) -> u64 {
        let mut w = vec![];
        let kind = match &e.kind {
            ExprKind::Literal(l) => {
                match l {
                    Literal::Int(v) => {
                        w.extend([1, (*v < 0) as u64, v.unsigned_abs() as u64]);
                    }
                    Literal::Float(_) => w.extend([2, 0, 0]),
                    Literal::Bool(_) => w.extend([3, 0, 0]),
                    Literal::String(_) => w.extend([4, 0, 0]),
                }
                1
            }
            ExprKind::Name(n) => {
                w.extend([self.name(n), n.capacity() as u64]);
                2
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let c = self.expr(callee);
                let mut a = vec![];
                for arg in args {
                    let x = self.expr(&arg.expr);
                    a.extend([pass_word(arg.pass), arg.explicit as u64, x]);
                }
                w.extend([
                    c,
                    types.len() as u64,
                    types.capacity() as u64,
                    args.len() as u64,
                    args.capacity() as u64,
                ]);
                w.extend(a);
                6
            }
            ExprKind::Binary(op, a, b) => {
                let x = self.expr(a);
                let y = self.expr(b);
                w.extend([self.name(op), op.capacity() as u64, x, y]);
                11
            }
            k => kind_number(k),
        };
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64]);
        self.exprs.extend(w);
        self.count += 1;
        self.count - 1
    }
}

fn kind_number(k: &ExprKind) -> u64 {
    match k {
        ExprKind::Literal(_) => 1,
        ExprKind::Name(_) => 2,
        ExprKind::Tuple(_) => 3,
        ExprKind::Struct { .. } => 4,
        ExprKind::Block { .. } => 5,
        ExprKind::Call { .. } => 6,
        ExprKind::Field(..) => 7,
        ExprKind::Index(..) => 8,
        ExprKind::Try(_) => 9,
        ExprKind::Unary(..) => 10,
        ExprKind::Binary(..) => 11,
        ExprKind::Assign(..) => 12,
        ExprKind::If { .. } => 13,
        ExprKind::Match { .. } => 14,
        ExprKind::Closure { .. } => 15,
        ExprKind::Handle { .. } => 16,
        ExprKind::Loop(_) => 17,
        ExprKind::For { .. } => 18,
        ExprKind::Range { .. } => 19,
        ExprKind::Break(_) => 20,
        ExprKind::Return(_) => 21,
        ExprKind::Continue => 22,
    }
}

fn type_code(t: &Type) -> u64 {
    match t {
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            "u8" => 1,
            "u32" => 2,
            "u64" => 3,
            "i64" => 4,
            _ => 6,
        },
        Type::IntLiteral(_) => 5,
        _ => 6,
    }
}

fn op_word(op: IntOp) -> u64 {
    match op {
        IntOp::Add => 0,
        IntOp::Sub => 1,
        IntOp::Mul => 2,
    }
}

fn term_words(t: &Term, out: &mut Vec<u64>) {
    out.push(t.kind as u64);
    match t.node {
        TermNode::Const(v) => out.extend([0, v as u64, ((v as u128) >> 64) as u64, 0]),
        TermNode::InputVar(v) => out.extend([1, v as u64, 0, 0]),
        TermNode::Checked { op, children } => {
            out.extend([2, op_word(op), children[0] as u64, children[1] as u64])
        }
        TermNode::Wrapping { op, children } => {
            out.extend([3, op_word(op), children[0] as u64, children[1] as u64])
        }
    }
}

fn proof_words(p: &Equality, out: &mut Vec<u64>) {
    out.extend([p.from as u64, p.to as u64]);
    match p.reason {
        Reason::Refl => out.extend([0, 0, 0, 0, 0, 0, 0]),
        Reason::Rule {
            rule,
            substitution,
            guard,
        } => out.extend([
            1,
            rule as u64,
            substitution.bindings[0] as u64,
            substitution.bindings[1] as u64,
            substitution.bindings[2] as u64,
            substitution.len as u64,
            guard.map_or(0, |g| g as u64 + 1),
        ]),
        Reason::Congruence { children } => {
            out.extend([2, children[0] as u64, children[1] as u64, 0, 0, 0, 0])
        }
        Reason::Symmetry { proof } => out.extend([3, proof as u64, 0, 0, 0, 0, 0]),
        Reason::Transitivity { first, second } => {
            out.extend([4, first as u64, second as u64, 0, 0, 0, 0])
        }
    }
}

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

/// One case's input (`optimize_verify` in lib.ves), appended to `out`.
fn case_bytes(c: &Check, build: bool, out: &mut Vec<u8>) {
    let l = c.limits;
    for x in [
        l.max_terms,
        l.max_proofs,
        l.max_input_nodes,
        l.max_output_nodes,
        l.max_depth,
        l.max_variables,
        l.max_name_bytes,
        l.max_metadata_bytes,
        l.max_work,
        c.spent.work,
        c.spent.metadata,
        c.spent.names,
    ] {
        put(out, x as u64);
    }
    let mut w = Words::default();
    let source = w.expr(&c.source);
    let candidate = w.expr(&c.candidate);
    put(out, w.names.len() as u64);
    for n in &w.names {
        put(out, n.len() as u64);
        out.extend(n.as_bytes());
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }
    let mut words = vec![c.types.len() as u64];
    for ((s, e), t) in &c.types {
        words.extend([*s as u64, *e as u64, type_code(t)]);
    }
    words.push(w.count);
    words.extend(&w.exprs);
    words.extend([source, candidate]);
    let cert = &c.certificate;
    words.extend([
        cert.schema_version as u64,
        cert.arithmetic_version as u64,
        c.terms_cap as u64,
        c.proofs_cap as u64,
        cert.input as u64,
        cert.output as u64,
        cert.conclusion as u64,
        cert.terms.len() as u64,
    ]);
    for t in &cert.terms {
        term_words(t, &mut words);
    }
    words.push(cert.proofs.len() as u64);
    for p in &cert.proofs {
        proof_words(p, &mut words);
    }
    words.push(build as u64);
    for x in words {
        put(out, x);
    }
}

/// The input bytes of a batch of cases.
fn encode_cases(cases: &[(&Check, bool)]) -> Vec<u8> {
    let mut out = vec![];
    put(&mut out, cases.len() as u64);
    for (c, build) in cases {
        case_bytes(c, *build, &mut out);
    }
    out
}

// ------------------------------------------------------------ expected text

fn error_code(r: Result<(), WitnessError>) -> u64 {
    match r {
        Ok(()) => 0,
        Err(e) => e as u64 + 1,
    }
}

fn pass_number(p: Pass) -> u64 {
    pass_word(p)
}

/// The dump `dump_expr` (lib.ves) writes.
fn dump(e: &Expr, o: &mut String) {
    use std::fmt::Write;
    let span = format!("@{}:{}", e.span.start, e.span.end);
    match &e.kind {
        ExprKind::Literal(Literal::Int(v)) => write!(o, "{v}{span}").unwrap(),
        ExprKind::Literal(_) => write!(o, "lit{span}").unwrap(),
        ExprKind::Name(n) => write!(o, "{n}{span}").unwrap(),
        ExprKind::Binary(op, a, b) => {
            write!(o, "({op}{span} ").unwrap();
            dump(a, o);
            o.push(' ');
            dump(b, o);
            o.push(')');
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            write!(o, "(call{span} ").unwrap();
            dump(callee, o);
            write!(o, " [{}]", types.len()).unwrap();
            for Arg {
                pass,
                explicit,
                expr,
            } in args
            {
                write!(o, " {}", pass_number(*pass)).unwrap();
                if *explicit {
                    o.push('!');
                }
                o.push(':');
                dump(expr, o);
            }
            o.push(')');
        }
        k => write!(o, "?{}{span}", kind_number(k)).unwrap(),
    }
}

fn expected_line(result: Result<(), WitnessError>, after: Spent) -> String {
    format!(
        "R {} {} {} {}\n",
        error_code(result),
        after.work,
        after.metadata,
        after.names
    )
}

fn rust_line(c: &Check) -> String {
    let mut spent = c.spent;
    let mut cert = c.certificate.clone();
    // Reproduce the recorded capacities, which `verify_spent` charges.
    cert.terms.reserve_exact(c.terms_cap - cert.terms.len());
    cert.proofs.reserve_exact(c.proofs_cap - cert.proofs.len());
    assert_eq!(
        (cert.terms.capacity(), cert.proofs.capacity()),
        (c.terms_cap, c.proofs_cap)
    );
    let r = witness::verify_spent(
        &c.source,
        &c.candidate,
        &c.types,
        &cert,
        c.limits,
        &mut spent,
    );
    expected_line(r, spent)
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-optimize"))
}

/// The `optimize_verify` entry, optimized, compiled once per test binary.
fn verify_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("optimize_verify", true))
}

/// Runs `cases` through the Vesper checker in batches; returns its lines.
fn vesper_lines(entry: &Entry, cases: &[(&Check, bool)]) -> Vec<String> {
    let mut out = vec![];
    for chunk in cases.chunks(256) {
        let text = entry.text(&encode_cases(chunk));
        out.extend(text.lines().map(|l| format!("{l}\n")));
    }
    out
}

// ------------------------------------------------------------ mutants

fn set_reason(c: &mut Check, i: usize, r: Reason) {
    c.certificate.proofs[i].reason = r;
}

/// Mutants of one genuine check: each changes one thing a checker must
/// look at. A large check (`small` false, over 20,000 units of the gate's
/// work: a few corpus islands) gets mutants of its first terms and proofs
/// only, which keeps the debug-build run inside the Bazel `long` timeout.
fn mutants(g: &Check, small: bool) -> Vec<Check> {
    let (term_count, proof_count) = if small { (12, 16) } else { (4, 4) };
    let mut out = vec![];
    let mut push = |f: &dyn Fn(&mut Check)| {
        let mut c = g.clone();
        f(&mut c);
        out.push(c);
    };
    push(&|c| c.certificate.schema_version = 2);
    push(&|c| c.certificate.arithmetic_version = 0);
    push(&|c| c.certificate.conclusion = u32::MAX);
    push(&|c| c.certificate.conclusion = 0);
    push(&|c| c.certificate.proofs.clear());
    push(&|c| c.certificate.terms.clear());
    push(&|c| c.certificate.input = u32::MAX);
    push(&|c| c.certificate.output = u32::MAX);
    push(&|c| c.certificate.output = c.certificate.input);
    push(&|c| c.certificate.input = c.certificate.output);
    push(&|c| c.candidate = c.source.clone());
    push(&|c| c.candidate.span.end += 1);
    push(&|c| c.source.span.start += 1);
    push(&|c| c.types.clear());
    push(&|c| {
        for t in c.types.values_mut() {
            *t = IntegerKind::U16.ty();
        }
    });
    push(&|c| {
        if let Some(t) = c.types.values_mut().last() {
            *t = Type::IntLiteral(0);
        }
    });
    push(&|c| {
        if let Some(t) = c.types.values_mut().next() {
            *t = IntegerKind::I64.ty();
        }
    });
    // Candidate leaves and operators.
    push(&|c| {
        edit(&mut c.candidate, &|e| match &mut e.kind {
            ExprKind::Name(n) => {
                n.push('q');
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.candidate, &|e| match &mut e.kind {
            ExprKind::Literal(Literal::Int(v)) => {
                *v += 1;
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.candidate, &|e| match &mut e.kind {
            ExprKind::Binary(op, _, _) => {
                *op = if op == "+" { "*".into() } else { "+".into() };
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.candidate, &|e| match &mut e.kind {
            ExprKind::Call { callee, .. } => {
                callee.span.start += 1;
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.candidate, &|e| match &mut e.kind {
            ExprKind::Call { args, .. } => {
                args[0].pass = Pass::Inout;
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.candidate, &|e| match &mut e.kind {
            ExprKind::Call { types, .. } => {
                types.push(Type::Infer);
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.source, &|e| match &mut e.kind {
            ExprKind::Call { callee, .. } => {
                callee.kind = ExprKind::Field(Box::new((**callee).clone()), "f".into());
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.source, &|e| match &mut e.kind {
            ExprKind::Literal(Literal::Int(v)) => {
                // The parser's largest magnitude: out of range below u64.
                *v = -(u64::MAX as i128);
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.source, &|e| match &mut e.kind {
            ExprKind::Name(_) => {
                e.kind = ExprKind::Literal(Literal::Bool(true));
                true
            }
            _ => false,
        })
    });
    push(&|c| {
        edit(&mut c.source, &|e| match &mut e.kind {
            ExprKind::Binary(op, _, _) => {
                *op = "==".into();
                true
            }
            _ => false,
        })
    });
    // Terms.
    for i in 0..g.certificate.terms.len().min(term_count) {
        push(&|c| c.certificate.terms[i].kind = IntegerKind::U16);
        push(&|c| {
            c.certificate.terms[i].node = match c.certificate.terms[i].node {
                TermNode::Const(v) => TermNode::Const(v + 1),
                TermNode::InputVar(v) => TermNode::InputVar(v + 1),
                TermNode::Checked { op, children } => TermNode::Wrapping { op, children },
                TermNode::Wrapping { op, children } => TermNode::Checked { op, children },
            }
        });
        push(&|c| {
            if let TermNode::Checked { children, .. } | TermNode::Wrapping { children, .. } =
                &mut c.certificate.terms[i].node
            {
                children.swap(0, 1);
            }
        });
        push(&|c| {
            if let TermNode::Checked { children, .. } | TermNode::Wrapping { children, .. } =
                &mut c.certificate.terms[i].node
            {
                children[0] = i as u32;
            }
        });
        push(&|c| {
            if let TermNode::Checked { op, .. } | TermNode::Wrapping { op, .. } =
                &mut c.certificate.terms[i].node
            {
                *op = match op {
                    IntOp::Add => IntOp::Sub,
                    IntOp::Sub => IntOp::Mul,
                    IntOp::Mul => IntOp::Add,
                };
            }
        });
        push(&|c| c.certificate.terms[i].node = TermNode::Const(-1));
        push(&|c| c.certificate.terms[i].node = TermNode::Const(i128::MAX));
    }
    // Proofs.
    for i in 0..g.certificate.proofs.len().min(proof_count) {
        push(&|c| set_reason(c, i, Reason::Refl));
        push(&|c| {
            let p = &mut c.certificate.proofs[i];
            std::mem::swap(&mut p.from, &mut p.to);
        });
        push(&|c| c.certificate.proofs[i].to = c.certificate.proofs[i].from);
        push(&|c| c.certificate.proofs[i].from = u32::MAX);
        push(&|c| {
            set_reason(
                c,
                i,
                Reason::Symmetry {
                    proof: i.saturating_sub(1) as u32,
                },
            )
        });
        push(&|c| {
            set_reason(
                c,
                i,
                Reason::Transitivity {
                    first: 0,
                    second: i as u32,
                },
            )
        });
        push(&|c| {
            set_reason(
                c,
                i,
                Reason::Congruence {
                    children: [0, i.saturating_sub(1) as u32],
                },
            )
        });
        match g.certificate.proofs[i].reason {
            Reason::Rule {
                rule,
                substitution,
                guard,
            } => {
                let rule_with = |c: &mut Check, rule: u16, s: Substitution, guard: Option<u32>| {
                    set_reason(
                        c,
                        i,
                        Reason::Rule {
                            rule,
                            substitution: s,
                            guard,
                        },
                    )
                };
                push(&|c| rule_with(c, rule % 24 + 1, substitution, guard));
                push(&|c| rule_with(c, 0, substitution, guard));
                push(&|c| rule_with(c, 25, substitution, guard));
                push(&|c| {
                    let mut s = substitution;
                    s.len += 1;
                    rule_with(c, rule, s, guard)
                });
                push(&|c| {
                    let mut s = substitution;
                    s.bindings[2] = 1;
                    rule_with(c, rule, s, guard)
                });
                push(&|c| {
                    let mut s = substitution;
                    s.bindings[0] = u32::MAX;
                    rule_with(c, rule, s, guard)
                });
                push(&|c| rule_with(c, rule, substitution, None));
                push(&|c| rule_with(c, rule, substitution, Some(0)));
                push(&|c| rule_with(c, rule, substitution, Some(i as u32)));
                push(&|c| rule_with(c, rule, substitution, Some(u32::MAX)));
            }
            Reason::Congruence { children } => {
                push(&|c| {
                    set_reason(
                        c,
                        i,
                        Reason::Congruence {
                            children: [children[1], children[0]],
                        },
                    )
                });
                push(&|c| {
                    set_reason(
                        c,
                        i,
                        Reason::Congruence {
                            children: [i as u32, children[1]],
                        },
                    )
                });
            }
            Reason::Transitivity { first, second } => {
                push(&|c| {
                    set_reason(
                        c,
                        i,
                        Reason::Transitivity {
                            first: second,
                            second: first,
                        },
                    )
                });
                push(&|c| {
                    set_reason(
                        c,
                        i,
                        Reason::Transitivity {
                            first,
                            second: u32::MAX,
                        },
                    )
                });
            }
            Reason::Symmetry { .. } => {
                push(&|c| set_reason(c, i, Reason::Symmetry { proof: i as u32 }))
            }
            Reason::Refl => {}
        }
    }
    // Limits: every limit at a few tight values, and the budget cut at
    // points through the check, so each charge site can be the one that
    // refuses.
    let spent = g.spent;
    let mut final_spent = g.spent;
    let mut cert = g.certificate.clone();
    cert.terms.reserve_exact(g.terms_cap - cert.terms.len());
    cert.proofs.reserve_exact(g.proofs_cap - cert.proofs.len());
    let _ = witness::verify_spent(
        &g.source,
        &g.candidate,
        &g.types,
        &cert,
        g.limits,
        &mut final_spent,
    );
    let hard = WitnessLimits::HARD;
    for k in 0..12 {
        let work = spent.work + (final_spent.work - spent.work) * k / 11;
        push(&|c| c.limits.max_work = work.clamp(1, hard.max_work));
        let meta = spent.metadata + (final_spent.metadata - spent.metadata) * k / 11;
        push(&|c| c.limits.max_metadata_bytes = meta.clamp(1, hard.max_metadata_bytes));
        let names = spent.names + (final_spent.names - spent.names) * k / 11;
        push(&|c| c.limits.max_name_bytes = names.clamp(1, hard.max_name_bytes));
    }
    for v in [1, 2, 3, 4, 6] {
        push(&|c| c.limits.max_terms = v);
        push(&|c| c.limits.max_proofs = v);
        push(&|c| c.limits.max_input_nodes = v);
        push(&|c| c.limits.max_output_nodes = v);
        push(&|c| c.limits.max_depth = v);
        push(&|c| c.limits.max_variables = v);
    }
    push(&|c| c.limits.max_work = 0);
    push(&|c| c.limits.max_terms = hard.max_terms + 1);
    push(&|c| c.limits = hard);
    push(&|c| c.spent = Spent::default());
    out
}

/// Applies `f` to the first node of `e` (pre-order) it accepts.
fn edit(e: &mut Expr, f: &dyn Fn(&mut Expr) -> bool) {
    mutate_first(e, f);
}

fn mutate_first(e: &mut Expr, f: &dyn Fn(&mut Expr) -> bool) -> bool {
    if f(e) {
        return true;
    }
    match &mut e.kind {
        ExprKind::Binary(_, a, b) => mutate_first(a, f) || mutate_first(b, f),
        ExprKind::Call { args, .. } => args.iter_mut().any(|a| mutate_first(&mut a.expr, f)),
        _ => false,
    }
}

// ------------------------------------------------------------ tests

#[test]
fn rust_sizes_match_the_vesper_constants() {
    let words = package().entry("optimize_sizes", true).words(&[]);
    use std::mem::size_of;
    assert_eq!(
        words,
        [
            size_of::<Term>(),
            size_of::<Equality>(),
            size_of::<Expr>(),
            size_of::<Arg>(),
            size_of::<Type>(),
        ]
        .map(|s| s as u64)
    );
}

#[test]
fn every_proposal_is_checked_and_rebuilt_like_rust() {
    let (all, counts) = proposals();
    eprintln!(
        "proposals: {} corpus, {} fixtures, {} generated; {} accepted",
        counts[0],
        counts[1],
        counts[2],
        all.iter().filter(|r| r.result.is_ok()).count()
    );
    assert!(counts[1] >= 20 && counts[2] >= 100, "{counts:?}");
    let entry = verify_entry();
    let cases: Vec<(&Check, bool)> = all.iter().map(|r| (&r.check, true)).collect();
    let lines = vesper_lines(entry, &cases);
    assert_eq!(lines.len(), 2 * all.len());
    let mut bad = 0;
    for (i, r) in all.iter().enumerate() {
        // The gate's answer as recorded, and again on the recorded copy.
        let recorded = expected_line(r.result, r.after);
        assert_eq!(rust_line(&r.check), recorded);
        let mut built = String::from("B 0 ");
        dump(&r.check.candidate, &mut built);
        built.push('\n');
        for (want, got) in [(&recorded, &lines[2 * i]), (&built, &lines[2 * i + 1])] {
            if want != got {
                bad += 1;
                if bad <= 5 {
                    eprintln!(
                        "proposal {i}: {:?}\n  rust:   {want}  vesper: {got}",
                        r.check.source
                    );
                }
            }
        }
    }
    assert_eq!(bad, 0);
}

#[test]
fn mutated_proposals_are_refused_for_the_same_reason() {
    let (all, _) = proposals();
    let entry = verify_entry();
    let mut checks = vec![];
    for r in all {
        checks.extend(mutants(
            &r.check,
            r.after.work - r.check.spent.work <= 20_000,
        ));
    }
    let cases: Vec<(&Check, bool)> = checks.iter().map(|c| (c, false)).collect();
    let start = std::time::Instant::now();
    let lines = vesper_lines(entry, &cases);
    eprintln!("vesper checked the mutants in {:?}", start.elapsed());
    assert_eq!(lines.len(), checks.len());
    let mut bad = 0;
    let mut reasons = BTreeMap::<String, usize>::new();
    for (c, got) in checks.iter().zip(&lines) {
        let want = rust_line(c);
        *reasons
            .entry(want.split(' ').nth(1).unwrap().to_owned())
            .or_default() += 1;
        if want != *got {
            bad += 1;
            if bad <= 5 {
                eprintln!(
                    "mutant: {:?}\n  {:?}\n  rust:   {want}  vesper: {got}",
                    c.source, c.certificate
                );
            }
        }
    }
    eprintln!(
        "{} mutants (both checkers {:?}), outcomes by code: {reasons:?}",
        checks.len(),
        start.elapsed()
    );
    assert_eq!(bad, 0);
}

/// The JIT with the optimizer off gives the same lines (every fifth
/// proposal and its mutants).
#[test]
fn unoptimized_agrees() {
    let (all, _) = proposals();
    let mut checks = vec![];
    for r in all.iter().step_by(5) {
        checks.push(r.check.clone());
        checks.extend(mutants(&r.check, false));
    }
    let cases: Vec<(&Check, bool)> = checks.iter().map(|c| (c, true)).collect();
    let on = vesper_lines(verify_entry(), &cases);
    let off = vesper_lines(&package().entry("optimize_verify", false), &cases);
    assert_eq!(on, off);
}

/// One side of the speed test, never inlined so that callgrind can collect
/// it alone (`--toggle-collect='*measured_side*'`).
#[inline(never)]
fn measured_side(f: &mut dyn FnMut() -> std::time::Duration) {
    std::hint::black_box(f());
}

/// ABBA over every genuine proposal: Rust's gate (`verify_spent` on
/// prepared copies) against the Vesper checker, whole (with its input
/// decode) and alone (decode subtracted). `CHECK_HARNESS_SPEED=1`.
#[test]
#[ignore]
fn speed_certificate_checker() {
    if !speed() {
        return;
    }
    let (all, _) = proposals();
    let checks: Vec<(Check, RewriteCertificate)> = all
        .iter()
        .map(|r| {
            let mut cert = r.check.certificate.clone();
            cert.terms
                .reserve_exact(r.check.terms_cap - cert.terms.len());
            cert.proofs
                .reserve_exact(r.check.proofs_cap - cert.proofs.len());
            (r.check.clone(), cert)
        })
        .collect();
    let cases: Vec<(&Check, bool)> = all.iter().map(|r| (&r.check, false)).collect();
    let input = encode_cases(&cases);
    let verify = verify_entry();
    let decode = package().entry("optimize_decode", true);
    // `OPTIMIZE_SIDE=rust|verify|decode|none` runs one side once, for
    // callgrind (the Ir of a side is its run minus the `none` run).
    let side = std::env::var("OPTIMIZE_SIDE").ok();
    let rounds = if side.is_some() { 1 } else { 20 };
    let mut rust = || {
        time(|| {
            for _ in 0..rounds {
                for (c, cert) in &checks {
                    let mut spent = c.spent;
                    let r = witness::verify_spent(
                        &c.source,
                        &c.candidate,
                        &c.types,
                        cert,
                        c.limits,
                        &mut spent,
                    );
                    let _ = std::hint::black_box((r, spent));
                }
            }
        })
    };
    let mut whole = || {
        time(|| {
            for _ in 0..rounds {
                std::hint::black_box(verify.words(&input));
            }
        })
    };
    if let Some(side) = side {
        match side.as_str() {
            "rust" => measured_side(&mut rust),
            "verify" => measured_side(&mut whole),
            "decode" => measured_side(&mut || time(|| drop(decode.words(&input)))),
            _ => {}
        }
        return;
    }
    let (r, v) = abba("verify (with decode)", &mut rust, &mut whole);
    let d = (0..6)
        .map(|_| {
            time(|| {
                for _ in 0..rounds {
                    std::hint::black_box(decode.words(&input));
                }
            })
        })
        .min()
        .unwrap();
    let alone = v.saturating_sub(d);
    eprintln!(
        "verify alone: rust {r:?} vesper {alone:?} (decode {d:?}) ratio {:.2}",
        alone.as_secs_f64() / r.as_secs_f64()
    );
}
