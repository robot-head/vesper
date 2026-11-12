//! Fuel quanta: guests are preempted after each quantum, resumed on their
//! next turn, and the schedule and accounting are identical across runs.

use vesper_nexus::fuel::{QuantumScheduler, RoundRobin, SliceEvent};
use vesper_nexus::guests::{BLOB_PLUGIN, scenario};
use vesper_nexus::{NexusConfig, NexusEngine, PluginError, ScheduledCall, TenantUsage};

const QUANTUM: u64 = 2_000;

struct Run {
    results: Vec<Result<i64, PluginError>>,
    usage: Vec<TenantUsage>,
    trace: Vec<SliceEvent>,
}

fn run_once(config: NexusConfig, jobs: &[(u64, u32, u64, u64)]) -> Run {
    let mut e = NexusEngine::new(config).unwrap();
    let c = e.compile(BLOB_PLUGIN.as_bytes()).unwrap();
    let calls = jobs
        .iter()
        .map(|&(tenant, scenario, arg, budget)| ScheduledCall {
            instance: e.instantiate_with_fuel(&c, tenant, budget).unwrap(),
            scenario,
            arg,
            quantum: QUANTUM,
        })
        .collect();
    let mut rr = RoundRobin::with_capacity(jobs.len());
    let (outcomes, trace) = e.run_scheduled(calls, &mut rr).unwrap();
    let mut results = Vec::new();
    let mut usage = Vec::new();
    for o in outcomes {
        results.push(o.result);
        usage.push(o.usage);
        e.destroy_instance(o.instance);
    }
    assert_eq!(e.leaked_instances(), 0);
    Run {
        results,
        usage,
        trace,
    }
}

#[test]
fn guests_are_preempted_per_quantum_and_resumed() {
    let jobs = [
        (1, scenario::SPIN, 20_000, 10_000_000),
        (2, scenario::SPIN, 5_000, 10_000_000),
    ];
    let run = run_once(NexusConfig::default(), &jobs);
    assert_eq!(run.results, vec![Ok(20_000), Ok(5_000)]);
    let (a, b) = (run.usage[0], run.usage[1]);
    assert!(a.slices > b.slices && b.slices > 1, "{a:?} {b:?}");
    // Fuel is checked at loop headers and calls, so a slice can overrun its
    // quantum by at most one straight-line block; the slice count is the
    // quantum count.
    for u in [a, b] {
        assert!(
            u.fuel_consumed > (u64::from(u.slices) - 1) * QUANTUM,
            "{u:?}"
        );
        assert!(
            u.fuel_consumed <= u64::from(u.slices) * QUANTUM + 64,
            "{u:?}"
        );
    }
    // Round robin interleaves the tenants while both are runnable.
    assert_eq!(
        &run.trace[..4],
        &[
            SliceEvent::Preempted {
                tenant: 1,
                slice: 1
            },
            SliceEvent::Preempted {
                tenant: 2,
                slice: 1
            },
            SliceEvent::Preempted {
                tenant: 1,
                slice: 2
            },
            SliceEvent::Preempted {
                tenant: 2,
                slice: 2
            },
        ]
    );
    assert!(run.trace.contains(&SliceEvent::Completed {
        tenant: 2,
        slice: b.slices
    }));
    assert_eq!(
        run.trace.last(),
        Some(&SliceEvent::Completed {
            tenant: 1,
            slice: a.slices
        })
    );
}

#[test]
fn scheduling_and_accounting_are_deterministic() {
    let jobs = [
        (1, scenario::SPIN, 12_345, 10_000_000),
        (2, scenario::SPIN, 777, 10_000_000),
        (3, scenario::SPIN, 50_000, 30_000),
        (4, scenario::HAPPY, 0, 10_000_000),
    ];
    let first = run_once(NexusConfig::default(), &jobs);
    for _ in 0..3 {
        let again = run_once(NexusConfig::default(), &jobs);
        assert_eq!(again.trace, first.trace);
        assert_eq!(again.usage, first.usage);
        assert_eq!(again.results, first.results);
    }
    // Tenant 3's budget is smaller than its work: it traps out of fuel after
    // budget / quantum slices while the others finish.
    assert_eq!(first.results[2], Err(PluginError::OutOfFuel));
    assert_eq!(first.usage[2].fuel_consumed, 30_000);
    assert_eq!(first.results[3], Ok(7007));
    assert!(first.trace.contains(&SliceEvent::Trapped {
        tenant: 3,
        slice: first.usage[2].slices
    }));
}

#[test]
fn epoch_deadline_aborts_a_runaway_guest_without_starving_siblings() {
    let config = NexusConfig {
        epoch_deadline_slices: Some(40),
        ..NexusConfig::default()
    };
    let jobs = [
        (1, scenario::FOREVER, 0, u64::MAX / 2),
        (2, scenario::SPIN, 4_000, 10_000_000),
    ];
    let run = run_once(config, &jobs);
    assert_eq!(run.results[0], Err(PluginError::EpochDeadline));
    assert_eq!(run.results[1], Ok(4_000));
    assert_eq!(
        run_once(config, &jobs).trace,
        run.trace,
        "epoch ticks are virtual"
    );
}

/// A SimPulse-shaped scheduler: run-queue tokens are opaque and a job may be
/// pinned to the front (here: strict priority for tenant 2).
struct Priority {
    queue: Vec<(u64, usize)>,
}
impl QuantumScheduler for Priority {
    type Token = (u64, usize);
    fn admit(&mut self, job: usize, tenant: u64) {
        self.queue.push((tenant, job));
    }
    fn next_job(&mut self) -> Option<((u64, usize), usize)> {
        let at = self
            .queue
            .iter()
            .position(|(t, _)| *t == 2)
            .or((!self.queue.is_empty()).then_some(0))?;
        let tok = self.queue.remove(at);
        Some((tok, tok.1))
    }
    fn preempted(&mut self, token: (u64, usize)) {
        self.queue.push(token);
    }
    fn finished(&mut self, _token: (u64, usize)) {}
}

#[test]
fn custom_scheduler_drives_the_order() {
    let mut e = NexusEngine::new(NexusConfig::default()).unwrap();
    let c = e.compile(BLOB_PLUGIN.as_bytes()).unwrap();
    let calls = [(1, 6_000), (2, 6_000)]
        .into_iter()
        .map(|(t, n)| ScheduledCall {
            instance: e.instantiate_with_fuel(&c, t, 10_000_000).unwrap(),
            scenario: scenario::SPIN,
            arg: n,
            quantum: QUANTUM,
        })
        .collect();
    let mut p = Priority { queue: Vec::new() };
    let (outcomes, trace) = e.run_scheduled(calls, &mut p).unwrap();
    let first_one = trace
        .iter()
        .position(|ev| {
            matches!(
                ev,
                SliceEvent::Preempted { tenant: 1, .. } | SliceEvent::Completed { tenant: 1, .. }
            )
        })
        .unwrap();
    assert!(trace[..first_one].iter().all(|ev| matches!(
        ev,
        SliceEvent::Preempted { tenant: 2, .. } | SliceEvent::Completed { tenant: 2, .. }
    )));
    assert!(matches!(
        trace[first_one - 1],
        SliceEvent::Completed { tenant: 2, .. }
    ));
    for o in outcomes {
        assert_eq!(o.result, Ok(6_000));
        e.destroy_instance(o.instance);
    }
}
