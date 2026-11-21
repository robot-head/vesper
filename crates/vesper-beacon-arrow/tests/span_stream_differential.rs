//! The Vesper span stream writer against the arrow-rs writer it replaced
//! (Rust-exit wave SH6, lane 18, #1413). `reference` is the previous
//! `encode`, kept as the oracle: arrow-rs 56 `StreamWriter` with 64-byte
//! alignment and metadata V5 over the same twelve columns. Every stream must
//! match it byte for byte. `speed_abba` (ignored; run with `--release
//! --ignored`) times both.
use arrow_array::{ArrayRef, RecordBatch, UInt64Array, UInt8Array};
use arrow_ipc::{
    writer::{IpcWriteOptions, StreamWriter},
    MetadataVersion,
};
use core::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;
use vesper_beacon::*;
use vesper_beacon_arrow::*;

const CAP: usize = 4096;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Mostly ordinary values, sometimes the edges of the domain.
    fn value(&mut self) -> u64 {
        match self.below(8) {
            0 => 0,
            1 => u64::MAX,
            2 => self.below(256),
            _ => self.next(),
        }
    }
}

fn limits() -> ExportLimits {
    ExportLimits {
        max_rows: MAX_ROWS,
        max_attribute_bytes: MAX_ROWS * 10,
        max_intermediate_bytes: 64 * 1024 * 1024,
        max_output_bytes: 32 * 1024 * 1024,
    }
}

fn ring(rng: &mut Rng, rows: usize) -> Box<TelemetryRing<CAP>> {
    let mut ring = Box::new(TelemetryRing::<CAP>::default());
    for _ in 0..rows {
        let trace = ((rng.value() as u128) << 64 | rng.value() as u128).max(1);
        let first = NonZeroU64::new(rng.value().clamp(1, u64::MAX - 1)).unwrap();
        let pool = SpanPool::<1>::new(trace, first, rng.below(256) as u8).unwrap();
        let parent = (rng.below(2) == 0)
            .then(|| W3cSpanContext::new(trace, rng.value().max(1), rng.below(256) as u8).unwrap());
        let schema = [
            SpanSchema::Request,
            SpanSchema::StorageAppend,
            SpanSchema::Background,
        ][rng.below(3) as usize];
        let start = rng.below(1 << 40);
        let span = pool.start_span(parent, schema, rng.value(), start).unwrap();
        let end = start + rng.below(1 << 30);
        let attribute = match rng.below(4) {
            0 => PrimaryAttribute::None,
            1 => PrimaryAttribute::ItemCount(rng.value()),
            2 => PrimaryAttribute::ByteCount(rng.value()),
            _ => PrimaryAttribute::Redacted(Secret::new("hidden").redact()),
        };
        let code = [
            ErrorCode::InvalidInput,
            ErrorCode::Storage,
            ErrorCode::Timeout,
            ErrorCode::Cancelled,
        ][rng.below(4) as usize];
        match rng.below(3) {
            0 => span.finish_ok(&mut ring, end, attribute).unwrap(),
            1 => span.finish_err(&mut ring, end, code, attribute).unwrap(),
            _ => span.cancel(&mut ring, end).unwrap(),
        }
    }
    ring
}

/// The arrow-rs encoder the port replaced, over the same rows.
fn reference(records: &[SpanRecord]) -> Vec<u8> {
    let mut wide: [Vec<u64>; 7] = Default::default();
    let mut narrow: [Vec<u8>; 5] = Default::default();
    for r in records {
        let (status, error) = match r.status {
            SpanStatus::Ok => (0, 0),
            SpanStatus::Error(code) => (
                1,
                match code {
                    ErrorCode::InvalidInput => 1,
                    ErrorCode::Storage => 2,
                    ErrorCode::Timeout => 3,
                    ErrorCode::Cancelled => 4,
                },
            ),
        };
        let (attribute, value) = match r.attribute {
            PrimaryAttribute::None => (0, 0),
            PrimaryAttribute::ItemCount(n) => (1, n),
            PrimaryAttribute::ByteCount(n) => (2, n),
            PrimaryAttribute::Redacted(_) => (3, 0),
        };
        let schema = match r.schema {
            SpanSchema::Request => 0,
            SpanSchema::StorageAppend => 1,
            SpanSchema::Background => 2,
        };
        let id = r.context.trace_id();
        for (c, v) in wide.iter_mut().zip([
            (id >> 64) as u64,
            id as u64,
            r.context.span_id(),
            r.parent_span_id,
            r.start_unix_ns,
            r.duration_ns,
            value,
        ]) {
            c.push(v);
        }
        for (c, v) in
            narrow
                .iter_mut()
                .zip([r.context.trace_flags(), schema, status, error, attribute])
        {
            c.push(v);
        }
    }
    let mut columns: Vec<ArrayRef> = Vec::new();
    let mut wide = wide.into_iter();
    for c in wide.by_ref().take(6) {
        columns.push(Arc::new(UInt64Array::from(c)));
    }
    for c in narrow {
        columns.push(Arc::new(UInt8Array::from(c)));
    }
    columns.push(Arc::new(UInt64Array::from(wide.next().unwrap())));
    let schema = Arc::new(span_schema());
    let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
    let options = IpcWriteOptions::try_new(64, false, MetadataVersion::V5).unwrap();
    let mut out = Vec::new();
    {
        let mut w = StreamWriter::try_new_with_options(&mut out, &schema, options).unwrap();
        w.write(&batch).unwrap();
        w.finish().unwrap();
    }
    out
}

fn on_big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn streams_match_arrow_rs_byte_for_byte() {
    on_big_stack(|| {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let cases: usize = std::env::var("BEACON_ARROW_DIFF_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let mut sizes = vec![0, 1, 7, 8, 9, 63, 64, 65, 127, 128, 129, CAP];
        sizes.extend((0..cases).map(|_| rng.below(CAP as u64 + 1) as usize));
        let (mut streams, mut bytes) = (0, 0);
        for rows in sizes {
            let mut ring = ring(&mut rng, rows);
            let records: Vec<_> = ring.records().collect();
            let want = reference(&records);
            // A borrowed batch, then the ring itself (prefix, then the rest).
            let batch = ring.drain::<CAP>();
            assert_eq!(export_batch(&batch, limits()).unwrap(), want, "{rows} rows");
            let mut ring = self::ring(&mut Rng(rows as u64 + 1), rows);
            let records: Vec<_> = ring.records().collect();
            let half = rows / 2;
            let first = export_ring(&mut ring, half, limits()).unwrap();
            assert_eq!(first, reference(&records[..half]), "{rows} rows, prefix");
            let rest = export_ring(&mut ring, usize::MAX, limits()).unwrap();
            assert_eq!(rest, reference(&records[half..]), "{rows} rows, rest");
            assert!(ring.is_empty());
            streams += 3;
            bytes += want.len() + first.len() + rest.len();
        }
        eprintln!("{streams} streams, {bytes} bytes, all equal to arrow-rs");
    });
}

#[test]
fn the_largest_batch_matches_arrow_rs() {
    on_big_stack(|| {
        let pool = SpanPool::<1>::new(u128::MAX, NonZeroU64::new(u64::MAX - 70_000).unwrap(), 0xff)
            .unwrap();
        let mut ring = Box::new(TelemetryRing::<MAX_ROWS>::default());
        for i in 0..MAX_ROWS as u64 {
            pool.start_span(None, SpanSchema::Background, u64::MAX - i, 0)
                .unwrap()
                .finish_ok(&mut ring, i, PrimaryAttribute::ByteCount(i))
                .unwrap();
        }
        let records: Vec<_> = ring.records().collect();
        let bytes = export_ring(&mut ring, usize::MAX, limits()).unwrap();
        assert_eq!(bytes, reference(&records));
    });
}

fn median_interval(ratios: &[f64]) -> (f64, f64, f64) {
    let mut v = ratios.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let total = 2f64.powi(n as i32);
    let (mut term, mut tail, mut d) = (1.0f64, 0.0f64, 0usize);
    for j in 0..n / 2 {
        tail += term / total;
        if tail > 0.025 {
            break;
        }
        d = j + 1;
        term = term * (n - j) as f64 / (j + 1) as f64;
    }
    let k = d.saturating_sub(1);
    (v[n / 2], v[k], v[n - 1 - k])
}

fn records_of(b: &ColumnBatch<CAP>) -> Vec<SpanRecord> {
    (0..b.len())
        .map(|i| SpanRecord {
            context: W3cSpanContext::new(b.trace_ids()[i], b.span_ids()[i], b.flags()[i]).unwrap(),
            parent_span_id: b.parent_span_ids()[i],
            start_unix_ns: b.start_unix_ns()[i],
            duration_ns: b.duration_ns()[i],
            schema: b.schemas()[i],
            status: b.statuses()[i],
            attribute: b.attributes()[i],
        })
        .collect()
}

/// The old `export_batch`: the plan, then the arrow-rs encoder. The rows are
/// converted before timing, which leaves out the old per-row copy.
fn reference_arm(
    batches: &[Box<ColumnBatch<CAP>>],
    records: &[Vec<SpanRecord>],
    reps: usize,
) -> u128 {
    let start = Instant::now();
    for _ in 0..reps {
        for (b, r) in batches.iter().zip(records) {
            plan_batch(b, limits()).unwrap();
            std::hint::black_box(reference(r));
        }
    }
    start.elapsed().as_nanos()
}

fn vesper_arm(batches: &[Box<ColumnBatch<CAP>>], reps: usize) -> u128 {
    let start = Instant::now();
    for _ in 0..reps {
        for b in batches {
            std::hint::black_box(export_batch(b, limits()).unwrap());
        }
    }
    start.elapsed().as_nanos()
}

#[test]
#[ignore = "timing; run with --release --ignored"]
fn speed_abba() {
    on_big_stack(|| {
        let epochs: usize = std::env::var("BEACON_ARROW_ABBA_EPOCHS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);
        for (name, rows, count, reps) in [
            ("8 rows", 8, 64, 40),
            ("256 rows", 256, 32, 10),
            ("4096 rows", 4096, 8, 4),
        ] {
            let mut rng = Rng(rows as u64 * 31 + 7);
            let batches: Vec<Box<ColumnBatch<CAP>>> = (0..count)
                .map(|_| Box::new(ring(&mut rng, rows).drain::<CAP>()))
                .collect();
            let records: Vec<_> = batches.iter().map(|b| records_of(b)).collect();
            reference_arm(&batches, &records, 1);
            vesper_arm(&batches, 1);
            let mut ratios = Vec::new();
            let (mut ta, mut tb) = (0u128, 0u128);
            for _ in 0..epochs {
                let a1 = reference_arm(&batches, &records, reps);
                let b1 = vesper_arm(&batches, reps);
                let b2 = vesper_arm(&batches, reps);
                let a2 = reference_arm(&batches, &records, reps);
                ta += a1 + a2;
                tb += b1 + b2;
                ratios.push((b1 + b2) as f64 / (a1 + a2) as f64);
            }
            let (m, lo, hi) = median_interval(&ratios);
            let per = |t: u128| t as f64 / (2 * epochs * reps * count) as f64 / 1000.0;
            eprintln!(
                "{name:<10} arrow-rs {:>8.2} us/batch  vesper {:>8.2} us/batch  vesper/arrow-rs {m:.3}x (95% median interval {lo:.3}x..{hi:.3}x, {epochs} epochs)",
                per(ta),
                per(tb)
            );
        }
    });
}
