//! SH4 lane 5 (#1279): the native-path optimizer policy
//! (`src/ir_opt_policy.rs`) gives exactly what `ir_opt::optimize` gives.
//!
//! - `policy_matches_optimize_on_the_corpus`: every corpus world's native
//!   input, optimized by `ir_opt::optimize` and by the policy (twice: the
//!   second call is served by the content-hash cache). The programs must be
//!   `Debug`-equal and the counts equal. Each world's second call is served
//!   by the cache (turned on here; it is off by default), which is also
//!   shared across worlds. It also checks that no function the screen rules
//!   out loses an instruction under `ir_opt::optimize`.
//! - `profile_package` (ignored): the per-function profile and the policy's
//!   timing for one package (`OPTIMIZER_BUDGET_PACKAGE`, a directory whose
//!   root defines `check_all`), used by
//!   docs/parallel-results/self-host-optimizer-budget.md.
#[path = "common/check_harness.rs"]
mod check_harness;

use vesper::{ir_opt, ir_opt_policy};

/// Every n-th corpus world (`OPTIMIZER_BUDGET_STEP`, default 1 in release
/// builds and 12 in debug ones, where egglog is slow).
fn worlds() -> impl Iterator<Item = &'static check_harness::Case> {
    let step = std::env::var("OPTIMIZER_BUDGET_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 12 } else { 1 })
        .max(1);
    check_harness::corpus().iter().step_by(step)
}

#[test]
fn policy_matches_optimize_on_the_corpus() {
    ir_opt_policy::set_cache(true);
    let corpus: Vec<_> = worlds().collect();
    assert!(!corpus.is_empty());
    let (mut functions, mut screened, mut cached, mut improved) = (0, 0, 0, 0);
    let mut failing = 0;
    for case in &corpus {
        let input = ir_opt_policy::native_input(&case.world);
        let mut reference = input.clone();
        let expected = ir_opt::optimize(&case.world, &mut reference);
        if expected.is_ok() {
            // The screen never drops an improvement: a function with no
            // candidate block keeps every instruction under the pass.
            for (name, f) in &input.functions {
                if ir_opt_policy::candidate_blocks(f) == 0 {
                    let dests = |f: &vesper::ir::FunctionIr| -> Vec<Vec<vesper::ir::ValueId>> {
                        f.blocks
                            .iter()
                            .map(|b| b.instructions.iter().map(|i| i.dest).collect())
                            .collect()
                    };
                    assert_eq!(
                        dests(f),
                        dests(&reference.functions[name]),
                        "{}: {name} was screened out but loses instructions",
                        case.name
                    );
                }
            }
        }
        for round in 0..2 {
            let mut program = input.clone();
            let got = ir_opt_policy::optimize_with_stats(&case.world, &mut program);
            match (&expected, &got) {
                (Ok(n), Ok(stats)) => {
                    assert_eq!(*n, stats.eliminated, "{} round {round}: count", case.name);
                    assert_eq!(
                        format!("{reference:?}"),
                        format!("{program:?}"),
                        "{} round {round}: program",
                        case.name
                    );
                    if round == 0 {
                        functions += stats.functions;
                        screened += stats.screened_out;
                        improved += stats.improved;
                    } else {
                        cached += stats.cached;
                        assert_eq!(stats.saturated, 0, "{}: second round is cached", case.name);
                    }
                }
                (Err(_), Err(_)) => {
                    if round == 0 {
                        failing += 1;
                    }
                }
                _ => panic!(
                    "{} round {round}: optimize {:?} vs policy {:?}",
                    case.name,
                    expected.as_ref().map(|_| ()),
                    got.as_ref().map(|_| ())
                ),
            }
        }
    }
    println!(
        "{} worlds ({failing} rejected by both), {functions} functions: {screened} screened out, {improved} improved, {cached} served from the cache on the second call",
        corpus.len()
    );
    assert!(screened > 0);
}

#[test]
#[ignore = "measurement; set OPTIMIZER_BUDGET_PACKAGE"]
fn profile_package() {
    use std::time::Instant;
    let Ok(dir) = std::env::var("OPTIMIZER_BUDGET_PACKAGE") else {
        return;
    };
    ir_opt_policy::set_cache(true);
    let assembled = vesper::packages::load(std::path::Path::new(&dir)).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let c = vesper::check::check(&assembled.module);
    assert!(c.diagnostics.is_empty(), "{:?}", &c.diagnostics[..1]);
    let input = ir_opt_policy::native_input(&c.world);

    let mut whole = input.clone();
    let t = Instant::now();
    let n = ir_opt::optimize(&c.world, &mut whole).unwrap();
    let whole_time = t.elapsed();

    let mut cold = input.clone();
    let t = Instant::now();
    let stats = ir_opt_policy::optimize_with_stats(&c.world, &mut cold).unwrap();
    let cold_time = t.elapsed();
    assert_eq!(n, stats.eliminated);
    assert_eq!(format!("{whole:?}"), format!("{cold:?}"));

    let mut warm = input.clone();
    let t = Instant::now();
    let again = ir_opt_policy::optimize_with_stats(&c.world, &mut warm).unwrap();
    let warm_time = t.elapsed();
    assert_eq!(format!("{whole:?}"), format!("{warm:?}"));

    let rows = ir_opt_policy::profile(&c.world, &input).unwrap();
    let total: u128 = rows.iter().map(|r| r.micros).sum();
    let screened: u128 = rows
        .iter()
        .filter(|r| r.candidate_blocks == 0)
        .map(|r| r.micros)
        .sum();
    let improved: Vec<_> = rows.iter().filter(|r| r.instructions_removed > 0).collect();
    let improved_time: u128 = improved.iter().map(|r| r.micros).sum();
    let wasted: u128 = rows
        .iter()
        .filter(|r| r.candidate_blocks > 0 && r.instructions_removed == 0)
        .map(|r| r.micros)
        .sum();
    println!(
        "{dir}: {} functions, {} blocks ({} pass the screen), ir_opt::optimize whole {whole_time:.2?}, policy cold {cold_time:.2?}, warm {warm_time:.2?}",
        stats.functions, stats.blocks, stats.candidate_blocks
    );
    println!("policy cold: {stats:?}");
    println!("policy warm: {again:?}");
    println!(
        "per-function sum {:.2} s: screened out {:.2} s, saturated and improved ({} fns) {:.2} s, saturated without change {:.2} s; {} reads removed",
        total as f64 / 1e6,
        screened as f64 / 1e6,
        improved.len(),
        improved_time as f64 / 1e6,
        wasted as f64 / 1e6,
        improved
            .iter()
            .map(|r| r.instructions_removed)
            .sum::<usize>()
    );
    let mut slow = rows.clone();
    slow.sort_by_key(|r| std::cmp::Reverse(r.micros));
    for r in slow.iter().take(15) {
        println!(
            "  {:>9} us  {:>5} blocks {:>6} values {:>4} candidates {:>3} removed  {}",
            r.micros, r.blocks, r.values, r.candidate_blocks, r.instructions_removed, r.name
        );
    }
}
