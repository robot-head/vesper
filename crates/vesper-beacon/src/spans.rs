use crate::{Redacted, TraceError, W3cSpanContext};
use core::num::NonZeroU64;

/// Fixed public schema vocabulary; no arbitrary names or payload strings enter records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanSchema {
    Request,
    StorageAppend,
    Background,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidInput,
    Storage,
    Timeout,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimaryAttribute {
    None,
    ItemCount(u64),
    ByteCount(u64),
    Redacted(Redacted),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanStatus {
    Ok,
    Error(ErrorCode),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    PoolFull,
    IdExhausted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishError {
    QueueFull,
    EndBeforeStart,
}

/// Fixed-capacity active-span registry and monotonic local ID source, without heap/TLS.
/// Not Sync: intended for explicit shard-local use. IDs are unique within this pool's
/// lifetime; callers must assign distinct ID ranges/trace namespaces across pools.
/// Occupied slots include spans dropped/forgotten without explicit finish or cancel.
/// There is deliberately no implicit Drop completion or slot-reuse escape hatch.
///
/// The port build keeps the trace, flags and next ID in the port's
/// `PoolState` mirror and the slots as one byte each; Vesper picks the slot
/// and the ID and updates both in place.
pub struct SpanPool<const ACTIVE: usize> {
    state: core::cell::Cell<crate::port::repr::PoolState>,
    occupied: core::cell::RefCell<[u8; ACTIVE]>,
}
impl<const ACTIVE: usize> SpanPool<ACTIVE> {
    pub fn new(trace_id: u128, first_span_id: NonZeroU64, flags: u8) -> Result<Self, TraceError> {
        let state = crate::port::pool_new(
            (trace_id >> 64) as u64,
            trace_id as u64,
            first_span_id.get(),
            flags,
        )?;
        Ok(Self {
            state: core::cell::Cell::new(state),
            occupied: core::cell::RefCell::new([0; ACTIVE]),
        })
    }
    pub fn unclosed_count(&self) -> usize {
        crate::port::pool_unclosed_count(&*self.occupied.borrow()) as usize
    }
    /// All context propagation and timestamps are explicit. start_unix_ns is only a label;
    /// duration uses the separately supplied monotonic clock reading.
    pub fn start_span(
        &self,
        parent: Option<W3cSpanContext>,
        schema: SpanSchema,
        start_unix_ns: u64,
        start_mono_ns: u64,
    ) -> Result<ActiveSpan<'_, ACTIVE>, StartError> {
        let mut state = self.state.get();
        let started = crate::port::pool_start_span(
            &mut state,
            &mut *self.occupied.borrow_mut(),
            parent.map(crate::port::context_value),
            schema,
            start_unix_ns,
            start_mono_ns,
        );
        // A skipped self-parent ID is consumed even when the start fails.
        self.state.set(state);
        let open = started?;
        Ok(ActiveSpan {
            pool: self,
            slot: open.slot as usize,
            context: crate::port::validated(&open.context),
            parent_span_id: open.parent_span_id,
            schema,
            start_unix_ns,
            start_mono_ns,
        })
    }
}
/// Non-Clone owner. Successful finish/cancel consumes it; failed finish/cancel returns it.
/// A second successful finish cannot reuse the consumed owner:
///
/// ```compile_fail
/// use vesper_beacon::*;
/// use core::num::NonZeroU64;
/// let pool = SpanPool::<1>::new(1, NonZeroU64::new(1).unwrap(), 0).unwrap();
/// let span = pool.start_span(None, SpanSchema::Request, 0, 0).unwrap();
/// let mut queue = TelemetryRing::<2>::default();
/// span.finish_ok(&mut queue, 1, PrimaryAttribute::None).unwrap();
/// span.finish_ok(&mut queue, 2, PrimaryAttribute::None).unwrap();
/// ```
///
/// Dropping it leaves an observable occupied slot. `must_use` is a warning, not a
/// compiler proof of completion, and mem::forget remains possible in Rust.
#[must_use = "explicitly finish or cancel; dropping leaves an unclosed pool slot"]
pub struct ActiveSpan<'a, const ACTIVE: usize> {
    pool: &'a SpanPool<ACTIVE>,
    slot: usize,
    context: W3cSpanContext,
    parent_span_id: u64,
    schema: SpanSchema,
    start_unix_ns: u64,
    start_mono_ns: u64,
}
impl<const ACTIVE: usize> core::fmt::Debug for ActiveSpan<'_, ACTIVE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActiveSpan")
            .field("context", &self.context)
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
#[must_use = "failure contains the live span owner; retry or cancel it"]
pub struct FinishFailure<S> {
    pub error: FinishError,
    pub span: S,
}
impl<const ACTIVE: usize> ActiveSpan<'_, ACTIVE> {
    pub const fn context(&self) -> W3cSpanContext {
        self.context
    }
    /// Completes with explicit cancellation attribution. Backpressure and invalid
    /// timestamps return the live owner exactly as `finish_err` does; cancellation
    /// never silently releases a slot or discards a completed record.
    pub fn cancel<const QUEUE: usize>(
        self,
        queue: &mut TelemetryRing<QUEUE>,
        end_mono_ns: u64,
    ) -> Result<(), FinishFailure<Self>> {
        self.finish_err(
            queue,
            end_mono_ns,
            ErrorCode::Cancelled,
            PrimaryAttribute::None,
        )
    }
    pub fn finish_ok<const QUEUE: usize>(
        self,
        queue: &mut TelemetryRing<QUEUE>,
        end_mono_ns: u64,
        attribute: PrimaryAttribute,
    ) -> Result<(), FinishFailure<Self>> {
        self.finish(queue, end_mono_ns, SpanStatus::Ok, attribute)
    }
    pub fn finish_err<const QUEUE: usize>(
        self,
        queue: &mut TelemetryRing<QUEUE>,
        end_mono_ns: u64,
        code: ErrorCode,
        attribute: PrimaryAttribute,
    ) -> Result<(), FinishFailure<Self>> {
        self.finish(queue, end_mono_ns, SpanStatus::Error(code), attribute)
    }
    /// The port enqueues the record and frees the slot, or fails with both
    /// unchanged.
    fn finish<const QUEUE: usize>(
        self,
        queue: &mut TelemetryRing<QUEUE>,
        end: u64,
        status: SpanStatus,
        attribute: PrimaryAttribute,
    ) -> Result<(), FinishFailure<Self>> {
        let (kind, value) = crate::port::attribute_code(attribute);
        let open = crate::port::repr::OpenSpan {
            slot: self.slot as u64,
            context: crate::port::context_mirror(self.context),
            parent_span_id: self.parent_span_id,
            schema: vesper_port_abi::Word::new(self.schema),
            start_unix_ns: self.start_unix_ns,
            start_mono_ns: self.start_mono_ns,
        };
        let finished = crate::port::span_finish(
            &mut queue.records,
            &mut queue.state,
            &mut *self.pool.occupied.borrow_mut(),
            &open,
            end,
            crate::port::status_code(status),
            kind,
            value,
        );
        match finished {
            Ok(()) => Ok(()),
            Err(error) => Err(FinishFailure { error, span: self }),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanRecord {
    pub context: W3cSpanContext,
    pub parent_span_id: u64,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
    pub schema: SpanSchema,
    pub status: SpanStatus,
    pub attribute: PrimaryAttribute,
}
/// Fixed-capacity FIFO. Owns completed records until drained; no overwrite-on-full.
///
/// The port build keeps the records as the port's `SpanRow` mirrors and the
/// head and length in its `RingState` mirror; Vesper pushes, pops and
/// drains them in place.
#[derive(Debug)]
pub struct TelemetryRing<const QUEUE: usize> {
    records: [crate::port::repr::SpanRow; QUEUE],
    state: crate::port::repr::RingState,
}
impl<const QUEUE: usize> Default for TelemetryRing<QUEUE> {
    fn default() -> Self {
        Self {
            records: [crate::port::empty_row(); QUEUE],
            state: crate::port::repr::RingState { head: 0, len: 0 },
        }
    }
}
impl<const QUEUE: usize> TelemetryRing<QUEUE> {
    pub const fn len(&self) -> usize {
        self.state.len as usize
    }
    pub const fn is_empty(&self) -> bool {
        self.state.len == 0
    }
    /// Borrows completed records in FIFO order without removing them.
    pub fn records(&self) -> SpanRecords<'_, QUEUE> {
        SpanRecords {
            ring: self,
            offset: 0,
            end: self.len(),
        }
    }
    /// Removes a prefix only after a consumer succeeds. A failed consumer leaves
    /// every queued record available in its original FIFO order for retry.
    /// The ring cannot be mutated while the prefix is being consumed:
    ///
    /// ```compile_fail
    /// use vesper_beacon::TelemetryRing;
    /// let mut ring = TelemetryRing::<1>::default();
    /// ring.try_consume_prefix(0, |_| {
    ///     ring.drain::<1>();
    ///     Ok::<_, ()>(())
    /// }).unwrap();
    /// ```
    pub fn try_consume_prefix<R, E>(
        &mut self,
        max_rows: usize,
        consumer: impl FnOnce(SpanRecords<'_, QUEUE>) -> Result<R, E>,
    ) -> Result<R, E> {
        let count = crate::port::ring_prefix_len(&self.state, max_rows as u64);
        let result = consumer(SpanRecords {
            ring: self,
            offset: 0,
            end: count as usize,
        })?;
        crate::port::ring_pop_front(&mut self.state, QUEUE as u64, count);
        Ok(result)
    }
    /// Transfers up to BATCH records into independent, fixed-width owned columns.
    /// No Arrow/OTLP schema, serialization compatibility, or zero-copy network claim.
    pub fn drain<const BATCH: usize>(&mut self) -> ColumnBatch<BATCH> {
        let mut rows = [crate::port::empty_row(); BATCH];
        let n = crate::port::ring_drain(&mut self.records, &mut self.state, &mut rows) as usize;
        let mut out = ColumnBatch::default();
        for row in &rows[..n] {
            out.push(crate::port::record_of(row));
        }
        out
    }
}
#[derive(Debug)]
pub struct ColumnBatch<const BATCH: usize> {
    len: usize,
    trace_ids: [u128; BATCH],
    span_ids: [u64; BATCH],
    flags: [u8; BATCH],
    parent_span_ids: [u64; BATCH],
    start_unix_ns: [u64; BATCH],
    duration_ns: [u64; BATCH],
    schemas: [SpanSchema; BATCH],
    statuses: [SpanStatus; BATCH],
    attributes: [PrimaryAttribute; BATCH],
}
impl<const BATCH: usize> Default for ColumnBatch<BATCH> {
    fn default() -> Self {
        Self {
            len: 0,
            trace_ids: [0; BATCH],
            span_ids: [0; BATCH],
            flags: [0; BATCH],
            parent_span_ids: [0; BATCH],
            start_unix_ns: [0; BATCH],
            duration_ns: [0; BATCH],
            schemas: [SpanSchema::Request; BATCH],
            statuses: [SpanStatus::Ok; BATCH],
            attributes: [PrimaryAttribute::None; BATCH],
        }
    }
}
impl<const BATCH: usize> ColumnBatch<BATCH> {
    /// Appends one drained record (the port build's drain; `len < BATCH`).
    fn push(&mut self, record: SpanRecord) {
        let i = self.len;
        self.trace_ids[i] = record.context.trace_id();
        self.span_ids[i] = record.context.span_id();
        self.flags[i] = record.context.trace_flags();
        self.parent_span_ids[i] = record.parent_span_id;
        self.start_unix_ns[i] = record.start_unix_ns;
        self.duration_ns[i] = record.duration_ns;
        self.schemas[i] = record.schema;
        self.statuses[i] = record.status;
        self.attributes[i] = record.attribute;
        self.len += 1;
    }
    pub const fn len(&self) -> usize {
        self.len
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn trace_ids(&self) -> &[u128] {
        &self.trace_ids[..self.len]
    }
    pub fn span_ids(&self) -> &[u64] {
        &self.span_ids[..self.len]
    }
    pub fn flags(&self) -> &[u8] {
        &self.flags[..self.len]
    }
    pub fn parent_span_ids(&self) -> &[u64] {
        &self.parent_span_ids[..self.len]
    }
    pub fn start_unix_ns(&self) -> &[u64] {
        &self.start_unix_ns[..self.len]
    }
    pub fn duration_ns(&self) -> &[u64] {
        &self.duration_ns[..self.len]
    }
    pub fn schemas(&self) -> &[SpanSchema] {
        &self.schemas[..self.len]
    }
    pub fn statuses(&self) -> &[SpanStatus] {
        &self.statuses[..self.len]
    }
    pub fn attributes(&self) -> &[PrimaryAttribute] {
        &self.attributes[..self.len]
    }
}

/// Nonmutating, exact-length view of a ring prefix; records are fixed typed values.
#[derive(Clone)]
pub struct SpanRecords<'a, const QUEUE: usize> {
    ring: &'a TelemetryRing<QUEUE>,
    offset: usize,
    end: usize,
}
impl<const QUEUE: usize> Iterator for SpanRecords<'_, QUEUE> {
    type Item = SpanRecord;
    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.end {
            return None;
        }
        let row =
            crate::port::ring_record(&self.ring.records, &self.ring.state, self.offset as u64)?;
        self.offset += 1;
        Some(crate::port::record_of_value(&row))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.offset;
        (remaining, Some(remaining))
    }
}
impl<const QUEUE: usize> ExactSizeIterator for SpanRecords<'_, QUEUE> {}
impl<const QUEUE: usize> core::iter::FusedIterator for SpanRecords<'_, QUEUE> {}
