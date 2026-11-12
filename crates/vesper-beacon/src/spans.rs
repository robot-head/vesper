use crate::{Redacted, TraceError, W3cSpanContext};
use core::{cell::Cell, num::NonZeroU64};

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
pub struct SpanPool<const ACTIVE: usize> {
    trace_id: u128,
    flags: u8,
    next_id: Cell<Option<NonZeroU64>>,
    occupied: [Cell<bool>; ACTIVE],
}
impl<const ACTIVE: usize> SpanPool<ACTIVE> {
    pub fn new(trace_id: u128, first_span_id: NonZeroU64, flags: u8) -> Result<Self, TraceError> {
        W3cSpanContext::new(trace_id, first_span_id.get(), flags)?;
        Ok(Self {
            trace_id,
            flags,
            next_id: Cell::new(Some(first_span_id)),
            occupied: core::array::from_fn(|_| Cell::new(false)),
        })
    }
    pub fn unclosed_count(&self) -> usize {
        self.occupied.iter().filter(|slot| slot.get()).count()
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
        let slot = self
            .occupied
            .iter()
            .position(|slot| !slot.get())
            .ok_or(StartError::PoolFull)?;
        let mut id = self.next_id.get().ok_or(StartError::IdExhausted)?;
        // Never make a child its own parent, including imported contexts.
        if parent.is_some_and(|p| p.span_id() == id.get()) {
            let next = id.get().checked_add(1).and_then(NonZeroU64::new);
            self.next_id.set(next);
            id = next.ok_or(StartError::IdExhausted)?;
        }
        let ctx = W3cSpanContext::new(
            parent.map_or(self.trace_id, W3cSpanContext::trace_id),
            id.get(),
            parent.map_or(self.flags, W3cSpanContext::trace_flags),
        )
        .expect("pool and parent contexts are validated");
        self.next_id
            .set(id.get().checked_add(1).and_then(NonZeroU64::new));
        self.occupied[slot].set(true);
        Ok(ActiveSpan {
            pool: self,
            slot,
            context: ctx,
            parent_span_id: parent.map_or(0, W3cSpanContext::span_id),
            schema,
            start_unix_ns,
            start_mono_ns,
        })
    }
}
/// Non-Clone owner. Successful finish/cancel consumes it; failed finish returns it.
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
    pub fn cancel(self) -> W3cSpanContext {
        self.pool.occupied[self.slot].set(false);
        self.context
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
    fn finish<const QUEUE: usize>(
        self,
        queue: &mut TelemetryRing<QUEUE>,
        end: u64,
        status: SpanStatus,
        attribute: PrimaryAttribute,
    ) -> Result<(), FinishFailure<Self>> {
        let Some(duration_ns) = end.checked_sub(self.start_mono_ns) else {
            return Err(FinishFailure {
                error: FinishError::EndBeforeStart,
                span: self,
            });
        };
        let record = SpanRecord {
            context: self.context,
            parent_span_id: self.parent_span_id,
            start_unix_ns: self.start_unix_ns,
            duration_ns,
            schema: self.schema,
            status,
            attribute,
        };
        if queue.push(record).is_err() {
            return Err(FinishFailure {
                error: FinishError::QueueFull,
                span: self,
            });
        }
        self.pool.occupied[self.slot].set(false);
        Ok(())
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
#[derive(Debug)]
pub struct TelemetryRing<const QUEUE: usize> {
    records: [Option<SpanRecord>; QUEUE],
    head: usize,
    len: usize,
}
impl<const QUEUE: usize> Default for TelemetryRing<QUEUE> {
    fn default() -> Self {
        Self {
            records: [None; QUEUE],
            head: 0,
            len: 0,
        }
    }
}
impl<const QUEUE: usize> TelemetryRing<QUEUE> {
    pub const fn len(&self) -> usize {
        self.len
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn push(&mut self, record: SpanRecord) -> Result<(), SpanRecord> {
        if self.len == QUEUE {
            return Err(record);
        }
        let tail = if self.len >= QUEUE - self.head {
            self.len - (QUEUE - self.head)
        } else {
            self.head + self.len
        };
        self.records[tail] = Some(record);
        self.len += 1;
        Ok(())
    }
    fn pop(&mut self) -> Option<SpanRecord> {
        if self.len == 0 {
            return None;
        }
        let record = self.records[self.head].take();
        self.head = if self.head == QUEUE - 1 {
            0
        } else {
            self.head + 1
        };
        self.len -= 1;
        record
    }
    /// Transfers up to BATCH records into independent, fixed-width owned columns.
    /// No Arrow/OTLP schema, serialization compatibility, or zero-copy network claim.
    pub fn drain<const BATCH: usize>(&mut self) -> ColumnBatch<BATCH> {
        let mut out = ColumnBatch::default();
        while out.len < BATCH {
            let Some(record) = self.pop() else {
                break;
            };
            let i = out.len;
            out.trace_ids[i] = record.context.trace_id();
            out.span_ids[i] = record.context.span_id();
            out.flags[i] = record.context.trace_flags();
            out.parent_span_ids[i] = record.parent_span_id;
            out.start_unix_ns[i] = record.start_unix_ns;
            out.duration_ns[i] = record.duration_ns;
            out.schemas[i] = record.schema;
            out.statuses[i] = record.status;
            out.attributes[i] = record.attribute;
            out.len += 1;
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
