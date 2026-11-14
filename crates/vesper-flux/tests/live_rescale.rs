//! Live rescaling: the worker count changes at a barrier while the job runs.
//! Keyed state moves between running workers as messages (nothing is read
//! back from storage, nothing restarts), and every output must equal both the
//! single-executor reference and a fixed-parallelism cluster run. Faults are
//! injected at every scheduler step around the reconfiguration.
use std::collections::BTreeMap;
use vesper_flux::*;
use vesper_loom_cas::Store;

fn l() -> Limits {
    Limits::default()
}
/// orders ⋈ customers on customer id, joined again with a region table on
/// region: the second join always re-shuffles (an exchange), so both input
/// and exchange state migrate.
fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let orders = b.input(2).unwrap(); // (cust, amount)
    let customers = b.input(2).unwrap(); // (cust, region)
    let regions = b.input(2).unwrap(); // (region, tax)
    let j = b.equijoin(orders, customers, 0, 0).unwrap();
    let r = b.equijoin(j, regions, 3, 0).unwrap();
    let out = b
        .map(
            r,
            vec![Field::Column(0), Field::Column(1), Field::Column(5)],
        )
        .unwrap();
    b.finish(out).unwrap()
}
fn workload(epochs: usize) -> Vec<TickInput> {
    let mut out = Vec::new();
    for e in 0..epochs as i64 {
        let orders: Vec<_> = (0..5)
            .map(|i| (vec![(e * 5 + i) % 9, e * 10 + i], 1))
            .chain((e >= 2).then(|| (vec![((e - 2) * 5) % 9, (e - 2) * 10], -1)))
            .collect();
        let customers: Vec<_> = if e < 2 {
            (0..9).map(|c| (vec![c, (c + e) % 3], 1)).collect()
        } else if e == 4 {
            vec![(vec![1, 2], -1), (vec![1, 0], 1)]
        } else if e == 6 {
            vec![(vec![7, 1], -1), (vec![7, 2], 1)]
        } else {
            Vec::new()
        };
        let regions: Vec<_> = if e == 0 {
            (0..3).map(|r| (vec![r, 10 * r], 1)).collect()
        } else if e == 5 {
            vec![(vec![2, 20], -1), (vec![2, 25], 1)]
        } else {
            Vec::new()
        };
        out.push(TickInput {
            deltas: vec![
                ZSet::consolidate(2, &orders, l()).unwrap(),
                ZSet::consolidate(2, &customers, l()).unwrap(),
                ZSet::consolidate(2, &regions, l()).unwrap(),
            ],
        });
    }
    out
}
fn reference(inputs: &[TickInput]) -> Vec<ZSet> {
    let mut e = circuit().executor();
    inputs.iter().map(|t| e.tick(t).unwrap()).collect()
}
fn new_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    (dir, store)
}
fn config(workers: usize, mode: CheckpointMode, rescales: &[(u64, usize)]) -> ClusterConfig {
    ClusterConfig {
        mode,
        seed: 11,
        live_rescales: rescales
            .iter()
            .map(|&(after_epoch, workers)| LiveRescale {
                after_epoch,
                workers,
            })
            .collect(),
        ..ClusterConfig::new(workers, vec![0, 0, 0])
    }
}
fn epochs_once(sink: &mut MemorySink, expected: &[ZSet], what: &str) {
    let outs = sink.outputs().unwrap();
    assert_eq!(
        outs.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
        (0..expected.len() as u64).collect::<Vec<_>>(),
        "{what}: every epoch exactly once"
    );
    let got: Vec<ZSet> = outs.into_iter().map(|(_, z)| z).collect();
    assert_eq!(got, expected, "{what}");
}
fn total(expected: &[ZSet]) -> ZSet {
    let mut t = ZSet::empty(3, l()).unwrap();
    for d in expected {
        t = t.plus(d, l()).unwrap();
    }
    t
}

#[test]
fn live_rescale_to_every_worker_count_matches_fixed_parallelism() {
    let inputs = workload(8);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for from in 1..=4 {
            // A fixed-parallelism cluster run of the same job.
            let (_d, store) = new_store();
            let fixed = run_cluster_with(
                &circuit(),
                &inputs,
                &mut MemorySink::new(),
                &store,
                &config(from, mode, &[]),
                &[],
            )
            .unwrap();
            assert_eq!(fixed.outputs, expected);
            for to in (1..=5).filter(|t| *t != from) {
                let what = format!("{mode:?} {from}->{to}");
                let (_d, store) = new_store();
                let mut sink = MemorySink::new();
                let run = run_cluster_with(
                    &circuit(),
                    &inputs,
                    &mut sink,
                    &store,
                    &config(from, mode, &[(3, to)]),
                    &[],
                )
                .unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!(run.outputs, fixed.outputs, "{what}");
                assert_eq!(run.materialized, total(&expected), "{what}");
                assert!(run.rescale.is_none() && run.recoveries.is_empty(), "{what}");
                assert_eq!(run.recovery_rescales, 0, "{what}");
                assert_eq!(run.migrations.len(), 1, "{what}");
                let m = run.migrations[0];
                assert_eq!((m.after_epoch, m.from_workers, m.to_workers), (3, from, to));
                assert!(m.rows_total > 0 && m.rows_moved <= m.rows_total, "{what}");
                assert!(m.rows_moved > 0, "{what}: some keys change owner");
                // The new layout is committed at the barrier, with no outputs.
                let at_barrier: Vec<_> = run
                    .committed
                    .iter()
                    .filter(|c| c.after_epoch == 3)
                    .collect();
                assert_eq!(at_barrier.len(), 2, "{what}");
                assert_eq!(at_barrier[1].manifest, m.manifest);
                assert_eq!(run.watermark, expected.len() as i64);
                epochs_once(&mut sink, &expected, &what);
            }
        }
    }
}

#[test]
fn repeated_live_rescales_up_and_down() {
    let inputs = workload(10);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for schedule in [
            vec![(1, 4), (5, 1), (7, 3)],
            vec![(3, 1), (5, 5)],
            vec![(1, 1), (3, 3), (5, 2), (7, 4)],
        ] {
            let what = format!("{mode:?} {schedule:?}");
            let (_d, store) = new_store();
            let mut sink = MemorySink::new();
            let run = run_cluster_with(
                &circuit(),
                &inputs,
                &mut sink,
                &store,
                &config(2, mode, &schedule),
                &[],
            )
            .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(run.outputs, expected, "{what}");
            let done: Vec<_> = run
                .migrations
                .iter()
                .map(|m| (m.after_epoch, m.to_workers))
                .collect();
            assert_eq!(done, schedule, "{what}");
            epochs_once(&mut sink, &expected, &what);
        }
    }
}

/// Inject `make(step)` at every step of the fault-free run of a 2 -> 3
/// (and 3 -> 2) live rescale. Every run must reproduce the reference with
/// every epoch in the sink once; faults inside the reconfiguration escalate
/// to a whole-cluster recovery that rescales offline.
fn sweep(
    from: usize,
    schedule: &[(u64, usize)],
    mode: CheckpointMode,
    make: impl Fn(u64) -> Fault,
) -> (usize, usize) {
    let (_d, store) = new_store();
    let inputs = workload(8);
    let expected = reference(&inputs);
    let cfg = config(from, mode, schedule);
    let base = run_cluster_with(
        &circuit(),
        &inputs,
        &mut MemorySink::new(),
        &store,
        &cfg,
        &[],
    )
    .unwrap();
    assert_eq!(base.outputs, expected);
    let (mut escalated, mut offline) = (0, 0);
    let mut phases: BTreeMap<CheckpointPhase, usize> = BTreeMap::new();
    for step in 1..=base.steps {
        let fault = make(step);
        let what = format!(
            "{mode:?} {from}->{schedule:?} {:?} at step {step}",
            fault.kind
        );
        let mut sink = MemorySink::new();
        let run = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &cfg, &[fault])
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(run.outputs, expected, "{what}");
        assert_eq!(run.materialized, total(&expected), "{what}");
        assert_eq!(run.recoveries.len(), 1, "{what}");
        let rec = run.recoveries[0];
        escalated += usize::from(rec.escalated);
        offline += run.recovery_rescales;
        // Each rescale either migrated live or a recovery redid it offline.
        assert_eq!(
            run.migrations.len() + run.recovery_rescales,
            schedule.len(),
            "{what}: {:?}",
            run.migrations
        );
        *phases.entry(rec.phase).or_insert(0) += 1;
        epochs_once(&mut sink, &expected, &what);
    }
    assert!(
        phases.len() >= 4,
        "{from}->{schedule:?} {mode:?}: {phases:?}"
    );
    (escalated, offline)
}

#[test]
fn whole_cluster_crash_at_every_step_of_a_live_rescale() {
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for (from, to) in [(2, 3), (3, 2)] {
            let (escalated, offline) = sweep(from, &[(3, to)], mode, Fault::cluster);
            assert_eq!(escalated, 0);
            assert!(offline > 0, "some crashes hit the reconfiguration window");
        }
    }
}

#[test]
fn worker_crash_at_every_step_of_a_live_rescale() {
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for (from, to) in [(2, 3), (3, 2)] {
            let slots = from.max(to);
            let (escalated, offline) = sweep(from, &[(3, to)], mode, |step| {
                Fault::worker(step, (step as usize) % slots)
            });
            assert!(
                escalated > 0 && escalated == offline,
                "{escalated} {offline}"
            );
        }
        // Worker 1 retires after epoch 1 and comes back after epoch 5;
        // faults also hit idle and retired worker slots.
        let (escalated, _) = sweep(2, &[(1, 1), (5, 3)], mode, |step| {
            Fault::worker(step, (step as usize) % 3)
        });
        assert!(escalated > 0);
    }
}

#[test]
fn coordinator_failover_at_every_step_of_a_live_rescale() {
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for (from, to) in [(2, 3), (3, 2)] {
            let (escalated, offline) = sweep(from, &[(3, to)], mode, Fault::coordinator);
            assert!(
                escalated > 0 && escalated == offline,
                "{escalated} {offline}"
            );
        }
    }
}

/// A job stopped right after the commit at the rescale barrier (before the
/// new layout committed) resumes by rescaling offline from that commit.
#[test]
fn a_run_ending_inside_the_reconfiguration_resumes_offline() {
    let inputs = workload(8);
    let expected = reference(&inputs);
    let (_d, store) = new_store();
    let mut sink = MemorySink::new();
    let cfg = config(2, CheckpointMode::Aligned, &[(3, 4)]);
    // The first run's source ends at the barrier: the rescale is inert there.
    let first = run_cluster_with(
        &circuit(),
        &inputs[..4].to_vec(),
        &mut sink,
        &store,
        &cfg,
        &[],
    )
    .unwrap();
    assert!(first.migrations.is_empty());
    assert_eq!(sink.last_commit().unwrap().unwrap().workers, 2);
    let second = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &cfg, &[]).unwrap();
    let r = second.rescale.as_ref().expect("offline rescale at start");
    assert_eq!((r.from_workers, r.to_workers, r.after_epoch), (2, 4, 3));
    assert_eq!(second.outputs, expected[4..]);
    epochs_once(&mut sink, &expected, "resumed");
}

#[test]
fn invalid_live_rescale_schedules_are_rejected() {
    let inputs = workload(4);
    let (_d, store) = new_store();
    for (schedule, err) in [
        (vec![(2, 3)], Error::Cluster("live rescale schedule")), // not a barrier
        (vec![(1, 2)], Error::Cluster("live rescale schedule")), // no change
        (
            vec![(3, 3), (1, 4)],
            Error::Cluster("live rescale schedule"),
        ),
        (vec![(1, 0)], Error::Limit("workers")),
        (vec![(1, 65)], Error::Limit("workers")),
    ] {
        let cfg = config(2, CheckpointMode::Aligned, &schedule);
        let got = run_cluster_with(
            &circuit(),
            &inputs,
            &mut MemorySink::new(),
            &store,
            &cfg,
            &[],
        )
        .unwrap_err();
        assert_eq!(got, err, "{schedule:?}");
    }
    // A fault on a worker slot that does not exist.
    let cfg = config(2, CheckpointMode::Aligned, &[(1, 3)]);
    let got = run_cluster_with(
        &circuit(),
        &inputs,
        &mut MemorySink::new(),
        &store,
        &cfg,
        &[Fault::worker(5, 3)],
    )
    .unwrap_err();
    assert_eq!(got, Error::Cluster("fault worker"));
}
