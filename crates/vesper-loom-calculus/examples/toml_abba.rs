//! ABBA benchmark of the Vesper TOML reader (@vesper-std/toml) against
//! toml_edit's `ImDocument::parse`, which `toml::from_str` runs first, on
//! every Loom.toml in the repository. Both arms are in one binary:
//!
//! ```sh
//! cargo run --release -p vesper-loom-calculus --example toml_abba
//! ```
//!
//! Each epoch times A (Rust) B (Vesper) B A windows of `PASSES` passes and
//! pairs A and B totals; `vesper_bench::analyze_pairs` gives the median
//! ratio's 95% interval.
use std::time::Instant;

const PASSES: usize = 50;
const EPOCHS: usize = 21;

fn manifests() -> Vec<String> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let mut out = Vec::new();
    let mut stack = vec![
        std::path::PathBuf::from(root).join("packages"),
        std::path::PathBuf::from(root).join("crates"),
    ];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    stack.push(p);
                }
            } else if p.file_name().is_some_and(|n| n == "Loom.toml")
                && let Ok(text) = std::fs::read_to_string(&p)
            {
                out.push(text);
            }
        }
    }
    out.sort();
    out
}

fn window(docs: &[String], vesper: bool) -> u64 {
    let start = Instant::now();
    let mut accepted = 0usize;
    for _ in 0..PASSES {
        for doc in docs {
            let ok = if vesper {
                vesper_loom_calculus::toml_accepts(doc)
            } else {
                toml_edit::ImDocument::parse(doc.as_str()).is_ok()
            };
            accepted += usize::from(std::hint::black_box(ok));
        }
    }
    assert_eq!(accepted, docs.len() * PASSES);
    start.elapsed().as_nanos() as u64
}

fn main() {
    let docs = manifests();
    let bytes: usize = docs.iter().map(String::len).sum();
    println!("{} manifests, {bytes} bytes", docs.len());
    window(&docs, false);
    window(&docs, true);
    let mut pairs = Vec::new();
    let mut ratios = Vec::new();
    let mut rust = Vec::new();
    for _ in 0..EPOCHS {
        let a1 = window(&docs, false);
        let b1 = window(&docs, true);
        let b2 = window(&docs, true);
        let a2 = window(&docs, false);
        let (ta, tb) = (a1 + a2, b1 + b2);
        ratios.push(tb as f64 / ta as f64);
        rust.push(ta as f64 / (2 * PASSES) as f64 / 1e3);
        pairs.push(vesper_bench::PairedSample::new(ta, tb, 2 * PASSES as u64).unwrap());
    }
    ratios.sort_by(f64::total_cmp);
    rust.sort_by(f64::total_cmp);
    println!(
        "Rust {:.1} us/pass; median Vesper/Rust {:.3} [min {:.3}, max {:.3}]",
        rust[EPOCHS / 2],
        ratios[EPOCHS / 2],
        ratios[0],
        ratios[EPOCHS - 1]
    );
    let analysis = vesper_bench::analyze_pairs(&pairs, 5_000).unwrap();
    println!(
        "1.5x gate: {:?}; median regression bp {:?}; interval {:?}",
        analysis.decision,
        analysis.median_upper.regression_bp_bounds(),
        analysis.interval
    );
}
