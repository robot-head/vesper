//! Fuel preemption driven by SimPulse's run queue: every quantum is a
//! SimPulse dispatch, schedules are seed-determined and fair per round, and a
//! recorded journal replays the same interleaving.

use vesper_nexus::fuel::{RoundRobin, SliceEvent};
use vesper_nexus::guests::{BLOB_PLUGIN, scenario};
use vesper_nexus::{
    NexusConfig, NexusEngine, PluginError, ScheduledCall, SimPulseScheduler, TenantId, TenantUsage,
};
use vesper_simpulse::{Journal, TraceEvent};

const QUANTUM: u64 = 2_000;
const BUDGET: u64 = 50_000_000;

/// (tenant, scenario, arg, budget)
type Job = (TenantId, u32, u64, u64);

const SPINS: [Job; 3] = [
    (1, scenario::SPIN, 12_000, BUDGET),
    (2, scenario::SPIN, 3_000, BUDGET),
    (3, scenario::SPIN, 7_000, BUDGET),
];

struct Run {
    results: Vec<Result<i64, PluginError>>,
    usage: Vec<TenantUsage>,
    trace: Vec<SliceEvent>,
    violations: u32,
}

fn calls(e: &mut NexusEngine, jobs: &[Job]) -> Vec<ScheduledCall> {
    let c = e.compile(BLOB_PLUGIN.as_bytes()).unwrap();
    jobs.iter()
        .map(|&(tenant, scenario, arg, budget)| ScheduledCall {
            instance: e.instantiate_with_fuel(&c, tenant, budget).unwrap(),
            scenario,
            arg,
            quantum: QUANTUM,
        })
        .collect()
}

fn run_with<S: vesper_nexus::QuantumScheduler>(jobs: &[Job], scheduler: &mut S) -> Run {
    let mut e = NexusEngine::new(NexusConfig::default()).unwrap();
    let calls = calls(&mut e, jobs);
    let run = e.run_scheduled(calls, scheduler).unwrap();
    let violations = run.scheduler_violations;
    let trace = run.trace;
    let mut results = Vec::new();
    let mut usage = Vec::new();
    for o in run.outcomes {
        results.push(o.result);
        usage.push(o.usage);
        e.destroy_instance(o.instance);
    }
    assert_eq!(e.leaked_instances(), 0);
    Run {
        results,
        usage,
        trace,
        violations,
    }
}

fn tenant_of(e: &SliceEvent) -> TenantId {
    match *e {
        SliceEvent::Preempted { tenant, .. }
        | SliceEvent::Completed { tenant, .. }
        | SliceEvent::Trapped { tenant, .. } => tenant,
    }
}

#[test]
fn every_quantum_is_a_simpulse_dispatch() {
    let mut sched = SimPulseScheduler::new(7);
    let run = run_with(&SPINS, &mut sched);
    assert_eq!(run.violations, 0);
    assert!(sched.faults().is_empty(), "{:?}", sched.faults());
    assert!(sched.is_complete());
    assert_eq!(run.results, vec![Ok(12_000), Ok(3_000), Ok(7_000)]);
    // One SimPulse dispatch per slice, naming the tenant that ran it.
    assert_eq!(sched.dispatches().len(), run.trace.len());
    for ((_, tenant), event) in sched.dispatches().iter().zip(&run.trace) {
        assert_eq!(*tenant, tenant_of(event));
    }
    // The journal records the same thing: a Dispatch per slice, a Yield per
    // preemption and a Finish per job.
    let events = &sched.journal().events;
    let count = |f: fn(&TraceEvent) -> bool| events.iter().filter(|e| f(e)).count();
    assert_eq!(count(|e| matches!(e, TraceEvent::Spawn { .. })), 3);
    assert_eq!(
        count(|e| matches!(e, TraceEvent::Dispatch { .. })),
        run.trace.len()
    );
    assert_eq!(
        count(|e| matches!(e, TraceEvent::Yield { .. })),
        run.trace
            .iter()
            .filter(|e| matches!(e, SliceEvent::Preempted { .. }))
            .count()
    );
    assert_eq!(count(|e| matches!(e, TraceEvent::Finish { .. })), 3);
}

#[test]
fn rounds_are_fair_while_tenants_are_runnable() {
    for seed in 0..8 {
        let mut sched = SimPulseScheduler::new(seed);
        let run = run_with(&SPINS, &mut sched);
        // While all three tenants are runnable, no tenant is ever more than
        // one quantum ahead of another (SimPulse runs each ready task once
        // per round, in a seeded order).
        let mut slices = [0i64; 4];
        let mut finished = false;
        for e in &run.trace {
            if finished {
                break;
            }
            slices[tenant_of(e) as usize] += 1;
            let (lo, hi) = (slices[1..].iter().min(), slices[1..].iter().max());
            assert!(
                hi.unwrap() - lo.unwrap() <= 1,
                "seed {seed}: {:?}",
                run.trace
            );
            finished = matches!(e, SliceEvent::Completed { .. });
        }
    }
}

#[test]
fn the_seed_decides_the_interleaving_but_not_the_results() {
    let reference = run_with(&SPINS, &mut RoundRobin::with_capacity(3));
    let mut orders = std::collections::BTreeSet::new();
    for seed in 0..16 {
        let a = run_with(&SPINS, &mut SimPulseScheduler::new(seed));
        let b = run_with(&SPINS, &mut SimPulseScheduler::new(seed));
        // Same seed: identical schedule.
        assert_eq!(a.trace, b.trace, "seed {seed}");
        // Any seed: the same results, slices and fuel per job as round robin;
        // preemption is transparent to the guests.
        assert_eq!(a.results, reference.results);
        assert_eq!(a.usage, reference.usage);
        orders.insert(a.trace.iter().map(tenant_of).collect::<Vec<_>>());
    }
    assert!(
        orders.len() > 1,
        "every seed produced the same interleaving"
    );
}

#[test]
fn a_recorded_journal_replays_the_same_schedule() {
    let mut recorder = SimPulseScheduler::new(0xC0FFEE);
    let recorded = run_with(&SPINS, &mut recorder);
    let journal: Journal = Journal::from_json(&recorder.journal().to_json().unwrap()).unwrap();

    let mut replayer = SimPulseScheduler::replay(journal).unwrap();
    let replayed = run_with(&SPINS, &mut replayer);
    assert!(replayer.faults().is_empty(), "{:?}", replayer.faults());
    assert_eq!(replayer.finish_replay(), Ok(()));
    assert_eq!(replayed.trace, recorded.trace);
    assert_eq!(replayed.usage, recorded.usage);
    assert_eq!(replayed.results, recorded.results);
}

#[test]
fn a_diverging_replay_is_reported_and_every_job_still_finishes() {
    let mut recorder = SimPulseScheduler::new(3);
    run_with(&SPINS, &mut recorder);
    let journal = recorder.journal().clone();

    // Same tenants, different work: the slice counts no longer match the
    // journal, so SimPulse rejects a transition.
    let jobs = [
        (1, scenario::SPIN, 2_000, BUDGET),
        (2, scenario::SPIN, 9_000, BUDGET),
        (3, scenario::SPIN, 7_000, BUDGET),
    ];
    let mut replayer = SimPulseScheduler::replay(journal).unwrap();
    let run = run_with(&jobs, &mut replayer);
    assert!(!replayer.faults().is_empty());
    assert!(run.violations > 0);
    // The engine drained the remaining jobs; nobody was stranded.
    assert_eq!(run.results, vec![Ok(2_000), Ok(9_000), Ok(7_000)]);
}

#[test]
fn traps_leave_the_run_queue() {
    let jobs = [
        (1, scenario::FOREVER, 0, 20_000),
        (2, scenario::SPIN, 4_000, BUDGET),
        (3, scenario::DOUBLE_DROP, 0, BUDGET),
    ];
    let mut sched = SimPulseScheduler::new(11);
    let run = run_with(&jobs, &mut sched);
    assert_eq!(run.violations, 0);
    assert!(sched.faults().is_empty(), "{:?}", sched.faults());
    assert!(sched.is_complete());
    assert_eq!(run.results[0], Err(PluginError::OutOfFuel));
    assert_eq!(run.results[1], Ok(4_000));
    assert!(matches!(run.results[2], Err(PluginError::GuestTrap(_))));
    // The out-of-fuel tenant ran about budget / quantum slices.
    assert!(run.usage[0].slices >= 10 && run.usage[0].slices <= 11);
    let trapped = run
        .trace
        .iter()
        .filter(|e| matches!(e, SliceEvent::Trapped { .. }))
        .count();
    assert_eq!(trapped, 2);
}
