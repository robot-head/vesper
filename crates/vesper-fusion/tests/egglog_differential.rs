//! The Vesper saturation engine against egglog 3, the library it replaced
//! (Rust-exit wave SH6, lane 18, #1413).
//!
//! - `extractions_match_egglog`: the shipped tensor and DSP rule sets on the
//!   graphs the crate's tests build and on generated graphs (shared operands,
//!   repeated parameters, every constructor), each extracted without and with
//!   saturation by both engines. Each case compares the extracted arena
//!   (nodes, order, root) and the tree cost; every saturated case's witness
//!   must also pass `witness::check`. Costs must be equal; equal-cost terms
//!   egglog breaks differently are counted and listed
//!   (`FUSION_DIFF_CASES` sets the generated graphs, `FUSION_DIFF_VERBOSE`
//!   prints each case and compares ties with egglog's non-proof mode).
//! - `speed_abba` (ignored; `FUSION_ABBA_EPOCHS` epochs): egglog's work in the
//!   old `fuse` (the unsaturated extraction, the saturated run with proofs,
//!   the `prove` query) against the Vesper engine's (the unsaturated
//!   extraction and the explained saturated run), in egglog, Vesper, Vesper,
//!   egglog order per epoch, with a 95% interval for the median ratio. The
//!   Rust checker both versions of `fuse` run is left out of both arms.
use egglog::{CommandOutput, EGraph, Term, TermDag, ast::Literal};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Instant;
use vesper_fusion::engine::{self, Extracted, ITERATIONS, Node};
use vesper_fusion::{Error, dsp, tensor};

// ------------------------------------------------------------ egglog side

fn convert(dag: &TermDag, cost: u64, term: egglog::TermId) -> Extracted {
    let mut nodes = Vec::new();
    let mut index: HashMap<Node, usize> = HashMap::new();
    let mut memo: HashMap<egglog::TermId, usize> = HashMap::new();
    let mut stack = vec![(term, false)];
    while let Some((id, expanded)) = stack.pop() {
        if memo.contains_key(&id) {
            continue;
        }
        let node = match dag.get(id) {
            Term::Lit(Literal::Int(v)) => Node::Int(*v),
            Term::Lit(Literal::String(s)) => Node::Str(s.clone()),
            Term::App(head, kids) => {
                if !expanded {
                    stack.push((id, true));
                    for k in kids.iter().rev() {
                        if !memo.contains_key(k) {
                            stack.push((*k, false));
                        }
                    }
                    continue;
                }
                Node::App(head.clone(), kids.iter().map(|k| memo[k]).collect())
            }
            other => panic!("unexpected term {other:?}"),
        };
        let slot = *index.entry(node.clone()).or_insert_with(|| {
            nodes.push(node);
            nodes.len() - 1
        });
        memo.insert(id, slot);
    }
    let root = memo[&term];
    Extracted {
        text: String::new(),
        nodes,
        root,
        cost,
    }
}

/// egglog's extraction, as the old engine ran it: proofs on for the
/// saturated run (the old `fuse` extracted from that run), off otherwise.
fn egglog_extract(rules: &str, bindings: &str, root: &str, saturate: bool) -> (EGraph, Extracted) {
    let mut program = format!("{rules}{bindings}");
    if saturate {
        let _ = writeln!(program, "(run {ITERATIONS})");
    }
    let _ = writeln!(program, "(extract {root})");
    let mut egraph = if saturate {
        EGraph::new_with_proofs()
    } else {
        EGraph::default()
    };
    let outputs = egraph.parse_and_run_program(None, &program).unwrap();
    let (dag, cost, term) = outputs
        .into_iter()
        .rev()
        .find_map(|o| match o {
            CommandOutput::ExtractBest(dag, cost, term) => Some((dag, cost, term)),
            _ => None,
        })
        .expect("an extraction");
    (egraph, convert(&dag, cost, term))
}

/// The old engine's proof query for an extracted term.
fn egglog_prove(egraph: &mut EGraph, root: &str, term: &Extracted) {
    fn write(nodes: &[Node], id: usize, out: &mut String) {
        match &nodes[id] {
            Node::Int(v) => {
                let _ = write!(out, "{v}");
            }
            Node::Str(s) => {
                let _ = write!(out, "\"{s}\"");
            }
            Node::App(head, kids) => {
                let _ = write!(out, "({head}");
                for &k in kids {
                    out.push(' ');
                    write(nodes, k, out);
                }
                out.push(')');
            }
        }
    }
    let mut query = format!("(prove (= {root} ");
    write(&term.nodes, term.root, &mut query);
    query.push_str("))\n");
    let outputs = egraph.parse_and_run_program(None, &query).unwrap();
    assert!(
        outputs
            .iter()
            .any(|o| matches!(o, CommandOutput::ProveExists { .. }))
    );
}

// ------------------------------------------------------------ cases

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

struct Case {
    rules: &'static str,
    bindings: String,
    root: String,
}

fn tensor_case(rng: &mut Rng, size: usize) -> Case {
    let mut out = String::new();
    let inputs = 1 + rng.below(3) as usize;
    for i in 0..inputs {
        let _ = writeln!(out, "(let t{i} (In \"x{i}\"))");
    }
    for i in inputs..inputs + size {
        let pick = |rng: &mut Rng| format!("t{}", rng.below(i as u64));
        let body = match rng.below(8) {
            0 | 1 => {
                let head = if rng.below(2) == 0 { "Mat" } else { "Deq" };
                let w = rng.below(3);
                {
                    let x = pick(rng);
                    format!("(MatVec ({head} \"w{w}\") {x})")
                }
            }
            2 => {
                let (a, b) = (pick(rng), pick(rng));
                format!("(RmsNorm {a} {b} {})", 1 + rng.below(2))
            }
            3 => {
                let a = rng.below(4);
                format!("(Act {a} {})", pick(rng))
            }
            4 => {
                let (a, b) = (pick(rng), pick(rng));
                format!("(Add {a} {b})")
            }
            5 => {
                let (a, b) = (pick(rng), pick(rng));
                format!("(Mul {a} {b})")
            }
            6 => {
                let c = rng.below(3) as i64 - 1;
                format!("(Scale {c} {})", pick(rng))
            }
            _ => {
                let (a, b) = (pick(rng), pick(rng));
                format!("(Add {a} {b})")
            }
        };
        let _ = writeln!(out, "(let t{i} {body})");
    }
    Case {
        rules: tensor::RULES,
        bindings: out,
        root: format!("t{}", inputs + size - 1),
    }
}

fn dsp_case(rng: &mut Rng, size: usize) -> Case {
    let mut out = String::new();
    let sources = 1 + rng.below(2) as usize;
    for i in 0..sources {
        let _ = writeln!(out, "(let s{i} (Src \"in{i}\"))");
    }
    for i in sources..sources + size {
        // Mostly chains (the last node), sometimes an earlier one.
        let pick = |rng: &mut Rng| {
            if rng.below(4) == 0 {
                format!("s{}", rng.below(i as u64))
            } else {
                format!("s{}", i - 1)
            }
        };
        let body = match rng.below(9) {
            0 | 1 => {
                let p = rng.below(3);
                format!("(Gain {p} {})", pick(rng))
            }
            2 => {
                let p = rng.below(3);
                format!("(Pan {p} {})", pick(rng))
            }
            3..=5 => {
                let p = rng.below(3);
                format!("(Biquad {p} {})", pick(rng))
            }
            _ => {
                let (a, b) = (pick(rng), pick(rng));
                format!("(Mix {a} {b})")
            }
        };
        let _ = writeln!(out, "(let s{i} {body})");
    }
    Case {
        rules: dsp::RULES,
        bindings: out,
        root: format!("s{}", sources + size - 1),
    }
}

/// The shapes the crate's tests fuse (tests/tensor_fusion.rs,
/// tests/dsp_fusion.rs, tests/witness.rs), as bindings.
fn fixed_cases() -> Vec<Case> {
    let t = |b: &str, r: &str| Case {
        rules: tensor::RULES,
        bindings: b.into(),
        root: r.into(),
    };
    let d = |b: &str, r: &str| Case {
        rules: dsp::RULES,
        bindings: b.into(),
        root: r.into(),
    };
    let mut cases = vec![
        // matmul + bias + activation; elementwise chains; dequantized matvec.
        t(
            "(let t0 (In \"x\"))\n(let t1 (In \"b\"))\n(let t2 (MatVec (Mat \"w\") t0))\n(let t3 (Add t2 t1))\n(let t4 (Act 1 t3))\n",
            "t4",
        ),
        t(
            "(let t0 (In \"x\"))\n(let t1 (In \"b\"))\n(let t2 (MatVec (Deq \"w\") t0))\n(let t3 (Add t2 t1))\n(let t4 (Mul t3 t1))\n(let t5 (Scale 3 t4))\n(let t6 (Act 2 t5))\n",
            "t6",
        ),
        t(
            "(let t0 (In \"x\"))\n(let t1 (In \"y\"))\n(let t2 (Add t0 t1))\n(let t3 (Mul t2 t2))\n(let t4 (Act 0 t3))\n(let t5 (Scale -1 t4))\n",
            "t5",
        ),
        // RMSNorm prologue; SwiGLU (two projections of one normalized input).
        t(
            "(let t0 (In \"x\"))\n(let t1 (In \"n\"))\n(let t2 (RmsNorm t0 t1 1))\n(let t3 (MatVec (Mat \"w\") t2))\n(let t4 (Act 3 t3))\n",
            "t4",
        ),
        t(
            "(let t0 (In \"x\"))\n(let t1 (In \"n\"))\n(let t2 (RmsNorm t0 t1 1))\n(let t3 (MatVec (Deq \"g\") t2))\n(let t4 (MatVec (Deq \"u\") t2))\n(let t5 (Act 2 t3))\n(let t6 (Mul t5 t4))\n(let t7 (MatVec (Mat \"d\") t6))\n",
            "t7",
        ),
        // Activation then add of its operand (witness tests).
        t(
            "(let t0 (In \"x\"))\n(let t1 (Act 0 t0))\n(let t2 (Add t1 t0))\n",
            "t2",
        ),
        // gain, pan, compose, mix with the source, cascade with a gain before it.
        d(
            "(let s0 (Src \"in\"))\n(let s1 (Gain 0 s0))\n(let s2 (Pan 1 s1))\n(let s3 (Gain 2 s2))\n(let s4 (Mix s3 s0))\n(let s5 (Gain 3 s4))\n(let s6 (Biquad 4 s5))\n(let s7 (Biquad 4 s6))\n(let s8 (Biquad 4 s7))\n",
            "s8",
        ),
        // a gain after a cascade; two sends of one source.
        d(
            "(let s0 (Src \"in\"))\n(let s1 (Biquad 0 s0))\n(let s2 (Biquad 0 s1))\n(let s3 (Gain 1 s2))\n",
            "s3",
        ),
        d(
            "(let s0 (Src \"in\"))\n(let s1 (Gain 0 s0))\n(let s2 (Gain 1 s0))\n(let s3 (Mix s1 s2))\n",
            "s3",
        ),
    ];
    // Long chains (tests/tensor_fusion.rs `long_chains_stay_within_budget`,
    // tests/dsp_fusion.rs cascades).
    let mut chain = String::from("(let t0 (In \"x\"))\n");
    for i in 1..=200 {
        let _ = writeln!(chain, "(let t{i} (Act {} t{}))", i % 4, i - 1);
    }
    cases.push(t(&chain, "t200"));
    let mut chain = String::from("(let s0 (Src \"in\"))\n");
    for i in 1..=8 {
        let _ = writeln!(chain, "(let s{i} (Biquad {} s{}))", i % 3, i - 1);
    }
    cases.push(d(&chain, "s8"));
    cases
}

fn cases() -> Vec<Case> {
    let count: usize = std::env::var("FUSION_DIFF_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = fixed_cases();
    for i in 0..count {
        let size = 1 + (i % 24);
        out.push(tensor_case(&mut rng, size));
        // Long runs of biquads make egglog's proof-mode list rewriting
        // (cascade-append-cons) grow past memory; the crate's chains are
        // short, so DSP graphs stay at 12 stages.
        out.push(dsp_case(&mut rng, 1 + (i % 12)));
    }
    out
}

fn show(nodes: &[Node], id: usize) -> String {
    match &nodes[id] {
        Node::Int(v) => v.to_string(),
        Node::Str(s) => format!("{s:?}"),
        Node::App(h, kids) => {
            let mut out = format!("({h}");
            for &k in kids {
                out.push(' ');
                out.push_str(&show(nodes, k));
            }
            out.push(')');
            out
        }
    }
}

fn same(a: &Extracted, b: &Extracted) -> bool {
    a.nodes == b.nodes && a.root == b.root && a.cost == b.cost
}

#[test]
fn extractions_match_egglog() {
    let cases = cases();
    let (mut exact, mut ties, mut other) = (0, Vec::new(), Vec::new());
    let mut refused = Vec::new();
    let mut steps = 0;
    let verbose = std::env::var_os("FUSION_DIFF_VERBOSE").is_some();
    for (n, case) in cases.iter().enumerate() {
        if verbose {
            eprintln!(
                "case {n}: {} root {}",
                case.bindings.lines().count(),
                case.root
            );
        }
        for saturate in [false, true] {
            let got = match engine::run(case.rules, &case.bindings, &case.root, saturate) {
                Ok(got) => got,
                // Runs of cascades whose list rewriting grows past the
                // port's e-node budget; egglog runs out of memory on them
                // (above 10 GB), so it is not run.
                Err(Error::Limit("e-graph nodes")) if saturate => {
                    refused.push(n);
                    continue;
                }
                Err(e) => panic!("case {n}: {e:?}"),
            };
            if verbose {
                eprintln!(
                    "  port done (saturate {saturate}, {} nodes)",
                    got.nodes.len()
                );
            }
            let (_, want) = egglog_extract(case.rules, &case.bindings, &case.root, saturate);
            if same(&got, &want) {
                exact += 1;
            } else if got.cost == want.cost {
                if saturate && verbose {
                    // Does egglog without proofs pick the same term?
                    let mut program = format!(
                        "{}{}(run {ITERATIONS})\n(extract {})\n",
                        case.rules, case.bindings, case.root
                    );
                    program.push('\n');
                    let mut g = EGraph::default();
                    let outs = g.parse_and_run_program(None, &program).unwrap();
                    let plain = outs
                        .into_iter()
                        .rev()
                        .find_map(|o| match o {
                            CommandOutput::ExtractBest(dag, cost, term) => {
                                Some(convert(&dag, cost, term))
                            }
                            _ => None,
                        })
                        .unwrap();
                    eprintln!(
                        "tie {n}: egglog without proofs {} the proof-mode term, {} the port's",
                        if same(&plain, &want) {
                            "matches"
                        } else {
                            "differs from"
                        },
                        if same(&plain, &got) {
                            "matches"
                        } else {
                            "differs from"
                        }
                    );
                }
                ties.push((n, saturate, got.text.clone(), show(&want.nodes, want.root)));
            } else {
                other.push((n, saturate, got.cost, want.cost, got.text.clone()));
            }
        }
        let set = if case.rules == tensor::RULES {
            tensor::rule_set()
        } else {
            dsp::rule_set()
        }
        .unwrap();
        if refused.last() == Some(&n) {
            continue;
        }
        let fusion = engine::fuse(case.rules, set, &case.bindings, &case.root).unwrap();
        steps += fusion.summary.steps;
    }
    eprintln!(
        "{} cases x 2 extractions: {exact} exact, {} equal-cost ties, {} other, {} over the e-node budget {refused:?}; {steps} checked witness steps",
        cases.len(),
        ties.len(),
        other.len(),
        refused.len()
    );
    for t in ties.iter().take(5) {
        eprintln!("tie: {t:?}");
    }
    // Equal-cost ties are accepted (the owner's call for the e-graph ports,
    // 2026-11-21): egglog picks among equal-cost e-nodes in its tables' row
    // order, which even differs between its proof and non-proof modes, and
    // every term the port extracts carries a witness `fuse` checked above.
    assert!(other.is_empty(), "{other:?}");
}

// ------------------------------------------------------------ speed

/// A distribution-free 95% interval for the median of `ratios` (as in
/// crates/vesper-aura/examples/abba.rs).
fn median_interval(ratios: &[f64]) -> (f64, f64, f64) {
    let mut v = ratios.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let total = 2f64.powi(n as i32);
    let (mut term, mut tail, mut d) = (1.0f64, 0.0f64, 0usize);
    for j in 0..n / 2 {
        tail += term / total;
        if tail > 0.025 {
            break;
        }
        d = j + 1;
        term = term * (n - j) as f64 / (j + 1) as f64;
    }
    let k = d.saturating_sub(1);
    (v[n / 2], v[k], v[n - 1 - k])
}

fn egglog_arm(cases: &[Case]) -> u128 {
    let start = Instant::now();
    for c in cases {
        let (_, before) = egglog_extract(c.rules, &c.bindings, &c.root, false);
        let (mut g, term) = egglog_extract(c.rules, &c.bindings, &c.root, true);
        egglog_prove(&mut g, &c.root, &term);
        std::hint::black_box((before.cost, term.cost));
    }
    start.elapsed().as_nanos()
}

fn vesper_arm(cases: &[Case]) -> u128 {
    let start = Instant::now();
    for c in cases {
        let set = if c.rules == tensor::RULES {
            tensor::rule_set()
        } else {
            dsp::rule_set()
        }
        .unwrap();
        let before = engine::run(c.rules, &c.bindings, &c.root, false).unwrap();
        let (term, witness) = engine::explained(c.rules, set, &c.bindings, &c.root).unwrap();
        std::hint::black_box((before.cost, term.cost, witness.steps.len()));
    }
    start.elapsed().as_nanos()
}

#[test]
#[ignore = "timing; run with --release --ignored"]
fn speed_abba() {
    let epochs: usize = std::env::var("FUSION_ABBA_EPOCHS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let workloads: [(&str, Vec<Case>); 3] = [
        ("crate-tests", fixed_cases()),
        ("tensor-24", {
            let mut rng = Rng(7);
            (0..40).map(|_| tensor_case(&mut rng, 24)).collect()
        }),
        ("dsp-12", {
            let mut rng = Rng(11);
            (0..40).map(|_| dsp_case(&mut rng, 12)).collect()
        }),
    ];
    for (name, cases) in &workloads {
        egglog_arm(cases);
        vesper_arm(cases);
        let mut ratios = Vec::new();
        let (mut ta, mut tb) = (0u128, 0u128);
        for _ in 0..epochs {
            let a1 = egglog_arm(cases);
            let b1 = vesper_arm(cases);
            let b2 = vesper_arm(cases);
            let a2 = egglog_arm(cases);
            ta += a1 + a2;
            tb += b1 + b2;
            ratios.push((b1 + b2) as f64 / (a1 + a2) as f64);
        }
        let (m, lo, hi) = median_interval(&ratios);
        let per = |t: u128| t as f64 / (2 * epochs * cases.len()) as f64 / 1000.0;
        eprintln!(
            "{name:<12} egglog {:>9.1} us/case  vesper {:>9.1} us/case  vesper/egglog {m:.3}x (95% median interval {lo:.3}x..{hi:.3}x, {epochs} epochs)",
            per(ta),
            per(tb)
        );
    }
}
