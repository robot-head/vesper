//! ABBA benchmarks on every Loom.toml in the repository, both arms in one
//! binary: the Vesper TOML reader (@vesper-std/toml) against toml_edit's
//! `ImDocument::parse`, which `toml::from_str` runs first; and the port's
//! manifest decoding (`manifest_toml`, then `serde_json` into `Manifest`,
//! as `parse_manifest` does) against `toml::from_str::<Manifest>`:
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

fn accepts(doc: &str, vesper: bool, manifest: bool) -> bool {
    match (manifest, vesper) {
        (false, false) => toml_edit::ImDocument::parse(doc).is_ok(),
        (false, true) => vesper_loom_calculus::toml_accepts(doc),
        (true, false) => toml::from_str::<vesper_loom_calculus::Manifest>(doc).is_ok(),
        (true, true) => {
            let json = vesper_loom_calculus::manifest_toml(doc);
            !json.starts_with('!')
                && serde_json::from_str::<vesper_loom_calculus::Manifest>(&json).is_ok()
        }
    }
}

fn window(docs: &[String], vesper: bool, manifest: bool) -> u64 {
    let start = Instant::now();
    let mut accepted = 0usize;
    for _ in 0..PASSES {
        for doc in docs {
            accepted += usize::from(std::hint::black_box(accepts(doc, vesper, manifest)));
        }
    }
    assert_eq!(accepted, docs.len() * PASSES);
    start.elapsed().as_nanos() as u64
}

fn main() {
    let docs = manifests();
    let bytes: usize = docs.iter().map(String::len).sum();
    println!("{} manifests, {bytes} bytes", docs.len());
    for manifest in [false, true] {
        println!(
            "{}",
            if manifest {
                "manifest decoding:"
            } else {
                "TOML reader:"
            }
        );
        let valid: Vec<String> = docs
            .iter()
            .filter(|d| accepts(d, false, manifest))
            .cloned()
            .collect();
        run(&valid, manifest);
    }
}

fn run(docs: &[String], manifest: bool) {
    window(docs, false, manifest);
    window(docs, true, manifest);
    let mut pairs = Vec::new();
    let mut ratios = Vec::new();
    let mut rust = Vec::new();
    for _ in 0..EPOCHS {
        let a1 = window(docs, false, manifest);
        let b1 = window(docs, true, manifest);
        let b2 = window(docs, true, manifest);
        let a2 = window(docs, false, manifest);
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
