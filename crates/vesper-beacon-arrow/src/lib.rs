//! Allocating Apache Arrow IPC stream adapter for Beacon, schema v1.
//!
//! Preflight reserves conservative logical-byte allowances before constructing
//! Arrow arrays or writing IPC. These are not allocator-capacity, RSS, or OOM
//! guarantees. The core remains dependency-free/no_std. No OTAP transport,
//! zero-allocation, fiber propagation, or performance claim is made.
#![forbid(unsafe_code)]

use arrow_array::{ArrayRef, RecordBatch, UInt64Array, UInt8Array};
use arrow_ipc::{
    writer::{IpcWriteOptions, StreamWriter},
    MetadataVersion,
};
use arrow_schema::{ArrowError, DataType, Field, Schema};
use std::{
    collections::HashMap,
    io::{self, Write},
    sync::Arc,
};
use vesper_beacon::{
    ColumnBatch, ErrorCode, PrimaryAttribute, SpanRecord, SpanSchema, SpanStatus, TelemetryRing,
};

pub const SCHEMA_VERSION: &str = "vesper.beacon.spans/1";
/// Fixed schema and batch metadata allowance (each), independent of row count.
/// Only twelve nonnullable primitive columns, no dictionaries, compression,
/// nested arrays, variable-width data, or user-controlled metadata are emitted.
pub const METADATA_ALLOWANCE: usize = 65_536;
pub const MAX_ROWS: usize = 65_536;
const BYTES_PER_ROW: usize = 61; // Seven u64 + five u8 columns.
const BUFFER_PADDING: usize = 24 * 63; // Values and validity buffers aligned to 64-byte offsets.

#[derive(Clone, Copy, Debug)]
pub struct ExportLimits {
    pub max_rows: usize,
    /// Sum of typed payload sizes: item/byte counts=8, redacted marker=10, none=0.
    pub max_attribute_bytes: usize,
    /// Conservative live Arrow values + encoded body + fixed metadata allowance.
    /// Excludes the output Vec, which has its own reservation below.
    pub max_intermediate_bytes: usize,
    /// Must admit the conservative IPC reservation reported by `plan_*`.
    pub max_output_bytes: usize,
}
impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_attribute_bytes: 4096 * 10,
            max_intermediate_bytes: 2 * 1024 * 1024,
            max_output_bytes: 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportPlan {
    pub rows: usize,
    pub attribute_bytes: usize,
    pub values_bytes: usize,
    pub intermediate_reservation: usize,
    pub output_reservation: usize,
}
#[derive(Debug)]
pub enum ExportError {
    RowLimit,
    AttributeLimit,
    IntermediateLimit,
    OutputLimit,
    Overflow,
    AllocationFailed,
    Arrow(ArrowError),
}
impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ExportError {}
fn add(a: usize, b: usize) -> Result<usize, ExportError> {
    a.checked_add(b).ok_or(ExportError::Overflow)
}
fn mul(a: usize, b: usize) -> Result<usize, ExportError> {
    a.checked_mul(b).ok_or(ExportError::Overflow)
}
fn payload_bytes(attribute: PrimaryAttribute) -> usize {
    match attribute {
        PrimaryAttribute::None => 0,
        PrimaryAttribute::ItemCount(_) | PrimaryAttribute::ByteCount(_) => 8,
        PrimaryAttribute::Redacted(marker) => marker.as_bytes().len(),
    }
}
fn plan(
    rows: usize,
    attributes: impl Iterator<Item = PrimaryAttribute>,
    limits: ExportLimits,
) -> Result<ExportPlan, ExportError> {
    if rows > MAX_ROWS || rows > limits.max_rows {
        return Err(ExportError::RowLimit);
    }
    let mut attribute_bytes = 0;
    for attribute in attributes {
        attribute_bytes = add(attribute_bytes, payload_bytes(attribute))?;
        if attribute_bytes > limits.max_attribute_bytes {
            return Err(ExportError::AttributeLimit);
        }
    }
    let values_bytes = mul(rows, BYTES_PER_ROW)?;
    // Pinned Apache writer emits an all-valid bitmap for each primitive column.
    let bitmap_bytes = add(rows, 7)? / 8;
    let body = add(add(values_bytes, mul(12, bitmap_bytes)?)?, BUFFER_PADDING)?;
    // Reserve four fixed metadata allowances: schema representation, flatbuffer
    // builders, batch metadata, and control descriptors. Values and body coexist.
    let intermediate_reservation = add(
        add(add(values_bytes, body)?, bitmap_bytes)?,
        mul(4, METADATA_ALLOWANCE)?,
    )?;
    // Schema message + batch message + continuation/EOS framing and values.
    let output_reservation = add(add(body, mul(2, METADATA_ALLOWANCE)?)?, 24)?;
    if intermediate_reservation > limits.max_intermediate_bytes {
        return Err(ExportError::IntermediateLimit);
    }
    if output_reservation > limits.max_output_bytes {
        return Err(ExportError::OutputLimit);
    }
    Ok(ExportPlan {
        rows,
        attribute_bytes,
        values_bytes,
        intermediate_reservation,
        output_reservation,
    })
}
pub fn plan_batch<const N: usize>(
    batch: &ColumnBatch<N>,
    limits: ExportLimits,
) -> Result<ExportPlan, ExportError> {
    plan(batch.len(), batch.attributes().iter().copied(), limits)
}
/// Preflights the requested prefix without mutation; `max_rows` is a prefix cap.
pub fn plan_ring<const N: usize>(
    ring: &TelemetryRing<N>,
    max_rows: usize,
    limits: ExportLimits,
) -> Result<ExportPlan, ExportError> {
    plan(
        ring.len().min(max_rows),
        ring.records().take(max_rows).map(|r| r.attribute),
        limits,
    )
}

/// Versioned custom Beacon schema, encoded as standard Apache Arrow IPC (not OTAP).
/// All columns are nonnullable. Trace ID halves are numeric high/low u64, without
/// byte-order ambiguity or loss of bits. Unix time and duration remain unsigned
/// nanoseconds so the complete u64 domain survives (not narrowed to i64 timestamp).
pub fn span_schema() -> Schema {
    let fields = [
        ("trace_id_hi", DataType::UInt64),
        ("trace_id_lo", DataType::UInt64),
        ("span_id", DataType::UInt64),
        ("parent_span_id", DataType::UInt64),
        ("start_unix_ns", DataType::UInt64),
        ("duration_ns", DataType::UInt64),
        ("trace_flags", DataType::UInt8),
        ("schema", DataType::UInt8),
        ("status", DataType::UInt8),
        ("error_code", DataType::UInt8),
        ("attribute_kind", DataType::UInt8),
        ("attribute_value", DataType::UInt64),
    ]
    .into_iter()
    .map(|(name, ty)| Field::new(name, ty, false))
    .collect::<Vec<_>>();
    let metadata = HashMap::from([
        ("vesper.schema".into(), SCHEMA_VERSION.into()),
        (
            "vesper.schema_codes".into(),
            "0=request,1=storage_append,2=background".into(),
        ),
        ("vesper.status_codes".into(), "0=ok,1=error".into()),
        (
            "vesper.error_codes".into(),
            "0=none,1=invalid_input,2=storage,3=timeout,4=cancelled".into(),
        ),
        (
            "vesper.attribute_codes".into(),
            "0=none,1=item_count,2=byte_count,3=redacted;inactive_value=0".into(),
        ),
        ("vesper.redacted_marker".into(), "[REDACTED]".into()),
    ]);
    Schema::new_with_metadata(fields, metadata)
}
fn schema_code(schema: SpanSchema) -> u8 {
    match schema {
        SpanSchema::Request => 0,
        SpanSchema::StorageAppend => 1,
        SpanSchema::Background => 2,
    }
}
fn status_codes(status: SpanStatus) -> (u8, u8) {
    match status {
        SpanStatus::Ok => (0, 0),
        SpanStatus::Error(error) => (
            1,
            match error {
                ErrorCode::InvalidInput => 1,
                ErrorCode::Storage => 2,
                ErrorCode::Timeout => 3,
                ErrorCode::Cancelled => 4,
            },
        ),
    }
}
fn attribute_codes(attribute: PrimaryAttribute) -> (u8, u64) {
    match attribute {
        PrimaryAttribute::None => (0, 0),
        PrimaryAttribute::ItemCount(n) => (1, n),
        PrimaryAttribute::ByteCount(n) => (2, n),
        PrimaryAttribute::Redacted(_) => (3, 0),
    }
}

/// Exports without consuming the owned source batch, including on success.
pub fn export_batch<const N: usize>(
    batch: &ColumnBatch<N>,
    limits: ExportLimits,
) -> Result<Vec<u8>, ExportError> {
    let plan = plan_batch(batch, limits)?;
    encode(
        plan,
        (0..batch.len()).map(|i| Row {
            trace_id: batch.trace_ids()[i],
            span_id: batch.span_ids()[i],
            parent: batch.parent_span_ids()[i],
            start: batch.start_unix_ns()[i],
            duration: batch.duration_ns()[i],
            flags: batch.flags()[i],
            schema: batch.schemas()[i],
            status: batch.statuses()[i],
            attribute: batch.attributes()[i],
        }),
    )
}
/// Commits removal only after the complete standard IPC stream (including EOS)
/// succeeds. Every returned error leaves all original records queued for retry.
pub fn export_ring<const N: usize>(
    ring: &mut TelemetryRing<N>,
    max_rows: usize,
    limits: ExportLimits,
) -> Result<Vec<u8>, ExportError> {
    let plan = plan_ring(ring, max_rows, limits)?;
    ring.try_consume_prefix(max_rows, |records| encode(plan, records.map(Row::from)))
}
struct Row {
    trace_id: u128,
    span_id: u64,
    parent: u64,
    start: u64,
    duration: u64,
    flags: u8,
    schema: SpanSchema,
    status: SpanStatus,
    attribute: PrimaryAttribute,
}
impl From<SpanRecord> for Row {
    fn from(r: SpanRecord) -> Self {
        Self {
            trace_id: r.context.trace_id(),
            span_id: r.context.span_id(),
            parent: r.parent_span_id,
            start: r.start_unix_ns,
            duration: r.duration_ns,
            flags: r.context.trace_flags(),
            schema: r.schema,
            status: r.status,
            attribute: r.attribute,
        }
    }
}
fn reserved<T>(n: usize) -> Result<Vec<T>, ExportError> {
    let mut out = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| ExportError::AllocationFailed)?;
    Ok(out)
}
fn encode(plan: ExportPlan, rows: impl Iterator<Item = Row>) -> Result<Vec<u8>, ExportError> {
    // All limits were checked before any adapter or Arrow buffers are allocated.
    let mut wide: [Vec<u64>; 7] = [(); 7].map(|_| Vec::new());
    let mut narrow: [Vec<u8>; 5] = [(); 5].map(|_| Vec::new());
    for column in &mut wide {
        *column = reserved(plan.rows)?;
    }
    for column in &mut narrow {
        *column = reserved(plan.rows)?;
    }
    for row in rows {
        let (status, error) = status_codes(row.status);
        let (attribute, value) = attribute_codes(row.attribute);
        for (column, value) in wide.iter_mut().zip([
            (row.trace_id >> 64) as u64,
            row.trace_id as u64,
            row.span_id,
            row.parent,
            row.start,
            row.duration,
            value,
        ]) {
            column.push(value);
        }
        for (column, value) in
            narrow
                .iter_mut()
                .zip([row.flags, schema_code(row.schema), status, error, attribute])
        {
            column.push(value);
        }
    }
    let mut columns: Vec<ArrayRef> = reserved(12)?;
    let mut wide = wide.into_iter();
    for column in wide.by_ref().take(6) {
        columns.push(Arc::new(UInt64Array::from(column)));
    }
    for column in narrow {
        columns.push(Arc::new(UInt8Array::from(column)));
    }
    columns.push(Arc::new(UInt64Array::from(
        wide.next().expect("seventh column"),
    )));
    let schema = Arc::new(span_schema());
    let batch = RecordBatch::try_new(schema.clone(), columns).map_err(ExportError::Arrow)?;
    let mut sink = BoundedOutput {
        bytes: reserved(plan.output_reservation)?,
        limit: plan.output_reservation,
    };
    let options =
        IpcWriteOptions::try_new(64, false, MetadataVersion::V5).map_err(ExportError::Arrow)?;
    {
        let mut writer = StreamWriter::try_new_with_options(&mut sink, &schema, options)
            .map_err(ExportError::Arrow)?;
        writer.write(&batch).map_err(ExportError::Arrow)?;
        writer.finish().map_err(ExportError::Arrow)?;
    }
    Ok(sink.bytes)
}
struct BoundedOutput {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("IPC output overflow"))?;
        if end > self.limit {
            return Err(io::Error::other("IPC output reservation exhausted"));
        }
        self.bytes.extend_from_slice(bytes); // Capacity reserved before the writer starts.
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroU64;
    use vesper_beacon::SpanPool;
    #[test]
    fn hard_row_limit_precedes_attribute_walk_and_allocations() {
        let bad = plan(
            MAX_ROWS + 1,
            core::iter::from_fn(|| panic!("must not walk attributes")),
            ExportLimits {
                max_rows: usize::MAX,
                ..ExportLimits::default()
            },
        );
        assert!(matches!(bad, Err(ExportError::RowLimit)));
    }
    #[test]
    fn writer_failure_after_array_construction_preserves_ring_for_retry() {
        let pool = SpanPool::<1>::new(1, NonZeroU64::new(1).unwrap(), 0).unwrap();
        let mut ring = TelemetryRing::<1>::default();
        pool.start_span(None, SpanSchema::Request, 0, 0)
            .unwrap()
            .cancel(&mut ring, 1)
            .unwrap();
        let before = ring.records().collect::<Vec<_>>();
        let mut plan = plan_ring(&ring, 1, ExportLimits::default()).unwrap();
        // Inject an impossible reservation after valid preflight, forcing the
        // bounded sink to fail inside the actual Apache IPC writer.
        plan.output_reservation = 1;
        let error = ring
            .try_consume_prefix(1, |records| encode(plan, records.map(Row::from)))
            .unwrap_err();
        assert!(matches!(error, ExportError::Arrow(_)));
        assert_eq!(ring.records().collect::<Vec<_>>(), before);
        assert!(export_ring(&mut ring, 1, ExportLimits::default()).is_ok());
        assert!(ring.is_empty());
    }
    #[test]
    fn bounded_sink_never_partially_writes_or_grows_past_its_reservation() {
        let mut sink = BoundedOutput {
            bytes: reserved(3).unwrap(),
            limit: 3,
        };
        sink.write_all(b"ab").unwrap();
        assert!(sink.write_all(b"cd").is_err());
        assert_eq!(sink.bytes, b"ab");
        sink.write_all(b"c").unwrap();
        assert_eq!(sink.bytes, b"abc");
    }
}
