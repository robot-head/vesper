//! #2024 slice 3 part 1: `@vesper/check-world` builds the input of
//! `@vesper/check-core-typing` from the Vesper parse and the collected world
//! (packages/vesper-check-world), in place of the Rust encoding of the Rust
//! checked world.
//!
//! - `typing_input_matches_rust` compares the stream with the Rust encoding
//!   (`exprs_input`) of `World::collect`'s world, for every corpus file the
//!   package does not decline: the same bytes.
//! - `typing_over_vesper_input_matches_rust` runs core typing over the
//!   Vesper stream: every function it checks has the types the Rust
//!   checker recorded and is checked over the Rust stream too (which can
//!   check a few more: it carries the inferred results).

#[path = "common/check_harness.rs"]
mod check_harness;

#[path = "common/core_typing_words.rs"]
mod core_typing_words;

use check_harness::*;
use core_typing_words::*;
use std::sync::OnceLock;
use vesper::check::World;

fn world_package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-world"))
}

fn typing_package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-core-typing"))
}

fn input_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| world_package().entry("typing_input_words", speed()))
}

fn typing_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| typing_package().entry("core_typing_exprs", true))
}

/// The Vesper stream of `source`, or `None` when the package declines it.
fn vesper_input(source: &str) -> Option<Vec<u8>> {
    let words = input_entry().words(source.as_bytes());
    if words.first() != Some(&1) {
        return None;
    }
    Some(words[1..].iter().flat_map(|w| w.to_le_bytes()).collect())
}

/// The Rust stream over `World::collect`'s world of the parsed module (no
/// expression types or inferred results yet).
fn rust_input(module: &vesper::ast::Module) -> Vec<u8> {
    let (world, _) = World::collect(module);
    exprs_input(&world, module)
}

/// The names table of a stream.
fn names(bytes: &[u8]) -> Vec<String> {
    let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let count = word(0) as usize;
    let mut at = 8;
    let mut out = vec![];
    for _ in 0..count {
        let len = word(at) as usize;
        at += 8;
        out.push(String::from_utf8_lossy(&bytes[at..at + len]).into_owned());
        at += len.div_ceil(8) * 8;
    }
    out
}

/// Where two streams first differ: the first differing name, else the
/// first differing word after the names.
fn stream_difference(rust: &[u8], vesper: &[u8]) -> Option<String> {
    if rust == vesper {
        return None;
    }
    let (r, v) = (names(rust), names(vesper));
    if r != v {
        let i = (0..r.len().max(v.len()))
            .find(|&i| r.get(i) != v.get(i))
            .unwrap_or(0);
        return Some(format!(
            "name {i}: rust {:?}, vesper {:?} ({} and {} names)",
            r.get(i),
            v.get(i),
            r.len(),
            v.len()
        ));
    }
    let words = |b: &[u8]| -> Vec<u64> {
        b.chunks(8)
            .map(|c| {
                let mut w = [0u8; 8];
                w[..c.len()].copy_from_slice(c);
                u64::from_le_bytes(w)
            })
            .collect()
    };
    let (a, b) = (words(rust), words(vesper));
    let i = (0..a.len().max(b.len()))
        .find(|&i| a.get(i) != b.get(i))
        .unwrap_or(0);
    let from = i.saturating_sub(4);
    Some(format!(
        "word {i} of {}/{}: rust {:?}, vesper {:?}",
        a.len(),
        b.len(),
        &a[from..(i + 6).min(a.len())],
        &b[from..(i + 6).min(b.len())]
    ))
}

/// The corpus files (packages are several sources; the package declines
/// none of them yet but reads one source) with their text.
fn corpus_files() -> Vec<(&'static Case, String)> {
    corpus()
        .iter()
        .filter(|c| !c.name.ends_with('/'))
        .filter_map(|c| {
            let source = std::fs::read_to_string(repo().join(&c.name)).ok()?;
            Some((c, source))
        })
        .collect()
}

#[test]
fn small_sources_match_rust() {
    let sources = [
        "fn f(x: u32) -> u32 { x + 1 }",
        "struct P { x: i64, y: i64 } fn g(p: P) -> i64 { p.x * p.y }",
        "enum E { A, B(u8) } fn h(e: E) -> u8 { match e { E::A => 0, E::B(v) => v } }",
        "effect Log { fn log(x: u64) -> u64; } fn k() -> u64 eff [Log] { Log::log(1) + 2 }",
        "fn m() -> u64 { let mut s: u64 = 0; for i in 0..10 { s += i; } s }",
    ];
    for source in sources {
        let module = vesper::parser::parse(source).module;
        let vesper = vesper_input(source).expect("declined");
        let rust = rust_input(&module);
        assert_eq!(stream_difference(&rust, &vesper), None, "{source}");
    }
}

#[test]
fn typing_input_matches_rust() {
    let _quiet = quiet_panics();
    let (mut matched, mut declined) = (0usize, 0usize);
    let mut differ = vec![];
    for (c, source) in corpus_files() {
        let Some(vesper) = vesper_input(&source) else {
            declined += 1;
            continue;
        };
        let rust = rust_input(&c.module);
        match stream_difference(&rust, &vesper) {
            None => matched += 1,
            Some(d) => differ.push(format!("{}: {d}", c.name)),
        }
    }
    eprintln!(
        "typing input: {matched} match, {declined} declined, {} differ",
        differ.len()
    );
    for d in differ.iter().take(12) {
        eprintln!("  {d}");
    }
    assert!(differ.is_empty());
    assert!(matched > declined);
}

#[test]
fn typing_over_vesper_input_matches_rust() {
    let _quiet = quiet_panics();
    let f = typing_entry();
    let (mut worlds, mut bad, mut over_vesper, mut over_rust) = (0, 0, 0, 0);
    let mut shown = 0;
    for (c, source) in corpus_files() {
        let Some(input) = vesper_input(&source) else {
            continue;
        };
        worlds += 1;
        let (v, checked, _) = vesper_types(&f.text(&input));
        over_vesper += checked.len();
        let (_, rust_checked, _) = vesper_types(&f.text(&exprs_input(&c.world, &c.module)));
        over_rust += rust_checked.len();
        // Every function checked over the Vesper input is checked over the
        // Rust input too, with the same types. The Vesper input has no
        // inferred results yet, so a call whose callee's result the body
        // refines (a returned closure's latent labels) is skipped there.
        let difference = if let Some(f) = checked.iter().find(|f| !rust_checked.contains(f)) {
            Some(format!("function {f:?} checked over the Vesper input only"))
        } else {
            first_difference(&rust_types(&c.world, &checked), &v)
        };
        if let Some(d) = difference {
            bad += 1;
            if shown < 8 {
                shown += 1;
                eprintln!("typing: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!(
        "typing over the Vesper input: {bad} of {worlds} worlds differ; {over_vesper} functions \
         checked (over the Rust input {over_rust})"
    );
    assert_eq!(bad, 0);
    assert!(over_vesper > 0);
}
