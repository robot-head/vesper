//! ABBA comparison of the replaced Rust implementation (baseline) and the
//! Vesper port (candidate). The two builds cannot share a binary (the public
//! error types name one `Session`), and the Rust implementation now lives only
//! in history (#979), so this example is built twice, once at a commit that
//! still has the Rust implementation, and, in `compare` mode, runs the two
//! executables as separate processes in Rust, port, port, Rust order per
//! epoch, then analyzes the paired totals with vesper-bench. Run
//! `aspect crate aura bench-abba`.
//!
//! `abba measure <ops>` warms each workload up and prints its total
//! nanoseconds over `<ops>` operations. `abba compare <rust> <port> <epochs>
//! <ops>` drives the two builds.
//!
//! Workloads, each over a 64-row tree (257 nodes: the root, and per row a
//! Container holding a Text, a Button and an Input):
//! - `one-text`: prepare and commit a tree that differs in one Text value.
//! - `reorder`: prepare and commit a tree whose rows are rotated by one and
//!   whose row texts all change.
//! - `encode-audit`: `encode_view_tree` plus `audit_view_tree`.
//!
//! Trees are built in setup, outside the timed body.
use std::{process::Command, time::Instant};
use vesper_aura::*;
use vesper_bench::{PairedSample, analyze_pairs};

const ROWS: u64 = 64;

fn tree(round: u64, rotate: bool) -> ViewTree {
    let order: Vec<u64> = (0..ROWS)
        .map(|r| if rotate { (r + round) % ROWS } else { r })
        .collect();
    let mut nodes =
        vec![Node::new(Key(0), Kind::Container).children(order.iter().map(|r| Key(1 + r * 4)))];
    for r in 0..ROWS {
        let base = 1 + r * 4;
        let text = if rotate || r == 0 {
            format!("row {r} round {round}")
        } else {
            format!("row {r}")
        };
        nodes.push(Node::new(Key(base), Kind::Container).children([
            Key(base + 1),
            Key(base + 2),
            Key(base + 3),
        ]));
        nodes
            .push(Node::new(Key(base + 1), Kind::Text).property(Property::Text, Value::Text(text)));
        nodes.push(
            Node::new(Key(base + 2), Kind::Button)
                .property(Property::Text, Value::Text("Open".into()))
                .on(Event::Click, HandlerKey(base)),
        );
        nodes.push(
            Node::new(Key(base + 3), Kind::Input)
                .property(Property::Text, Value::Text("Name".into()))
                .property(Property::Value, Value::Text(String::new()))
                .on(Event::Changed, HandlerKey(base + 1)),
        );
    }
    ViewTree::new(Key(0), nodes)
}

const WORKLOADS: [&str; 3] = ["one-text", "reorder", "encode-audit"];

/// Total nanoseconds for `ops` timed operations after `ops / 4` warm-up ones.
/// Inputs are built before the clock starts.
fn measure(name: &str, ops: u64) -> u128 {
    let ui = UiThread::claim_current();
    let limits = Limits::default();
    if name == "encode-audit" {
        let input = tree(0, false);
        let mut sink = 0;
        let mut once = || {
            let (ops, _) = encode_view_tree(&input, &limits).unwrap();
            let report = audit_view_tree(&input, &limits).unwrap();
            sink += ops.len() + report.violations.len();
        };
        for _ in 0..ops / 4 {
            once();
        }
        let start = Instant::now();
        for _ in 0..ops {
            once();
        }
        let elapsed = start.elapsed().as_nanos();
        std::hint::black_box(sink);
        return elapsed;
    }
    let rotate = name == "reorder";
    let backend = RecordingBackend::new(&ui, limits.clone());
    let mut app = Session::new(&ui, backend, limits).unwrap();
    let first = app.prepare(&ui, Some(tree(0, rotate))).unwrap();
    app.commit(&ui, first).unwrap();
    let warm = ops / 4;
    let trees: Vec<ViewTree> = (1..=warm + ops).map(|r| tree(r, rotate)).collect();
    let mut trees = trees.into_iter();
    let mut step = |next: ViewTree| {
        let update = app.prepare(&ui, Some(next)).unwrap();
        app.commit(&ui, update).unwrap().patches
    };
    for next in trees.by_ref().take(warm as usize) {
        step(next);
    }
    let mut patches = 0;
    let start = Instant::now();
    for next in trees {
        patches += step(next);
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(patches);
    drop(app.shutdown(&ui).unwrap());
    elapsed
}

fn run(exe: &str, ops: u64) -> Vec<u64> {
    let out = Command::new(exe)
        .args(["measure", &ops.to_string()])
        .output()
        .expect("run a build");
    assert!(out.status.success(), "{exe} failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect()
}

fn compare(rust: &str, port: &str, epochs: usize, ops: u64) {
    let mut pairs = vec![Vec::new(); WORKLOADS.len()];
    for _ in 0..epochs {
        let a1 = run(rust, ops);
        let b1 = run(port, ops);
        let b2 = run(port, ops);
        let a2 = run(rust, ops);
        for (w, pair) in pairs.iter_mut().enumerate() {
            pair.push(PairedSample::new(a1[w] + a2[w], b1[w] + b2[w], 2 * ops).unwrap());
        }
    }
    for (name, pairs) in WORKLOADS.iter().zip(&pairs) {
        let per_op = |t: u64| t as f64 / (2 * ops) as f64;
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let rust_ns = median(pairs.iter().map(|p| per_op(p.baseline_total())).collect());
        let port_ns = median(pairs.iter().map(|p| per_op(p.candidate_total())).collect());
        let ratios: Vec<f64> = pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
            .collect();
        let lo = ratios.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ratios.iter().copied().fold(0.0, f64::max);
        let (ci_lo, ci_hi) = median_interval(&ratios);
        let ratio = median(ratios);
        let analysis = analyze_pairs(pairs, 0).unwrap();
        let bp = |s: PairedSample| s.regression_bp_bounds().floor as f64 / 100.0;
        println!(
            "{name:<13} rust {rust_ns:>9.0} ns/op  port {port_ns:>9.0} ns/op  port/rust {ratio:.2}x \
             (95% median interval {ci_lo:.2}x..{ci_hi:.2}x, range {lo:.2}x..{hi:.2}x, \
             sample median regression {:+.0}%..{:+.0}%, {epochs} epochs)",
            bp(analysis.median_lower),
            bp(analysis.median_upper),
        );
    }
}

/// A distribution-free interval for the median of the per-epoch ratios:
/// the `d`-th smallest and `d`-th largest ratio for the largest `d` with
/// P(X <= d - 1) <= 2.5%, X ~ Binomial(n, 1/2), so the interval covers the
/// median with at least 95% probability (the whole range when there are
/// too few epochs for that). With 15 epochs `d` is 4 (96.5%).
fn median_interval(ratios: &[f64]) -> (f64, f64) {
    let mut v = ratios.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let total = 2f64.powi(n as i32);
    // `term` is C(n, j); `tail` accumulates P(X <= j).
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
    (v[k], v[n - 1 - k])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["measure", ops] => {
            let ops = ops.parse().unwrap();
            for name in WORKLOADS {
                println!("{name} {}", measure(name, ops));
            }
        }
        ["compare", rust, port, epochs, ops] => {
            compare(rust, port, epochs.parse().unwrap(), ops.parse().unwrap())
        }
        _ => {
            eprintln!("usage: abba measure <ops> | abba compare <rust> <port> <epochs> <ops>");
            std::process::exit(2);
        }
    }
}
