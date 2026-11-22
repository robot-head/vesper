//! Self-hosting SH3 lane 8 (#1247): `@vesper/egraph` (packages/vesper-egraph)
//! against the egg and egglog runs it replaces.
//!
//! - `math_matches_egg`: `src/optimize.rs`'s 18 rules, run as
//!   `optimize/certificate.rs`'s `run_graph` configures egg (iteration and
//!   node limits, the match cap per rule, the recording applier's caps and
//!   the root-leaf hook, without the witness budget), on every pure integer
//!   island of the corpus and on generated expressions. Each case compares
//!   the stop reason, the iterations, the graph's sizes, a digest of every
//!   id's original e-node, the application records and the extracted term.
//!   Extraction ties can differ (egg visits classes in hash-map order, the
//!   port in id order): those are listed with their costs, and each must be
//!   an equal-cost term that evaluates like egg's.
//! - `memory_matches_egglog`: `src/ir_opt.rs`'s egglog program, built the
//!   way `ir_opt::optimize` builds it, on every block of every corpus
//!   program's IR and one mutant of each (`CHECK_HARNESS_MUTANTS` for
//!   more); the read each candidate forwards to
//!   must be egglog's.
//! - `unoptimized_agrees`: the JIT with the optimizer off gives the same
//!   words.
//! - `speed_math` and `speed_memory` (ignored; `CHECK_HARNESS_SPEED=1`):
//!   ABBA against egg and egglog.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use egg::{
    Applier, Id, PatternAst, RecExpr, Rewrite, RewriteScheduler, Runner, SearchMatches, StopReason,
    Subst, Symbol, Var, define_language, rewrite,
};
use egglog::Read;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use vesper::ast::{Expr, ExprKind, Literal, Pass, PatternKind, Statement, Type};

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-egraph"))
}

fn words_to_bytes(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

// ----------------------------------------------------------------- egg side

define_language! {enum Math {
    Num(i64),
    "+"=Add([Id;2]), "-"=Sub([Id;2]), "*"=Mul([Id;2]),
    "wrapping_add"=WAdd([Id;2]), "wrapping_sub"=WSub([Id;2]), "wrapping_mul"=WMul([Id;2]),
    Input(u32),
}}
type Graph = egg::EGraph<Math, ()>;

// The constants of `optimize/certificate.rs`.
const ITERATIONS: u64 = 8;
const RAW_NODES: u64 = 2048;
const MATCHES: u64 = 128;
const RECORDS: u64 = 4096;
const APPLICATION_NODES: u64 = 8;

fn total(var: &str) -> impl Fn(&mut Graph, Id, &Subst) -> bool + use<> {
    let var: Var = var.parse().unwrap();
    move |g, _, subst| {
        g[subst[var]]
            .nodes
            .iter()
            .any(|n| matches!(n, Math::Num(_) | Math::Input(_)))
    }
}

/// `optimize::rules()`, verbatim.
fn rules() -> Vec<Rewrite<Math, ()>> {
    vec![
        rewrite!("add-zero";"(+ ?x 0)"=>"?x"),
        rewrite!("zero-add";"(+ 0 ?x)"=>"?x"),
        rewrite!("sub-zero";"(- ?x 0)"=>"?x"),
        rewrite!("mul-one";"(* ?x 1)"=>"?x"),
        rewrite!("one-mul";"(* 1 ?x)"=>"?x"),
        rewrite!("mul-zero";"(* ?x 0)"=>"0" if total("?x")),
        rewrite!("zero-mul";"(* 0 ?x)"=>"0" if total("?x")),
        rewrite!("self-sub";"(- ?x ?x)"=>"0" if total("?x")),
        rewrite!("wadd-zero";"(wrapping_add ?x 0)"=>"?x"),
        rewrite!("wsub-zero";"(wrapping_sub ?x 0)"=>"?x"),
        rewrite!("wmul-one";"(wrapping_mul ?x 1)"=>"?x"),
        rewrite!("wadd-comm";"(wrapping_add ?x ?y)"=>"(wrapping_add ?y ?x)"),
        rewrite!("wmul-comm";"(wrapping_mul ?x ?y)"=>"(wrapping_mul ?y ?x)"),
        rewrite!("wadd-assoc";"(wrapping_add (wrapping_add ?x ?y) ?z)"=>"(wrapping_add ?x (wrapping_add ?y ?z))"),
        rewrite!("wmul-assoc";"(wrapping_mul (wrapping_mul ?x ?y) ?z)"=>"(wrapping_mul ?x (wrapping_mul ?y ?z))"),
        rewrite!("wadd-cancel";"(wrapping_sub (wrapping_add ?x ?y) ?y)"=>"?x" if total("?y")),
        rewrite!("wmul-zero";"(wrapping_mul ?x 0)"=>"0" if total("?x")),
        rewrite!("wself-sub";"(wrapping_sub ?x ?x)"=>"0" if total("?x")),
    ]
}

#[derive(Default)]
struct Capture {
    failed: bool,
    records: Vec<u64>,
    count: u64,
}

/// certificate.rs's `RecordingApplier` without the witness budget.
struct Recording {
    inner: Arc<dyn Applier<Math, ()> + Send + Sync>,
    rule: u64,
    vars: Vec<Var>,
    capture: Arc<Mutex<Capture>>,
}

impl Applier<Math, ()> for Recording {
    fn get_pattern_ast(&self) -> Option<&PatternAst<Math>> {
        self.inner.get_pattern_ast()
    }
    fn vars(&self) -> Vec<Var> {
        self.inner.vars()
    }
    fn apply_one(
        &self,
        graph: &mut Graph,
        class: Id,
        subst: &Subst,
        pattern: Option<&PatternAst<Math>>,
        name: Symbol,
    ) -> Vec<Id> {
        let mut state = self.capture.lock().unwrap();
        if state.failed
            || graph.nodes().len() as u64 + APPLICATION_NODES > RAW_NODES
            || state.count >= RECORDS
        {
            state.failed = true;
            return vec![];
        }
        let bindings: Vec<u64> = self
            .vars
            .iter()
            .map(|v| usize::from(subst[*v]) as u64)
            .collect();
        drop(state);
        let result = self.inner.apply_one(graph, class, subst, pattern, name);
        if !result.is_empty() {
            let mut state = self.capture.lock().unwrap();
            state.records.push(self.rule);
            state.records.push(bindings.len() as u64);
            state.records.extend(bindings);
            state.count += 1;
        }
        result
    }
}

/// certificate.rs's `BoundedScheduler` without the witness budget.
struct Bounded {
    capture: Arc<Mutex<Capture>>,
}

impl RewriteScheduler<Math, ()> for Bounded {
    fn search_rewrite<'a>(
        &mut self,
        _: usize,
        graph: &Graph,
        rewrite: &'a Rewrite<Math, ()>,
    ) -> Vec<SearchMatches<'a, Math>> {
        let matches = rewrite.search_with_limit(graph, MATCHES as usize);
        let count: usize = matches.iter().map(|m| m.substs.len()).sum();
        if count as u64 >= MATCHES {
            self.capture.lock().unwrap().failed = true;
            vec![]
        } else {
            matches
        }
    }
}

/// One expression of `optimize`'s language: the integer kind's range (for
/// the root-leaf hook), the variable count and the nodes.
#[derive(Clone, Debug)]
struct MathCase {
    name: String,
    lo: i64,
    hi: i64,
    vars: u32,
    nodes: Vec<Math>,
}

const MINIMUM: &str = "minimum root complete";

fn node_words(n: &Math) -> [u64; 4] {
    let c = |ids: &[Id; 2]| [usize::from(ids[0]) as u64, usize::from(ids[1]) as u64];
    match n {
        Math::Num(v) => [0, *v as u64, 0, 0],
        Math::Add(ids) => [1, 0, c(ids)[0], c(ids)[1]],
        Math::Sub(ids) => [2, 0, c(ids)[0], c(ids)[1]],
        Math::Mul(ids) => [3, 0, c(ids)[0], c(ids)[1]],
        Math::WAdd(ids) => [4, 0, c(ids)[0], c(ids)[1]],
        Math::WSub(ids) => [5, 0, c(ids)[0], c(ids)[1]],
        Math::WMul(ids) => [6, 0, c(ids)[0], c(ids)[1]],
        Math::Input(v) => [7, *v as u64, 0, 0],
    }
}

fn digest_nodes(nodes: &[Math]) -> u64 {
    let mut h = SEED;
    for n in nodes {
        for x in node_words(n) {
            h = fold(h, x);
        }
    }
    h
}

/// egg on one case, as the words `egraph_math` returns.
fn egg_case(case: &MathCase) -> Vec<u64> {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let names: [Var; 3] = [
        "?x".parse().unwrap(),
        "?y".parse().unwrap(),
        "?z".parse().unwrap(),
    ];
    let mut actual = rules();
    for (index, rule) in actual.iter_mut().enumerate() {
        let present = rule.searcher.vars();
        let vars = names
            .iter()
            .copied()
            .filter(|v| present.contains(v))
            .collect();
        rule.applier = Arc::new(Recording {
            inner: rule.applier.clone(),
            rule: index as u64,
            vars,
            capture: capture.clone(),
        });
    }
    let expr = RecExpr::from(case.nodes.clone());
    let hook_capture = capture.clone();
    let (lo, hi, vars) = (case.lo, case.hi, case.vars);
    let runner = Runner::default()
        .with_iter_limit(ITERATIONS as usize)
        .with_node_limit(RAW_NODES as usize)
        .with_scheduler(Bounded {
            capture: capture.clone(),
        })
        .with_hook(move |runner| {
            if hook_capture.lock().unwrap().failed {
                return Err("failed".to_owned());
            }
            let root = runner.roots[0];
            if runner.egraph[root].nodes.iter().any(|n| match n {
                Math::Num(v) => *v >= lo && *v <= hi,
                Math::Input(i) => *i < vars,
                _ => false,
            }) {
                Err(MINIMUM.to_owned())
            } else {
                Ok(())
            }
        })
        .with_expr(&expr)
        .run(&actual);
    let state = capture.lock().unwrap();
    let stop = if state.failed {
        5
    } else {
        match &runner.stop_reason {
            Some(StopReason::Saturated) => 1,
            Some(StopReason::IterationLimit(_)) => 2,
            Some(StopReason::NodeLimit(_)) => 3,
            Some(StopReason::Other(m)) if m == MINIMUM => 4,
            _ => 6,
        }
    };
    let g = &runner.egraph;
    let mut out = vec![
        stop,
        runner.iterations.len() as u64,
        g.nodes().len() as u64,
        g.total_size() as u64,
        g.number_of_classes() as u64,
        digest_nodes(g.nodes()),
        state.records.len() as u64,
    ];
    out.extend(&state.records);
    if stop == 5 {
        out.extend([0, 0]);
    } else {
        let extractor = egg::Extractor::new(g, egg::AstSize);
        let root = runner.roots[0];
        let (cost, best) = extractor.find_best(root);
        out.push(cost as u64);
        out.push(best.as_ref().len() as u64);
        for n in best.as_ref() {
            out.extend(node_words(n));
        }
    }
    out
}

fn encode_math(cases: &[MathCase]) -> Vec<u8> {
    let mut w = vec![
        cases.len() as u64,
        ITERATIONS,
        RAW_NODES,
        MATCHES,
        RAW_NODES,
        APPLICATION_NODES,
        RECORDS,
    ];
    for c in cases {
        w.extend([
            c.lo as u64,
            c.hi as u64,
            c.vars as u64,
            c.nodes.len() as u64,
        ]);
        for n in &c.nodes {
            w.extend(node_words(n));
        }
    }
    words_to_bytes(&w)
}

/// Splits `egraph_math`'s output into one word list per case.
fn split_math(words: &[u64], cases: usize) -> Vec<Vec<u64>> {
    let mut out = vec![];
    let mut i = 0;
    for _ in 0..cases {
        let start = i;
        i += 6;
        let rec = words[i] as usize;
        i += 1 + rec;
        if words[start] == 5 {
            i += 2;
        } else {
            i += 1;
            let n = words[i] as usize;
            i += 1 + 4 * n;
        }
        out.push(words[start..i].to_vec());
    }
    assert_eq!(i, words.len());
    out
}

// ------------------------------------------------------------ math inputs

/// `optimize`'s language for one expression, if it is a pure integer island
/// (`+ - *`, the wrapping intrinsics, integer literals and names).
fn to_math(e: &Expr, vars: &mut Vec<String>, nodes: &mut Vec<Math>) -> Option<(Id, bool)> {
    let push = |nodes: &mut Vec<Math>, n: Math| {
        nodes.push(n);
        Id::from(nodes.len() - 1)
    };
    match &e.kind {
        ExprKind::Literal(Literal::Int(v)) => {
            let v = i64::try_from(*v).ok()?;
            Some((push(nodes, Math::Num(v)), false))
        }
        ExprKind::Name(n) => {
            let i = match vars.iter().position(|x| x == n) {
                Some(i) => i,
                None => {
                    vars.push(n.clone());
                    vars.len() - 1
                }
            };
            Some((push(nodes, Math::Input(i as u32)), false))
        }
        ExprKind::Binary(op, a, b) if matches!(op.as_str(), "+" | "-" | "*") => {
            let (x, _) = to_math(a, vars, nodes)?;
            let (y, _) = to_math(b, vars, nodes)?;
            let n = match op.as_str() {
                "+" => Math::Add([x, y]),
                "-" => Math::Sub([x, y]),
                _ => Math::Mul([x, y]),
            };
            Some((push(nodes, n), true))
        }
        ExprKind::Call { callee, args, .. } if args.len() == 2 => {
            let ExprKind::Name(f) = &callee.kind else {
                return None;
            };
            if !matches!(f.as_str(), "wrapping_add" | "wrapping_sub" | "wrapping_mul") {
                return None;
            }
            let (x, _) = to_math(&args[0].expr, vars, nodes)?;
            let (y, _) = to_math(&args[1].expr, vars, nodes)?;
            let n = match f.as_str() {
                "wrapping_add" => Math::WAdd([x, y]),
                "wrapping_sub" => Math::WSub([x, y]),
                _ => Math::WMul([x, y]),
            };
            Some((push(nodes, n), true))
        }
        _ => None,
    }
}

fn children(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Tuple(es) => es.iter().collect(),
        ExprKind::Struct { fields, .. } => fields.iter().map(|(_, e)| e).collect(),
        ExprKind::Block { statements, tail } => {
            let mut v: Vec<&Expr> = statements
                .iter()
                .map(|s| match s {
                    Statement::Let { value, .. } => value,
                    Statement::Expr(e) => e,
                })
                .collect();
            v.extend(tail.as_deref());
            v
        }
        ExprKind::Call { callee, args, .. } => {
            let mut v = vec![&**callee];
            v.extend(args.iter().map(|a| &a.expr));
            v
        }
        ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => vec![e],
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            vec![a, b]
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            let mut v = vec![&**condition, &**then];
            v.extend(otherwise.as_deref());
            v
        }
        ExprKind::Match { value, arms } => {
            let mut v = vec![&**value];
            v.extend(arms.iter().map(|(_, e)| e));
            v
        }
        ExprKind::Closure { body, .. } => vec![body],
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            let mut v = vec![];
            if let Some((_, e)) = state {
                v.push(&**e);
            }
            v.push(&**body);
            v.push(&ret.body);
            v.extend(ops.iter().map(|c| &c.body));
            v
        }
        k => k.control_children().unwrap_or_default(),
    }
}

fn islands(e: &Expr, name: &str, out: &mut Vec<MathCase>) {
    let mut vars = vec![];
    let mut nodes = vec![];
    if let Some((_, true)) = to_math(e, &mut vars, &mut nodes) {
        out.push(MathCase {
            name: format!("{name}@{}", e.span.start),
            lo: i64::MIN,
            hi: i64::MAX,
            vars: vars.len() as u32,
            nodes,
        });
        return;
    }
    for c in children(e) {
        islands(c, name, out);
    }
}

/// Every pure integer island of the corpus, in corpus order.
fn corpus_math() -> Vec<MathCase> {
    let mut out = vec![];
    for c in corpus() {
        for item in &c.module.items {
            let bodies: Vec<&Expr> = match item {
                vesper::ast::Item::Function(f) => f.body.iter().collect(),
                vesper::ast::Item::Impl { methods, .. } => {
                    methods.iter().filter_map(|f| f.body.as_ref()).collect()
                }
                _ => vec![],
            };
            for b in bodies {
                islands(b, &c.name, &mut out);
            }
        }
    }
    out
}

/// A small deterministic generator (SplitMix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn gen_expr(r: &mut Rng, depth: u32, vars: u32, wrapping: bool, nodes: &mut Vec<Math>) -> Id {
    if depth == 0 || r.below(4) == 0 {
        let n = if r.below(3) == 0 {
            Math::Num([0, 1, 0, 2, -1, 5][r.below(6) as usize])
        } else {
            Math::Input(r.below(vars as u64) as u32)
        };
        nodes.push(n);
        return Id::from(nodes.len() - 1);
    }
    let a = gen_expr(r, depth - 1, vars, wrapping, nodes);
    let b = gen_expr(r, depth - 1, vars, wrapping, nodes);
    let op = if wrapping && r.below(4) != 0 {
        3 + r.below(3)
    } else {
        r.below(3)
    };
    let n = match op {
        0 => Math::Add([a, b]),
        1 => Math::Sub([a, b]),
        2 => Math::Mul([a, b]),
        3 => Math::WAdd([a, b]),
        4 => Math::WSub([a, b]),
        _ => Math::WMul([a, b]),
    };
    nodes.push(n);
    Id::from(nodes.len() - 1)
}

/// The kinds the hook ranges come from: u8, i32, u64 (as i64 data), i64.
const KINDS: [(i64, i64); 4] = [
    (0, 255),
    (i32::MIN as i64, i32::MAX as i64),
    (0, i64::MAX),
    (i64::MIN, i64::MAX),
];

fn generated_math(count: usize) -> Vec<MathCase> {
    let mut r = Rng(0x5eed_e9a4);
    (0..count)
        .map(|i| {
            let vars = 1 + r.below(4) as u32;
            let depth = 1 + r.below(5) as u32;
            let wrapping = r.below(3) != 0;
            let mut nodes = vec![];
            gen_expr(&mut r, depth, vars, wrapping, &mut nodes);
            let (lo, hi) = KINDS[r.below(4) as usize];
            MathCase {
                name: format!("generated#{i}"),
                lo,
                hi,
                vars,
                nodes,
            }
        })
        .collect()
}

/// Long wrapping sums and products: commutativity and associativity fill
/// the graph until a limit stops them.
fn chains() -> Vec<MathCase> {
    let mut out = vec![];
    for (op, len) in [
        (4u8, 3usize),
        (4, 4),
        (4, 5),
        (4, 6),
        (6, 4),
        (6, 7),
        (5, 5),
    ] {
        let mut nodes = vec![Math::Input(0)];
        for i in 1..len {
            nodes.push(Math::Input(i as u32));
            let a = Id::from(nodes.len() - 2);
            let a = if i == 1 { Id::from(0) } else { a };
            let b = Id::from(nodes.len() - 1);
            let n = match op {
                4 => Math::WAdd([a, b]),
                5 => Math::WSub([a, b]),
                _ => Math::WMul([a, b]),
            };
            nodes.push(n);
        }
        out.push(MathCase {
            name: format!("chain op{op} x{len}"),
            lo: i64::MIN,
            hi: i64::MAX,
            vars: len as u32,
            nodes,
        });
    }
    out
}

// ------------------------------------------------------------ evaluation

/// The value of an extracted term (nodes as `egraph_math` returns them) on
/// one assignment: `None` is the checked overflow failure.
fn eval(nodes: &[[u64; 4]], inputs: &[i64]) -> Option<i64> {
    let mut v: Vec<Option<i64>> = vec![];
    for n in nodes {
        let a = |i: u64| v[i as usize];
        let x = match n[0] {
            0 => Some(n[1] as i64),
            7 => Some(inputs[n[1] as usize]),
            op => {
                let (p, q) = (a(n[2]), a(n[3]));
                match (p, q) {
                    (Some(p), Some(q)) => match op {
                        1 => p.checked_add(q),
                        2 => p.checked_sub(q),
                        3 => p.checked_mul(q),
                        4 => Some(p.wrapping_add(q)),
                        5 => Some(p.wrapping_sub(q)),
                        _ => Some(p.wrapping_mul(q)),
                    },
                    _ => None,
                }
            }
        };
        v.push(x);
    }
    *v.last().unwrap()
}

fn term_nodes(words: &[u64]) -> Vec<[u64; 4]> {
    let rec = words[6] as usize;
    let at = 7 + rec + 1;
    let n = words[at] as usize;
    (0..n)
        .map(|k| {
            let b = at + 1 + 4 * k;
            [words[b], words[b + 1], words[b + 2], words[b + 3]]
        })
        .collect()
}

fn same_values(case: &MathCase, a: &[[u64; 4]], b: &[[u64; 4]]) -> bool {
    let mut r = Rng(case.nodes.len() as u64);
    let interesting = [0, 1, -1, 2, i64::MAX, i64::MIN, 1 << 32];
    (0..64).all(|t| {
        let inputs: Vec<i64> = (0..case.vars.max(1))
            .map(|_| {
                if t % 2 == 0 {
                    interesting[r.below(interesting.len() as u64) as usize]
                } else {
                    r.next() as i64 >> r.below(60)
                }
            })
            .collect();
        eval(a, &inputs) == eval(b, &inputs)
    })
}

/// Compares egg's and the port's words over `cases`: returns (exact,
/// equal-cost term differences, other differences).
fn compare_math(label: &str, cases: &[MathCase]) -> (usize, usize, usize) {
    let words = package()
        .entry("egraph_math", true)
        .words(&encode_math(cases));
    let ours = split_math(&words, cases.len());
    let (mut exact, mut order, mut ties, mut other, mut sizes, mut refused) = (0, 0, 0, 0, 0, 0);
    let mut kinds = BTreeMap::new();
    for (c, v) in cases.iter().zip(&ours) {
        let e = egg_case(c);
        if e == *v {
            exact += 1;
            continue;
        }
        // Same stop and cost: the class visit order (egg's hash order, the
        // port's id order) changed which match was applied first (and so,
        // sometimes, how many duplicate e-nodes a stale hash-cons made) or
        // which equal-cost e-node was kept. The terms must agree on every
        // input.
        let rec_e = e[6] as usize;
        let rec_v = v[6] as usize;
        let cost_e = e.get(7 + rec_e);
        let cost_v = v.get(7 + rec_v);
        // Both refused (a cap failed the run): certificate.rs installs
        // nothing either way; the records before the failure differ in order.
        if e[0] == 5 && v[0] == 5 {
            refused += 1;
            continue;
        }
        if e[0] == v[0] && cost_e == cost_v {
            if e[1..5] != v[1..5] || rec_e != rec_v {
                sizes += 1;
            }
            let (te, tv) = (term_nodes(&e), term_nodes(v));
            assert!(
                same_values(c, &te, &tv),
                "{label}: {} extracts a term that differs in value\n egg:    {te:?}\n vesper: {tv:?}",
                c.name
            );
            if te == tv {
                order += 1;
            } else {
                ties += 1;
                if ties <= 5 {
                    eprintln!(
                        "{label}: {} equal-cost tie (cost {:?})\n  egg:    {te:?}\n  vesper: {tv:?}",
                        c.name, cost_e
                    );
                }
            }
        } else {
            other += 1;
            let key = (e[0], v[0], cost_e.copied(), cost_v.copied());
            *kinds.entry(key).or_insert(0usize) += 1;
            if other <= 5 {
                eprintln!(
                    "{label}: {} differs\n  case:   {:?}\n  egg:    {e:?}\n  vesper: {v:?}",
                    c.name, c.nodes
                );
            }
        }
    }
    eprintln!(
        "{label}: {} cases, {exact} exact, {refused} refused by both after another search order, {order} same term in another search order, {ties} equal-cost ties ({sizes} of these with other graph sizes), {other} other differences",
        cases.len()
    );
    if !kinds.is_empty() {
        eprintln!(
            "{label}: other differences by (egg stop, port stop, egg cost, port cost): {kinds:?}"
        );
    }
    (exact, ties, other)
}

#[test]
fn math_matches_egg() {
    let corpus = corpus_math();
    assert!(!corpus.is_empty());
    let (_, _, other) = compare_math("corpus", &corpus);
    assert_eq!(other, 0);
    let generated = generated_math(3000);
    let (_, _, other) = compare_math("generated", &generated);
    assert_eq!(other, 0);
    let (_, _, other) = compare_math("chains", &chains());
    assert_eq!(other, 0);
}

// ----------------------------------------------------------------- ir_opt

/// ir_opt's egglog `RULES`, verbatim.
const MEMORY_RULES: &str = r#"(sort V)
(sort S)
(constructor Ref (i64) V)
(constructor Start (i64) S)
(constructor Write (S i64 V) S)
(constructor Barrier (S i64) S)
(constructor Read (S i64) V)
(relation disjoint (i64 i64))
(rewrite (Read (Write s p v) p) v)
(rule ((= r (Read (Write s p v) q)) (disjoint p q))
      ((union r (Read s q))))
"#;
const ROUNDS: u64 = 12;

fn scalar(t: &Type) -> bool {
    (vesper::numeric::is_scalar_name(t.name()) && t.name() != "Never") || *t == Type::unit()
}

fn place(root: &str, path: &[String]) -> vesper::check::Place {
    vesper::check::Place {
        root: root.into(),
        path: path.to_vec(),
    }
}

/// One block's memory observations, as ir_opt builds them.
struct MemoryBlock {
    name: String,
    code: String,
    words: Vec<u64>,
    queries: Vec<(u32, Vec<u32>)>,
}

/// The blocks `ir_opt::optimize` runs egglog on, with its program text and
/// the port's input words (a transcription of the loop in src/ir_opt.rs).
fn memory_blocks(name: &str, program: &vesper::ir::Program) -> Vec<MemoryBlock> {
    use vesper::ir::Op;
    let mut out = vec![];
    for f in program.functions.values() {
        for (bi, block) in f.blocks.iter().enumerate() {
            let mut places = BTreeMap::<vesper::check::Place, i64>::new();
            for i in &block.instructions {
                let p = match &i.op {
                    Op::Read { root, path, .. } if f.locals.contains_key(root) => {
                        Some(place(root, path))
                    }
                    Op::Write { place: p, .. } => {
                        let mut path = p.split('.');
                        Some(place(
                            path.next().unwrap(),
                            &path.map(str::to_owned).collect::<Vec<_>>(),
                        ))
                    }
                    Op::Bind { pattern, .. } => {
                        if let PatternKind::Bind(n) = &pattern.kind {
                            Some(place(n, &[]))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(p) = p {
                    let next = places.len() as i64;
                    places.entry(p).or_insert(next);
                }
            }
            let mut code = MEMORY_RULES.to_owned();
            let mut words = vec![];
            let mut graph = vesper::ir::PlaceGraph::default();
            for p in places.keys() {
                graph.insert(p);
            }
            let mut pairs = vec![];
            for (p, a) in &places {
                for (q, b) in &places {
                    if !graph.overlaps(p, q) {
                        code.push_str(&format!("(disjoint {a} {b})\n"));
                        pairs.push((*a, *b));
                    }
                }
            }
            words.push(pairs.len() as u64);
            for (a, b) in pairs {
                words.extend([a as u64, b as u64]);
            }
            code.push_str(&format!(
                "(let s{} (Start {}))\n",
                block.entry_token, block.entry_token
            ));
            words.push(block.entry_token as u64);
            for v in f.values.keys() {
                code.push_str(&format!("(let v{v} (Ref {v}))\n"));
            }
            let mut steps = vec![];
            let mut prior = block
                .params
                .iter()
                .copied()
                .filter(|v| scalar(&f.values[v]))
                .collect::<Vec<_>>();
            let mut queries = vec![];
            let mut bad = false;
            for i in &block.instructions {
                if let Op::Read {
                    root,
                    path,
                    pass: Pass::Sink,
                    ..
                } = &i.op
                    && scalar(&i.ty)
                    && let Some(p) = places.get(&place(root, path))
                {
                    let Some((input, _)) = i.token else {
                        bad = true;
                        break;
                    };
                    code.push_str(&format!("(union v{} (Read s{input} {p}))\n", i.dest));
                    steps.extend([1, i.dest as u64, input as u64, *p as u64]);
                    queries.push((
                        i.dest,
                        prior
                            .iter()
                            .copied()
                            .filter(|v| f.values[v] == i.ty)
                            .collect::<Vec<_>>(),
                    ));
                }
                if let Some((input, output)) = i.token
                    && input != output
                {
                    let write = match &i.op {
                        Op::Write {
                            place: p,
                            op,
                            value,
                        } if op == "=" && scalar(&f.values[value]) => {
                            let mut path = p.split('.');
                            places
                                .get(&place(
                                    path.next().unwrap(),
                                    &path.map(str::to_owned).collect::<Vec<_>>(),
                                ))
                                .map(|p| (*p, *value))
                        }
                        Op::Bind { pattern, value } if scalar(&f.values[value]) => {
                            if let PatternKind::Bind(n) = &pattern.kind {
                                places.get(&place(n, &[])).map(|p| (*p, *value))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some((p, v)) = write {
                        code.push_str(&format!("(let s{output} (Write s{input} {p} v{v}))\n"));
                        steps.extend([2, output as u64, input as u64, p as u64, v as u64]);
                    } else {
                        code.push_str(&format!("(let s{output} (Barrier s{input} {}))\n", i.dest));
                        steps.extend([3, output as u64, input as u64, i.dest as u64]);
                    }
                }
                if scalar(&i.ty) {
                    prior.push(i.dest);
                }
            }
            // ir_opt refuses a read without state evidence; there is no
            // egglog run to compare.
            if bad || queries.is_empty() {
                continue;
            }
            code.push_str(&format!("(run {ROUNDS})\n"));
            let mut count = 0;
            let mut k = 0;
            while k < steps.len() {
                count += 1;
                k += match steps[k] {
                    1 | 3 => 4,
                    _ => 5,
                };
            }
            words.push(count);
            words.extend(&steps);
            words.push(queries.len() as u64);
            for (d, cs) in &queries {
                words.push(*d as u64);
                words.push(cs.len() as u64);
                words.extend(cs.iter().map(|c| *c as u64));
            }
            out.push(MemoryBlock {
                name: format!("{name}::{}#{bi}", f.name),
                code,
                words,
                queries,
            });
        }
    }
    out
}

/// egglog's answers: `None` when egglog refuses the program, else per query
/// the first candidate in the read's class.
fn egglog_block(b: &MemoryBlock) -> Option<Vec<Option<u32>>> {
    let mut egraph = egglog::EGraph::default();
    egraph.parse_and_run_program(None, &b.code).ok()?;
    let mut out = vec![];
    for (dest, candidates) in &b.queries {
        let found = egraph
            .update(|fs| -> Result<Option<u32>, egglog::Error> {
                let root = fs.eclass_of("Ref", i64::from(*dest))?;
                for c in candidates {
                    if fs.eclass_of("Ref", i64::from(*c))? == root {
                        return Ok(Some(*c));
                    }
                }
                Ok(None)
            })
            .ok()?;
        out.push(found);
    }
    Some(out)
}

fn encode_memory(blocks: &[MemoryBlock]) -> Vec<u8> {
    let mut w = vec![ROUNDS, blocks.len() as u64];
    for b in blocks {
        w.extend(&b.words);
    }
    words_to_bytes(&w)
}

fn split_memory(words: &[u64], blocks: &[MemoryBlock]) -> Vec<Option<Vec<Option<u32>>>> {
    let mut i = 0;
    let mut out = vec![];
    for b in blocks {
        let bad = words[i] == 1;
        i += 1;
        if bad {
            out.push(None);
            continue;
        }
        let mut v = vec![];
        for _ in &b.queries {
            let x = words[i];
            i += 1;
            v.push((x != u32::MAX as u64).then_some(x as u32));
        }
        out.push(Some(v));
    }
    assert_eq!(i, words.len());
    out
}

fn memory_corpus() -> Vec<MemoryBlock> {
    let mut out = vec![];
    // One mutant per program by default (CHECK_HARNESS_MUTANTS=5 for all).
    for c in ir_cases(1) {
        out.extend(memory_blocks(&c.name, &c.program));
    }
    out
}

#[test]
fn memory_matches_egglog() {
    let blocks = memory_corpus();
    assert!(!blocks.is_empty());
    let words = package()
        .entry("egraph_memory", true)
        .words(&encode_memory(&blocks));
    let ours = split_memory(&words, &blocks);
    let mut bad = 0;
    let mut forwarded = 0;
    let mut queries = 0;
    // egglog builds and parses one program per block, about 400 s for the
    // corpus in a release build; every block runs in Vesper, every
    // `EGRAPH_MEMORY_STRIDE`-th (default 20) also in egglog.
    let stride = std::env::var("EGRAPH_MEMORY_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20usize)
        .max(1);
    let mut compared = 0;
    for (b, v) in blocks.iter().zip(&ours).step_by(stride) {
        compared += 1;
        let e = egglog_block(b);
        if let Some(e) = &e {
            queries += e.len();
            forwarded += e.iter().filter(|x| x.is_some()).count();
        }
        if e != *v {
            bad += 1;
            if bad <= 5 {
                eprintln!("{} differs\n  egglog: {e:?}\n  vesper: {v:?}", b.name);
            }
        }
    }
    eprintln!(
        "memory: {} blocks, {compared} compared with egglog, {queries} reads, {forwarded} forwarded, {bad} differ",
        blocks.len()
    );
    assert_eq!(bad, 0);
}

#[test]
fn unoptimized_agrees() {
    let cases = generated_math(200);
    let input = encode_math(&cases);
    let a = package().entry("egraph_math", true).words(&input);
    let b = package().entry("egraph_math", false).words(&input);
    assert_eq!(a, b);
    let mut blocks = memory_corpus();
    blocks.truncate(5000);
    let input = encode_memory(&blocks);
    let a = package().entry("egraph_memory", true).words(&input);
    let b = package().entry("egraph_memory", false).words(&input);
    assert_eq!(a, b);
}

// ------------------------------------------------------------------ speed

#[test]
#[ignore]
fn speed_math() {
    if !speed() {
        return;
    }
    let mut cases = corpus_math();
    cases.extend(generated_math(3000));
    cases.extend(chains());
    let input = encode_math(&cases);
    let entry = package().entry("egraph_math", true);
    let mut rust = || {
        let t = Instant::now();
        for c in &cases {
            std::hint::black_box(egg_case(c));
        }
        t.elapsed()
    };
    let mut ves = || {
        time(|| {
            std::hint::black_box(entry.words(&input));
        })
    };
    abba("egraph math", &mut rust, &mut ves);
}

#[test]
#[ignore]
fn speed_memory() {
    if !speed() {
        return;
    }
    // Every 20th block: egglog takes about 400 s per pass over all of them.
    let blocks: Vec<MemoryBlock> = memory_corpus().into_iter().step_by(20).collect();
    let input = encode_memory(&blocks);
    let entry = package().entry("egraph_memory", true);
    let mut rust = || {
        let t = Instant::now();
        for b in &blocks {
            std::hint::black_box(egglog_block(b));
        }
        t.elapsed()
    };
    let mut ves = || {
        time(|| {
            std::hint::black_box(entry.words(&input));
        })
    };
    abba("egraph memory", &mut rust, &mut ves);
}
