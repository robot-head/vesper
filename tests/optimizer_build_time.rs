//! SH5 lane 9 (#1332): optimizer build time on large packages.
//!
//! - `policy_matches_optimize_on_lowered_worlds`: the wasm backend and
//!   `vesper emit --emit ir` hand the optimizer `ir::lower_world`'s program
//!   (the native paths hand it `drops::lower_world`'s, which lane 5's
//!   tests/optimizer_budget.rs covers). Over the corpus the policy must give
//!   that program exactly what `ir_opt::optimize` gives, so wasm modules and
//!   emitted IR are byte-identical.
//! - `measure_package` (ignored): the policy's phase timings and the
//!   slowest candidate blocks, each saturated alone, for one package
//!   (`OPTIMIZER_BUILD_TIME_PACKAGE`, a package directory;
//!   `OPTIMIZER_BUILD_TIME_PASS=1` also times the unchanged pass and checks
//!   the policy's program equals it, `OPTIMIZER_BUILD_TIME_BLOCKS=1` adds
//!   the per-block profile). Used by
//!   docs/parallel-results/self-host-optimizer-build-time.md.
#[path = "common/check_harness.rs"]
mod check_harness;

use vesper::{ir, ir_opt, ir_opt_policy};

/// Every n-th corpus world (`OPTIMIZER_BUILD_TIME_STEP`, default 1 in
/// release builds and 4 in debug ones, where egglog is slow).
fn worlds() -> impl Iterator<Item = &'static check_harness::Case> {
    let step = std::env::var("OPTIMIZER_BUILD_TIME_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 4 } else { 1 })
        .max(1);
    check_harness::corpus().iter().step_by(step)
}

#[test]
fn policy_matches_optimize_on_lowered_worlds() {
    let (mut worlds, mut functions, mut rejected) = (0, 0, 0);
    for case in self::worlds() {
        let input = ir::lower_world(&case.world);
        let mut reference = input.clone();
        let expected = ir_opt::optimize(&case.world, &mut reference);
        let mut program = input;
        let got = ir_opt_policy::optimize_with_stats(&case.world, &mut program);
        match (&expected, &got) {
            (Ok(n), Ok(stats)) => {
                assert_eq!(*n, stats.eliminated, "{}: count", case.name);
                assert_eq!(
                    serde_json::to_string(&reference).unwrap(),
                    serde_json::to_string(&program).unwrap(),
                    "{}: emitted IR",
                    case.name
                );
                assert_eq!(
                    format!("{reference:?}"),
                    format!("{program:?}"),
                    "{}: program",
                    case.name
                );
                functions += stats.functions;
            }
            (Err(_), Err(_)) => rejected += 1,
            _ => panic!(
                "{}: optimize {:?} vs policy {:?}",
                case.name,
                expected.as_ref().map(|_| ()),
                got.as_ref().map(|_| ())
            ),
        }
        worlds += 1;
    }
    println!("{worlds} worlds ({rejected} rejected by both), {functions} functions");
    assert!(worlds > 0);
}

#[test]
#[ignore = "measurement; set OPTIMIZER_BUILD_TIME_PACKAGE"]
fn measure_package() {
    use std::time::Instant;
    let Ok(dir) = std::env::var("OPTIMIZER_BUILD_TIME_PACKAGE") else {
        return;
    };
    let t = Instant::now();
    let assembled = vesper::packages::load(std::path::Path::new(&dir)).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let c = vesper::check::check(&assembled.module);
    assert!(c.diagnostics.is_empty(), "{:?}", &c.diagnostics[..1]);
    let input = ir_opt_policy::native_input(&c.world);
    println!("{dir}: load, check and lower {:.2?}", t.elapsed());

    let mut out = input.clone();
    let t = Instant::now();
    let stats = ir_opt_policy::optimize_with_stats(&c.world, &mut out).unwrap();
    println!("policy {:.2?}: {stats:?}", t.elapsed());

    if std::env::var("OPTIMIZER_BUILD_TIME_PASS").is_ok() {
        let mut whole = input.clone();
        let t = Instant::now();
        let n = ir_opt::optimize(&c.world, &mut whole).unwrap();
        println!("ir_opt::optimize {:.2?}: {n} eliminated", t.elapsed());
        assert_eq!(n, stats.eliminated);
        assert_eq!(format!("{whole:?}"), format!("{out:?}"), "policy == pass");
    }

    if std::env::var("OPTIMIZER_BUILD_TIME_BLOCKS").is_err() {
        return;
    }
    let t = Instant::now();
    let mut rows = ir_opt_policy::profile_blocks(&c.world, &input).unwrap();
    let total: u128 = rows.iter().map(|r| r.micros).sum();
    let removed: usize = rows.iter().map(|r| r.removed).sum();
    println!(
        "{} candidate blocks one at a time: {:.2} s summed ({:.2?} wall), {removed} reads removed, {} blocks lose nothing ({:.2} s)",
        rows.len(),
        total as f64 / 1e6,
        t.elapsed(),
        rows.iter().filter(|r| r.removed == 0).count(),
        rows.iter()
            .filter(|r| r.removed == 0)
            .map(|r| r.micros)
            .sum::<u128>() as f64
            / 1e6,
    );
    let screened: Vec<_> = rows.iter().filter(|r| r.screened).collect();
    println!(
        "the reach screen drops {} of them ({:.2} s); they remove {} reads",
        screened.len(),
        screened.iter().map(|r| r.micros).sum::<u128>() as f64 / 1e6,
        screened.iter().map(|r| r.removed).sum::<usize>(),
    );
    assert!(
        screened.iter().all(|r| r.removed == 0),
        "the reach screen dropped an improvement"
    );
    rows.sort_by_key(|r| std::cmp::Reverse(r.micros));
    for r in rows.iter().take(25) {
        println!(
            "  {:>9} us  {:>5} insts {:>4} places {:>5} values {:>4} reads {:>3} removed {:>5}  {}#{}",
            r.micros,
            r.instructions,
            r.places,
            r.values,
            r.sink_reads,
            r.removed,
            if r.screened { "drop" } else { "keep" },
            r.function,
            r.block
        );
    }
}
