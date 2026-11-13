//! Multi-worker execution under SimPulse: differential against the single
//! executor, determinism, crash-at-every-step recovery and partition checks.
use vesper_flux::*;
use vesper_loom_cas::Store;

fn l() -> Limits {
    Limits::default()
}
/// orders(cust, amount) ⋈ customers(cust, region), projected, unioned with
/// region-1 customers. Every join is on the partition key (column 0).
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

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> i64 {
        (self.next() % n) as i64
    }
}

/// Inserts and retractions of earlier inserts; customers only change early,
/// so their arrangements are reused by later incremental checkpoints.
fn workload(seed: u64, epochs: usize) -> Vec<TickInput> {
    let mut rng = Rng(seed);
    let mut live: Vec<Vec<i64>> = Vec::new();
    let mut out = Vec::new();
    for epoch in 0..epochs {
        let mut orders = Vec::new();
        for _ in 0..4 {
            let row = vec![rng.below(8), rng.below(40)];
            orders.push((row.clone(), 1));
            live.push(row);
        }
        if epoch > 0 && !live.is_empty() {
            let victim = live.remove(rng.below(live.len() as u64) as usize);
            orders.push((victim, -1));
        }
        let mut customers = Vec::new();
        if epoch < 2 {
            for c in 0..8 {
                if (c + epoch as i64) % 2 == 0 {
                    customers.push((vec![c, rng.below(3)], 1));
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
fn config(workers: usize, seed: u64) -> ClusterConfig {
    ClusterConfig {
        seed,
        ..ClusterConfig::new(workers, vec![0, 0])
    }
}
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    (dir, store)
}
fn check(run: &ClusterRun, expected: &[ZSet], what: &str) {
    assert_eq!(run.outputs.len(), expected.len(), "{what}");
    for (epoch, (got, want)) in run.outputs.iter().zip(expected).enumerate() {
        assert_eq!(got, want, "{what}: epoch {epoch}");
    }
    let mut total = ZSet::empty(3, l()).unwrap();
    for d in expected {
        total = total.plus(d, l()).unwrap();
    }
    assert_eq!(run.materialized, total, "{what}");
    assert_eq!(run.watermark, expected.len() as i64, "{what}");
    for pair in run.committed.windows(2) {
        assert!(pair[0].after_epoch < pair[1].after_epoch, "{what}");
    }
    assert_eq!(
        run.committed.last().map(|c| c.after_epoch),
        Some(expected.len() as u64 - 1),
        "{what}"
    );
}

#[test]
fn multi_worker_matches_single_executor() {
    let (_dir, store) = store();
    for data_seed in 0..3 {
        let inputs = workload(data_seed, 7);
        let expected = reference(&inputs);
        for workers in 1..=4 {
            for seed in 0..4 {
                let run =
                    run_cluster(&circuit(), &inputs, &config(workers, seed), &[], &store).unwrap();
                check(
                    &run,
                    &expected,
                    &format!("data {data_seed} workers {workers} seed {seed}"),
                );
                assert!(run.recoveries.is_empty());
                // interval 2 over 7 epochs: barriers after 1, 3, 5 and 6.
                assert_eq!(
                    run.committed
                        .iter()
                        .map(|c| c.after_epoch)
                        .collect::<Vec<_>>(),
                    vec![1, 3, 5, 6]
                );
                assert!(run.arrangements_reused > 0, "incremental reuse");
            }
        }
    }
}

#[test]
fn simulation_is_deterministic_per_seed() {
    let (_dir, store) = store();
    let inputs = workload(9, 6);
    let a = run_cluster(&circuit(), &inputs, &config(3, 42), &[], &store).unwrap();
    let b = run_cluster(&circuit(), &inputs, &config(3, 42), &[], &store).unwrap();
    assert_eq!(a.journal.to_json().unwrap(), b.journal.to_json().unwrap());
    assert_eq!((a.steps, a.messages), (b.steps, b.messages));
    assert_eq!(a.committed, b.committed);
    let mut journals = std::collections::BTreeSet::new();
    for seed in 0..6 {
        let r = run_cluster(&circuit(), &inputs, &config(3, seed), &[], &store).unwrap();
        assert_eq!(r.outputs, a.outputs, "seed {seed}");
        journals.insert(r.journal.to_json().unwrap());
    }
    assert!(journals.len() > 1, "seeds explore different interleavings");
}

#[test]
fn crash_at_every_step_recovers_to_the_same_output() {
    let (_dir, store) = store();
    let inputs = workload(3, 6);
    let expected = reference(&inputs);
    let cfg = config(3, 7);
    let base = run_cluster(&circuit(), &inputs, &cfg, &[], &store).unwrap();
    check(&base, &expected, "uninterrupted");
    let mut restored_from_checkpoint = 0;
    for step in 1..=base.steps {
        let run = run_cluster(&circuit(), &inputs, &cfg, &[step], &store)
            .unwrap_or_else(|e| panic!("crash at step {step}: {e}"));
        check(&run, &expected, &format!("crash at step {step}"));
        assert_eq!(run.recoveries.len(), 1, "step {step}");
        let recovery = run.recoveries[0];
        assert_eq!(recovery.step, step);
        // Recovery restores exactly the last checkpoint committed before it.
        let before: Vec<_> = run.committed.iter().filter(|c| c.step < step).collect();
        assert_eq!(
            recovery.restored.as_ref(),
            before.last().copied(),
            "step {step}"
        );
        if recovery.restored.is_some() {
            restored_from_checkpoint += 1;
        }
    }
    assert!(restored_from_checkpoint > base.steps / 3);
}

#[test]
fn repeated_crashes_including_during_recovery_replay() {
    let (_dir, store) = store();
    let inputs = workload(5, 6);
    let expected = reference(&inputs);
    let cfg = config(4, 11);
    let base = run_cluster(&circuit(), &inputs, &cfg, &[], &store).unwrap();
    for first in (1..base.steps).step_by(7) {
        for gap in [1, 3, 25] {
            let crashes = [first, first + gap, first + gap + 40];
            let run = run_cluster(&circuit(), &inputs, &cfg, &crashes, &store)
                .unwrap_or_else(|e| panic!("crashes {crashes:?}: {e}"));
            check(&run, &expected, &format!("crashes {crashes:?}"));
            assert!(!run.recoveries.is_empty());
        }
    }
}

#[test]
fn single_epoch_and_empty_inputs() {
    let (_dir, store) = store();
    let inputs = workload(1, 1);
    let run = run_cluster(&circuit(), &inputs, &config(2, 0), &[1, 2], &store).unwrap();
    check(&run, &reference(&inputs), "single epoch");
    let empty = run_cluster(&circuit(), &[], &config(2, 0), &[], &store).unwrap();
    assert!(empty.outputs.is_empty() && empty.materialized.is_empty());
}

#[test]
fn non_co_partitioned_joins_and_bad_configs_are_rejected() {
    let (_dir, store) = store();
    let inputs = workload(0, 2);
    // Orders partitioned by amount: the join on customer id is not local.
    let err = run_cluster(
        &circuit(),
        &inputs,
        &ClusterConfig::new(2, vec![1, 0]),
        &[],
        &store,
    )
    .unwrap_err();
    assert_eq!(err, Error::Partition("join key is not the partition key"));
    // A map that drops the key column loses partitioning for a later join.
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let y = b.input(2).unwrap();
    let swapped = b
        .map(x, vec![Field::Column(1), Field::Constant(0)])
        .unwrap();
    let j = b.equijoin(swapped, y, 0, 0).unwrap();
    let c = b.finish(j).unwrap();
    assert_eq!(
        ClusterConfig::new(2, vec![0, 0]).validate(&c),
        Err(Error::Partition("join key is not the partition key"))
    );
    // A column-preserving reorder keeps it.
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let y = b.input(2).unwrap();
    let swapped = b.map(x, vec![Field::Column(1), Field::Column(0)]).unwrap();
    let j = b.equijoin(swapped, y, 1, 0).unwrap();
    let c = b.finish(j).unwrap();
    assert_eq!(ClusterConfig::new(3, vec![0, 0]).validate(&c), Ok(()));
    for bad in [
        ClusterConfig::new(0, vec![0, 0]),
        ClusterConfig::new(65, vec![0, 0]),
        ClusterConfig::new(2, vec![0]),
        ClusterConfig::new(2, vec![0, 2]),
        ClusterConfig {
            checkpoint_interval: 0,
            ..ClusterConfig::new(2, vec![0, 0])
        },
        ClusterConfig {
            max_link_delay_ns: 0,
            ..ClusterConfig::new(2, vec![0, 0])
        },
    ] {
        assert!(run_cluster(&circuit(), &inputs, &bad, &[], &store).is_err());
    }
    // Step budget is a typed limit, never a hang.
    let tiny = ClusterConfig {
        max_steps: 5,
        ..ClusterConfig::new(2, vec![0, 0])
    };
    assert_eq!(
        run_cluster(&circuit(), &inputs, &tiny, &[], &store).unwrap_err(),
        Error::Limit("simulation steps")
    );
    // Malformed input epochs are typed errors.
    let wrong = vec![TickInput {
        deltas: vec![ZSet::empty(2, l()).unwrap()],
    }];
    assert_eq!(
        run_cluster(&circuit(), &wrong, &config(2, 0), &[], &store).unwrap_err(),
        Error::InputCount
    );
}
