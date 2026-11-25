//! ABBA benchmark of the B+Tree PAX leaf decoder (`leaf_decode`, whose
//! validation is the Vesper `leaf_check` since #1401): decode 256 full
//! leaves of a six-column table per pass. One binary holds one build, so the
//! arms are two builds of this example (the Rust arm from a revision before
//! the port, with this file and the `bench_leaf_*` hooks copied in):
//!
//! ```sh
//! leaf_abba driver RUST_BIN PORT_BIN
//! ```
//!
//! `window N` prints the nanoseconds of N passes after one warm-up pass and
//! the digest of the decoded entries. `driver A B` runs epochs of windows in
//! the order A B B A, checks that both arms decoded the same entries, pairs
//! each epoch's totals and reports the median B/A with its 95% interval.
use std::process::Command;
use std::time::Instant;

const PAGES: usize = 256;
const PASSES: usize = 400;
const EPOCHS: usize = 15;

fn window(passes: usize) -> (u128, u64) {
    let pages = vesper_quarry::store::bench_leaf_pages(PAGES);
    let digest = vesper_quarry::store::bench_leaf_decode(&pages);
    let start = Instant::now();
    for _ in 0..passes {
        std::hint::black_box(vesper_quarry::store::bench_leaf_decode(
            std::hint::black_box(&pages),
        ));
    }
    (start.elapsed().as_nanos(), digest)
}

fn run(bin: &str) -> (u64, String) {
    let out = Command::new(bin)
        .args(["window", &PASSES.to_string()])
        .output()
        .expect("run arm");
    assert!(out.status.success(), "{bin} failed");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (ns, digest) = text.trim().split_once(' ').expect("`ns digest`");
    (ns.parse().unwrap(), digest.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("window") => {
            let (ns, digest) = window(args[2].parse().unwrap());
            println!("{ns} {digest}");
        }
        Some("driver") => {
            let (a, b) = (&args[2], &args[3]);
            let mut pairs = Vec::new();
            let mut ratios = Vec::new();
            for epoch in 0..EPOCHS {
                let (a1, da) = run(a);
                let (b1, db) = run(b);
                let (b2, _) = run(b);
                let (a2, _) = run(a);
                assert_eq!(da, db, "the arms decoded different entries");
                let per_page = |t: u64| t as f64 / (2 * PASSES * PAGES) as f64;
                println!(
                    "epoch {epoch}: A {:.0} ns/page, B {:.0} ns/page, B/A {:.3}",
                    per_page(a1 + a2),
                    per_page(b1 + b2),
                    (b1 + b2) as f64 / (a1 + a2) as f64
                );
                ratios.push((b1 + b2) as f64 / (a1 + a2) as f64);
                pairs.push(
                    vesper_bench::PairedSample::new(a1 + a2, b1 + b2, 2 * PASSES as u64).unwrap(),
                );
            }
            ratios.sort_by(f64::total_cmp);
            let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
            println!(
                "median B/A {:.3}x (min {:.3}x, max {:.3}x); 2x gate: {:?}",
                ratios[EPOCHS / 2],
                ratios[0],
                ratios[EPOCHS - 1],
                analysis.decision
            );
            if let Some(interval) = analysis.interval {
                println!(
                    "95% median interval of B/A: {:.3}x..{:.3}x",
                    ratios[interval.lower_rank - 1],
                    ratios[interval.upper_rank - 1]
                );
            }
        }
        _ => {
            eprintln!("usage: leaf_abba window N | driver RUST_BIN PORT_BIN");
            std::process::exit(2);
        }
    }
}
