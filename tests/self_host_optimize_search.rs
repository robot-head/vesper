//! Self-hosting SH3 lane 10 (#1249), part 2: the proof producer of
//! `src/optimize/certificate.rs` (`propose_rewrite`: folding, the budgeted
//! egg search, extraction and the proof forest) in Vesper
//! (`packages/vesper-optimize/src/search.ves`, on `@vesper/egraph`) against
//! the Rust producer.
//!
//! The test installs a recording producer at `src/optimize.rs`'s swap
//! point, so every expression the Rust optimizer visits reaches it: over
//! every checked corpus world (every candidate and error, and a fixed
//! sample of barriers and unchanged leaves, `OPTIMIZE_SAMPLE_STEP`), the
//! optimizer's own fixtures and a generated corpus of integer islands. For each one the Vesper producer must give the
//! same outcome (barrier, unchanged, the same error, or a candidate) and, for
//! a candidate, the same candidate, certificate and spent budget. The one
//! accepted difference is the e-graph's search order (`@vesper/egraph` visits
//! classes in id order, egg in hash order): the Vesper proposal may then
//! differ, and must still pass the Rust gate with a candidate of the same
//! size.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;
use vesper::ast::{Arg, Expr, ExprKind, Literal, Type, TypeMap};
use vesper::optimize::witness::{
    self, Equality, IntOp, Reason, RewriteCertificate, Spent, Substitution, Term, TermNode,
    WitnessError, WitnessLimits,
};
use vesper::optimize::{self, OptimizeTable, Produced, Proposal};

// ------------------------------------------------------------ recording

/// One expression the optimizer visited, and the Rust producer's answer.
struct Visit {
    source: Expr,
    types: TypeMap,
    limits: WitnessLimits,
    result: Result<Proposal, WitnessError>,
    /// `rust_text` of the answer, written before a clone drops the
    /// certificate's vector capacities.
    text: String,
}

thread_local! {
    /// This thread's visits since the last [`take_visits`]: each corpus
    /// worker records its own, so the merged order is the corpus order.
    static VISITS: std::cell::RefCell<Vec<Visit>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Whether this thread samples (the corpus), and its barrier and
    /// unchanged counters, reset per corpus world.
    static SAMPLING: std::cell::Cell<(bool, usize, usize)> = const { std::cell::Cell::new((false, 0, 0)) };
}

fn take_visits() -> Vec<Visit> {
    VISITS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

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

/// The entries of `types` the producer can look up for `source`.
fn relevant_types(source: &Expr, types: &TypeMap) -> TypeMap {
    let mut s = vec![];
    spans(source, &mut s);
    s.into_iter()
        .filter_map(|k| types.get(&k).map(|t| (k, t.clone())))
        .collect()
}

fn recording_propose(
    source: &Expr,
    types: &TypeMap,
    limits: WitnessLimits,
) -> Result<Proposal, WitnessError> {
    let result = optimize::propose_rust(source, types, limits);
    if !sampled(&result) {
        return result;
    }
    let visit = Visit {
        source: source.clone(),
        types: relevant_types(source, types),
        limits,
        text: rust_text(&result),
        result: result.clone(),
    };
    VISITS.with(|v| v.borrow_mut().push(visit));
    result
}

/// Every `n`-th barrier and unchanged visit of each corpus world
/// (`OPTIMIZE_SAMPLE_STEP`, default 64; 1 keeps all). The corpus makes about
/// 900k visits, nearly all barriers and unchanged leaves. Candidates and
/// errors are always kept, and fixtures and generated islands keep every
/// visit.
fn sampled(result: &Result<Proposal, WitnessError>) -> bool {
    static STEP: OnceLock<usize> = OnceLock::new();
    let step = *STEP.get_or_init(|| {
        std::env::var("OPTIMIZE_SAMPLE_STEP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64)
            .max(1)
    });
    let (on, mut barriers, mut unchanged) = SAMPLING.get();
    if !on {
        return true;
    }
    let seen = match result {
        Ok(Proposal::Barrier) => &mut barriers,
        Ok(Proposal::Unchanged) => &mut unchanged,
        _ => return true,
    };
    let keep = *seen % step == 0;
    *seen += 1;
    SAMPLING.set((on, barriers, unchanged));
    keep
}

fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        optimize::install_optimize(OptimizeTable {
            verify: witness::verify_spent,
            propose: Some(recording_propose),
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

/// Generated islands: `count` functions, deeper ones every fifth.
fn generated(seed: u64, count: usize, max_depth: u32) -> Vec<String> {
    let mut rng = Rng(seed);
    let mut out = vec![];
    for i in 0..count {
        let (kind, constants) = KINDS[i % 4];
        let checked = i % 3 != 0;
        let body = island(&mut rng, constants, checked, 1 + (i as u32 % max_depth));
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

/// The visits of optimizing `module`, if it checks.
fn optimize_module(mut module: vesper::ast::Module) -> Vec<Visit> {
    let checked = vesper::check::check(&module);
    if !checked.diagnostics.is_empty() {
        return vec![];
    }
    optimize::optimize(&mut module, &checked.expression_types);
    take_visits()
}

fn optimize_text(text: &str) -> Vec<Visit> {
    let parsed = vesper::parser::parse(text);
    if !parsed.diagnostics.is_empty() {
        return vec![];
    }
    optimize_module(parsed.module)
}

/// Every corpus world's visits, in corpus order (each world parsed and
/// checked once, on a few threads; see [`map_checked_corpus`]).
fn all_corpus_visits() -> Vec<Visit> {
    map_checked_corpus(|mut module, types| {
        take_visits();
        SAMPLING.set((true, 0, 0));
        optimize::optimize(&mut module, types);
        SAMPLING.set((false, 0, 0));
        take_visits()
    })
    .into_iter()
    .flatten()
    .collect()
}

/// Every visit of the corpus, the fixtures and the generated islands,
/// recorded once per test binary, with the three sources' counts.
fn all_visits() -> &'static (Vec<Visit>, [usize; 3]) {
    static ALL: OnceLock<(Vec<Visit>, [usize; 3])> = OnceLock::new();
    ALL.get_or_init(|| {
        install();
        let start = std::time::Instant::now();
        let mut all = if std::env::var_os("OPTIMIZE_SKIP_CORPUS").is_some() {
            vec![]
        } else {
            all_corpus_visits()
        };
        let corpus_count = all.len();
        all.extend(FIXTURES.iter().flat_map(|t| optimize_text(t)));
        let fixture_count = all.len() - corpus_count;
        all.extend(
            generated(0x9e37_79b9_7f4a_7c15, 400, 5)
                .iter()
                .chain(&generated(0x2545_f491_4f6c_dd1d, 200, 8))
                .flat_map(|t| optimize_text(t)),
        );
        let generated_count = all.len() - corpus_count - fixture_count;
        eprintln!("recorded {} visits in {:?}", all.len(), start.elapsed());
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

    /// Appends `e` children first; returns its input index (the input
    /// format of `read_exprs` in input.ves).
    fn expr(&mut self, e: &Expr) -> u64 {
        let mut w = vec![];
        let kind = match &e.kind {
            ExprKind::Literal(l) => {
                match l {
                    Literal::Int(v) => {
                        let m = v.unsigned_abs();
                        w.extend([1, (*v < 0) as u64, m as u64, (m >> 64) as u64]);
                    }
                    Literal::Float(_) => w.extend([2, 0, 0, 0]),
                    Literal::Bool(_) => w.extend([3, 0, 0, 0]),
                    Literal::String(_) => w.extend([4, 0, 0, 0]),
                    Literal::WideInt(_) => w.extend([5, 0, 0, 0]),
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

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

/// One case's input (`optimize_propose` in lib.ves), appended to `out`.
fn case_bytes(v: &Visit, out: &mut Vec<u8>) {
    let l = v.limits;
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
        0,
        0,
        0,
    ] {
        put(out, x as u64);
    }
    let mut w = Words::default();
    let source = w.expr(&v.source);
    put(out, w.names.len() as u64);
    for n in &w.names {
        put(out, n.len() as u64);
        out.extend(n.as_bytes());
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }
    let mut words = vec![v.types.len() as u64];
    for ((s, e), t) in &v.types {
        words.extend([*s as u64, *e as u64, type_code(t)]);
    }
    words.push(w.count);
    words.extend(&w.exprs);
    words.push(source);
    for x in words {
        put(out, x);
    }
}

fn encode_cases(cases: &[&Visit]) -> Vec<u8> {
    let mut out = vec![];
    put(&mut out, cases.len() as u64);
    for v in cases {
        case_bytes(v, &mut out);
    }
    out
}

// ------------------------------------------------------------ expected text

fn dump(e: &Expr, o: &mut String) {
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
                write!(o, " {}", pass_word(*pass)).unwrap();
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

fn op_word(op: IntOp) -> u64 {
    match op {
        IntOp::Add => 0,
        IntOp::Sub => 1,
        IntOp::Mul => 2,
    }
}

fn term_text(t: &Term, o: &mut String) {
    let (tag, op, value, a, b) = match t.node {
        TermNode::Const(v) => (0, 0, v, 0, 0),
        TermNode::InputVar(v) => (1, 0, 0, v, 0),
        TermNode::Checked { op, children } => (2, op_word(op), 0, children[0], children[1]),
        TermNode::Wrapping { op, children } => (3, op_word(op), 0, children[0], children[1]),
    };
    write!(o, " {}:{tag}:{op}:{value}:{a}:{b}", t.kind as u64).unwrap();
}

fn proof_text(p: &Equality, o: &mut String) {
    let f: [u64; 9] = match p.reason {
        Reason::Refl => [0; 9],
        Reason::Rule {
            rule,
            substitution,
            guard,
        } => [
            1,
            rule as u64,
            substitution.bindings[0] as u64,
            substitution.bindings[1] as u64,
            substitution.bindings[2] as u64,
            substitution.len as u64,
            guard.map_or(0, |g| g as u64 + 1),
            0,
            0,
        ],
        Reason::Congruence { children } => {
            [2, 0, 0, 0, 0, 0, 0, children[0] as u64, children[1] as u64]
        }
        Reason::Symmetry { proof } => [3, 0, 0, 0, 0, 0, 0, proof as u64, 0],
        Reason::Transitivity { first, second } => {
            [4, 0, 0, 0, 0, 0, 0, first as u64, second as u64]
        }
    };
    write!(o, " {}:{}", p.from, p.to).unwrap();
    for x in f {
        write!(o, ":{x}").unwrap();
    }
}

fn produced_text(p: &Produced, o: &mut String) {
    let c = &p.certificate;
    writeln!(
        o,
        "P C {} {} {} {} {} {} {} {}",
        p.spent.work,
        p.spent.metadata,
        p.spent.names,
        c.terms.capacity(),
        c.proofs.capacity(),
        c.input,
        c.output,
        c.conclusion
    )
    .unwrap();
    o.push_str("C ");
    dump(&p.candidate, o);
    o.push_str("\nT");
    for t in &c.terms {
        term_text(t, o);
    }
    o.push_str("\nQ");
    for q in &c.proofs {
        proof_text(q, o);
    }
    o.push('\n');
}

fn rust_text(r: &Result<Proposal, WitnessError>) -> String {
    let mut o = String::new();
    match r {
        Err(e) => writeln!(o, "P E {}", *e as u64 + 1).unwrap(),
        Ok(Proposal::Barrier) => o.push_str("P B\n"),
        Ok(Proposal::Unchanged) => o.push_str("P U\n"),
        Ok(Proposal::Candidate(p)) => produced_text(p, &mut o),
    }
    o
}

// ------------------------------------------------------------ vesper side

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-optimize"))
}

fn propose_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("optimize_propose", true))
}

/// The Vesper producer's text for each case (one `P` line, and for a
/// candidate its `C`, `T` and `Q` lines).
fn vesper_texts(entry: &Entry, cases: &[&Visit]) -> Vec<String> {
    let mut out = vec![];
    for chunk in cases.chunks(128) {
        let text = entry.text(&encode_cases(chunk));
        let mut current = String::new();
        for line in text.lines() {
            if line.starts_with("P ") && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            current.push_str(line);
            current.push('\n');
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

/// Decodes a Vesper candidate proposal's certificate.
fn parse_certificate(text: &str) -> (RewriteCertificate, Spent) {
    let mut lines = text.lines();
    let p: Vec<u64> = lines.next().unwrap()[4..]
        .split(' ')
        .map(|x| x.parse().unwrap())
        .collect();
    let _candidate = lines.next().unwrap();
    let terms_line = lines.next().unwrap();
    let proofs_line = lines.next().unwrap();
    let kind_of = |k: u64| match k {
        0 => vesper::numeric::IntegerKind::U8,
        1 => vesper::numeric::IntegerKind::U32,
        2 => vesper::numeric::IntegerKind::U64,
        _ => vesper::numeric::IntegerKind::I64,
    };
    let op_of = |k: u64| match k {
        0 => IntOp::Add,
        1 => IntOp::Sub,
        _ => IntOp::Mul,
    };
    let mut terms = vec![];
    for t in terms_line[1..].split_whitespace() {
        let f: Vec<&str> = t.split(':').collect();
        let n = |i: usize| f[i].parse::<u64>().unwrap();
        let node = match n(1) {
            0 => TermNode::Const(f[3].parse::<i128>().unwrap()),
            1 => TermNode::InputVar(n(4) as u32),
            2 => TermNode::Checked {
                op: op_of(n(2)),
                children: [n(4) as u32, n(5) as u32],
            },
            _ => TermNode::Wrapping {
                op: op_of(n(2)),
                children: [n(4) as u32, n(5) as u32],
            },
        };
        terms.push(Term {
            kind: kind_of(n(0)),
            node,
        });
    }
    let mut proofs = vec![];
    for q in proofs_line[1..].split_whitespace() {
        let f: Vec<u64> = q.split(':').map(|x| x.parse().unwrap()).collect();
        let reason = match f[2] {
            0 => Reason::Refl,
            1 => Reason::Rule {
                rule: f[3] as u16,
                substitution: Substitution {
                    bindings: [f[4] as u32, f[5] as u32, f[6] as u32],
                    len: f[7] as u8,
                },
                guard: (f[8] != 0).then(|| (f[8] - 1) as u32),
            },
            2 => Reason::Congruence {
                children: [f[9] as u32, f[10] as u32],
            },
            3 => Reason::Symmetry { proof: f[9] as u32 },
            _ => Reason::Transitivity {
                first: f[9] as u32,
                second: f[10] as u32,
            },
        };
        proofs.push(Equality {
            from: f[0] as u32,
            to: f[1] as u32,
            reason,
        });
    }
    let cert = RewriteCertificate {
        schema_version: 1,
        arithmetic_version: 1,
        terms,
        proofs,
        input: p[5] as u32,
        output: p[6] as u32,
        conclusion: p[7] as u32,
    };
    let spent = Spent {
        work: p[0] as usize,
        metadata: p[1] as usize,
        names: p[2] as usize,
    };
    (cert, spent)
}

/// The number of AST nodes of a candidate dump (`AstSize` of its term).
fn dump_size(line: &str) -> usize {
    line.matches('@').count()
}

// ------------------------------------------------------------ tests

#[derive(Default)]
struct Tally {
    exact: usize,
    tie: usize,
    by_outcome: BTreeMap<String, usize>,
}

#[test]
fn every_visit_is_proposed_like_rust() {
    let (all, counts) = all_visits();
    eprintln!(
        "visits: {} corpus, {} fixtures, {} generated",
        counts[0], counts[1], counts[2]
    );
    let cases: Vec<&Visit> = all.iter().collect();
    let start = std::time::Instant::now();
    let texts = vesper_texts(propose_entry(), &cases);
    eprintln!(
        "vesper producer over {} visits: {:?}",
        cases.len(),
        start.elapsed()
    );
    assert_eq!(texts.len(), cases.len());
    let mut tally = Tally::default();
    let mut differences = 0;
    for (v, got) in cases.iter().zip(&texts) {
        let want = v.text.clone();
        let key = want.split('\n').next().unwrap()[..3].to_owned();
        *tally.by_outcome.entry(key).or_default() += 1;
        if *got == want {
            tally.exact += 1;
            continue;
        }
        // Another search order: both candidates, the Vesper one passes the
        // Rust gate on the spent budget it reports, and it is as small.
        let accepted = match &v.result {
            Ok(Proposal::Candidate(p)) if got.starts_with("P C ") => {
                let (cert, mut spent) = parse_certificate(got);
                let rust_size = dump_size(want.lines().nth(1).unwrap());
                let vesper_size = dump_size(got.lines().nth(1).unwrap());
                let candidate = vesper_candidate(v, &cert);
                let gate = candidate.as_ref().map(|c| {
                    witness::verify_spent(&v.source, c, &v.types, &cert, p.limits, &mut spent)
                });
                let same_dump = candidate.as_ref().is_some_and(|c| {
                    let mut s = String::from("C ");
                    dump(c, &mut s);
                    got.lines().nth(1) == Some(s.as_str())
                });
                matches!(gate, Some(Ok(()))) && same_dump && vesper_size == rust_size
            }
            // Egg extracted another term of the source's own size; the id
            // order keeps the source.
            Ok(Proposal::Candidate(_)) if got == "P U\n" => {
                let mut s = String::new();
                dump(&v.source, &mut s);
                dump_size(want.lines().nth(1).unwrap()) == dump_size(&s)
            }
            _ => false,
        };
        if accepted {
            tally.tie += 1;
            if tally.tie <= 3 {
                eprintln!("tie\n  rust:   {want}  vesper: {got}");
            }
            continue;
        }
        differences += 1;
        if differences <= 5 {
            let mut s = String::new();
            dump(&v.source, &mut s);
            eprintln!("difference at {s}\n  rust:   {want}  vesper: {got}");
        }
    }
    eprintln!(
        "exact {}, other search order {}, differences {differences}; outcomes {:?}",
        tally.exact, tally.tie, tally.by_outcome
    );
    assert_eq!(differences, 0);
    // Every outcome occurs.
    for key in ["P B", "P U", "P C", "P E"] {
        assert!(tally.by_outcome.contains_key(key), "no {key} case");
    }
}

/// The Vesper candidate, rebuilt from its certificate by the Rust builder
/// (part 1 shows the Vesper builder gives the same expression).
fn vesper_candidate(v: &Visit, cert: &RewriteCertificate) -> Option<Expr> {
    let mut limits = v.limits;
    limits.max_work = WitnessLimits::HARD.max_work;
    witness::rebuild_candidate(&v.source, &v.types, cert, limits).ok()
}

#[test]
fn unoptimized_agrees() {
    let (all, _) = all_visits();
    let cases: Vec<&Visit> = all.iter().step_by(7).collect();
    let optimized = vesper_texts(propose_entry(), &cases);
    let plain = package().entry("optimize_propose", false);
    assert_eq!(vesper_texts(&plain, &cases), optimized);
}

/// One side of the speed test, never inlined so that callgrind can collect
/// it alone (`--toggle-collect='*measured_side*'`).
#[inline(never)]
fn measured_side(f: &mut dyn FnMut() -> std::time::Duration) {
    std::hint::black_box(f());
}

/// ABBA over every recorded visit: Rust's producer (`propose_rust`) against
/// the Vesper producer, whole (with its input decode) and alone (decode
/// subtracted). The Vesper side writes one `P` per case, so neither side
/// prints its proposal. `CHECK_HARNESS_SPEED=1`; `OPTIMIZE_SIDE=rust|
/// propose|decode|none` runs one side once, for callgrind.
#[test]
#[ignore]
fn speed_producer() {
    if !speed() {
        return;
    }
    let (all, _) = all_visits();
    let cases: Vec<&Visit> = all.iter().collect();
    let input = encode_cases(&cases);
    let quiet = package().entry("optimize_propose_quiet", true);
    let decode = package().entry("optimize_propose_decode", true);
    let side = std::env::var("OPTIMIZE_SIDE").ok();
    let rounds = if side.is_some() { 1 } else { 5 };
    let mut rust = || {
        time(|| {
            for _ in 0..rounds {
                for v in &cases {
                    let _ =
                        std::hint::black_box(optimize::propose_rust(&v.source, &v.types, v.limits));
                }
            }
        })
    };
    let mut whole = || {
        time(|| {
            for _ in 0..rounds {
                std::hint::black_box(quiet.words(&input));
            }
        })
    };
    if let Some(side) = side {
        match side.as_str() {
            "rust" => measured_side(&mut rust),
            "propose" => measured_side(&mut whole),
            "decode" => measured_side(&mut || time(|| drop(decode.words(&input)))),
            _ => {}
        }
        return;
    }
    let (r, v) = abba("propose (with decode)", &mut rust, &mut whole);
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
        "propose alone: rust {r:?} vesper {alone:?} (decode {d:?}) ratio {:.2}",
        alone.as_secs_f64() / r.as_secs_f64()
    );
}
