//! The Vesper port frees everything it allocates: after warm-up, repeated
//! CDC ingest (accepted and rejected batches), circuit ticks, checkpoints
//! and restores, and input-log appends, reads and reopens leave no live
//! port allocation behind. Its own test binary, so no other test allocates
//! concurrently.
use vesper_flux::{
    CdcHeader, CdcPolicy, CheckpointLimits, CircuitBuilder, Executor, InputLog, Limits, LogLimits,
    SourceBinding, TickInput, WeightPolicy, port_live_allocations,
};

fn executor() -> Executor {
    let mut b = CircuitBuilder::new(Limits::default()).unwrap();
    let a = b.input(2).unwrap();
    let c = b.input(1).unwrap();
    let j = b.equijoin(a, c, 0, 0).unwrap();
    let mut e = b.finish(j).unwrap().executor();
    e.bind_sources(&CdcPolicy {
        bindings: vec![
            SourceBinding {
                partition_id: 1,
                schema_id: 1,
                input: 0,
                first_lsn: 0,
                weights: WeightPolicy::NonZero,
            },
            SourceBinding {
                partition_id: 2,
                schema_id: 1,
                input: 1,
                first_lsn: 0,
                weights: WeightPolicy::Unit,
            },
        ],
        max_batch_bytes: 1 << 16,
        max_records: 256,
    })
    .unwrap();
    e
}

fn record(e: &Executor, partition: u32, lsn: u64, weight: i64, row: &[i64]) -> Vec<u8> {
    let header = CdcHeader {
        source_lsn: lsn,
        event_ts_ms: lsn as i64,
        z_weight: weight,
        partition_id: partition,
        schema_id: 1,
        flags: 0,
    };
    e.encode_cdc_record(header, row).unwrap()
}

fn cycle(dir: &std::path::Path, round: u64) {
    let mut e = executor();
    let mut lsns = [0u64; 2];
    let mut inputs = Vec::new();
    for tick in 0..6i64 {
        let mut bytes = Vec::new();
        for k in 0..12i64 {
            bytes.extend(record(&e, 1, lsns[0], 1 + k % 3, &[k % 5, tick]));
            lsns[0] += 1;
            bytes.extend(record(&e, 2, lsns[1], 1, &[k % 7]));
            lsns[1] += 1;
        }
        let ingested = e.ingest_cdc(&bytes).unwrap();
        inputs.push(ingested.delta);
        // A rejected batch: an LSN gap after a valid record.
        let mut bad = record(&e, 1, lsns[0], 1, &[0, 0]);
        bad.extend(record(&e, 1, lsns[0] + 2, 1, &[1, 0]));
        assert!(e.ingest_cdc(&bad).is_err());
        // A truncated one.
        let cut = record(&e, 2, lsns[1], 1, &[3]);
        assert!(e.ingest_cdc(&cut[..cut.len() - 1]).is_err());
    }
    let image = e.checkpoint(CheckpointLimits::default()).unwrap();
    let mut restored = executor();
    restored
        .restore_checkpoint(&image, CheckpointLimits::default())
        .unwrap();
    let path = dir.join(format!("log-{round}"));
    let mut log = InputLog::create(&path, &[3], LogLimits::default()).unwrap();
    for delta in &inputs {
        log.append(&TickInput {
            deltas: vec![delta.clone()],
        })
        .unwrap();
    }
    // A delta of the wrong width is refused.
    assert!(
        log.append(&TickInput {
            deltas: vec![vesper_flux::ZSet::empty(1, Limits::default()).unwrap()],
        })
        .is_err()
    );
    for epoch in 0..log.len() {
        log.read(epoch).unwrap();
    }
    drop(log);
    let (log, recovery) = InputLog::open(&path, LogLimits::default()).unwrap();
    assert_eq!(recovery.epochs, inputs.len() as u64);
    drop(log);
}

#[test]
fn the_port_frees_what_it_allocates() {
    let dir = tempfile::tempdir().unwrap();
    cycle(dir.path(), 0);
    let live = port_live_allocations();
    for round in 1..20 {
        cycle(dir.path(), round);
    }
    assert_eq!(port_live_allocations(), live);
}
