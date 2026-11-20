//! Measurements of the parse swap (self-hosting SH1 lane 12, #1155), run by
//! hand: `cargo test --profile port-compiler -p vesper-hybrid --test
//! parse_bench -- --ignored --nocapture`. Corpus: every repository `.ves`
//! file (the corpus of tests/self_host_ast_bench.rs).
//!
//! - `abba` times the Rust parser (`rust_parse`) against the swapped one
//!   (`parse_words` then `decode`), and the two halves of the swap, in ABBA
//!   rounds, and prints the medians.
//! - `once_for_callgrind` runs each driver once over the corpus, for
//!   `valgrind --tool=callgrind --toggle-collect=<driver>`.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/parse.rs"]
mod binding;

#[allow(dead_code)]
#[path = "../src/parse_words.rs"]
mod parse_words;

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vesper_syntax::parser;

fn corpus() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(
                    name.to_str(),
                    Some("target" | ".git" | "node_modules" | "bazel-out")
                ) && !name.to_string_lossy().starts_with("bazel-")
                {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut files = vec![];
    walk(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        &mut files,
    );
    files.sort();
    files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter(|s| s.len() <= 1 << 20)
        .collect()
}

#[inline(never)]
fn run_rust(sources: &[String]) -> usize {
    sources
        .iter()
        .map(|s| black_box(parser::rust_parse(s)).module.items.len())
        .sum()
}

#[inline(never)]
fn run_vesper(sources: &[String]) -> usize {
    sources
        .iter()
        .map(|s| {
            let words = binding::parse_words(s);
            black_box(parse_words::decode(s, &words)).module.items.len()
        })
        .sum()
}

#[inline(never)]
fn run_words(sources: &[String]) -> usize {
    sources
        .iter()
        .map(|s| black_box(binding::parse_words(s)).len())
        .sum()
}

#[inline(never)]
fn run_decode(sources: &[String], words: &[Vec<u64>]) -> usize {
    sources
        .iter()
        .zip(words)
        .map(|(s, w)| black_box(parse_words::decode(s, w)).module.items.len())
        .sum()
}

fn time(f: impl FnOnce() -> usize) -> Duration {
    let t = Instant::now();
    black_box(f());
    t.elapsed()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

#[test]
#[ignore = "measurement"]
fn abba() {
    let sources = corpus();
    let bytes: usize = sources.iter().map(String::len).sum();
    let words: Vec<Vec<u64>> = sources.iter().map(|s| binding::parse_words(s)).collect();
    let stream: usize = words.iter().map(Vec::len).sum();
    assert_eq!(run_rust(&sources), run_vesper(&sources));
    let rounds: usize = std::env::var("VESPER_PARSE_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let (mut rust, mut vesper, mut half_words, mut half_decode) = (vec![], vec![], vec![], vec![]);
    for _ in 0..rounds {
        rust.push(time(|| run_rust(&sources)));
        vesper.push(time(|| run_vesper(&sources)));
        vesper.push(time(|| run_vesper(&sources)));
        rust.push(time(|| run_rust(&sources)));
        half_words.push(time(|| run_words(&sources)));
        half_decode.push(time(|| run_decode(&sources, &words)));
    }
    let (r, v) = (median(rust), median(vesper));
    println!(
        "corpus: {} files, {bytes} bytes, {stream} stream words ({:.2} words/byte)",
        sources.len(),
        stream as f64 / bytes as f64
    );
    println!("rust parse          {r:?}");
    println!(
        "vesper parse (swap) {v:?}  {:.2}x",
        v.as_secs_f64() / r.as_secs_f64()
    );
    println!("  parse_words       {:?}", median(half_words));
    println!("  decode            {:?}", median(half_decode));
}

#[test]
#[ignore = "measurement"]
fn once_for_callgrind() {
    let sources = corpus();
    let words: Vec<Vec<u64>> = sources.iter().map(|s| binding::parse_words(s)).collect();
    black_box(run_rust(&sources));
    black_box(run_vesper(&sources));
    black_box(run_words(&sources));
    black_box(run_decode(&sources, &words));
}
