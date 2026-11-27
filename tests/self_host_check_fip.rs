//! Self-hosting SH2 lane 12 (#1184): the fip checks of `src/fip.rs`
//! (`fip::check_world`) in Vesper (`packages/vesper-check-fip`), run
//! differentially against Rust over the corpus of lane 4's harness, the fip
//! checker test sources (`packages/vesper-check-fip/tests/cases.txt`) and
//! mutants of both that make every diagnostic of the module fire. Every
//! diagnostic's code, span, message, help and labels are compared.
//!
//! The input is the harness encoding of the world followed by the fip task
//! section ([`fip_task`]), which `read_ast` in
//! `packages/vesper-check-fip/src/ast.ves` decodes.

#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/fip_words.rs"]
mod fip_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::ast::Pass;
use vesper::check::World;

// ------------------------------------------------------------------ encoder

pub fn fip_input(world: &World) -> Vec<u8> {
    fip_words::encode_world(world)
}

// ------------------------------------------------------------------ texts

/// `fip::check_world` as the Vesper module prints it: one line per
/// diagnostic, then its help and related labels (the fip checks have none,
/// so a difference there is a Rust change the port must follow).
fn rust_text(world: &World) -> String {
    let mut out = String::new();
    for d in vesper::fip::check_world(world) {
        out += &format!("{} {} {} {}\n", d.code, d.span.start, d.span.end, d.message);
        if let Some(h) = &d.help {
            out += &format!("  help {h}\n");
        }
        for l in &d.related {
            out += &format!("  related {} {} {}\n", l.span.start, l.span.end, l.message);
        }
    }
    out
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-fip"))
}

fn entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("check_fip", optimize))
}

fn vesper_text(world: &World, optimize: bool) -> String {
    entry(optimize).text(&fip_input(world))
}

// ------------------------------------------------------------------ cases

/// The fip checker test sources, checked (with their diagnostics: the
/// worlds still hold every function and expression type).
fn checker_cases() -> &'static [(String, World)] {
    static CASES: OnceLock<Vec<(String, World)>> = OnceLock::new();
    CASES.get_or_init(|| {
        let text =
            std::fs::read_to_string(repo().join("packages/vesper-check-fip/tests/cases.txt"))
                .unwrap();
        let mut out = vec![];
        let mut name = String::new();
        let mut body = String::new();
        let flush = |name: &str, body: &str, out: &mut Vec<(String, World)>| {
            if name.is_empty() {
                return;
            }
            let parsed = vesper::parser::parse(body);
            if !parsed.diagnostics.is_empty() {
                return;
            }
            out.push((name.to_string(), check::check(&parsed.module).world));
        };
        for line in text.lines() {
            if let Some(n) = line.strip_prefix("==== ") {
                flush(&name, &body, &mut out);
                name = n.to_string();
                body.clear();
            } else if !name.is_empty() {
                body += line;
                body.push('\n');
            }
        }
        flush(&name, &body, &mut out);
        out
    })
}

use vesper::check;

/// The mutants: 1 every function `@fip`; 2 every function `@fip` with
/// every parameter `sink`; 3 every `@fip` function plain (so its
/// `Box::new_in_caller` calls are E0802).
pub const MUTANTS: usize = 3;

fn mutant(world: &World, m: usize) -> World {
    let mut w = world.clone();
    for f in w.functions.values_mut() {
        match m {
            1 => f.fip = true,
            2 => {
                f.fip = true;
                for p in &mut f.params {
                    p.pass = Pass::Sink;
                }
            }
            _ => f.fip = !f.fip,
        }
    }
    w
}

/// Every (name, world) the differential runs: the corpus, the checker
/// cases, and each one's mutants.
fn all_cases(mutants: bool) -> Vec<(String, World)> {
    case_stream(mutants).collect()
}

/// [`all_cases`] one world at a time, so the differential holds a single
/// clone or mutant beside the corpus instead of four copies of it.
fn case_stream(mutants: bool) -> impl Iterator<Item = (String, World)> {
    let extra = if mutants { MUTANTS } else { 0 };
    corpus()
        .iter()
        .map(|c| (&c.name, &c.world))
        .chain(checker_cases().iter().map(|(n, w)| (n, w)))
        .flat_map(move |(name, w)| {
            (0..=extra).map(move |m| {
                if m == 0 {
                    (name.clone(), w.clone())
                } else {
                    (format!("{name}#m{m}"), mutant(w, m))
                }
            })
        })
}

fn run<W: std::borrow::Borrow<World>>(
    label: &str,
    cases: impl IntoIterator<Item = (String, W)>,
    optimize: bool,
) -> (usize, usize, Vec<String>) {
    let mut bad = 0;
    let mut count = 0;
    let mut diags = 0;
    let mut codes = std::collections::BTreeMap::<String, usize>::new();
    for (name, w) in cases {
        let w = w.borrow();
        count += 1;
        let r = rust_text(w);
        let v = vesper_text(w, optimize);
        diags += r.lines().count();
        for l in r.lines() {
            *codes
                .entry(l.split(' ').next().unwrap_or("").to_string())
                .or_default() += 1;
        }
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 5 {
                eprintln!("{label}: {name} differs at {d}");
            }
        }
    }
    eprintln!("{label}: {bad} of {count} worlds differ ({diags} diagnostics: {codes:?})");
    (bad, diags, codes.keys().cloned().collect())
}

// ------------------------------------------------------------------ tests

#[test]
fn checker_cases_match_rust() {
    let cases: Vec<_> = checker_cases().to_vec();
    assert!(
        cases.len() >= 30,
        "only {} checker cases parse",
        cases.len()
    );
    let (bad, diags, codes) = run("fip cases", cases, true);
    assert_eq!(bad, 0);
    assert!(diags > 0);
    // Every diagnostic of the module fires.
    for code in [
        "E0801", "E0802", "E0804", "E0805", "E0806", "E0807", "E0808", "E0809",
    ] {
        assert!(codes.iter().any(|c| c == code), "{code} never fires");
    }
}

#[test]
fn corpus_and_mutants_match_rust() {
    let (bad, diags, _) = run("fip corpus", case_stream(true), true);
    assert_eq!(bad, 0);
    assert!(diags > 0);
}

#[test]
fn unoptimized_jit_agrees() {
    // The JIT with the optimizer off on the checker cases and their mutants.
    let mut cases = vec![];
    for (n, w) in checker_cases() {
        cases.push((n.clone(), w.clone()));
        for m in 1..=MUTANTS {
            cases.push((format!("{n}#m{m}"), mutant(w, m)));
        }
    }
    let (bad, _, _) = run("fip unoptimized", cases, false);
    assert_eq!(bad, 0);
}

/// ABBA timing of the checks alone (CHECK_HARNESS_SPEED=1): Rust's
/// `check_world` against the Vesper entry on pre-encoded inputs, and the
/// decode-only entry as the encoding baseline.
#[test]
#[ignore]
fn abba_check_fip() {
    if !speed() {
        return;
    }
    for mutants in [false, true] {
        abba_cases(
            &all_cases(mutants),
            if mutants {
                "with mutants"
            } else {
                "corpus and cases"
            },
        );
    }
}

fn abba_cases(cases: &[(String, World)], label: &str) {
    let inputs: Vec<Vec<u8>> = cases.iter().map(|(_, w)| fip_input(w)).collect();
    let count = package().entry("check_fip_count", true);
    let decode = package().entry("check_fip_decode", true);
    let sink = std::cell::Cell::new(0u64);
    let (r, v) = abba(
        &format!("fip check_world, {label}"),
        &mut || {
            time(|| {
                for (_, w) in cases {
                    sink.set(
                        sink.get()
                            .wrapping_add(vesper::fip::check_world(w).len() as u64),
                    );
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    sink.set(sink.get().wrapping_add(count.word(i)));
                }
            })
        },
    );
    // The swap encodes each world itself: the flip gate's workload.
    abba(
        &format!("fip with encoding, {label}"),
        &mut || {
            time(|| {
                for (_, w) in cases {
                    sink.set(
                        sink.get()
                            .wrapping_add(vesper::fip::check_world(w).len() as u64),
                    );
                }
            })
        },
        &mut || {
            time(|| {
                for (_, w) in cases {
                    sink.set(sink.get().wrapping_add(count.word(&fip_input(w))));
                }
            })
        },
    );
    let (_, d) = abba(&format!("fip decode only, {label}"), &mut || r, &mut || {
        time(|| {
            for i in &inputs {
                sink.set(sink.get().wrapping_add(decode.word(i)));
            }
        })
    });
    eprintln!(
        "fip checks without decoding, {label}: vesper {:?} ratio {:.2}",
        v.saturating_sub(d),
        v.saturating_sub(d).as_secs_f64() / r.as_secs_f64()
    );
    let enc = time(|| {
        for (_, w) in cases {
            sink.set(sink.get().wrapping_add(fip_input(w).len() as u64));
        }
    });
    eprintln!("encoding all inputs, {label}: {enc:?} ({})", sink.get());
}

/// One side once, for callgrind (`CHECK_FIP_SIDE=rust|vesper|none`, with
/// `CHECK_HARNESS_STRIDE` to shrink the corpus): the inputs are encoded and
/// the entry compiled before the side runs, so `none` is the baseline both
/// sides subtract.
#[test]
#[ignore]
fn callgrind_side() {
    let Ok(side) = std::env::var("CHECK_FIP_SIDE") else {
        return;
    };
    let cases = all_cases(true);
    let inputs: Vec<Vec<u8>> = cases.iter().map(|(_, w)| fip_input(w)).collect();
    let count = package().entry("check_fip_count", true);
    let mut n = 0u64;
    match side.as_str() {
        "rust" => {
            for (_, w) in &cases {
                n += vesper::fip::check_world(w).len() as u64;
            }
        }
        "vesper" => {
            for i in &inputs {
                n += count.word(i);
            }
        }
        "phases" => {
            use std::time::Instant;
            let (mut tr, mut tw, mut tt, mut tf) = (0f64, 0f64, 0f64, 0f64);
            for (_, w) in &cases {
                let t = Instant::now();
                n += vesper::fip::check_world(w).len() as u64;
                tr += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let mut enc = check_words::Enc::default();
                let ws = enc.world(w);
                tw += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let mut tail = vec![];
                fip_words::fip_task(&mut enc, &mut tail, w);
                tt += t.elapsed().as_secs_f64();
                let t = Instant::now();
                n += enc.finish(&ws, &tail).len() as u64;
                tf += t.elapsed().as_secs_f64();
            }
            eprintln!("rust {tr:.3}s world {tw:.3}s task {tt:.3}s finish {tf:.3}s");
            // The same work as separate passes (the ABBA's shape).
            let t = Instant::now();
            for (_, w) in &cases {
                n += vesper::fip::check_world(w).len() as u64;
            }
            let r = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let inputs: Vec<Vec<u8>> = cases.iter().map(|(_, w)| fip_input(w)).collect();
            let e = t.elapsed().as_secs_f64();
            let t = Instant::now();
            for i in &inputs {
                n += count.word(i);
            }
            let v = t.elapsed().as_secs_f64();
            eprintln!("passes: rust {r:.3}s encode {e:.3}s vesper {v:.3}s");
            let types: usize = cases.iter().map(|(_, w)| w.types.len()).sum();
            let fns: usize = cases.iter().map(|(_, w)| w.functions.len()).sum();
            eprintln!("{} worlds, {types} expression types, {fns} functions", cases.len());
        }
        "encode" => {
            for (_, w) in &cases {
                n += fip_input(w).len() as u64;
            }
        }
        _ => {}
    }
    eprintln!(
        "callgrind side {side}: {} worlds, {n} diagnostics",
        cases.len()
    );
}
