//! The durable input log, the exactly-once Quarry sink, and an end-to-end
//! run over files that is killed, resumed and rescaled.
use vesper_flux::*;
use vesper_loom_cas::Store;
use vesper_quarry::device::{CrashChoice, FsDevice, SimDevice};
use vesper_quarry::durable::DbLimits;

fn l() -> Limits {
    Limits::default()
}
fn set(rows: &[(&[i64], i64)]) -> ZSet {
    let rows: Vec<_> = rows.iter().map(|(r, w)| (r.to_vec(), *w)).collect();
    ZSet::consolidate(2, &rows, l()).unwrap()
}
fn epoch(e: i64) -> TickInput {
    TickInput {
        deltas: vec![
            set(&[(&[e % 5, e * 3], 1), (&[(e + 1) % 5, e], 1)]),
            if e < 2 {
                set(&[(&[e % 5, 7], 1), (&[(e + 2) % 5, 8], 1)])
            } else {
                set(&[])
            },
        ],
    }
}
fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let y = b.input(2).unwrap();
    let j = b.equijoin(x, y, 0, 0).unwrap();
    b.finish(j).unwrap()
}

#[test]
fn input_log_round_trips_and_cuts_torn_appends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orders.log");
    let mut log = InputLog::create(&path, &[2, 2], LogLimits::default()).unwrap();
    assert_eq!(
        InputLog::create(&path, &[2, 2], LogLimits::default()).unwrap_err(),
        Error::Storage("log exists")
    );
    for e in 0..5 {
        assert_eq!(log.append(&epoch(e)).unwrap(), e as u64);
    }
    // Wrong shapes are typed errors and append nothing.
    assert_eq!(
        log.append(&TickInput {
            deltas: vec![set(&[])]
        }),
        Err(Error::InputCount)
    );
    drop(log);
    let (log, rec) = InputLog::open(&path, LogLimits::default()).unwrap();
    assert_eq!(
        rec,
        LogRecovery {
            epochs: 5,
            truncated_bytes: 0
        }
    );
    for e in 0..5 {
        assert_eq!(log.read(e as u64).unwrap().deltas, epoch(e).deltas);
    }
    assert!(log.read(5).is_err());
    drop(log);
    // A torn final append (any prefix of the last frame) is cut off durably.
    let full = std::fs::read(&path).unwrap();
    let (log, _) = InputLog::open(&path, LogLimits::default()).unwrap();
    drop(log);
    let four = {
        let tmp = dir.path().join("four.log");
        let mut l4 = InputLog::create(&tmp, &[2, 2], LogLimits::default()).unwrap();
        for e in 0..4 {
            l4.append(&epoch(e)).unwrap();
        }
        std::fs::read(&tmp).unwrap().len()
    };
    for cut in four + 1..full.len() {
        std::fs::write(&path, &full[..cut]).unwrap();
        let (log, rec) = InputLog::open(&path, LogLimits::default()).unwrap();
        assert_eq!(rec.epochs, 4, "cut {cut}");
        assert_eq!(rec.truncated_bytes, (cut - four) as u64);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), four as u64);
        drop(log);
    }
    // A corrupted frame followed by more frames is corruption, not a tear.
    let mut bad = full.clone();
    bad[four - 10] ^= 1;
    std::fs::write(&path, &bad).unwrap();
    assert_eq!(
        InputLog::open(&path, LogLimits::default()).unwrap_err(),
        Error::Storage("corrupt log")
    );
    // A corrupted header is rejected; so is a missing file.
    let mut bad = full.clone();
    bad[13] ^= 1;
    std::fs::write(&path, &bad).unwrap();
    assert!(InputLog::open(&path, LogLimits::default()).is_err());
    assert_eq!(
        InputLog::open(dir.path().join("nope"), LogLimits::default()).unwrap_err(),
        Error::Storage("missing log")
    );
    // Frames over the configured bounds are refused on append.
    let tiny = LogLimits {
        max_rows_per_frame: 1,
        ..LogLimits::default()
    };
    let mut small = InputLog::create(dir.path().join("small.log"), &[2, 2], tiny).unwrap();
    assert_eq!(small.append(&epoch(0)), Err(Error::Limit("log rows")));
    assert_eq!(small.len(), 0);
}

fn record(after: u64, digest: u8) -> CommitRecord {
    CommitRecord {
        fingerprint: [7; 32],
        after_epoch: after,
        workers: 2,
        global: vesper_loom_cas::ObjectId::new(
            vesper_loom_cas::Domain::Artifact,
            vesper_loom_cas::ObjectKind::Blob,
            vesper_loom_cas::Digest([digest; 32]),
        ),
    }
}

#[test]
fn quarry_sink_commits_atomically_at_every_crash_point() {
    let mut dev = SimDevice::new();
    {
        let (mut sink, rec) = QuarrySink::open(&mut dev, DbLimits::default(), l()).unwrap();
        assert_eq!(rec.record, None);
        sink.commit(
            &record(1, 1),
            &[(0, set(&[(&[1, 2], 1)])), (1, set(&[(&[3, 4], -2)]))],
        )
        .unwrap();
        // Ordering rules: no regression, no epoch at or before the record.
        assert_eq!(
            sink.commit(&record(0, 2), &[]),
            Err(Error::Checkpoint("sink offset regression"))
        );
        assert_eq!(
            sink.commit(&record(3, 2), &[(1, set(&[]))]),
            Err(Error::Checkpoint("sink output epoch"))
        );
        let mut other = record(3, 2);
        other.fingerprint = [9; 32];
        assert_eq!(
            sink.commit(&other, &[]),
            Err(Error::Checkpoint("sink belongs to another circuit"))
        );
    }
    let base = dev.crash(CrashChoice::LoseUnsynced);
    let next = [
        (2, set(&[(&[5, 6], 1), (&[7, 8], 1)])),
        (3, set(&[(&[1, 2], -1)])),
    ];
    let mut outcomes = [0usize; 2];
    for k in 0..400 {
        let mut d = base.clone();
        d.crash_after(k);
        let committed = match QuarrySink::open(&mut d, DbLimits::default(), l()) {
            Ok((mut sink, _)) => sink.commit(&record(3, 3), &next).is_ok(),
            Err(_) => false,
        };
        for choice in [CrashChoice::LoseUnsynced, CrashChoice::KeepUnsynced] {
            let after = d.crash(choice);
            let (mut sink, rec) = QuarrySink::open(after, DbLimits::default(), l()).unwrap();
            let outs = sink.outputs().unwrap();
            match rec.record {
                Some(r) if r == record(3, 3) => {
                    assert_eq!(outs.len(), 4, "k {k}");
                    assert_eq!(&outs[2..], &next[..]);
                    outcomes[1] += 1;
                }
                Some(r) => {
                    assert!(!committed, "k {k}: an acknowledged commit was lost");
                    assert_eq!(r, record(1, 1), "k {k}");
                    assert_eq!(outs.len(), 2, "k {k}: all or nothing");
                    outcomes[0] += 1;
                }
                None => panic!("k {k}: the first commit was durable"),
            }
            // A commit that returned Ok survives any crash.
            if committed {
                assert_eq!(sink.last_commit().unwrap(), Some(record(3, 3)), "k {k}");
            }
        }
        if committed && k > 0 {
            break;
        }
    }
    assert!(outcomes[0] > 0 && outcomes[1] > 0, "{outcomes:?}");
}

/// Files on disk end to end: a log, a Quarry sink and a CAS store. A run is
/// killed (whole-cluster fault), the sink and log are reopened as by a new
/// process, more epochs are appended and the job resumes on more workers.
#[test]
fn end_to_end_over_files_with_kill_resume_and_rescale() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    let log_path = dir.path().join("input.log");
    let sink_dir = dir.path().join("sink");
    let all: Vec<TickInput> = (0..9).map(epoch).collect();
    let mut reference = circuit().executor();
    let expected: Vec<ZSet> = all.iter().map(|t| reference.tick(t).unwrap()).collect();

    let mut log = InputLog::create(&log_path, &[2, 2], LogLimits::default()).unwrap();
    for t in &all[..5] {
        log.append(t).unwrap();
    }
    let (mut sink, _) =
        QuarrySink::open(FsDevice::open(&sink_dir).unwrap(), DbLimits::default(), l()).unwrap();
    let cfg = ClusterConfig {
        seed: 4,
        ..ClusterConfig::new(2, vec![1, 0])
    };
    let first = run_cluster_with(
        &circuit(),
        &log,
        &mut sink,
        &store,
        &cfg,
        &[
            Fault::cluster(25),
            Fault::coordinator(41),
            Fault::worker(50, 1),
        ],
    )
    .unwrap();
    assert_eq!(first.outputs, expected[..5]);
    assert_eq!(first.recoveries.len(), 3);
    drop(sink);
    drop(log);

    // A new process: reopen everything, append four more epochs.
    let (mut log, rec) = InputLog::open(&log_path, LogLimits::default()).unwrap();
    assert_eq!(rec.epochs, 5);
    for t in &all[5..] {
        log.append(t).unwrap();
    }
    let (mut sink, rec) =
        QuarrySink::open(FsDevice::open(&sink_dir).unwrap(), DbLimits::default(), l()).unwrap();
    assert_eq!(rec.record.map(|r| (r.after_epoch, r.workers)), Some((4, 2)));
    let cfg = ClusterConfig {
        seed: 5,
        ..ClusterConfig::new(3, vec![1, 0])
    };
    let second = run_cluster_with(
        &circuit(),
        &log,
        &mut sink,
        &store,
        &cfg,
        &[Fault::worker(20, 2)],
    )
    .unwrap();
    assert_eq!(second.start_epoch, 5);
    assert_eq!(second.rescale.as_ref().map(|r| r.to_workers), Some(3));
    assert_eq!(second.outputs, expected[5..]);
    let committed: Vec<ZSet> = sink
        .outputs()
        .unwrap()
        .into_iter()
        .map(|(_, z)| z)
        .collect();
    assert_eq!(committed, expected);
}

/// Faults of every kind at many steps with the Quarry sink on the simulated
/// power-loss device (only synced bytes survive a crash).
#[test]
fn cluster_over_a_quarry_sink_survives_faults_at_many_steps() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("cas")).unwrap();
    let all: Vec<TickInput> = (0..6).map(epoch).collect();
    let mut reference = circuit().executor();
    let expected: Vec<ZSet> = all.iter().map(|t| reference.tick(t).unwrap()).collect();
    let cfg = ClusterConfig {
        seed: 8,
        mode: CheckpointMode::Unaligned,
        ..ClusterConfig::new(3, vec![1, 0])
    };
    let fresh = || {
        QuarrySink::open(SimDevice::new(), DbLimits::default(), l())
            .unwrap()
            .0
    };
    let base = run_cluster_with(&circuit(), &all, &mut fresh(), &store, &cfg, &[]).unwrap();
    for step in (1..base.steps).step_by(4) {
        for fault in [
            Fault::cluster(step),
            Fault::coordinator(step),
            Fault::worker(step, (step % 3) as usize),
        ] {
            let mut sink = fresh();
            let run = run_cluster_with(&circuit(), &all, &mut sink, &store, &cfg, &[fault])
                .unwrap_or_else(|e| panic!("{fault:?}: {e}"));
            assert_eq!(run.outputs, expected, "{fault:?}");
            let committed: Vec<ZSet> = sink
                .outputs()
                .unwrap()
                .into_iter()
                .map(|(_, z)| z)
                .collect();
            assert_eq!(committed, expected, "{fault:?}");
        }
    }
}
