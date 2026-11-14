//! Kill-and-recover at every scheduler step for whole-cluster, single-worker
//! and coordinator faults, in aligned and unaligned mode, over a circuit with
//! a re-shuffled join; every fault phase of the checkpoint protocol is hit.
use std::collections::BTreeMap;
use vesper_flux::*;
use vesper_loom_cas::Store;

fn l() -> Limits {
    Limits::default()
}
/// orders(cust, amount) ⋈ customers(cust, region), projected, unioned with
/// region-1 customers. Orders are partitioned by amount, so the join
/// re-shuffles them by customer id.
fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let orders = b.input(2).unwrap();
    let customers = b.input(2).unwrap();
    let big = b
        .filter(
            orders,
            Predicate::AtLeast {
                column: 1,
                value: 10,
            },
        )
        .unwrap();
    let j = b.equijoin(big, customers, 0, 0).unwrap();
    let view = b
        .map(
            j,
            vec![Field::Column(0), Field::Column(3), Field::Column(1)],
        )
        .unwrap();
    let r1 = b
        .filter(
            customers,
            Predicate::Equal {
                column: 1,
                value: 1,
            },
        )
        .unwrap();
    let r1 = b
        .map(
            r1,
            vec![Field::Column(0), Field::Column(1), Field::Constant(0)],
        )
        .unwrap();
    let out = b.union(view, r1).unwrap();
    b.finish(out).unwrap()
}
fn workload(seed: u64, epochs: usize) -> Vec<TickInput> {
    let mut z = seed;
    let mut rng = move |n: u64| {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = z;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((x ^ (x >> 31)) % n) as i64
    };
    let mut live: Vec<Vec<i64>> = Vec::new();
    let mut out = Vec::new();
    for epoch in 0..epochs {
        let mut orders = Vec::new();
        for _ in 0..4 {
            let row = vec![rng(8), rng(40)];
            orders.push((row.clone(), 1));
            live.push(row);
        }
        if epoch > 0 {
            let victim = live.remove(rng(live.len() as u64) as usize);
            orders.push((victim, -1));
        }
        let mut customers = Vec::new();
        if epoch < 3 {
            for c in 0..8 {
                if (c + epoch as i64) % 2 == 0 {
                    customers.push((vec![c, rng(3)], if epoch == 2 { -1 } else { 1 }));
                }
            }
        }
        out.push(TickInput {
            deltas: vec![
                ZSet::consolidate(2, &orders, l()).unwrap(),
                ZSet::consolidate(2, &customers, l()).unwrap(),
            ],
        });
    }
    out
}
fn reference(inputs: &[TickInput]) -> Vec<ZSet> {
    let mut e = circuit().executor();
    inputs.iter().map(|t| e.tick(t).unwrap()).collect()
}
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    (dir, store)
}
fn config(workers: usize, mode: CheckpointMode, seed: u64) -> ClusterConfig {
    ClusterConfig {
        seed,
        mode,
        ..ClusterConfig::new(workers, vec![1, 0])
    }
}
fn check(run: &ClusterRun, expected: &[ZSet], what: &str) {
    let start = run.start_epoch as usize;
    assert_eq!(run.outputs.len(), expected.len() - start, "{what}");
    for (i, (got, want)) in run.outputs.iter().zip(&expected[start..]).enumerate() {
        assert_eq!(got, want, "{what}: epoch {}", start + i);
    }
    let mut total = ZSet::empty(3, l()).unwrap();
    for d in expected {
        total = total.plus(d, l()).unwrap();
    }
    assert_eq!(run.materialized, total, "{what}");
    assert_eq!(run.watermark, expected.len() as i64, "{what}");
}
fn store_objects(store: &Store) -> usize {
    let probe = store.object_path(&vesper_loom_cas::blob_id(
        vesper_loom_cas::Domain::Artifact,
        b"",
    ));
    let base = probe.parent().unwrap().parent().unwrap();
    std::fs::read_dir(base)
        .map(|dirs| {
            dirs.flatten()
                .map(|d| std::fs::read_dir(d.path()).unwrap().count())
                .sum()
        })
        .unwrap_or(0)
}

/// Inject `make(step)` at every step of the fault-free run; every run must
/// produce the reference output. Returns the phases the faults hit.
fn sweep(
    mode: CheckpointMode,
    kind: &str,
    make: impl Fn(u64) -> Fault,
) -> BTreeMap<CheckpointPhase, usize> {
    let (_dir, store) = store();
    let inputs = workload(3, 6);
    let expected = reference(&inputs);
    let cfg = config(3, mode, 7);
    let base = run_cluster_with(
        &circuit(),
        &inputs,
        &mut MemorySink::new(),
        &store,
        &cfg,
        &[],
    )
    .unwrap();
    check(&base, &expected, "fault-free");
    assert_eq!(base.exchanges, 1);
    let mut phases = BTreeMap::new();
    for step in 1..=base.steps {
        let fault = make(step);
        let mut sink = MemorySink::new();
        let run = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &cfg, &[fault])
            .unwrap_or_else(|e| panic!("{mode:?} {kind} fault at step {step}: {e}"));
        check(
            &run,
            &expected,
            &format!("{mode:?} {kind} fault at step {step}"),
        );
        assert_eq!(run.recoveries.len(), 1, "step {step}");
        let rec = run.recoveries[0];
        assert_eq!((rec.step, rec.kind), (step, fault.kind));
        // Recovery restores exactly the last checkpoint committed before it.
        let before: Vec<_> = run.committed.iter().filter(|c| c.step < step).collect();
        assert_eq!(rec.restored.as_ref(), before.last().copied(), "step {step}");
        // The sink holds every epoch exactly once.
        let outs = sink.outputs().unwrap();
        assert_eq!(
            outs.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            (0..expected.len() as u64).collect::<Vec<_>>()
        );
        *phases.entry(rec.phase).or_insert(0) += 1;
    }
    phases
}

fn all_phases(phases: &BTreeMap<CheckpointPhase, usize>, what: &str) {
    for p in [
        CheckpointPhase::Idle,
        CheckpointPhase::Injected,
        CheckpointPhase::Snapshotting,
        CheckpointPhase::Acknowledged,
        CheckpointPhase::ManifestWritten,
        CheckpointPhase::Committed,
    ] {
        assert!(
            phases.contains_key(&p),
            "{what}: phase {p:?} never hit: {phases:?}"
        );
    }
}

#[test]
fn whole_cluster_crash_at_every_step_and_every_phase() {
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let phases = sweep(mode, "cluster", Fault::cluster);
        all_phases(&phases, &format!("{mode:?} cluster"));
    }
}

#[test]
fn single_worker_crash_at_every_step_and_every_phase() {
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let phases = sweep(mode, "worker", |step| {
            Fault::worker(step, (step % 3) as usize)
        });
        all_phases(&phases, &format!("{mode:?} worker"));
    }
}

#[test]
fn coordinator_failover_at_every_step_and_every_phase() {
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let phases = sweep(mode, "coordinator", Fault::coordinator);
        all_phases(&phases, &format!("{mode:?} coordinator"));
    }
}

/// A single-worker failure only rewinds that worker: peers resend their
/// buffered messages and nothing visible rolls back.
#[test]
fn partial_recovery_replays_only_the_failed_worker() {
    let (_dir, store) = store();
    let inputs = workload(11, 8);
    let expected = reference(&inputs);
    let cfg = config(4, CheckpointMode::Aligned, 5);
    let base = run_cluster(&circuit(), &inputs, &cfg, &[], &store).unwrap();
    let mut resent = 0;
    for step in (5..base.steps).step_by(9) {
        let faults = [
            Fault::worker(step, 1),
            Fault::worker(step + 13, 3),
            Fault::worker(step + 13, 0),
        ];
        let run = run_cluster_with(
            &circuit(),
            &inputs,
            &mut MemorySink::new(),
            &store,
            &cfg,
            &faults,
        )
        .unwrap_or_else(|e| panic!("faults {faults:?}: {e}"));
        check(&run, &expected, &format!("faults {faults:?}"));
        resent += run.resent;
        assert!(run.duplicates > 0 || run.resent == 0);
    }
    assert!(resent > 0);
}

/// Mixed faults of every kind, several per run, in both modes.
#[test]
fn mixed_fault_storms() {
    let (_dir, store) = store();
    let inputs = workload(21, 7);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let cfg = ClusterConfig {
            checkpoint_interval: 1,
            ..config(3, mode, 9)
        };
        let base = run_cluster(&circuit(), &inputs, &cfg, &[], &store).unwrap();
        for first in (1..base.steps).step_by(11) {
            let faults = [
                Fault::worker(first, 2),
                Fault::coordinator(first + 4),
                Fault::worker(first + 9, 0),
                Fault::cluster(first + 30),
                Fault::coordinator(first + 31),
                Fault::worker(first + 33, 1),
            ];
            let run = run_cluster_with(
                &circuit(),
                &inputs,
                &mut MemorySink::new(),
                &store,
                &cfg,
                &faults,
            )
            .unwrap_or_else(|e| panic!("{mode:?} faults {faults:?}: {e}"));
            check(&run, &expected, &format!("{mode:?} faults {faults:?}"));
        }
    }
}

#[test]
fn unaligned_checkpoints_persist_channel_state_and_do_not_block() {
    let (_dir, store) = store();
    let inputs = workload(4, 9);
    let expected = reference(&inputs);
    let mut channels = 0;
    for seed in 0..6 {
        let cfg = ClusterConfig {
            checkpoint_interval: 1,
            ..config(4, CheckpointMode::Unaligned, seed)
        };
        let run = run_cluster(&circuit(), &inputs, &cfg, &[], &store).unwrap();
        check(&run, &expected, &format!("unaligned seed {seed}"));
        channels += run.channel_blobs;
        let aligned = run_cluster(
            &circuit(),
            &inputs,
            &ClusterConfig {
                mode: CheckpointMode::Aligned,
                ..cfg.clone()
            },
            &[],
            &store,
        )
        .unwrap();
        assert_eq!(aligned.channel_blobs, 0);
        assert_eq!(aligned.outputs, run.outputs);
    }
    assert!(
        channels > 0,
        "some snapshot was taken before its barrier epoch"
    );
}

/// Garbage collection keeps exactly what the last commit references.
#[test]
fn gc_collects_superseded_and_orphaned_checkpoints() {
    let inputs = workload(8, 10);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        let (_d1, kept) = store();
        let (_d2, collected) = store();
        let cfg = ClusterConfig {
            checkpoint_interval: 1,
            ..config(3, mode, 2)
        };
        let no_gc = ClusterConfig {
            collect_garbage: false,
            ..cfg.clone()
        };
        let faults = [
            Fault::worker(40, 1),
            Fault::cluster(90),
            Fault::coordinator(120),
        ];
        let a = run_cluster_with(
            &circuit(),
            &inputs,
            &mut MemorySink::new(),
            &kept,
            &no_gc,
            &faults,
        )
        .unwrap();
        let b = run_cluster_with(
            &circuit(),
            &inputs,
            &mut MemorySink::new(),
            &collected,
            &cfg,
            &faults,
        )
        .unwrap();
        check(&a, &expected, "no gc");
        check(&b, &expected, "gc");
        assert_eq!(a.gc, GcReport::default());
        assert!(b.gc.superseded > 0, "{:?}", b.gc);
        let live = store_objects(&collected);
        assert!(live < store_objects(&kept), "{live}");
        // What is left is the last global manifest and what it references:
        // one manifest per worker plus their spine batches.
        let last = b.committed.last().unwrap().manifest;
        assert!(collected.contains(&last).unwrap());
        assert!(live <= 1 + 3 + 3 * circuit().node_count() * MAX_SPINE_LEVELS);
    }
}

#[test]
fn faults_name_existing_workers() {
    let (_dir, store) = store();
    let inputs = workload(1, 3);
    let err = run_cluster_with(
        &circuit(),
        &inputs,
        &mut MemorySink::new(),
        &store,
        &config(2, CheckpointMode::Aligned, 0),
        &[Fault::worker(3, 2)],
    )
    .unwrap_err();
    assert_eq!(err, Error::Cluster("fault worker"));
}
