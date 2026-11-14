//! Rescaling: stop a cluster at a committed checkpoint, restart it with a
//! different worker count (or partition columns) and check that every output
//! equals the single-worker run.
use vesper_flux::*;
use vesper_loom_cas::Store;

fn l() -> Limits {
    Limits::default()
}
/// orders ⋈ customers on customer id, joined again with a region table on
/// region: the second join re-shuffles whatever the partitioning is.
fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let orders = b.input(2).unwrap(); // (cust, amount)
    let customers = b.input(2).unwrap(); // (cust, region)
    let regions = b.input(2).unwrap(); // (region, tax)
    let j = b.equijoin(orders, customers, 0, 0).unwrap(); // cust amount cust region
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
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    (dir, store)
}

/// Run the first `split` epochs on `from` workers, then the rest on `to`.
fn two_phase(
    from: usize,
    to: usize,
    cols: (Vec<usize>, Vec<usize>),
    mode: CheckpointMode,
    split: usize,
    faults: &[Fault],
) -> (ClusterRun, ClusterRun, MemorySink) {
    let (_dir, store) = store();
    let inputs = workload(8);
    let mut sink = MemorySink::new();
    let first = ClusterConfig {
        mode,
        seed: from as u64,
        ..ClusterConfig::new(from, cols.0)
    };
    let a = run_cluster_with(
        &circuit(),
        &inputs[..split].to_vec(),
        &mut sink,
        &store,
        &first,
        &[],
    )
    .unwrap();
    let second = ClusterConfig {
        mode,
        seed: 100 + to as u64,
        ..ClusterConfig::new(to, cols.1)
    };
    let b = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &second, faults)
        .unwrap_or_else(|e| panic!("{from}->{to} {mode:?}: {e}"));
    (a, b, sink)
}

#[test]
fn rescale_to_every_worker_count_matches_a_single_worker() {
    let inputs = workload(8);
    let expected = reference(&inputs);
    for mode in [CheckpointMode::Aligned, CheckpointMode::Unaligned] {
        for from in 1..=4 {
            for to in 1..=5 {
                let (a, b, mut sink) =
                    two_phase(from, to, (vec![0, 0, 0], vec![0, 0, 0]), mode, 3, &[]);
                assert_eq!(a.outputs, expected[..3], "{from}->{to}");
                assert_eq!(b.start_epoch, 3);
                assert_eq!(b.outputs, expected[3..], "{from}->{to} {mode:?}");
                match &b.rescale {
                    Some(r) => {
                        assert_ne!(from, to);
                        assert_eq!((r.from_workers, r.to_workers, r.after_epoch), (from, to, 2));
                    }
                    None => assert_eq!(from, to),
                }
                // The sink holds every epoch once, in order.
                let all: Vec<ZSet> = sink
                    .outputs()
                    .unwrap()
                    .into_iter()
                    .map(|(_, z)| z)
                    .collect();
                assert_eq!(all, expected);
                let mut total = ZSet::empty(3, l()).unwrap();
                for d in &expected {
                    total = total.plus(d, l()).unwrap();
                }
                assert_eq!(b.materialized, total);
            }
        }
    }
}

#[test]
fn rescale_can_change_partition_columns_and_survive_faults() {
    let inputs = workload(8);
    let expected = reference(&inputs);
    // Repartition orders by amount and regions by tax: different exchanges.
    for (from, to) in [(2, 3), (3, 1), (1, 4)] {
        let (_, b, _) = two_phase(
            from,
            to,
            (vec![0, 0, 0], vec![1, 0, 1]),
            CheckpointMode::Aligned,
            4,
            &[
                Fault::worker(30, 0),
                Fault::cluster(60),
                Fault::coordinator(75),
            ],
        );
        let r = b.rescale.expect("rescaled");
        assert_eq!(r.from_columns, vec![0, 0, 0]);
        assert_eq!(b.outputs, expected[4..], "{from}->{to}");
        assert!(b.exchanges >= 2);
    }
}

#[test]
fn a_finished_run_is_a_no_op_and_stale_sinks_are_rejected() {
    let (_dir, store) = store();
    let inputs = workload(4);
    let mut sink = MemorySink::new();
    let cfg = ClusterConfig::new(2, vec![0, 0, 0]);
    let first = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &cfg, &[]).unwrap();
    assert!(!first.materialized.is_empty());
    let again = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &cfg, &[]).unwrap();
    assert!(again.outputs.is_empty() && again.steps == 0);
    // The no-op still reports the committed view, also across a rescale.
    assert_eq!(again.materialized, first.materialized);
    let wider = ClusterConfig::new(3, vec![0, 0, 0]);
    let rescaled = run_cluster_with(&circuit(), &inputs, &mut sink, &store, &wider, &[]).unwrap();
    assert!(rescaled.rescale.is_some() && rescaled.steps == 0);
    assert_eq!(rescaled.materialized, first.materialized);
    // A sink that is ahead of its source, or belongs to another circuit.
    assert_eq!(
        run_cluster_with(
            &circuit(),
            &inputs[..2].to_vec(),
            &mut sink,
            &store,
            &cfg,
            &[]
        )
        .unwrap_err(),
        Error::Checkpoint("sink is ahead of the source")
    );
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let y = b.input(2).unwrap();
    let z = b.input(2).unwrap();
    let u = b.union(x, y).unwrap();
    let u = b.union(u, z).unwrap();
    let other = b.finish(u).unwrap();
    assert_eq!(
        run_cluster_with(&other, &inputs, &mut sink, &store, &cfg, &[]).unwrap_err(),
        Error::Checkpoint("sink belongs to another circuit")
    );
}

#[test]
fn rescale_gathers_inputs_larger_than_one_worker_may_hold() {
    // 48 input rows overall, at most 40 per worker: the global input integral
    // exceeds the per-worker row limit, every destination partition fits.
    let lim = Limits {
        max_rows: 40,
        max_input_entries: 40,
        ..Limits::default()
    };
    let mut b = CircuitBuilder::new(lim).unwrap();
    let x = b.input(2).unwrap();
    let f = b
        .filter(
            x,
            Predicate::AtLeast {
                column: 1,
                value: 1000,
            },
        )
        .unwrap();
    let c = b.finish(f).unwrap();
    let inputs: Vec<TickInput> = (0..6i64)
        .map(|e| TickInput {
            deltas: vec![
                ZSet::consolidate(
                    2,
                    &(0..8).map(|i| (vec![e * 8 + i, i], 1)).collect::<Vec<_>>(),
                    lim,
                )
                .unwrap(),
            ],
        })
        .collect();
    let (_dir, store) = store();
    let mut sink = MemorySink::new();
    run_cluster_with(
        &c,
        &inputs[..3].to_vec(),
        &mut sink,
        &store,
        &ClusterConfig::new(2, vec![0]),
        &[],
    )
    .unwrap();
    run_cluster_with(
        &c,
        &inputs,
        &mut sink,
        &store,
        &ClusterConfig::new(2, vec![0]),
        &[],
    )
    .unwrap();
    let run = run_cluster_with(
        &c,
        &inputs,
        &mut sink,
        &store,
        &ClusterConfig::new(3, vec![0]),
        &[],
    )
    .unwrap();
    assert!(run.rescale.is_some() && run.materialized.is_empty());
}
