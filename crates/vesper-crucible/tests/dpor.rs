//! Acceptance: DPOR against exhaustive exploration of the SimPulse step
//! machine. On every program both find the same set of distinct outcomes
//! (final worlds and failures); DPOR runs no more executions, and strictly
//! fewer whenever some transitions are independent.

mod common;

use common::{Cells, Op, Rpc, applied_once, build, random_program, rpc};
use std::collections::BTreeSet;
use vesper_crucible::{
    Exploration, ExploreConfig, Outcome, explore_dpor, explore_exhaustive, replay_schedule,
};

fn both(program: &[Vec<Op>]) -> (Exploration<Cells>, Exploration<Cells>) {
    let cfg = ExploreConfig::default();
    let ex = explore_exhaustive(&cfg, || build(program), |w: &Cells| Ok(w.clone())).unwrap();
    let dp = explore_dpor(&cfg, || build(program), |w: &Cells| Ok(w.clone())).unwrap();
    (ex, dp)
}

fn same_outcomes<O: Ord + std::fmt::Debug>(ex: &Exploration<O>, dp: &Exploration<O>) {
    let a: BTreeSet<&Outcome<O>> = ex.outcome_set();
    let b: BTreeSet<&Outcome<O>> = dp.outcome_set();
    assert_eq!(a, b, "outcome sets differ");
}

#[test]
fn independent_fibers_need_one_execution() {
    // Three fibers, three steps each, on disjoint cells: 9!/(3!^3) = 1680
    // interleavings, one Mazurkiewicz trace.
    let program = vec![
        vec![Op::Write(0), Op::Update(0), Op::Read(0)],
        vec![Op::Write(1), Op::Update(1), Op::Read(1)],
        vec![Op::Write(2), Op::Update(2), Op::Read(2)],
    ];
    let (ex, dp) = both(&program);
    same_outcomes(&ex, &dp);
    assert_eq!(ex.executions, 1680);
    assert_eq!(dp.executions, 1);
    assert_eq!(dp.outcomes.len(), 1);
}

#[test]
fn lost_update_races_are_all_found() {
    // Read-then-write in two steps: the classic lost update.
    let program = vec![
        vec![Op::Read(0), Op::Update(0)],
        vec![Op::Read(0), Op::Update(0)],
        vec![Op::Write(1), Op::Local],
    ];
    let (ex, dp) = both(&program);
    same_outcomes(&ex, &dp);
    println!(
        "lost update: exhaustive {} executions, dpor {} (+{} sleep-blocked), {} outcomes",
        ex.executions,
        dp.executions,
        dp.sleep_blocked,
        ex.outcomes.len()
    );
    assert!(dp.executions < ex.executions);
}

#[test]
fn timeout_race_outcomes_match() {
    let cfg = ExploreConfig::default();
    for checks in [false, true] {
        let observe = |w: &Rpc| applied_once(w).map(|()| w.clone());
        let ex = explore_exhaustive(&cfg, || rpc(checks), observe).unwrap();
        let dp = explore_dpor(&cfg, || rpc(checks), observe).unwrap();
        same_outcomes(&ex, &dp);
        assert_eq!(ex.first_failure().is_some(), !checks);
        assert!(dp.executions <= ex.executions);
        println!(
            "rpc checks_timeout={checks}: exhaustive {}, dpor {}",
            ex.executions, dp.executions
        );
    }
}

#[test]
fn timers_and_io_with_shared_cells() {
    let program = vec![
        vec![Op::Write(0), Op::Sleep(10), Op::Update(1), Op::Local],
        vec![Op::External(1), Op::Update(0), Op::Read(1)],
        vec![Op::Scheduled(10), Op::Update(1), Op::Write(2)],
    ];
    let (ex, dp) = both(&program);
    same_outcomes(&ex, &dp);
    println!(
        "timers+io: exhaustive {} executions, dpor {}, {} outcomes",
        ex.executions,
        dp.executions,
        ex.outcomes.len()
    );
    assert!(dp.executions < ex.executions);
    // Every reported schedule reproduces its outcome.
    for (outcome, schedule) in &dp.outcomes {
        let (w, f) = build(&program);
        let trace = replay_schedule(w, f, schedule, None, &ExploreConfig::default()).unwrap();
        assert_eq!(Ok(&trace.world), outcome.as_ref());
    }
}

#[test]
fn random_programs_differential() {
    let (mut ex_total, mut dp_total, mut programs, mut strictly_fewer) = (0u64, 0u64, 0u64, 0u64);
    let cfg = ExploreConfig {
        max_executions: 20_000,
        ..ExploreConfig::default()
    };
    // `VESPER_CRUCIBLE_DPOR_SEEDS=n` widens the sweep (and uses up to four
    // ops per fiber) for local stress runs.
    let (seeds, max_ops) = match std::env::var("VESPER_CRUCIBLE_DPOR_SEEDS") {
        Ok(n) => (n.parse().expect("seed count"), 4),
        Err(_) => (150u64, 3),
    };
    for seed in 0..seeds {
        let fibers = 2 + (seed % 2) as usize;
        let program = random_program(seed, fibers, max_ops);
        let Ok(ex) = explore_exhaustive(&cfg, || build(&program), |w: &Cells| Ok(w.clone())) else {
            continue; // too many interleavings for the exhaustive baseline
        };
        let dp = explore_dpor(&cfg, || build(&program), |w: &Cells| Ok(w.clone())).unwrap();
        assert_eq!(
            ex.outcome_set(),
            dp.outcome_set(),
            "seed {seed}: program {program:?}"
        );
        assert!(dp.executions <= ex.executions, "seed {seed}");
        programs += 1;
        ex_total += ex.executions;
        dp_total += dp.executions;
        strictly_fewer += u64::from(dp.executions < ex.executions);
    }
    println!(
        "{programs} random programs: exhaustive {ex_total} executions, dpor {dp_total}; \
         dpor strictly fewer on {strictly_fewer}"
    );
    assert!(programs * 100 >= seeds * 90);
    assert!(dp_total * 4 < ex_total);
}

/// Reads a flag and fails if it is unset, or sets it.
struct Flag {
    reader: bool,
}
impl vesper_crucible::SimFiber<bool> for Flag {
    fn footprint(&self) -> vesper_crucible::Footprint {
        if self.reader {
            vesper_crucible::Footprint::none().read(0)
        } else {
            vesper_crucible::Footprint::none().write(0)
        }
    }
    fn step(
        &mut self,
        flag: &mut bool,
        cx: &mut vesper_crucible::StepCx,
    ) -> Result<vesper_crucible::SimStep, vesper_crucible::PropError> {
        if self.reader {
            cx.read(0);
            if !*flag {
                return Err(vesper_crucible::PropError::fail("unset", "flag not set"));
            }
        } else {
            cx.write(0);
            *flag = true;
        }
        Ok(vesper_crucible::SimStep::Done)
    }
}

#[test]
fn a_failing_step_still_reverses_its_races() {
    // Review regression: the failure ended the execution before its race
    // with the writer was analysed, so DPOR never ran the writer first.
    let cfg = ExploreConfig::default();
    let setup = || (false, vec![Flag { reader: true }, Flag { reader: false }]);
    let ex = explore_exhaustive(&cfg, setup, |f: &bool| Ok(*f)).unwrap();
    let dp = explore_dpor(&cfg, setup, |f: &bool| Ok(*f)).unwrap();
    same_outcomes(&ex, &dp);
    assert_eq!(dp.outcomes.len(), 2);
    assert!(dp.first_failure().is_some());
}

#[test]
fn wake_identities_do_not_depend_on_the_interleaving() {
    // Review regression: SimPulse's completion IDs come from one shared
    // counter, so exposing them made disjoint sleepers order-dependent.
    // Fibers see per-fiber sequence numbers instead.
    let program = vec![
        vec![Op::Sleep(10), Op::Write(0)],
        vec![Op::Sleep(10), Op::Write(1)],
    ];
    let (ex, dp) = both(&program);
    same_outcomes(&ex, &dp);
    assert_eq!(ex.outcomes.len(), 1);
}
