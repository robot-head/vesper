//! `@vesper/native-clif-opt` (packages/vesper-native-clif-opt) against the
//! steps of `native_clif_opt::run` (#2362).
//!
//! Every function the native optimize pipeline hands `native_clif_opt::run`
//! over the corpus `main`s is captured before the pass
//! (`native_clif_opt::capture`). The Rust steps of `native_clif_opt::STEPS`
//! then run on it in order. Before each step the Vesper package has ported,
//! the function is also read in the words of tests/common/clif_words.rs and
//! the package's edit script for that step is replayed on a copy
//! (`clif_words::apply`). The printed functions after the step must be
//! equal.
//!
//! - `NATIVE_CLIF_OPT_STRIDE=n` keeps every n-th corpus world (default 1).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/clif_opt_programs.rs"]
mod clif_opt_programs;
#[path = "common/clif_words.rs"]
mod clif_words;
#[path = "common/vector_programs.rs"]
mod vector_programs;

use check_harness::*;
use cranelift_codegen::ir;
use std::sync::OnceLock;
use vesper::{check, native, native_clif_opt, parser};

/// The step codes of the package (`STEP_*`), by `STEPS` name; 0 for a step
/// not ported yet.
const PORTED: &[(&str, u64)] = &[
    ("remove_unreachable", 1),
    ("float_constants", 2),
    ("fold_constant_branches", 3),
    ("shrink_unused_slots", 4),
    ("merge_blocks", 5),
    ("remove_constant_params", 6),
    ("fold_narrow_extends", 7),
    ("fold_trivial_arith_and_bool_extends", 8),
    ("split_join_slots", 9),
    ("promote_slots", 10),
    ("vector_registers", 11),
    ("remove_dead", 12),
    ("sink_slot_inits", 13),
    ("share_invariant_loads", 14),
    ("hoist_loop_invariants", 15),
    ("merge_tails", 16),
    ("eliminate_guarded_loop_add_overflows", 17),
    ("reuse_seq_header_loads", 18),
    ("sink_compares", 32),
    ("thread_branches", 33),
    ("thread_decided_branches", 34),
    ("thread_constant_param_jumps", 35),
    ("propagate_branch_conditions", 36),
    ("canonicalize_imm_ops", 37),
    ("fold_load_store_offsets", 38),
    ("cse_dom_pure", 39),
];

/// The package's answer word for a step it ran.
const ANSWER_PORTED: u64 = 1;
/// The step that reads the shapes section (`clif_words::shapes`).
const MERGE_TAILS_CODE: u64 = 16;

/// How many differing functions the differential prints.
const MAX_REPORTED: usize = 6;

/// Each step the package has ported must change at least this many
/// functions over the corpus.
const MIN_CHANGES_PER_STEP: usize = 1;

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| Package::load("packages/vesper-native-clif-opt").entry("clif_opt_words", true))
}

/// The `PORTED` index and step code of the step `name`.
fn step_code(name: &str) -> Option<(usize, u64)> {
    PORTED
        .iter()
        .position(|(n, _)| *n == name)
        .map(|at| (at, PORTED[at].1))
}

/// `f` after the Vesper step `code`.
fn through_vesper(f: &ir::Function, code: u64) -> Result<ir::Function, String> {
    let mut input = code.to_le_bytes().to_vec();
    input.extend(clif_words::encode(f, false));
    if code == MERGE_TAILS_CODE {
        input.extend(clif_words::shapes(f));
    }
    let words = entry().words(&input);
    let (&answer, script) = words.split_first().ok_or("no answer")?;
    if answer != ANSWER_PORTED {
        return Err(format!("step {code} is not ported"));
    }
    let mut g = f.clone();
    clif_words::apply(&mut g, script)?;
    Ok(g)
}

/// How often each ported step (by `PORTED` index) ran, and changed the
/// function.
#[derive(Default)]
struct Runs {
    ran: Vec<usize>,
    changed: Vec<usize>,
}

/// Runs every step on `f`, comparing the ported ones; counts them in
/// `runs` and pushes the differences on `bad`.
fn compare_steps(f: &ir::Function, runs: &mut Runs, bad: &mut Vec<String>) {
    let mut rust = f.clone();
    for (k, (name, step)) in native_clif_opt::STEPS.iter().enumerate() {
        let Some((at, code)) = step_code(name) else {
            step(&mut rust);
            continue;
        };
        let vesper = through_vesper(&rust, code);
        let before = rust.display().to_string();
        step(&mut rust);
        let after = rust.display().to_string();
        runs.ran[at] += 1;
        if after != before {
            runs.changed[at] += 1;
        }
        let diff = match vesper {
            Err(e) => Some(e),
            Ok(g) => first_difference(&after, &g.display().to_string()),
        };
        if let Some(d) = diff {
            bad.push(format!("step {k} ({name}): {d}"));
        }
    }
}

/// The vector programs, and the programs of tests/native_clif_opt.rs that
/// exercise particular steps, added to the corpus by name.
const EXTRA_PROGRAMS: &[(&str, &str)] = &[
    ("vector acc4", vector_programs::ACC4),
    ("vector tick", vector_programs::TICK),
    ("vector ramp", vector_programs::RAMP),
    ("vector sums", vector_programs::SUMS),
    ("guarded add", clif_opt_programs::GUARDED_ADD),
    ("guarded add const", clif_opt_programs::GUARDED_ADD_CONST),
    ("guarded join param", clif_opt_programs::GUARDED_JOIN_PARAM),
    ("seq try_set", clif_opt_programs::SEQ_TRY_SET),
    (
        "seq try_get diamond",
        clif_opt_programs::SEQ_TRY_GET_DIAMOND,
    ),
];

/// The checked world of an extra program.
fn extra_world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

fn stride() -> usize {
    std::env::var("NATIVE_CLIF_OPT_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

#[test]
fn corpus_matches_native_clif_opt_steps() {
    let start = std::time::Instant::now();
    let cases = corpus();
    let (mut funcs, mut bad_funcs) = (0, 0);
    let mut runs = Runs {
        ran: vec![0; PORTED.len()],
        changed: vec![0; PORTED.len()],
    };
    // The corpus mains have no vector slots or bounds-hoisted copy loops;
    // the extra programs do.
    let vectors: Vec<(String, check::World)> = EXTRA_PROGRAMS
        .iter()
        .map(|&(name, source)| (name.to_string(), extra_world(source)))
        .collect();
    let worlds = cases
        .iter()
        .step_by(stride())
        .map(|case| (case.name.as_str(), &case.world))
        .chain(vectors.iter().map(|(name, world)| (name.as_str(), world)));
    for (name, world) in worlds {
        let (_, captured) =
            native_clif_opt::capture(|| native::object_with_options(world, "main", None, true));
        for (n, f) in captured.iter().enumerate() {
            funcs += 1;
            let mut bad = Vec::new();
            compare_steps(f, &mut runs, &mut bad);
            if !bad.is_empty() {
                bad_funcs += 1;
                if bad_funcs <= MAX_REPORTED {
                    eprintln!("native clif opt: {name} function {n}: {}", bad[0]);
                }
            }
        }
    }
    eprintln!(
        "native clif opt corpus: {bad_funcs} differ over {funcs} functions of {} worlds; \
         steps ran {:?}, changed {:?}; {:.1?}",
        cases.len().div_ceil(stride()) + EXTRA_PROGRAMS.len(),
        runs.ran,
        runs.changed,
        start.elapsed()
    );
    println!("flip-gate differential differences={bad_funcs} cases={funcs}");
    assert_eq!(bad_funcs, 0);
    if stride() == 1 {
        for (&(name, _), &n) in PORTED.iter().zip(&runs.changed) {
            assert!(
                n >= MIN_CHANGES_PER_STEP,
                "{name} changed only {n} functions"
            );
        }
    }
}
