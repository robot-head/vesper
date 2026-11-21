//! Self-hosting SH3 lane 12 (#1251): `@vesper/bounds-hoist` against
//! `src/bounds_hoist.rs` over the corpus.
//!
//! Every corpus world is lowered through the native optimize pipeline up to
//! bounds hoisting (`ffi_lowering::drops::lower_world`, `ir_opt::optimize`,
//! `ir_inline::inline`, `fip::elaborate_non_fip_drops`). For each function,
//! `bounds_hoist::hoist_function_traced` reports every decision the pass
//! makes, on the function as it stands before the pass applies it: the
//! accesses comparisons prove in bounds, the counted loops in the order they
//! are tried, each counted loop's plan (or none), each compared loop's plan,
//! and the checks a checked access dominates. The Vesper port decides each
//! step on the same IR (tests/self_host_bounds_hoist/words.rs encodes it)
//! and must print the same text. `stable_views` and `header_hoists`, which
//! the native backend reads after the pass, are compared on the pass's input
//! and on its output.
//!
//! The rewrite (loop versioning, `SeqOp::Hoisted`) stays in Rust until lane
//! 1's IR writer (#1259) lands; each step's input is Rust's rewrite of the
//! previous one, so every decision on every intermediate IR is covered.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "self_host_bounds_hoist/words.rs"]
mod words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::bounds_hoist::{self, Step, StepKind};
use vesper::ir::{FunctionIr, Program};
use words::Ask;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-bounds-hoist"))
}

/// A corpus world's program at the bounds-hoisting boundary of the native
/// optimize pipeline (`native::compile_with` before `bounds_hoist::run`).
fn at_hoist(world: &vesper::check::World) -> Option<Program> {
    let mut program = vesper::ffi_lowering::drops::lower_world(world);
    vesper::ir_opt::optimize(world, &mut program).ok()?;
    vesper::ir_inline::inline(world, &mut program, &vesper::ir_inline::Budget::default());
    vesper::fip::elaborate_non_fip_drops(world, &mut program);
    Some(program)
}

thread_local! {
    /// Set while this thread lowers a world: its panics are expected.
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs, once, a panic hook that is silent on a thread lowering a world
/// and the previous hook everywhere else, so parallel tests never race on
/// swapping hooks.
fn quiet_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.get() {
                previous(info);
            }
        }));
    });
}

/// The corpus programs at the hoisting boundary, by world name, every
/// `stride`th world; each world is lowered once, on first use.
fn programs(stride: usize) -> Vec<(&'static str, &'static Program)> {
    static P: OnceLock<Vec<OnceLock<Option<Program>>>> = OnceLock::new();
    let slots = P.get_or_init(|| corpus().iter().map(|_| OnceLock::new()).collect());
    let mut out = vec![];
    for (c, slot) in corpus().iter().zip(slots).step_by(stride) {
        let p = slot.get_or_init(|| {
            quiet_panics();
            QUIET.set(true);
            let p = std::panic::catch_unwind(|| at_hoist(&c.world))
                .ok()
                .flatten();
            QUIET.set(false);
            p
        });
        if let Some(p) = p {
            out.push((c.name.as_str(), p));
        }
    }
    out
}

/// Every step of the pass on `f`, with the function each was decided on.
fn steps(f: &FunctionIr) -> (Vec<(FunctionIr, Step)>, FunctionIr) {
    let mut f = f.clone();
    let mut out = vec![];
    bounds_hoist::hoist_function_traced(&mut f, &mut |g, s| out.push((g.clone(), s)));
    (out, f)
}

#[derive(Default)]
struct Tally {
    functions: usize,
    steps: usize,
    differ: usize,
    /// Steps whose Rust decision is not empty or `none`, by kind.
    decided: [usize; 7],
}

fn decided(kind: usize, decision: &str) -> bool {
    match kind {
        2 => decision != "none",
        3 => !decision.starts_with("none"),
        5 | 6 => !decision.is_empty(),
        _ => !decision.is_empty(),
    }
}

fn compare(
    t: &mut Tally,
    entry: &Entry,
    label: &str,
    kind: usize,
    ask: &Ask,
    f: &FunctionIr,
    rust: &str,
) {
    let input = words::input(ask, f);
    let vesper = entry.text(&input);
    t.steps += 1;
    if decided(kind, rust) {
        t.decided[kind] += 1;
    }
    if vesper != rust {
        t.differ += 1;
        if t.differ <= 8 {
            eprintln!("bounds hoist: {label} differs:\n  rust:   {rust:?}\n  vesper: {vesper:?}");
        }
    }
}

fn differential(entry: &Entry, stride: usize) -> Tally {
    let mut t = Tally::default();
    let start = std::time::Instant::now();
    let programs = programs(stride);
    for (world, program) in &programs {
        for (name, f) in &program.functions {
            t.functions += 1;
            let views = words::hoists_text(&bounds_hoist::stable_views(f));
            compare(
                &mut t,
                entry,
                &format!("{world} {name} stable_views"),
                5,
                &Ask::StableViews,
                f,
                &views,
            );
            let hoists = words::hoists_text(&bounds_hoist::header_hoists(f));
            compare(
                &mut t,
                entry,
                &format!("{world} {name} header_hoists"),
                6,
                &Ask::HeaderHoists,
                f,
                &hoists,
            );
            let (steps, out) = steps(f);
            for (g, s) in &steps {
                let kind = words::kind_word(s.kind) as usize;
                let label = format!("{world} {name} {:?} {}", s.kind, s.header);
                compare(&mut t, entry, &label, kind, &Ask::Step(s), g, &s.decision);
            }
            let views = words::hoists_text(&bounds_hoist::stable_views(&out));
            compare(
                &mut t,
                entry,
                &format!("{world} {name} stable_views after"),
                5,
                &Ask::StableViews,
                &out,
                &views,
            );
            let hoists = words::hoists_text(&bounds_hoist::header_hoists(&out));
            compare(
                &mut t,
                entry,
                &format!("{world} {name} header_hoists after"),
                6,
                &Ask::HeaderHoists,
                &out,
                &hoists,
            );
        }
    }
    eprintln!(
        "bounds hoist: {} of {} steps differ over {} functions of {} programs ({:.1} s); decided: compared {}, counted {}, compared loops {}, dominated {}, stable views {}, header hoists {}",
        t.differ,
        t.steps,
        t.functions,
        programs.len(),
        start.elapsed().as_secs_f64(),
        t.decided[0],
        t.decided[2],
        t.decided[3],
        t.decided[4],
        t.decided[5],
        t.decided[6],
    );
    t
}

fn stride() -> usize {
    std::env::var("BOUNDS_HOIST_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(12)
}

/// The traced pass is the pass: tracing changes nothing it does (every 14th
/// program).
#[test]
fn traced_pass_matches_the_pass() {
    for (world, program) in programs(14) {
        for (name, f) in &program.functions {
            let mut plain = f.clone();
            let n = bounds_hoist::hoist_function(&mut plain);
            let (steps, traced) = steps(f);
            assert_eq!(
                format!("{plain:?}"),
                format!("{traced:?}"),
                "{world} {name}"
            );
            let versioned = steps
                .iter()
                .filter(|(_, s)| {
                    (s.kind == StepKind::Counted && s.decision != "none")
                        || (s.kind == StepKind::ComparedLoop && !s.decision.starts_with("none"))
                })
                .count();
            assert_eq!(n, versioned, "{world} {name}");
        }
    }
}

/// Every decision of the pass, and the stable views and header hoists of
/// its input and output, over every 12th corpus program
/// (`BOUNDS_HOIST_STRIDE=1` for the whole corpus, about an hour in debug).
#[test]
fn analyses_match_rust() {
    let entry = package().entry("decide_text", true);
    let t = differential(&entry, stride());
    assert!(
        t.decided[2] > 0 && t.decided[6] > 0,
        "the corpus hoisted nothing"
    );
    assert_eq!(t.differ, 0);
}

/// The optimizer off agrees with it on (every 9th program).
#[test]
fn analyses_unoptimized_match_rust() {
    let entry = package().entry("decide_text", false);
    let t = differential(&entry, 9 * stride());
    assert_eq!(t.differ, 0);
}

/// ABBA timing (`CHECK_HARNESS_SPEED=1 cargo test --release --test
/// self_host_bounds_hoist timing -- --ignored --nocapture`): the whole Rust
/// pass against the Vesper port making every one of its decisions on the
/// same inputs (encoded once, before timing; the decode is inside the
/// Vesper side), and `stable_views` plus `header_hoists` alone.
#[test]
#[ignore]
fn timing() {
    if !speed() {
        return;
    }
    let entry = package().entry("decide_text", true);
    let functions: Vec<&FunctionIr> = programs(1)
        .into_iter()
        .flat_map(|(_, p)| p.functions.values())
        .collect();
    let mut step_inputs = vec![];
    let mut hoist_inputs = vec![];
    let start = std::time::Instant::now();
    for f in &functions {
        let (steps, out) = steps(f);
        for (g, s) in &steps {
            step_inputs.push(words::input(&Ask::Step(s), g));
        }
        hoist_inputs.push(words::input(&Ask::StableViews, &out));
        hoist_inputs.push(words::input(&Ask::HeaderHoists, &out));
    }
    let encode = start.elapsed();
    let bytes: usize = step_inputs.iter().chain(&hoist_inputs).map(Vec::len).sum();
    eprintln!(
        "bounds hoist: {} functions, {} step inputs, {} MB encoded in {encode:?}",
        functions.len(),
        step_inputs.len(),
        bytes >> 20
    );
    let outputs: Vec<FunctionIr> = functions.iter().map(|f| steps(f).1).collect();
    abba(
        "bounds hoist pass (Rust rewrite included) vs Vesper decisions",
        &mut || {
            let mut copies: Vec<FunctionIr> = functions.iter().map(|f| (*f).clone()).collect();
            time(|| {
                for f in &mut copies {
                    bounds_hoist::hoist_function(f);
                }
            })
        },
        &mut || {
            time(|| {
                for i in &step_inputs {
                    std::hint::black_box(entry.words(i));
                }
            })
        },
    );
    abba(
        "stable_views + header_hoists",
        &mut || {
            time(|| {
                for f in &outputs {
                    std::hint::black_box(bounds_hoist::stable_views(f));
                    std::hint::black_box(bounds_hoist::header_hoists(f));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &hoist_inputs {
                    std::hint::black_box(entry.words(i));
                }
            })
        },
    );
}
