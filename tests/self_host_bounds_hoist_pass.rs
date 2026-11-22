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
    println!("flip-gate differential differences={bad} cases={programs}");
    assert!(changed > 0, "the sample has nothing to hoist");
    assert_eq!(bad, 0);
}

/// The flip gate's ABBA workloads (scripts/ci/flip-gate/bounds-hoist.toml;
/// `CHECK_HARNESS_SPEED` set, release build). Each epoch runs Rust, Vesper,
/// Vesper, Rust over every corpus program and prints one pair per epoch in
/// the gate's `flip-gate pairs` format (`BOUNDS_HOIST_GATE_EPOCHS`, default
/// 12, at least the gate's 6; `BOUNDS_HOIST_GATE_WORKLOAD` runs only one):
/// - `hook`: what the hybrid's `hoist` hook runs (Rust program -> input bytes
///   -> pass -> words -> Rust program) against `bounds_hoist::run`.
/// - `build`: the native optimize pipeline's object of every corpus `main`
///   with the Vesper pass as the `hoist` hook, against Rust's pass.
/// - `pass`: the pass on the input bytes with the place-ir identity read and
///   write of the same input, timed in the same epoch, subtracted. That is
///   the port's own cost once its neighbours are Vesper too.
#[test]
fn flip_gate() {
    if !speed() {
        return;
    }
    let epochs: usize = std::env::var("BOUNDS_HOIST_GATE_EPOCHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let identity = Package::load("packages/vesper-place-ir").entry("place_ir_words", true);
    let programs: Vec<&Program> = corpus::programs(1).into_iter().map(|(_, p)| p).collect();
    let inputs: Vec<(Vec<u8>, ir_words::Pool)> = programs
        .iter()
        .filter_map(|p| ir_words::program_only_input(p))
        .collect();
    let rust = || {
        let mut c: Vec<Program> = programs.iter().map(|p| (*p).clone()).collect();
        time(|| {
            for p in &mut c {
                bounds_hoist::run(p);
            }
        })
        .as_nanos()
    };
    let hook = || {
        time(|| {
            for p in &programs {
                std::hint::black_box(ir_words::through_vesper(None, p, |i| entry().words(i)));
            }
        })
        .as_nanos()
    };
    let on_bytes = |e: &Entry| {
        time(|| {
            for (i, _) in &inputs {
                std::hint::black_box(e.words(i));
            }
        })
        .as_nanos()
    };
    // Warm-up, untimed.
    rust();
    hook();
    on_bytes(entry());
    on_bytes(&identity);
    // `build`: the whole native optimize pipeline (`object_with_options`)
    // of every corpus `main`, with the Vesper pass installed as the `hoist`
    // hook against Rust's `bounds_hoist::run`, in the same process.
    vesper::ir_swap::install(SWITCHED).ok();
    let mains: Vec<&vesper::check::World> = corpus::programs(1)
        .into_iter()
        .map(|(c, _)| c)
        .filter(|c| !c.name.ends_with('/'))
        .filter(|c| {
            c.world
                .functions
                .get("main")
                .is_some_and(|f| f.params.is_empty())
        })
        .map(|c| &c.world)
        .filter(|w| build_once(w))
        .collect();
    let build = |vesper: bool| {
        HOOK_VESPER.store(vesper, std::sync::atomic::Ordering::Relaxed);
        let mut failed = 0usize;
        let t = time(|| {
            for w in &mains {
                failed += usize::from(!std::hint::black_box(build_once(w)));
            }
        })
        .as_nanos();
        HOOK_VESPER.store(false, std::sync::atomic::Ordering::Relaxed);
        // A failed build is not a timing sample: every main that Rust's
        // pass builds must build with the Vesper hook too.
        assert_eq!(
            failed, 0,
            "{failed} corpus mains failed to build (vesper hook: {vesper})"
        );
        t
    };
    let only = std::env::var("BOUNDS_HOIST_GATE_WORKLOAD").ok();
    let wants = |w: &str| only.as_deref().is_none_or(|o| o == w);
    let (mut hook_pairs, mut pass_pairs) = (String::new(), String::new());
    let mut build_pairs = String::new();
    if wants("build") {
        build(false);
        build(true);
    }
    for _ in 0..epochs {
        if wants("build") {
            let (r1, v1, v2, r2) = (build(false), build(true), build(true), build(false));
            build_pairs += &format!(" {}:{}", r1 + r2, v1 + v2);
        }
        if wants("hook") {
            let (r1, v1, v2, r2) = (rust(), hook(), hook(), rust());
            hook_pairs += &format!(" {}:{}", r1 + r2, v1 + v2);
        }
        if wants("pass") {
            let (r1, v1, i1) = (rust(), on_bytes(entry()), on_bytes(&identity));
            let (i2, v2, r2) = (on_bytes(&identity), on_bytes(entry()), rust());
            let pass = (v1 + v2).saturating_sub(i1 + i2);
            pass_pairs += &format!(" {}:{}", r1 + r2, pass);
        }
    }
    if wants("hook") {
        println!("flip-gate pairs hook{hook_pairs}");
    }
    if wants("pass") {
        println!("flip-gate pairs pass{pass_pairs}");
    }
    if wants("build") {
        eprintln!("build workload: {} corpus mains", mains.len());
        println!("flip-gate pairs build{build_pairs}");
    }
}

/// Whether the `hoist` hook runs the Vesper pass (`build` workload).
static HOOK_VESPER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The `hoist` hook of the `build` workload: the Vesper pass while
/// [`HOOK_VESPER`] is set (Rust for a program the words cannot carry, as in
/// the hybrid), else `bounds_hoist::run`.
const SWITCHED: vesper::ir_swap::IrPasses = vesper::ir_swap::IrPasses {
    lower: None,
    opt: None,
    inline: None,
    hoist: Some(|p| {
        if HOOK_VESPER.load(std::sync::atomic::Ordering::Relaxed)
            && let Some(q) = ir_words::through_vesper(None, p, |i| entry().words(i))
        {
            *p = q;
        } else {
            bounds_hoist::run(p);
        }
        0
    }),
};

/// One optimized native object of `world`'s `main`; whether it built.
fn build_once(world: &vesper::check::World) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        vesper::native::object_with_options(world, "main", None, true).is_ok()
    }))
    .unwrap_or(false)
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
