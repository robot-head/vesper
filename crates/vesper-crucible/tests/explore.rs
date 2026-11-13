//! Acceptance: schedule exploration drives SimPulse timers and I/O
//! completions and finds a planted reply-after-timeout race; shrinking gives
//! the minimal schedule, which replays under SimPulse journal validation.

mod common;

use common::{Op, Rpc, applied_once, build, rpc};
use vesper_crucible::{
    ExploreConfig, ExploreError, Footprint, FuzzConfig, IoRequest, IoResult, Proc, PropError,
    SimFiber, SimStep, Source, StepCx, Strategy, Verdict, explore_dpor, explore_random,
    format_schedule, fuzz, replay, replay_schedule,
};

fn rpc_prop(checks_timeout: bool) -> impl FnMut(&mut Source) -> Result<(), PropError> {
    move |src| {
        let (world, fibers) = rpc(checks_timeout);
        let trace = explore_random(src, world, fibers, &ExploreConfig::default())?;
        applied_once(&trace.world)
    }
}

#[test]
fn fuzzing_finds_the_reply_after_timeout_race_and_shrinks_it() {
    let config = FuzzConfig {
        strategy: Strategy::Random,
        max_execs: 2_000,
        ..FuzzConfig::default()
    };
    let report = fuzz(&config, rpc_prop(false));
    let cx = report
        .counterexample
        .expect("the timeout race is reachable by random schedules");
    assert_eq!(cx.failure.kind, "double-apply");
    println!(
        "found after {} execs; tape {:?} -> {:?}",
        cx.found_after_execs, cx.original.values, cx.tape.values
    );
    // Replaying the shrunk tape reproduces the schedule it encodes.
    let mut src = Source::replay(&cx.tape);
    let (world, fibers) = rpc(false);
    let trace = explore_random(&mut src, world, fibers, &ExploreConfig::default()).unwrap();
    assert_eq!(trace.world.applied, 2);
    let shown = format_schedule(&trace.schedule);
    println!("shrunk schedule: {shown}");
    // The bug needs exactly: both fibers start, the clock fires the timeout
    // while the reply is still in flight, the watchdog applies the fallback,
    // then the reply arrives and is applied. Six transitions, and the clock
    // advance precedes the delivery.
    assert_eq!(trace.schedule.len(), 6, "{shown}");
    let tick = trace
        .schedule
        .iter()
        .position(|p| matches!(p, Proc::Timer { .. }));
    let deliver = trace
        .schedule
        .iter()
        .position(|p| matches!(p, Proc::Io { .. }));
    assert!(tick.unwrap() < deliver.unwrap(), "{shown}");
    assert_eq!(trace.now_ns, 100);
    // SimPulse validates every transition of the recorded journal.
    let (world, fibers) = rpc(false);
    let again = replay_schedule(
        world,
        fibers,
        &trace.schedule,
        Some(trace.journal.clone()),
        &ExploreConfig::default(),
    )
    .unwrap();
    assert_eq!(again.world, trace.world);
    assert_eq!(again.journal, trace.journal);
    // A truncated journal is rejected.
    let mut cut = trace.journal.clone();
    cut.events.truncate(cut.events.len() - 2);
    let (world, fibers) = rpc(false);
    assert!(
        replay_schedule(
            world,
            fibers,
            &trace.schedule,
            Some(cut),
            &ExploreConfig::default()
        )
        .is_err()
    );
    assert!(matches!(
        replay(&mut rpc_prop(false), &cx.tape),
        Verdict::Fail(_)
    ));
}

#[test]
fn the_fixed_requester_survives_fuzzing_and_systematic_exploration() {
    let config = FuzzConfig {
        strategy: Strategy::Random,
        max_execs: 2_000,
        ..FuzzConfig::default()
    };
    assert!(fuzz(&config, rpc_prop(true)).counterexample.is_none());
    let found = explore_dpor(
        &ExploreConfig::default(),
        || rpc(true),
        |w: &Rpc| Ok(w.clone()),
    )
    .unwrap();
    assert!(found.first_failure().is_none());
    assert!(
        found
            .outcomes
            .keys()
            .all(|o| o.as_ref().unwrap().applied == 1)
    );
}

#[test]
fn dpor_finds_the_race_with_its_schedule() {
    let found = explore_dpor(
        &ExploreConfig::default(),
        || rpc(false),
        |w: &Rpc| applied_once(w).map(|()| w.clone()),
    )
    .unwrap();
    let (failure, schedule) = found.first_failure().expect("race found");
    assert_eq!(failure.kind, "double-apply");
    let (world, fibers) = rpc(false);
    let trace = replay_schedule(world, fibers, schedule, None, &ExploreConfig::default()).unwrap();
    assert_eq!(trace.world.applied, 2);
    println!(
        "dpor: {} executions, schedule {}",
        found.executions,
        format_schedule(schedule)
    );
}

#[test]
fn virtual_time_orders_timers_and_scheduled_io() {
    // Fiber 0 waits on I/O SimPulse completes at t=30, fiber 1 sleeps until
    // t=20, fiber 2 until t=50; each appends its id and wake time.
    let program = vec![
        vec![Op::Scheduled(30), Op::Local],
        vec![Op::Sleep(20), Op::Local],
        vec![Op::Sleep(50), Op::Local],
    ];
    let found = explore_dpor(
        &ExploreConfig::default(),
        || build(&program),
        |w: &common::Cells| Ok(w.clone()),
    )
    .unwrap();
    // Virtual time makes the wake order deterministic: one final state.
    assert_eq!(found.outcomes.len(), 1);
    let schedule = found.outcomes.values().next().unwrap();
    let ticks = schedule
        .iter()
        .filter(|p| matches!(p, Proc::Timer { .. }))
        .count();
    assert_eq!(ticks, 3, "{}", format_schedule(schedule));
    let (world, fibers) = build(&program);
    let trace = replay_schedule(world, fibers, schedule, None, &ExploreConfig::default()).unwrap();
    assert_eq!(trace.now_ns, 50);
}

struct Liar;
impl SimFiber<()> for Liar {
    fn footprint(&self) -> Footprint {
        Footprint::none().read(7)
    }
    fn step(&mut self, _: &mut (), cx: &mut StepCx) -> Result<SimStep, PropError> {
        cx.write(7);
        Ok(SimStep::Done)
    }
}

struct Sleeper(u64);
impl SimFiber<()> for Sleeper {
    fn footprint(&self) -> Footprint {
        Footprint::none()
    }
    fn step(&mut self, _: &mut (), _: &mut StepCx) -> Result<SimStep, PropError> {
        Ok(match std::mem::take(&mut self.0) {
            0 => SimStep::Done,
            1 => SimStep::Sleep { ns: 0 },
            _ => SimStep::Io(IoRequest::Scheduled {
                delay_ns: 0,
                result: IoResult::Cancelled,
            }),
        })
    }
}

#[test]
fn undeclared_accesses_and_zero_delays_are_typed_errors() {
    let cfg = ExploreConfig::default();
    let err = explore_dpor(&cfg, || ((), vec![Liar]), |_: &()| Ok(())).unwrap_err();
    assert!(
        matches!(err, ExploreError::Footprint { task: 0, .. }),
        "{err}"
    );
    for kind in [1, 2] {
        let err = explore_dpor(&cfg, || ((), vec![Sleeper(kind)]), |_: &()| Ok(())).unwrap_err();
        assert_eq!(err, ExploreError::ZeroDelay { task: 0 });
    }
    // A schedule naming a disabled process is rejected, not ignored.
    let (world, fibers) = rpc(false);
    let err = replay_schedule(
        world,
        fibers,
        &[Proc::Timer { task: 1, seq: 0 }],
        None,
        &cfg,
    )
    .unwrap_err();
    assert!(matches!(err, PropError::Fail(f) if f.kind == "explore"));
    // Over-long executions are typed errors.
    let tight = ExploreConfig {
        max_depth: 2,
        ..ExploreConfig::default()
    };
    let err = explore_dpor(&tight, || rpc(false), |_: &Rpc| Ok(())).unwrap_err();
    assert_eq!(err, ExploreError::DepthExceeded { limit: 2 });
}

#[test]
fn deliveries_can_overtake_each_other() {
    // Two fibers each submit external I/O; deliveries are independent of each
    // other and of unrelated steps, yet both orders are reachable.
    let program = vec![
        vec![Op::External(1), Op::Write(0)],
        vec![Op::External(2), Op::Write(0)],
    ];
    let found = explore_dpor(
        &ExploreConfig::default(),
        || build(&program),
        |w: &common::Cells| Ok(w.cells[0]),
    )
    .unwrap();
    let finals: Vec<u64> = found
        .outcomes
        .keys()
        .map(|o| *o.as_ref().unwrap())
        .collect();
    // The last writer wins: fiber 0's tag (1002) or fiber 1's (2002).
    assert_eq!(finals, vec![1002, 2002]);
}
