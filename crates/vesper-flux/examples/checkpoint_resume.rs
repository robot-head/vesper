//! Self-checking local image resume, including a fresh process. Images travel
//! over a bounded pipe, not a durable filesystem/storage transaction.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use vesper_flux::*;

fn configured() -> Result<Executor, Error> {
    let mut b = CircuitBuilder::new(Limits::default())?;
    let a = b.input(1)?;
    let c = b.input(1)?;
    let shared = b.union(a, a)?;
    let root = b.equijoin(shared, c, 0, 0)?;
    let mut e = b.finish(root)?.executor();
    e.bind_sources(&CdcPolicy {
        bindings: vec![
            SourceBinding {
                partition_id: 0,
                schema_id: 1,
                input: 0,
                first_lsn: 0,
                weights: WeightPolicy::Unit,
            },
            SourceBinding {
                partition_id: 1,
                schema_id: 2,
                input: 1,
                first_lsn: 0,
                weights: WeightPolicy::Unit,
            },
        ],
        max_batch_bytes: 1024,
        max_records: 8,
    })?;
    Ok(e)
}
fn batch(e: &Executor, tick: usize, weight: i64) -> Result<Vec<u8>, Error> {
    let mut batch = Vec::new();
    for partition in 0..2 {
        let header = CdcHeader {
            source_lsn: tick as u64,
            event_ts_ms: 100 - tick as i64,
            z_weight: weight,
            partition_id: partition,
            schema_id: partition as u16 + 1,
            flags: 0,
        };
        batch.extend(e.encode_cdc_record(header, &[7])?);
    }
    Ok(batch)
}
fn worker() -> Result<(), Box<dyn std::error::Error>> {
    let limits = CheckpointLimits::default();
    let mut image = Vec::new();
    std::io::stdin()
        .take(u64::try_from(
            limits.max_bytes.checked_add(1).ok_or(Error::Overflow)?,
        )?)
        .read_to_end(&mut image)?;
    let mut resumed = configured()?;
    resumed.restore_checkpoint(&image, limits)?;
    assert_eq!(resumed.ticks(), 2);
    let mut uninterrupted = configured()?;
    for (tick, weight) in [1, 1, -1, -1].into_iter().enumerate() {
        let bytes = batch(&uninterrupted, tick, weight)?;
        let delta = uninterrupted.ingest_cdc(&bytes)?.delta;
        if tick >= 2 {
            assert_eq!(resumed.ingest_cdc(&bytes)?.delta, delta);
        }
    }
    assert_eq!(
        resumed.checkpoint(limits)?,
        uninterrupted.checkpoint(limits)?
    );
    println!("fresh-process resume matches uninterrupted shared join/retractions");
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--verify-resume") {
        return worker();
    }
    let mut live = configured()?;
    let mut resumed = configured()?;
    for (tick, weight) in [1, 1, -1, -1].into_iter().enumerate() {
        let bytes = batch(&live, tick, weight)?;
        let emitted = live.ingest_cdc(&bytes)?;
        if tick == 1 {
            let image = live.checkpoint(CheckpointLimits::default())?;
            resumed.restore_checkpoint(&image, CheckpointLimits::default())?;
            println!(
                "restored {}-byte local image after {} ticks",
                image.len(),
                resumed.ticks()
            );
            let mut child = Command::new(std::env::current_exe()?)
                .arg("--verify-resume")
                .stdin(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .ok_or("missing child stdin")?
                .write_all(&image)?;
            if !child.wait()?.success() {
                return Err("fresh-process resume failed".into());
            }
        } else if tick > 1 {
            assert_eq!(resumed.ingest_cdc(&bytes)?.delta, emitted.delta);
            assert_eq!(resumed.materialized(), live.materialized());
        }
    }
    assert!(resumed.materialized().is_empty());
    assert_eq!(resumed.next_source_lsn(0), Some(4));
    assert_eq!(
        resumed.checkpoint(CheckpointLimits::default())?,
        live.checkpoint(CheckpointLimits::default())?
    );
    println!("local resumed execution matches; both committed source offsets are 4");
    Ok(())
}
