//! Self-hosting SH3 lane 12 (#1251): the `@vesper/bounds-hoist` pass
//! against `bounds_hoist::run` over the corpus.
//!
//! Every corpus world is lowered to the bounds-hoisting boundary of the
//! native optimize pipeline (tests/self_host_bounds_hoist/corpus.rs). The
//! program goes through lane 1's pass harness (`ir_words::compare_pass`):
//! Rust runs `bounds_hoist::run`, the Vesper package reads the program into
//! @vesper/place-ir tables, versions loops, marks accesses unchecked and
//! writes the program back. The two outputs must have the same canonical
//! dump, be `Debug`-equal and get the same IR check diagnostics. The
//! guards and unchecked accesses the port makes are the pool kinds 5 and 6
//! of tests/common/ir_words.rs (`made_payload`).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "self_host_bounds_hoist/corpus.rs"]
mod corpus;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::bounds_hoist;
use vesper::ir::Program;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-bounds-hoist"))
}

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("hoist_pass", true))
}

/// Every `BOUNDS_HOIST_PASS_STRIDE`th corpus world (default 24: about 13
/// programs, about 20 minutes in a debug build with the IR check).
fn stride() -> usize {
    std::env::var("BOUNDS_HOIST_PASS_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24)
}

/// The first function whose output differs, for the failure message.
fn first_function_difference(input: &Program) -> String {
    let mut ours = input.clone();
    bounds_hoist::run(&mut ours);
    let Some(theirs) = ir_words::through_vesper(None, input, |i| entry().words(i)) else {
        return "not encodable".into();
    };
    for (name, f) in &ours.functions {
        match theirs.functions.get(name) {
            None => return format!("{name}: missing"),
            Some(g) if format!("{f:?}") != format!("{g:?}") => {
                let (a, b) = (format!("{f:#?}"), format!("{g:#?}"));
                let line = a
                    .lines()
                    .zip(b.lines())
                    .enumerate()
                    .find(|(_, (x, y))| x != y)
                    .map(|(i, (x, y))| format!("line {i}:\n    rust:   {x}\n    vesper: {y}"))
                    .unwrap_or_else(|| "lengths differ".into());
                return format!("{name}: {line}");
            }
            Some(_) => {}
        }
    }
    "no function differs".into()
}

#[test]
fn pass_matches_bounds_hoist_run() {
    let (mut programs, mut skipped, mut bad, mut versioned, mut changed) = (0, 0, 0, 0, 0);
    let start = std::time::Instant::now();
    for (case, program) in corpus::programs(stride()) {
        let mut probe = program.clone();
        let n = bounds_hoist::run(&mut probe);
        versioned += n;
        changed += (format!("{probe:?}") != format!("{program:?}")) as usize;
        let out = ir_words::compare_pass(
            &case.world,
            program,
            |p| {
                bounds_hoist::run(p);
            },
            |i| entry().words(i),
        );
        if out.skipped {
            skipped += 1;
            continue;
        }
        programs += 1;
        if !out.agrees() {
            bad += 1;
            if bad <= 6 {
                eprintln!(
                    "bounds hoist pass: {} differs ({out:?})\n  {}",
                    case.name,
                    first_function_difference(program)
                );
            }
        }
    }
    eprintln!(
        "bounds hoist pass: {bad} of {programs} programs differ ({skipped} not encodable); \
         Rust versions {versioned} loops and changes {changed} programs; {:.1?}",
        start.elapsed()
    );
    assert!(changed > 0, "the sample has nothing to hoist");
    assert_eq!(bad, 0);
}

/// Wall clock of the pass over every corpus program (`CHECK_HARNESS_SPEED`
/// set; release build): Rust's `bounds_hoist::run` against the Vesper pass
/// with its encoding (Rust -> input bytes -> words -> Rust), on the input
/// bytes alone, and the pass alone (`place_ir_words`, the identity read and
/// write of the same input, subtracted).
#[test]
fn timing() {
    if !speed() {
        return;
    }
    let identity = Package::load("packages/vesper-place-ir").entry("place_ir_words", true);
    let programs: Vec<&Program> = corpus::programs(1).into_iter().map(|(_, p)| p).collect();
    let inputs: Vec<(Vec<u8>, ir_words::Pool)> = programs
        .iter()
        .filter_map(|p| ir_words::program_only_input(p))
        .collect();
    let bytes: usize = inputs.iter().map(|(i, _)| i.len()).sum();
    let instructions: usize = programs.iter().map(|p| ir_words::instructions(p)).sum();
    eprintln!(
        "bounds hoist pass timing: {} programs ({} encodable), {instructions} instructions, {} MB of input",
        programs.len(),
        inputs.len(),
        bytes >> 20
    );
    let copies = || -> Vec<Program> { programs.iter().map(|p| (*p).clone()).collect() };
    abba(
        "bounds hoist: Rust run vs Vesper with encode and decode",
        &mut || {
            let mut c = copies();
            time(|| {
                for p in &mut c {
                    bounds_hoist::run(p);
                }
            })
        },
        &mut || {
            time(|| {
                for p in &programs {
                    std::hint::black_box(ir_words::through_vesper(None, p, |i| entry().words(i)));
                }
            })
        },
    );
    let (rust, words) = abba(
        "bounds hoist: Rust run vs Vesper on input bytes",
        &mut || {
            let mut c = copies();
            time(|| {
                for p in &mut c {
                    bounds_hoist::run(p);
                }
            })
        },
        &mut || {
            time(|| {
                for (i, _) in &inputs {
                    std::hint::black_box(entry().words(i));
                }
            })
        },
    );
    let (_, read_write) = abba(
        "bounds hoist: Rust run vs place-ir identity read and write",
        &mut || {
            let mut c = copies();
            time(|| {
                for p in &mut c {
                    bounds_hoist::run(p);
                }
            })
        },
        &mut || {
            time(|| {
                for (i, _) in &inputs {
                    std::hint::black_box(identity.words(i));
                }
            })
        },
    );
    let alone = words.saturating_sub(read_write);
    eprintln!(
        "bounds hoist: pass alone {alone:?} ({:.2}x of Rust's {rust:?}; tables read and written in {read_write:?})",
        alone.as_secs_f64() / rust.as_secs_f64()
    );
}
