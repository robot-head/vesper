//! Self-hosting SH2 lane 8 (#1180): `@vesper/check-effects`, the capability
//! effect rules of `src/capability_effects.rs` in Vesper, against the Rust
//! `check_world` over the corpus, the capability checker fixtures, and
//! mutants that turn the capabilities on for every function so that each
//! diagnostic of the module fires.
//!
//! Both sides write one text line per diagnostic (code, span, message, help
//! and related labels) and the texts must be equal. The task words are the
//! world's functions with their bodies, its effect declarations and its
//! call instantiations (`encode_task`; the decoder is `read_prog` in
//! `packages/vesper-check-effects/src/prog.ves`).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/effects_words.rs"]
mod effects_words;

use check_harness::*;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::OnceLock;
use vesper::ast::{Diagnostic, Effect, Effects};
use vesper::check::World;

// ------------------------------------------------------------------ encoder

fn input(world: &World) -> Vec<u8> {
    effects_words::encode_world(world)
}

// ------------------------------------------------------------------ texts

fn diag_line(out: &mut String, d: &Diagnostic) {
    let _ = writeln!(
        out,
        "{} {}..{} {} | {} | {}",
        d.code,
        d.span.start,
        d.span.end,
        d.message,
        d.help.as_deref().unwrap_or("-"),
        d.related.len()
    );
    for l in &d.related {
        let _ = writeln!(out, "  {}..{} {}", l.span.start, l.span.end, l.message);
    }
}

fn rust_text(world: &World) -> String {
    let mut out = String::new();
    for d in vesper::capability_effects::check_world(world) {
        diag_line(&mut out, &d);
    }
    out
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-effects"))
}

fn entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("check_effects", optimize))
}

fn vesper_text(world: &World, optimize: bool) -> String {
    entry(optimize).text(&input(world))
}

// ------------------------------------------------------------------ cases

/// The capability checker's fixtures and every other fixture that parses,
/// checked whatever their diagnostics (the corpus keeps only clean worlds,
/// and the rejected fixtures are what make the module's diagnostics fire).
fn fixture_worlds() -> &'static [(String, World)] {
    static CASES: OnceLock<Vec<(String, World)>> = OnceLock::new();
    CASES.get_or_init(|| {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|e| e == "ves") {
                    out.push(p);
                }
            }
        }
        let mut files = vec![];
        for d in [
            "capability_effects",
            "capability_verifiers",
            "floats",
            "vector",
        ] {
            walk(&repo().join("tests/fixtures").join(d), &mut files);
        }
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut out = vec![];
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let r = std::panic::catch_unwind(|| {
                let parsed = vesper::parser::parse(&src);
                parsed
                    .diagnostics
                    .is_empty()
                    .then(|| vesper::check::check(&parsed.module).world)
            });
            if let Ok(Some(w)) = r {
                out.push((
                    f.strip_prefix(repo()).unwrap_or(&f).display().to_string(),
                    w,
                ));
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

/// The capability mutants: every function and effect operation of the
/// world carries `caps` in its row (after its own effects).
fn mutant(world: &World, caps: &[&str]) -> World {
    let mut w = world.clone();
    let add = |e: &mut Effects| {
        for c in caps {
            if !e.instances.iter().any(|i| i.name == *c) {
                e.instances.push(Effect {
                    label: None,
                    name: c.to_string(),
                    args: vec![],
                });
            }
        }
    };
    for f in w.functions.values_mut() {
        add(&mut f.effects);
    }
    w
}

pub const MUTANTS: [&[&str]; 5] = [
    &["ConstTime"],
    &["Realtime"],
    &["DetFloat"],
    &["DetFloat", "ConstDenorm"],
    &["ConstTime", "Realtime", "DetFloat", "ConstDenorm"],
];

/// Counts the diagnostic codes of a text.
fn codes(text: &str, out: &mut BTreeMap<String, usize>) {
    for l in text.lines() {
        if let Some(code) = l.split(' ').next()
            && code.starts_with('E')
        {
            *out.entry(code.to_string()).or_default() += 1;
        }
    }
}

fn compare(label: &str, cases: &[(String, &World)], optimize: bool) -> usize {
    let mut bad = 0;
    let mut lines = 0;
    let mut seen = BTreeMap::new();
    for (name, w) in cases {
        let r = rust_text(w);
        let v = vesper_text(w, optimize);
        lines += r.lines().count();
        codes(&r, &mut seen);
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 5 {
                eprintln!("{label}: {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "{label}: {bad} of {} worlds differ ({lines} diagnostics: {seen:?})",
        cases.len()
    );
    bad
}

// ------------------------------------------------------------------ tests

#[test]
fn effects_match_rust_on_the_corpus() {
    let cases: Vec<(String, &World)> = corpus()
        .iter()
        .map(|c| (c.name.clone(), &c.world))
        .collect();
    assert!(cases.len() > 100, "{} corpus worlds", cases.len());
    assert_eq!(compare("corpus", &cases, true), 0);
}

#[test]
fn effects_match_rust_on_the_checker_fixtures() {
    let cases: Vec<(String, &World)> = fixture_worlds()
        .iter()
        .map(|(n, w)| (n.clone(), w))
        .collect();
    assert!(cases.len() > 30, "{} fixture worlds", cases.len());
    let mut seen = BTreeMap::new();
    for (_, w) in &cases {
        codes(&rust_text(w), &mut seen);
    }
    // The rejected fixtures make the module's own diagnostics fire.
    for code in [
        "E0700", "E0702", "E0703", "E0704", "E0705", "E0706", "E0707", "E0708", "E0720", "E0721",
        "E0722", "E0723", "E0730", "E0731",
    ] {
        assert!(seen.contains_key(code), "no fixture fires {code}: {seen:?}");
    }
    assert_eq!(compare("fixtures", &cases, true), 0);
}

#[test]
fn effects_match_rust_on_capability_mutants() {
    let stride = std::env::var("CHECK_EFFECTS_MUTANT_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1usize)
        .max(1);
    let base: Vec<(String, &World)> = corpus()
        .iter()
        .map(|c| (c.name.clone(), &c.world))
        .chain(fixture_worlds().iter().map(|(n, w)| (n.clone(), w)))
        .step_by(stride)
        .collect();
    let mut bad = 0;
    for (m, caps) in MUTANTS.iter().enumerate() {
        let worlds: Vec<(String, World)> = base
            .iter()
            .map(|(n, w)| (format!("{n}#m{}", m + 1), mutant(w, caps)))
            .collect();
        let cases: Vec<(String, &World)> = worlds.iter().map(|(n, w)| (n.clone(), w)).collect();
        bad += compare(&format!("mutant {caps:?}"), &cases, true);
    }
    assert_eq!(bad, 0);
}

#[test]
fn jit_without_the_optimizer_agrees() {
    let cases: Vec<(String, &World)> = fixture_worlds()
        .iter()
        .map(|(n, w)| (n.clone(), w))
        .collect();
    assert_eq!(compare("fixtures (optimizer off)", &cases, false), 0);
}

#[test]
#[ignore = "ABBA timing: CHECK_HARNESS_SPEED=1 cargo test --release -- --ignored"]
fn effects_abba() {
    if !speed() {
        return;
    }
    let worlds: Vec<World> = corpus()
        .iter()
        .map(|c| c.world.clone())
        .chain(fixture_worlds().iter().map(|(_, w)| w.clone()))
        .collect();
    let all = mutant_set(&worlds);
    let inputs: Vec<Vec<u8>> = all.iter().map(input).collect();
    let f = entry(true);
    abba(
        "check_world (corpus, fixtures, mutant 5)",
        &mut || {
            time(|| {
                for w in &all {
                    std::hint::black_box(vesper::capability_effects::check_world(w));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.words(i));
                }
            })
        },
    );
    let t = time(|| {
        for w in &all {
            std::hint::black_box(input(w));
        }
    });
    eprintln!("encoding the inputs: {t:?}");
}

fn mutant_set(worlds: &[World]) -> Vec<World> {
    worlds
        .iter()
        .cloned()
        .chain(worlds.iter().map(|w| mutant(w, MUTANTS[4])))
        .collect()
}

/// One side of the differential for callgrind: `CHECK_EFFECTS_SIDE=rust`,
/// `vesper` or `none` (the baseline: corpus, inputs and the compiled entry
/// only). Instructions of a side are its run minus the baseline's.
#[test]
#[ignore = "callgrind: CHECK_EFFECTS_SIDE=rust|vesper|none CHECK_HARNESS_STRIDE=4"]
fn effects_callgrind_side() {
    let Ok(side) = std::env::var("CHECK_EFFECTS_SIDE") else {
        return;
    };
    let worlds: Vec<World> = corpus()
        .iter()
        .map(|c| c.world.clone())
        .chain(fixture_worlds().iter().map(|(_, w)| w.clone()))
        .collect();
    let all = mutant_set(&worlds);
    let inputs: Vec<Vec<u8>> = all.iter().map(input).collect();
    let f = entry(true);
    match side.as_str() {
        "rust" => {
            for w in &all {
                std::hint::black_box(vesper::capability_effects::check_world(w));
            }
        }
        "vesper" => {
            for i in &inputs {
                std::hint::black_box(f.words(i));
            }
        }
        _ => {}
    }
    eprintln!("callgrind side {side}: {} worlds", all.len());
}
