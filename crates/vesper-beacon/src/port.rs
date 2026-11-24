//! The port build's side of the boundary (`cfg(vesper_port)`): the Rust
//! value types of the Vesper copy types that only the port uses, the error
//! unions of the functions whose row also has `Fail[ArithOverflow]`, and the
//! conversions between the crate's public records and the port's rows.
//! The generated binding (`port/binding.rs`) names these as `crate::port::*`.
//!
//! The crate's own state (histogram counters, pool slots, ring records)
//! stays in fixed-size Rust arrays and `repr` mirrors, which the binding
//! lends to Vesper in place (export ABI v3), so nothing here allocates.

#![cfg_attr(not(feature = "vesper-port"), allow(dead_code, unused_imports))]

pub use crate::port_binding::*;
pub use crate::{
    BinRange, ErrorCode, FinishError, HistogramError, PrimaryAttribute, Redacted, SpanRecord,
    SpanSchema, SpanStatus, StartError, TraceError, W3cSpanContext,
};
pub use vesper_port_abi::ArithOverflow;
use vesper_port_abi::Word;

// The value forms of the port-only copy structs. The binding converts to
// and from them; the crate works on their `repr` mirrors instead, so most
// are only constructed by the binding's `Abi` impls.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistogramStats {
    pub total_count: u64,
    pub sum_lo: u64,
    pub sum_hi: u64,
    pub min: u64,
    pub max: u64,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpState {
    pub scale: i32,
    pub offset: i32,
    pub len: usize,
    pub zero_count: u64,
    pub total_count: u64,
    pub sum_lo: u64,
    pub sum_hi: u64,
    pub min: u64,
    pub max: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanContext {
    pub trace_hi: u64,
    pub trace_lo: u64,
    pub span_id: u64,
    pub trace_flags: u8,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub trace_hi: u64,
    pub trace_lo: u64,
    pub flags: u8,
    pub next_id: u64,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenSpan {
    pub slot: u64,
    pub context: SpanContext,
    pub parent_span_id: u64,
    pub schema: SpanSchema,
    pub start_unix_ns: u64,
    pub start_mono_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanRow {
    pub context: SpanContext,
    pub parent_span_id: u64,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
    pub schema: SpanSchema,
    pub status: u8,
    pub attribute: u8,
    pub attribute_value: u64,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingState {
    pub head: u64,
    pub len: u64,
}

/// `histogram_record` raises a histogram error or (never, for any input)
/// an arithmetic overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistogramRecordError {
    HistogramError(HistogramError),
    ArithOverflow(ArithOverflow),
}

/// The same union for `exp_record`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpRecordError {
    HistogramError(HistogramError),
    ArithOverflow(ArithOverflow),
}

/// The port's arithmetic is checked, and no input reaches an overflow
/// (every bin index, shift and window bound is in range); an overflow here
/// is a bug in the port, not an input error.
pub fn no_overflow<T>(result: Result<T, ArithOverflow>) -> T {
    match result {
        Ok(value) => value,
        Err(_) => unreachable!("vesper-beacon port: checked arithmetic overflowed"),
    }
}

pub const fn join_u128(hi: u64, lo: u64) -> u128 {
    ((hi as u128) << 64) | lo as u128
}

pub fn context_mirror(context: W3cSpanContext) -> repr::SpanContext {
    let trace_id = context.trace_id();
    repr::SpanContext {
        trace_hi: (trace_id >> 64) as u64,
        trace_lo: trace_id as u64,
        span_id: context.span_id(),
        trace_flags: Word::new(context.trace_flags()),
    }
}

pub fn context_value(context: W3cSpanContext) -> SpanContext {
    let trace_id = context.trace_id();
    SpanContext {
        trace_hi: (trace_id >> 64) as u64,
        trace_lo: trace_id as u64,
        span_id: context.span_id(),
        trace_flags: context.trace_flags(),
    }
}

/// A context the port validated (nonzero trace and span IDs).
pub fn validated(context: &repr::SpanContext) -> W3cSpanContext {
    W3cSpanContext::from_validated(
        join_u128(context.trace_hi, context.trace_lo),
        context.span_id,
        context.trace_flags.get(),
    )
}

/// An empty ring row (`TelemetryRing::default` and drain buffers).
pub fn empty_row() -> repr::SpanRow {
    repr::SpanRow {
        context: repr::SpanContext {
            trace_hi: 0,
            trace_lo: 0,
            span_id: 0,
            trace_flags: Word::new(0),
        },
        parent_span_id: 0,
        start_unix_ns: 0,
        duration_ns: 0,
        schema: Word::new(SpanSchema::Request),
        status: Word::new(0),
        attribute: Word::new(0),
        attribute_value: 0,
    }
}

const ERROR_CODES: [ErrorCode; 4] = [
    ErrorCode::InvalidInput,
    ErrorCode::Storage,
    ErrorCode::Timeout,
    ErrorCode::Cancelled,
];

/// `SpanStatus` as the row's status byte: 0 is Ok, 1 + index an error.
pub fn status_code(status: SpanStatus) -> u8 {
    match status {
        SpanStatus::Ok => 0,
        SpanStatus::Error(code) => 1 + code as u8,
    }
}

fn status_of(code: u8) -> SpanStatus {
    match code {
        0 => SpanStatus::Ok,
        c => SpanStatus::Error(ERROR_CODES[usize::from(c - 1)]),
    }
}

/// `PrimaryAttribute` as the row's (kind, payload) pair.
pub fn attribute_code(attribute: PrimaryAttribute) -> (u8, u64) {
    match attribute {
        PrimaryAttribute::None => (0, 0),
        PrimaryAttribute::ItemCount(n) => (1, n),
        PrimaryAttribute::ByteCount(n) => (2, n),
        PrimaryAttribute::Redacted(_) => (3, 0),
    }
}

fn attribute_of(kind: u8, value: u64) -> PrimaryAttribute {
    match kind {
        0 => PrimaryAttribute::None,
        1 => PrimaryAttribute::ItemCount(value),
        2 => PrimaryAttribute::ByteCount(value),
        _ => PrimaryAttribute::Redacted(Redacted::marker()),
    }
}

/// The public record of a ring row (rows are only written by
/// `span_finish`, from validated contexts and the codes above).
pub fn record_of(row: &repr::SpanRow) -> SpanRecord {
    SpanRecord {
        context: validated(&row.context),
        parent_span_id: row.parent_span_id,
        start_unix_ns: row.start_unix_ns,
        duration_ns: row.duration_ns,
        schema: row.schema.get(),
        status: status_of(row.status.get()),
        attribute: attribute_of(row.attribute.get(), row.attribute_value),
    }
}

/// The same for a row returned by value.
pub fn record_of_value(row: &SpanRow) -> SpanRecord {
    SpanRecord {
        context: W3cSpanContext::from_validated(
            join_u128(row.context.trace_hi, row.context.trace_lo),
            row.context.span_id,
            row.context.trace_flags,
        ),
        parent_span_id: row.parent_span_id,
        start_unix_ns: row.start_unix_ns,
        duration_ns: row.duration_ns,
        schema: row.schema,
        status: status_of(row.status),
        attribute: attribute_of(row.attribute, row.attribute_value),
    }
}
