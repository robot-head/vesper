//! Bounded native-platform recovery checks. The exhaustive per-step sweeps in
//! `recovery.rs` and `live_rescale.rs` remain separate, unchanged tests.
use vesper_flux::*;
use vesper_loom_cas::Store;

fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(Limits::default()).unwrap();
    let orders = b.input(2).unwrap();
    let customers = b.input(2).unwrap();
    let joined = b.equijoin(orders, customers, 0, 0).unwrap();
    let out = b
        .map(
            joined,
            vec![Field::Column(0), Field::Column(1), Field::Column(3)],
        )
        .unwrap();
    b.finish(out).unwrap()
}

fn workload(epochs: i64) -> Vec<TickInput> {
    (0..epochs)
        .map(|epoch| {
            let mut orders: Vec<_> = (0..4)
                .map(|i| (vec![(epoch + i) % 4, epoch * 10 + i], 1))
                .collect();
            if epoch > 0 {
                orders.push((vec![(epoch - 1) % 4, (epoch - 1) * 10], -1));
            }
            let customers = match epoch {
                0 => (0..4).map(|id| (vec![id, id % 2], 1)).collect(),
                2 => vec![(vec![1, 1], -1), (vec![1, 0], 1)],
                _ => vec![],
            };
            TickInput {
                deltas: vec![
                    ZSet::consolidate(2, &orders, Limits::default()).unwrap(),
                    ZSet::consolidate(2, &customers, Limits::default()).unwrap(),
                ],
            }
        })
        .collect()
}

fn reference(inputs: &[TickInput]) -> Vec<ZSet> {
    let mut executor = circuit().executor();
    inputs
        .iter()
        .map(|input| executor.tick(input).unwrap())
        .collect()
}

fn config(workers: usize, mode: CheckpointMode) -> ClusterConfig {
    ClusterConfig {
        mode,
        seed: 7,
        ..ClusterConfig::new(workers, vec![1, 0])
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("cas")).unwrap();
    (directory, store)
}

#[derive(Clone, Copy)]
enum Restore {
    Exact,
    Rescaled,
}

fn run(
    inputs: &dyn EpochSource,
    expected: &[ZSet],
    store: &Store,
    cfg: &ClusterConfig,
    fault: Option<(Fault, Restore)>,
) -> ClusterRun {
    let mut sink = MemorySink::new();
    let faults: Vec<_> = fault.map(|(fault, _)| fault).into_iter().collect();
    let run = run_cluster_with(&circuit(), inputs, &mut sink, store, cfg, &faults).unwrap();
    assert_eq!(run.start_epoch, 0);
    assert_eq!(run.outputs, expected);
    assert_eq!(run.watermark, expected.len() as i64);
    let mut total = ZSet::empty(3, Limits::default()).unwrap();
    for delta in expected {
        total = total.plus(delta, Limits::default()).unwrap();
    }
    assert_eq!(run.materialized, total);
    let outputs = sink.outputs().unwrap();
    assert_eq!(
        outputs.iter().map(|(epoch, _)| *epoch).collect::<Vec<_>>(),
        (0..expected.len() as u64).collect::<Vec<_>>(),
        "each epoch must commit exactly once"
    );
    assert_eq!(
        outputs
            .into_iter()
            .map(|(_, delta)| delta)
            .collect::<Vec<_>>(),
        expected
    );
    if let Some((fault, restore)) = fault {
        assert_eq!(run.recoveries.len(), 1, "{fault:?}");
        let recovered = run.recoveries[0];
        assert_eq!((recovered.step, recovered.kind), (fault.step, fault.kind));
        let prior = run.committed.iter().rfind(|c| c.step < fault.step);
        match restore {
            Restore::Exact => assert_eq!(
                recovered.restored.as_ref(),
                prior,
                "recover exactly the checkpoint committed before {fault:?}"
            ),
            Restore::Rescaled => {
                // Whole-cluster recovery first rewrites the old-layout commit
                // into the scheduled layout. It preserves the source offset,
                // but creates a new manifest outside scheduler history (step 0).
                let prior = prior.expect("the rescale barrier already committed");
                let restored = recovered.restored.expect("restore the rescaled commit");
                assert_eq!(restored.after_epoch, prior.after_epoch);
                assert_ne!(restored.manifest, prior.manifest);
                assert_eq!(restored.step, 0);
                assert_eq!(run.recovery_rescales, 1);
            }
        }
    } else {
        assert!(run.recoveries.is_empty());
    }
    run
}

fn fault(kind: FaultKind, step: u64) -> Fault {
    Fault { step, kind }
}

#[test]
fn native_checkpoint_commit_boundaries_recover_each_fault_kind() {
    let inputs = workload(4);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let (_directory, store) = store();
        let cfg = config(2, mode);
        let base = run(&inputs, &expected, &store, &cfg, None);
        assert_eq!(base.exchanges, 1, "the join must reshuffle its input");
        let commit = base.committed.iter().find(|c| c.after_epoch == 1).unwrap();
        for (step, phase) in [
            (commit.step, CheckpointPhase::ManifestWritten),
            (commit.step + 1, CheckpointPhase::Committed),
        ] {
            for kind in [
                FaultKind::Cluster,
                FaultKind::Worker(1),
                FaultKind::Coordinator,
            ] {
                let recovered = run(
                    &inputs,
                    &expected,
                    &store,
                    &cfg,
                    Some((fault(kind, step), Restore::Exact)),
                );
                assert_eq!(recovered.recoveries[0].phase, phase, "{mode:?} {kind:?}");
                assert!(!recovered.recoveries[0].in_reconfiguration);
            }
        }
    }
}

#[test]
fn native_rescale_up_and_down_preserves_exactly_once_recovery() {
    let inputs = workload(4);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for (from, to) in [(2, 3), (3, 2)] {
            let (_directory, store) = store();
            let cfg = ClusterConfig {
                live_rescales: vec![LiveRescale {
                    after_epoch: 1,
                    workers: to,
                }],
                ..config(from, mode)
            };
            let base = run(&inputs, &expected, &store, &cfg, None);
            assert_eq!(base.migrations.len(), 1);
            let migration = base.migrations[0];
            assert_eq!((migration.from_workers, migration.to_workers), (from, to));
            assert!(migration.rows_moved > 0);
            let commits: Vec<_> = base
                .committed
                .iter()
                .filter(|c| c.after_epoch == 1)
                .collect();
            assert_eq!(commits.len(), 2, "old and new layouts must both commit");
            let step = commits[0].step + 1;
            for kind in [
                FaultKind::Cluster,
                FaultKind::Worker(1),
                FaultKind::Coordinator,
            ] {
                // Only a whole-cluster crash rescales offline; a worker or
                // the coordinator recovers inside the window and the
                // migration completes live.
                let whole = kind == FaultKind::Cluster;
                let restore = if whole { Restore::Rescaled } else { Restore::Exact };
                let recovered = run(&inputs, &expected, &store, &cfg, Some((fault(kind, step), restore)));
                assert_eq!(recovered.recoveries[0].phase, CheckpointPhase::Committed);
                assert!(recovered.recoveries[0].in_reconfiguration);
                assert_eq!(recovered.recovery_rescales, usize::from(whole));
                assert_eq!(recovered.migrations.len(), usize::from(!whole));
            }
        }
    }
}

#[test]
fn native_retired_worker_fault_does_not_duplicate_outputs() {
    let inputs = workload(6);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let (_directory, store) = store();
        let cfg = ClusterConfig {
            live_rescales: vec![
                LiveRescale {
                    after_epoch: 1,
                    workers: 1,
                },
                LiveRescale {
                    after_epoch: 3,
                    workers: 3,
                },
            ],
            ..config(2, mode)
        };
        let base = run(&inputs, &expected, &store, &cfg, None);
        let commits: Vec<_> = base
            .committed
            .iter()
            .filter(|c| c.after_epoch == 1)
            .collect();
        assert_eq!(commits.len(), 2);
        let recovered = run(
            &inputs,
            &expected,
            &store,
            &cfg,
            Some((Fault::worker(commits[1].step + 1, 1), Restore::Exact)),
        );
        assert_eq!(recovered.recoveries[0].phase, CheckpointPhase::Committed);
        assert!(!recovered.recoveries[0].in_reconfiguration);
        assert_eq!(recovered.migrations.len() + recovered.recovery_rescales, 2);
    }
}
