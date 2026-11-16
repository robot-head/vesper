//! OTAP producer: Beacon spans, histograms and logs to `BatchArrowRecords`.
//!
//! Each payload type owns one long-lived Arrow IPC stream. The first payload
//! of a stream carries the schema message; later payloads carry only the new
//! messages (record batch, plus a dictionary batch when a dictionary grew).
//! Dictionaries are append-only per stream, so growth is sent as an IPC
//! **delta** dictionary (`isDelta = true`) holding only the new values.
//!
//! Encoding runs in two phases. Planning builds every column and checks every
//! limit (row counts, signed ranges, dictionary sizes) without touching a
//! stream, so a batch that does not fit fails with a typed error and leaves
//! the encoder unchanged; the `*_split` entry points use this to cut input
//! into batches that fit. Writing then opens or resets streams and appends
//! the IPC messages. A stream is reset (new `schema_id`, schema message and
//! full dictionaries again) when a dictionary would exceed its key width or
//! the configured entry limit, on `reset()`, and after any write error,
//! because a failed write may leave the IPC writer and the receiver out of
//! step.
//!
//! Id columns use upstream's transport encodings by default (see
//! [`schema`](super::schema)): the collector's Go decoder applies them
//! regardless of metadata.

use super::attributes::{AttributeValue, Attributes, SpanAttributes};
use super::cbor;
use super::model::{
    Exemplar, ExpHistogramPoint, LogRecord, NumberKind, NumberPoint, NumberValue, Resource,
    ResourceGroup, Scope, ScopeGroup, SpanEvent, SpanKind, SpanLink, TraceState,
};
use super::proto::{ArrowPayload, ArrowPayloadType, BatchArrowRecords};
use super::schema::{self, value_type, IdEncoding};
use arrow_array::{
    builder::{Float64Builder, ListBuilder, UInt64Builder},
    types::{UInt16Type, UInt8Type},
    ArrayRef, BinaryArray, BooleanArray, DictionaryArray, DurationNanosecondArray,
    FixedSizeBinaryArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
    StructArray, TimestampNanosecondArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use arrow_ipc::writer::{DictionaryHandling, IpcWriteOptions, StreamWriter};
use arrow_ipc::MetadataVersion;
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use vesper_beacon::{
    ErrorCode, FastLogHistogram, PrimaryAttribute, SpanRecord, SpanSchema, SpanStatus,
    HISTOGRAM_BINS,
};

/// Hard ceiling on the rows of any one payload: main-record ids are `u16`,
/// and a default decoder accepts this many rows.
pub const MAX_BATCH_ROWS: usize = 65_536;

#[derive(Clone, Copy, Debug)]
pub struct EncoderConfig {
    /// Main-record rows (spans, metrics, logs) per batch, at most
    /// [`MAX_BATCH_ROWS`]. Related payloads (attributes, events, links,
    /// data points) are bounded by [`MAX_BATCH_ROWS`].
    pub max_rows: usize,
    /// Entries per dictionary column before the stream is reset. The key
    /// width (256 for `u8` keys, 65,536 for `u16`) caps it further.
    pub max_dictionary_entries: usize,
    /// Id column encoding; [`IdEncoding::Transport`] for upstream peers.
    pub id_encoding: IdEncoding,
    /// Arrow IPC buffer compression (upstream `payload_compression`). Off
    /// by default: gRPC message compression already compresses the whole
    /// batch, and upstream turns IPC compression on only without it.
    pub ipc_compression: IpcCompression,
}
/// Arrow IPC `BodyCompression` codec of emitted record and dictionary
/// batches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IpcCompression {
    #[default]
    None,
    /// Per-buffer zstd (`CompressionType::ZSTD`), as upstream writes it.
    Zstd,
}
impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_dictionary_entries: 4096,
            id_encoding: IdEncoding::Transport,
            ipc_compression: IpcCompression::None,
        }
    }
}

#[derive(Debug)]
pub enum EncodeError {
    /// More main-record rows than `max_rows`.
    RowLimit,
    /// One attribute payload of the batch would exceed [`MAX_BATCH_ROWS`].
    AttributeRowLimit,
    /// Events, links or data points of the batch would exceed
    /// [`MAX_BATCH_ROWS`].
    RelatedRowLimit(ArrowPayloadType),
    /// A batch alone needs more distinct values than one dictionary can hold.
    DictionaryOverflow {
        column: &'static str,
    },
    /// An unsigned Beacon value does not fit the signed OTLP/Arrow column.
    ValueOutOfRange {
        column: &'static str,
    },
    BatchIdExhausted,
    /// A data point that OTLP does not allow (named field), such as an
    /// exponential histogram whose bucket counts do not add up to `count`.
    InvalidPoint(&'static str),
    Arrow(ArrowError),
}
impl EncodeError {
    /// Whether a smaller batch might succeed (the `*_split` entry points
    /// split on these).
    pub fn is_capacity(&self) -> bool {
        matches!(
            self,
            Self::RowLimit
                | Self::AttributeRowLimit
                | Self::RelatedRowLimit(_)
                | Self::DictionaryOverflow { .. }
        )
    }
}
impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EncodeError {}
impl From<ArrowError> for EncodeError {
    fn from(e: ArrowError) -> Self {
        Self::Arrow(e)
    }
}

/// A span with its exported attributes, kind, trace state, events, links and
/// dropped counts. `From<SpanRecord>` carries only the record's primary
/// attribute and leaves the rest empty.
#[derive(Clone, Debug, PartialEq)]
pub struct OtapSpan {
    pub record: SpanRecord,
    pub attributes: SpanAttributes,
    pub kind: SpanKind,
    pub trace_state: Option<TraceState>,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
    pub dropped_attributes_count: u32,
    pub dropped_events_count: u32,
    pub dropped_links_count: u32,
}
impl From<SpanRecord> for OtapSpan {
    fn from(record: SpanRecord) -> Self {
        Self {
            record,
            attributes: SpanAttributes::new(),
            kind: SpanKind::Unspecified,
            trace_state: None,
            events: Vec::new(),
            links: Vec::new(),
            dropped_attributes_count: 0,
            dropped_events_count: 0,
            dropped_links_count: 0,
        }
    }
}

/// Exported span name of each fixed Beacon schema.
pub fn span_name(schema: SpanSchema) -> &'static str {
    match schema {
        SpanSchema::Request => "vesper.request",
        SpanSchema::StorageAppend => "vesper.storage_append",
        SpanSchema::Background => "vesper.background",
    }
}
/// Exported `status.status_message` of each Beacon error code.
pub fn error_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidInput => "invalid_input",
        ErrorCode::Storage => "storage",
        ErrorCode::Timeout => "timeout",
        ErrorCode::Cancelled => "cancelled",
    }
}
/// Reserved attribute keys carrying a record's `PrimaryAttribute`.
pub const ITEM_COUNT_KEY: &str = "vesper.item_count";
pub const BYTE_COUNT_KEY: &str = "vesper.byte_count";
pub const PRIMARY_REDACTED_KEY: &str = "vesper.attribute";

/// The attributes a span exports: its own plus the reserved key carrying the
/// record's primary attribute.
pub(crate) fn exported_attributes(span: &OtapSpan) -> Result<SpanAttributes, EncodeError> {
    let mut all = span.attributes.clone();
    match span.record.attribute {
        PrimaryAttribute::None => {}
        PrimaryAttribute::ItemCount(v) => all.push_reserved(
            ITEM_COUNT_KEY,
            AttributeValue::Int(signed(v, ITEM_COUNT_KEY)?),
        ),
        PrimaryAttribute::ByteCount(v) => all.push_reserved(
            BYTE_COUNT_KEY,
            AttributeValue::Int(signed(v, BYTE_COUNT_KEY)?),
        ),
        PrimaryAttribute::Redacted(_) => {
            all.push_reserved(PRIMARY_REDACTED_KEY, AttributeValue::Redacted)
        }
    }
    Ok(all)
}

// ------------------------------------------------------------------ columns

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    U8,
    U16,
}
impl Key {
    fn cap(self) -> usize {
        match self {
            Key::U8 => 256,
            Key::U16 => 65_536,
        }
    }
}

/// Planned column values, in schema order. Building them into Arrow arrays
/// is the only step that touches stream dictionaries.
enum Col {
    U8(Vec<Option<u8>>),
    U16(Vec<Option<u16>>),
    U32(Vec<Option<u32>>),
    U64(Vec<Option<u64>>),
    I32(Vec<Option<i32>>),
    I64(Vec<Option<i64>>),
    F64(Vec<Option<f64>>),
    Bool(Vec<Option<bool>>),
    Time(Vec<Option<i64>>),
    Duration(Vec<Option<i64>>),
    Fixed(Vec<Option<Vec<u8>>>),
    Utf8(Vec<Option<String>>),
    Binary(Vec<Option<Vec<u8>>>),
    Dict(&'static str, Key, Vec<Option<String>>),
    Struct(Vec<Col>),
    ListU64(Vec<Option<Vec<u64>>>),
    ListF64(Vec<Option<Vec<f64>>>),
}
impl Col {
    fn dicts<'a>(&'a self, out: &mut Vec<(&'static str, Key, BTreeSet<&'a str>)>) {
        match self {
            Col::Dict(name, key, values) => out.push((
                name,
                *key,
                values.iter().flatten().map(String::as_str).collect(),
            )),
            Col::Struct(children) => children.iter().for_each(|c| c.dicts(out)),
            _ => {}
        }
    }
    fn build(
        self,
        field: &Field,
        dicts: &mut std::slice::IterMut<'_, Dict>,
    ) -> Result<ArrayRef, EncodeError> {
        Ok(match self {
            Col::U8(v) => Arc::new(UInt8Array::from(v)),
            Col::U16(v) => Arc::new(UInt16Array::from(v)),
            Col::U32(v) => Arc::new(UInt32Array::from(v)),
            Col::U64(v) => Arc::new(UInt64Array::from(v)),
            Col::I32(v) => Arc::new(Int32Array::from(v)),
            Col::I64(v) => Arc::new(Int64Array::from(v)),
            Col::F64(v) => Arc::new(Float64Array::from(v)),
            Col::Bool(v) => Arc::new(BooleanArray::from(v)),
            Col::Time(v) => Arc::new(TimestampNanosecondArray::from(v)),
            Col::Duration(v) => Arc::new(DurationNanosecondArray::from(v)),
            Col::Fixed(v) => {
                let DataType::FixedSizeBinary(width) = field.data_type() else {
                    unreachable!("schema and plan agree")
                };
                Arc::new(fixed(v, *width)?)
            }
            Col::Utf8(v) => Arc::new(StringArray::from(v)),
            Col::Binary(v) => Arc::new(BinaryArray::from_iter(v)),
            Col::Dict(_, key, v) => {
                let dict = dicts.next().expect("one dictionary per dictionary column");
                match key {
                    Key::U8 => dict.u8_array(&v)?,
                    Key::U16 => dict.u16_array(&v)?,
                }
            }
            Col::Struct(children) => {
                let DataType::Struct(fields) = field.data_type() else {
                    unreachable!("schema and plan agree")
                };
                let arrays = children
                    .into_iter()
                    .zip(fields.iter())
                    .map(|(c, f)| c.build(f, dicts))
                    .collect::<Result<Vec<_>, _>>()?;
                Arc::new(StructArray::try_new(fields.clone(), arrays, None)?)
            }
            Col::ListU64(v) => {
                let mut b = ListBuilder::new(UInt64Builder::new());
                for row in v {
                    match row {
                        Some(values) => {
                            b.values().append_slice(&values);
                            b.append(true);
                        }
                        None => b.append(false),
                    }
                }
                Arc::new(b.finish())
            }
            Col::ListF64(v) => {
                let mut b = ListBuilder::new(Float64Builder::new());
                for row in v {
                    match row {
                        Some(values) => {
                            b.values().append_slice(&values);
                            b.append(true);
                        }
                        None => b.append(false),
                    }
                }
                Arc::new(b.finish())
            }
        })
    }
}

fn fixed(values: Vec<Option<Vec<u8>>>, width: i32) -> Result<FixedSizeBinaryArray, ArrowError> {
    if values.is_empty() {
        return Ok(FixedSizeBinaryArray::new_null(width, 0));
    }
    if values.iter().all(Option::is_some) {
        FixedSizeBinaryArray::try_from_iter(values.into_iter().flatten())
    } else {
        FixedSizeBinaryArray::try_from_sparse_iter_with_size(values.into_iter(), width)
    }
}

/// Append-only string dictionary of one column of one stream.
#[derive(Debug)]
struct Dict {
    values: Vec<String>,
    index: HashMap<String, usize>,
    cap: usize,
}
impl Dict {
    fn new(cap: usize) -> Self {
        Self {
            values: Vec::new(),
            index: HashMap::new(),
            cap,
        }
    }
    fn fits(&self, distinct: &BTreeSet<&str>) -> bool {
        let missing = distinct
            .iter()
            .filter(|s| !self.index.contains_key(**s))
            .count();
        self.values.len() + missing <= self.cap
    }
    fn intern(&mut self, s: &str) -> usize {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.values.len();
        self.values.push(s.to_owned());
        self.index.insert(s.to_owned(), i);
        i
    }
    fn values(&self) -> ArrayRef {
        Arc::new(StringArray::from_iter_values(self.values.iter()))
    }
    fn u8_array(&mut self, items: &[Option<String>]) -> Result<ArrayRef, ArrowError> {
        let keys: UInt8Array = items
            .iter()
            .map(|s| s.as_ref().map(|s| self.intern(s) as u8))
            .collect();
        Ok(Arc::new(DictionaryArray::<UInt8Type>::try_new(
            keys,
            self.values(),
        )?))
    }
    fn u16_array(&mut self, items: &[Option<String>]) -> Result<ArrayRef, ArrowError> {
        let keys: UInt16Array = items
            .iter()
            .map(|s| s.as_ref().map(|s| self.intern(s) as u16))
            .collect();
        Ok(Arc::new(DictionaryArray::<UInt16Type>::try_new(
            keys,
            self.values(),
        )?))
    }
}

// ------------------------------------------------------------------ streams

/// One payload type's live IPC stream.
struct TxStream {
    schema_id: String,
    writer: StreamWriter<Vec<u8>>,
    dicts: Vec<Dict>,
}

/// Static description of a payload stream.
struct Kind {
    ty: ArrowPayloadType,
    prefix: &'static str,
    schema: fn(IdEncoding) -> Schema,
    /// Rows above [`MAX_BATCH_ROWS`] map to this error.
    limit: Limit,
}
#[derive(Clone, Copy)]
enum Limit {
    Main,
    Attributes,
    Related,
}
const SPANS: Kind = Kind {
    ty: ArrowPayloadType::Spans,
    prefix: "spans",
    schema: schema::spans_schema_with,
    limit: Limit::Main,
};
const SPAN_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::SpanAttrs,
    prefix: "span_attrs",
    schema: schema::attrs16_schema_with,
    limit: Limit::Attributes,
};
const RESOURCE_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::ResourceAttrs,
    prefix: "resource_attrs",
    schema: schema::attrs16_schema_with,
    limit: Limit::Attributes,
};
const SCOPE_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::ScopeAttrs,
    prefix: "scope_attrs",
    schema: schema::attrs16_schema_with,
    limit: Limit::Attributes,
};
const SPAN_EVENTS: Kind = Kind {
    ty: ArrowPayloadType::SpanEvents,
    prefix: "span_events",
    schema: schema::span_events_schema_with,
    limit: Limit::Related,
};
const SPAN_EVENT_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::SpanEventAttrs,
    prefix: "span_event_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const SPAN_LINKS: Kind = Kind {
    ty: ArrowPayloadType::SpanLinks,
    prefix: "span_links",
    schema: schema::span_links_schema_with,
    limit: Limit::Related,
};
const SPAN_LINK_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::SpanLinkAttrs,
    prefix: "span_link_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const METRICS: Kind = Kind {
    ty: ArrowPayloadType::UnivariateMetrics,
    prefix: "metrics",
    schema: schema::metrics_schema_with,
    limit: Limit::Main,
};
const HISTOGRAM_POINTS: Kind = Kind {
    ty: ArrowPayloadType::HistogramDataPoints,
    prefix: "histogram_dps",
    schema: schema::histogram_points_schema_with,
    limit: Limit::Related,
};
const HISTOGRAM_DP_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::HistogramDpAttrs,
    prefix: "histogram_dp_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const NUMBER_POINTS: Kind = Kind {
    ty: ArrowPayloadType::NumberDataPoints,
    prefix: "number_dps",
    schema: schema::number_points_schema_with,
    limit: Limit::Related,
};
const NUMBER_DP_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::NumberDpAttrs,
    prefix: "number_dp_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const NUMBER_DP_EXEMPLARS: Kind = Kind {
    ty: ArrowPayloadType::NumberDpExemplars,
    prefix: "number_dp_exemplars",
    schema: schema::exemplars_schema_with,
    limit: Limit::Related,
};
const NUMBER_DP_EXEMPLAR_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::NumberDpExemplarAttrs,
    prefix: "number_dp_exemplar_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const HISTOGRAM_DP_EXEMPLARS: Kind = Kind {
    ty: ArrowPayloadType::HistogramDpExemplars,
    prefix: "histogram_dp_exemplars",
    schema: schema::exemplars_schema_with,
    limit: Limit::Related,
};
const HISTOGRAM_DP_EXEMPLAR_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::HistogramDpExemplarAttrs,
    prefix: "histogram_dp_exemplar_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const EXP_HISTOGRAM_POINTS: Kind = Kind {
    ty: ArrowPayloadType::ExpHistogramDataPoints,
    prefix: "exp_histogram_dps",
    schema: schema::exp_histogram_points_schema_with,
    limit: Limit::Related,
};
const EXP_HISTOGRAM_DP_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::ExpHistogramDpAttrs,
    prefix: "exp_histogram_dp_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const EXP_HISTOGRAM_DP_EXEMPLARS: Kind = Kind {
    ty: ArrowPayloadType::ExpHistogramDpExemplars,
    prefix: "exp_histogram_dp_exemplars",
    schema: schema::exemplars_schema_with,
    limit: Limit::Related,
};
const EXP_HISTOGRAM_DP_EXEMPLAR_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::ExpHistogramDpExemplarAttrs,
    prefix: "exp_histogram_dp_exemplar_attrs",
    schema: schema::attrs32_schema_with,
    limit: Limit::Attributes,
};
const LOGS: Kind = Kind {
    ty: ArrowPayloadType::Logs,
    prefix: "logs",
    schema: schema::logs_schema_with,
    limit: Limit::Main,
};
const LOG_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::LogAttrs,
    prefix: "log_attrs",
    schema: schema::attrs16_schema_with,
    limit: Limit::Attributes,
};

/// One planned payload.
struct Plan {
    kind: &'static Kind,
    rows: usize,
    cols: Vec<Col>,
    /// Emitted even when empty (main records, histogram points).
    always: bool,
}

/// Batch ids, stream generations and live streams of one OTAP signal stream.
struct StreamSet {
    config: EncoderConfig,
    next_batch_id: i64,
    next_generation: u64,
    streams: BTreeMap<ArrowPayloadType, TxStream>,
    resets: u64,
}
impl StreamSet {
    fn new(config: EncoderConfig) -> Self {
        Self {
            config,
            next_batch_id: 0,
            next_generation: 0,
            streams: BTreeMap::new(),
            resets: 0,
        }
    }
    fn dict_cap(&self, key: Key) -> usize {
        self.config.max_dictionary_entries.min(key.cap())
    }
    /// Pure check of one batch: row limits and dictionary sizes.
    fn check(&self, plans: &[Plan]) -> Result<(), EncodeError> {
        for plan in plans {
            let max = match plan.kind.limit {
                Limit::Main => self.config.max_rows.min(MAX_BATCH_ROWS),
                Limit::Attributes | Limit::Related => MAX_BATCH_ROWS,
            };
            if plan.rows > max {
                return Err(match plan.kind.limit {
                    Limit::Main => EncodeError::RowLimit,
                    Limit::Attributes => EncodeError::AttributeRowLimit,
                    Limit::Related => EncodeError::RelatedRowLimit(plan.kind.ty),
                });
            }
            let mut dicts = Vec::new();
            plan.cols.iter().for_each(|c| c.dicts(&mut dicts));
            for (column, key, distinct) in &dicts {
                if distinct.len() > self.dict_cap(*key) {
                    return Err(EncodeError::DictionaryOverflow { column });
                }
            }
        }
        Ok(())
    }
    fn open(&mut self, kind: &Kind, caps: &[usize]) -> Result<(), EncodeError> {
        let schema: SchemaRef = Arc::new((kind.schema)(self.config.id_encoding));
        let compression = match self.config.ipc_compression {
            IpcCompression::None => None,
            IpcCompression::Zstd => Some(arrow_ipc::CompressionType::ZSTD),
        };
        let options = IpcWriteOptions::try_new(8, false, MetadataVersion::V5)?
            .with_dictionary_handling(DictionaryHandling::Delta)
            .try_with_compression(compression)?;
        let writer = StreamWriter::try_new_with_options(Vec::new(), &schema, options)?;
        let generation = self.next_generation;
        self.next_generation += 1;
        let stream = TxStream {
            schema_id: format!("{}:{generation}", kind.prefix),
            writer,
            dicts: caps.iter().map(|c| Dict::new(*c)).collect(),
        };
        if self.streams.insert(kind.ty, stream).is_some() {
            self.resets += 1;
        }
        Ok(())
    }
    /// Writes checked plans. Any error resets every stream.
    fn write(&mut self, plans: Vec<Plan>) -> Result<BatchArrowRecords, EncodeError> {
        let result = self.write_inner(plans);
        if result.is_err() {
            self.reset();
        }
        result
    }
    fn write_inner(&mut self, plans: Vec<Plan>) -> Result<BatchArrowRecords, EncodeError> {
        let mut payloads = Vec::with_capacity(plans.len());
        for plan in plans {
            if plan.rows == 0 && !plan.always {
                continue;
            }
            let kind = plan.kind;
            let (caps, fits) = {
                let mut dicts = Vec::new();
                plan.cols.iter().for_each(|c| c.dicts(&mut dicts));
                let caps: Vec<usize> = dicts.iter().map(|(_, k, _)| self.dict_cap(*k)).collect();
                let fits = self.streams.get(&kind.ty).is_some_and(|s| {
                    s.dicts.len() == dicts.len()
                        && s.dicts
                            .iter()
                            .zip(&dicts)
                            .all(|(d, (_, _, distinct))| d.fits(distinct))
                });
                (caps, fits)
            };
            if !fits {
                self.open(kind, &caps)?;
            }
            let stream = self.streams.get_mut(&kind.ty).expect("opened above");
            let schema = Arc::new((kind.schema)(self.config.id_encoding));
            let mut dicts = stream.dicts.iter_mut();
            let arrays = plan
                .cols
                .into_iter()
                .zip(schema.fields().iter())
                .map(|(c, f)| c.build(f, &mut dicts))
                .collect::<Result<Vec<_>, _>>()?;
            let batch = RecordBatch::try_new(schema, arrays)?;
            stream.writer.write(&batch)?;
            let record = std::mem::take(stream.writer.get_mut());
            payloads.push(ArrowPayload {
                schema_id: stream.schema_id.clone(),
                r#type: kind.ty as i32,
                record,
            });
        }
        let batch_id = self.next_batch_id;
        self.next_batch_id = batch_id
            .checked_add(1)
            .ok_or(EncodeError::BatchIdExhausted)?;
        Ok(BatchArrowRecords {
            batch_id,
            arrow_payloads: payloads,
            headers: Vec::new(),
        })
    }
    fn reset(&mut self) {
        if !self.streams.is_empty() {
            self.resets += 1;
        }
        self.streams.clear();
    }
}

pub(crate) fn signed(value: u64, column: &'static str) -> Result<i64, EncodeError> {
    i64::try_from(value).map_err(|_| EncodeError::ValueOutOfRange { column })
}

// ------------------------------------------------------------------ id encodings

/// Delta-encodes non-null ids (each minus the previous non-null id) unless
/// the encoding is plain. Ids must be nondecreasing; Beacon assigns them in
/// row order.
fn delta_ids(ids: Vec<Option<u32>>, encoding: IdEncoding) -> Vec<Option<u32>> {
    if encoding == IdEncoding::Plain {
        return ids;
    }
    let mut prev = 0u32;
    ids.into_iter()
        .map(|id| {
            id.map(|id| {
                let delta = id - prev;
                prev = id;
                delta
            })
        })
        .collect()
}
/// Quasi-delta encodes parent ids: a difference to the previous row's parent
/// when `same_as_previous(i)`, absolute otherwise.
fn quasi_delta(
    parents: &[u32],
    same_as_previous: impl Fn(usize) -> bool,
    encoding: IdEncoding,
) -> Vec<u32> {
    (0..parents.len())
        .map(|i| {
            if encoding == IdEncoding::Transport && i > 0 && same_as_previous(i) {
                parents[i] - parents[i - 1]
            } else {
                parents[i]
            }
        })
        .collect()
}
fn to_u16(v: Vec<Option<u32>>) -> Vec<Option<u16>> {
    v.into_iter().map(|x| x.map(|x| x as u16)).collect()
}

// ------------------------------------------------------------------ attributes

/// One flattened attribute row.
struct AttrRow {
    parent: u32,
    key: &'static str,
    value: AttrCell,
}
enum AttrCell {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    /// CBOR of a slice or map, with its type code.
    Ser(u8, Vec<u8>),
}
impl AttrCell {
    fn of(value: &AttributeValue) -> Self {
        match value {
            AttributeValue::Bool(b) => AttrCell::Bool(*b),
            AttributeValue::Int(v) => AttrCell::Int(*v),
            AttributeValue::Double(v) => AttrCell::Double(*v),
            AttributeValue::Bytes(b) => AttrCell::Bytes(b.to_vec()),
            AttributeValue::Slice(_) => AttrCell::Ser(value_type::SLICE, cbor::encode(value)),
            AttributeValue::Map(_) => AttrCell::Ser(value_type::MAP, cbor::encode(value)),
            other => AttrCell::Str(other.export_str().expect("string forms")),
        }
    }
    fn type_code(&self) -> u8 {
        match self {
            AttrCell::Str(_) => value_type::STR,
            AttrCell::Int(_) => value_type::INT,
            AttrCell::Double(_) => value_type::DOUBLE,
            AttrCell::Bool(_) => value_type::BOOL,
            AttrCell::Bytes(_) => value_type::BYTES,
            AttrCell::Ser(t, _) => *t,
        }
    }
    /// Upstream `carrow.Equal`: scalars and bytes by value (`f64 ==`),
    /// slices and maps never equal.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (AttrCell::Str(a), AttrCell::Str(b)) => a == b,
            (AttrCell::Int(a), AttrCell::Int(b)) => a == b,
            (AttrCell::Double(a), AttrCell::Double(b)) => a == b,
            (AttrCell::Bool(a), AttrCell::Bool(b)) => a == b,
            (AttrCell::Bytes(a), AttrCell::Bytes(b)) => a == b,
            _ => false,
        }
    }
}
/// The `type`, `str`, `int`, `double`, `bool`, `bytes` and `ser` cells of
/// one value (attribute or log body).
struct Cells {
    types: Vec<Option<u8>>,
    strs: Vec<Option<String>>,
    ints: Vec<Option<i64>>,
    doubles: Vec<Option<f64>>,
    bools: Vec<Option<bool>>,
    bytes: Vec<Option<Vec<u8>>>,
    sers: Vec<Option<Vec<u8>>>,
}
impl Cells {
    fn with_capacity(n: usize) -> Self {
        Self {
            types: Vec::with_capacity(n),
            strs: Vec::with_capacity(n),
            ints: Vec::with_capacity(n),
            doubles: Vec::with_capacity(n),
            bools: Vec::with_capacity(n),
            bytes: Vec::with_capacity(n),
            sers: Vec::with_capacity(n),
        }
    }
    fn push(&mut self, cell: Option<AttrCell>) {
        self.types.push(Some(
            cell.as_ref().map_or(value_type::EMPTY, AttrCell::type_code),
        ));
        let (mut s, mut i, mut d, mut b, mut y, mut z) = (None, None, None, None, None, None);
        match cell {
            None => {}
            Some(AttrCell::Str(v)) => s = Some(v),
            Some(AttrCell::Int(v)) => i = Some(v),
            Some(AttrCell::Double(v)) => d = Some(v),
            Some(AttrCell::Bool(v)) => b = Some(v),
            Some(AttrCell::Bytes(v)) => y = Some(v),
            Some(AttrCell::Ser(_, v)) => z = Some(v),
        }
        self.strs.push(s);
        self.ints.push(i);
        self.doubles.push(d);
        self.bools.push(b);
        self.bytes.push(y);
        self.sers.push(z);
    }
    /// Columns in schema order, after `type` (which comes first).
    fn into_cols(self, str_name: &'static str) -> [Col; 7] {
        [
            Col::U8(self.types),
            Col::Dict(str_name, Key::U16, self.strs),
            Col::I64(self.ints),
            Col::F64(self.doubles),
            Col::Bool(self.bools),
            Col::Binary(self.bytes),
            Col::Binary(self.sers),
        ]
    }
}
fn push_attrs(rows: &mut Vec<AttrRow>, parent: u32, attrs: &Attributes) {
    for (key, value) in attrs.iter() {
        rows.push(AttrRow {
            parent,
            key,
            value: AttrCell::of(&value),
        });
    }
}
fn attrs_plan(kind: &'static Kind, rows: Vec<AttrRow>, wide: bool, encoding: IdEncoding) -> Plan {
    let parents: Vec<u32> = rows.iter().map(|r| r.parent).collect();
    // Upstream `AttrsParentIDDecoder`: equal key and `carrow.Equal` value.
    let parents = quasi_delta(
        &parents,
        |i| rows[i].key == rows[i - 1].key && rows[i].value.same(&rows[i - 1].value),
        encoding,
    );
    let parent = if wide {
        Col::U32(parents.into_iter().map(Some).collect())
    } else {
        Col::U16(parents.into_iter().map(|p| Some(p as u16)).collect())
    };
    let n = rows.len();
    let mut keys = Vec::with_capacity(n);
    let mut cells = Cells::with_capacity(n);
    for row in rows {
        keys.push(Some(row.key.to_owned()));
        cells.push(Some(row.value));
    }
    let [types, strs, ints, doubles, bools, bytes, sers] = cells.into_cols("str");
    Plan {
        kind,
        rows: n,
        cols: vec![
            parent,
            Col::Dict("key", Key::U8, keys),
            types,
            strs,
            ints,
            doubles,
            bools,
            bytes,
            sers,
        ],
        always: false,
    }
}

// ------------------------------------------------------------------ resource and scope

/// One item with its resource and scope; `group` identifies the scope group
/// (resource group index, scope index) so consecutive items can be grouped.
struct Entry<'a, T> {
    group: (usize, usize),
    resource: &'a Resource,
    scope: &'a Scope,
    item: &'a T,
}
fn flatten<T>(groups: &[ResourceGroup<T>]) -> Vec<Entry<'_, T>> {
    let mut out = Vec::new();
    for (g, rg) in groups.iter().enumerate() {
        for (s, sg) in rg.scopes.iter().enumerate() {
            for item in &sg.items {
                out.push(Entry {
                    group: (g, s),
                    resource: &rg.resource,
                    scope: &sg.scope,
                    item,
                });
            }
        }
    }
    out
}

/// Resource and scope ids of a chunk's rows, numbered from 0 in row order.
struct Groups<'a> {
    resource_ids: Vec<u32>,
    scope_ids: Vec<u32>,
    resources: Vec<&'a Resource>,
    scopes: Vec<&'a Scope>,
}
fn groups<'a, T>(entries: &[Entry<'a, T>]) -> Groups<'a> {
    let mut g = Groups {
        resource_ids: Vec::with_capacity(entries.len()),
        scope_ids: Vec::with_capacity(entries.len()),
        resources: Vec::new(),
        scopes: Vec::new(),
    };
    let mut prev: Option<(usize, usize)> = None;
    for e in entries {
        if prev.map(|p| p.0) != Some(e.group.0) {
            g.resources.push(e.resource);
        }
        if prev != Some(e.group) {
            g.scopes.push(e.scope);
        }
        prev = Some(e.group);
        g.resource_ids.push(g.resources.len() as u32 - 1);
        g.scope_ids.push(g.scopes.len() as u32 - 1);
    }
    g
}
fn nonzero(v: u32) -> Option<u32> {
    (v != 0).then_some(v)
}
fn opt_str(s: Option<&'static str>) -> Option<String> {
    s.map(str::to_owned)
}
/// `resource`, `scope` and `schema_url` columns plus the `RESOURCE_ATTRS` and
/// `SCOPE_ATTRS` plans.
fn resource_scope(g: &Groups<'_>, encoding: IdEncoding) -> ([Col; 3], [Plan; 2]) {
    let n = g.resource_ids.len();
    let rid = to_u16(delta_ids(
        g.resource_ids.iter().map(|i| Some(*i)).collect(),
        encoding,
    ));
    let sid = to_u16(delta_ids(
        g.scope_ids.iter().map(|i| Some(*i)).collect(),
        encoding,
    ));
    let mut r_url = Vec::with_capacity(n);
    let mut r_dropped = Vec::with_capacity(n);
    let mut s_name = Vec::with_capacity(n);
    let mut s_version = Vec::with_capacity(n);
    let mut s_dropped = Vec::with_capacity(n);
    let mut s_url = Vec::with_capacity(n);
    for i in 0..n {
        let r = g.resources[g.resource_ids[i] as usize];
        let s = g.scopes[g.scope_ids[i] as usize];
        r_url.push(opt_str(r.schema_url));
        r_dropped.push(nonzero(r.dropped_attributes_count));
        s_name.push(opt_str(s.name));
        s_version.push(opt_str(s.version));
        s_dropped.push(nonzero(s.dropped_attributes_count));
        s_url.push(opt_str(s.schema_url));
    }
    let mut r_rows = Vec::new();
    for (i, r) in g.resources.iter().enumerate() {
        push_attrs(&mut r_rows, i as u32, &r.attributes);
    }
    let mut s_rows = Vec::new();
    for (i, s) in g.scopes.iter().enumerate() {
        push_attrs(&mut s_rows, i as u32, &s.attributes);
    }
    (
        [
            Col::Struct(vec![
                Col::U16(rid),
                Col::Dict("resource.schema_url", Key::U8, r_url),
                Col::U32(r_dropped),
            ]),
            Col::Struct(vec![
                Col::U16(sid),
                Col::Dict("scope.name", Key::U8, s_name),
                Col::Dict("scope.version", Key::U8, s_version),
                Col::U32(s_dropped),
            ]),
            Col::Dict("schema_url", Key::U8, s_url),
        ],
        [
            attrs_plan(&RESOURCE_ATTRS, r_rows, false, encoding),
            attrs_plan(&SCOPE_ATTRS, s_rows, false, encoding),
        ],
    )
}

/// Encodes `entries`, splitting it in halves while a half is too large,
/// until every piece fits or a single item alone does not fit.
fn encode_chunked<T>(
    set: &mut StreamSet,
    entries: &[Entry<'_, T>],
    plan: &impl Fn(&[Entry<'_, T>], IdEncoding) -> Result<Vec<Plan>, EncodeError>,
    out: &mut Vec<BatchArrowRecords>,
) -> Result<(), EncodeError> {
    let encoding = set.config.id_encoding;
    let checked = plan(entries, encoding).and_then(|p| set.check(&p).map(|()| p));
    match checked {
        Ok(plans) => {
            out.push(set.write(plans)?);
            Ok(())
        }
        Err(e) if e.is_capacity() && entries.len() > 1 => {
            let mid = entries.len() / 2;
            encode_chunked(set, &entries[..mid], plan, out)?;
            encode_chunked(set, &entries[mid..], plan, out)
        }
        Err(e) => Err(e),
    }
}
/// [`encode_chunked`] over all of `entries`. Pieces already written advance
/// the streams; if a later piece fails they are discarded with the error, so
/// the streams are reset and the next batch starts fresh schemas the
/// receiver can read.
fn encode_split_all<T>(
    set: &mut StreamSet,
    entries: &[Entry<'_, T>],
    plan: &impl Fn(&[Entry<'_, T>], IdEncoding) -> Result<Vec<Plan>, EncodeError>,
) -> Result<Vec<BatchArrowRecords>, EncodeError> {
    let mut out = Vec::new();
    match encode_chunked(set, entries, plan, &mut out) {
        Ok(()) => Ok(out),
        Err(e) => {
            if !out.is_empty() {
                set.reset();
            }
            Err(e)
        }
    }
}
fn encode_one<T>(
    set: &mut StreamSet,
    entries: &[Entry<'_, T>],
    plan: impl Fn(&[Entry<'_, T>], IdEncoding) -> Result<Vec<Plan>, EncodeError>,
) -> Result<BatchArrowRecords, EncodeError> {
    let plans = plan(entries, set.config.id_encoding)?;
    set.check(&plans)?;
    set.write(plans)
}

fn default_group<T: Clone>(items: &[T]) -> [ResourceGroup<T>; 1] {
    [ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        items.to_vec(),
    )]
}

// ------------------------------------------------------------------ traces

fn be(v: impl Into<u128>, width: usize) -> Vec<u8> {
    v.into().to_be_bytes()[16 - width..].to_vec()
}

fn plan_traces(
    entries: &[Entry<'_, OtapSpan>],
    encoding: IdEncoding,
) -> Result<Vec<Plan>, EncodeError> {
    let n = entries.len();
    let g = groups(entries);
    let (rs_cols, rs_attrs) = resource_scope(&g, encoding);
    let mut start = Vec::with_capacity(n);
    let mut duration = Vec::with_capacity(n);
    let mut trace_ids = Vec::with_capacity(n);
    let mut span_ids = Vec::with_capacity(n);
    let mut trace_states = Vec::with_capacity(n);
    let mut parents = Vec::with_capacity(n);
    let mut flags = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut kinds = Vec::with_capacity(n);
    let mut dropped = [
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    ];
    let mut codes = Vec::with_capacity(n);
    let mut messages = Vec::with_capacity(n);
    let mut attrs = Vec::new();
    let mut events: Vec<(u32, &SpanEvent)> = Vec::new();
    let mut links: Vec<(u32, &SpanLink)> = Vec::new();
    // Row counts are checked before ids are narrowed to u16.
    if n > MAX_BATCH_ROWS {
        return Err(EncodeError::RowLimit);
    }
    for (i, e) in entries.iter().enumerate() {
        let span = e.item;
        let r = &span.record;
        let id = i as u32;
        start.push(Some(signed(r.start_unix_ns, "start_time_unix_nano")?));
        duration.push(Some(signed(r.duration_ns, "duration_time_unix_nano")?));
        trace_ids.push(Some(be(r.context.trace_id(), 16)));
        span_ids.push(Some(be(r.context.span_id(), 8)));
        trace_states.push(span.trace_state.as_ref().map(|t| t.as_str().to_owned()));
        parents.push((r.parent_span_id != 0).then(|| be(r.parent_span_id, 8)));
        flags.push(Some(u32::from(r.context.trace_flags())));
        names.push(Some(span_name(r.schema).to_owned()));
        kinds.push(Some(span.kind as i32));
        dropped[0].push(nonzero(span.dropped_attributes_count));
        dropped[1].push(nonzero(span.dropped_events_count));
        dropped[2].push(nonzero(span.dropped_links_count));
        match r.status {
            SpanStatus::Ok => {
                codes.push(Some(schema::STATUS_CODE_OK));
                messages.push(None);
            }
            SpanStatus::Error(code) => {
                codes.push(Some(schema::STATUS_CODE_ERROR));
                messages.push(Some(error_message(code).to_owned()));
            }
        }
        push_attrs(&mut attrs, id, &exported_attributes(span)?);
        events.extend(span.events.iter().map(|ev| (id, ev)));
        links.extend(span.links.iter().map(|l| (id, l)));
        if attrs.len() > MAX_BATCH_ROWS {
            return Err(EncodeError::AttributeRowLimit);
        }
    }
    let [resource, scope, schema_url] = rs_cols;
    let [dropped_attrs, dropped_events, dropped_links] = dropped;
    let spans = Plan {
        kind: &SPANS,
        rows: n,
        cols: vec![
            Col::U16(to_u16(delta_ids(
                (0..n as u32).map(Some).collect(),
                encoding,
            ))),
            resource,
            scope,
            schema_url,
            Col::Time(start),
            Col::Duration(duration),
            Col::Fixed(trace_ids),
            Col::Fixed(span_ids),
            Col::Utf8(trace_states),
            Col::Fixed(parents),
            Col::U32(flags),
            Col::Dict("name", Key::U8, names),
            Col::I32(kinds),
            Col::U32(dropped_attrs),
            Col::U32(dropped_events),
            Col::U32(dropped_links),
            Col::Struct(vec![
                Col::I32(codes),
                Col::Dict("status_message", Key::U8, messages),
            ]),
        ],
        always: true,
    };
    if events.len() > MAX_BATCH_ROWS {
        return Err(EncodeError::RelatedRowLimit(ArrowPayloadType::SpanEvents));
    }
    if links.len() > MAX_BATCH_ROWS {
        return Err(EncodeError::RelatedRowLimit(ArrowPayloadType::SpanLinks));
    }
    // Events: `id` (delta) names the event for its attributes; `parent_id`
    // is the span id, quasi-delta keyed on the event name.
    let mut event_attrs = Vec::new();
    let mut ev_time = Vec::with_capacity(events.len());
    let mut ev_name = Vec::with_capacity(events.len());
    let mut ev_dropped = Vec::with_capacity(events.len());
    for (k, (_, ev)) in events.iter().enumerate() {
        ev_time.push(Some(signed(ev.time_unix_ns, "time_unix_nano")?));
        ev_name.push(Some(ev.name.to_owned()));
        ev_dropped.push(nonzero(ev.dropped_attributes_count));
        push_attrs(&mut event_attrs, k as u32, &ev.attributes);
    }
    let ev_parents: Vec<u32> = events.iter().map(|(p, _)| *p).collect();
    let ev_parents = quasi_delta(
        &ev_parents,
        |i| events[i].1.name == events[i - 1].1.name,
        encoding,
    );
    let span_events = Plan {
        kind: &SPAN_EVENTS,
        rows: events.len(),
        cols: vec![
            Col::U32(delta_ids(
                (0..events.len() as u32).map(Some).collect(),
                encoding,
            )),
            Col::U16(ev_parents.into_iter().map(|p| Some(p as u16)).collect()),
            Col::Time(ev_time),
            Col::Dict("span_events.name", Key::U8, ev_name),
            Col::U32(ev_dropped),
        ],
        always: false,
    };
    let mut link_attrs = Vec::new();
    let mut l_trace = Vec::with_capacity(links.len());
    let mut l_span = Vec::with_capacity(links.len());
    let mut l_state = Vec::with_capacity(links.len());
    let mut l_dropped = Vec::with_capacity(links.len());
    let mut l_flags = Vec::with_capacity(links.len());
    for (k, (_, l)) in links.iter().enumerate() {
        l_trace.push(Some(be(l.trace_id, 16)));
        l_span.push(Some(be(l.span_id, 8)));
        l_state.push(l.trace_state.as_ref().map(|t| t.as_str().to_owned()));
        l_dropped.push(nonzero(l.dropped_attributes_count));
        l_flags.push(l.flags);
        push_attrs(&mut link_attrs, k as u32, &l.attributes);
    }
    let l_parents: Vec<u32> = links.iter().map(|(p, _)| *p).collect();
    let l_parents = quasi_delta(
        &l_parents,
        |i| links[i].1.trace_id == links[i - 1].1.trace_id,
        encoding,
    );
    let span_links = Plan {
        kind: &SPAN_LINKS,
        rows: links.len(),
        cols: vec![
            Col::U32(delta_ids(
                (0..links.len() as u32).map(Some).collect(),
                encoding,
            )),
            Col::U16(l_parents.into_iter().map(|p| Some(p as u16)).collect()),
            Col::Fixed(l_trace),
            Col::Fixed(l_span),
            Col::Utf8(l_state),
            Col::U32(l_dropped),
            Col::U32(l_flags),
        ],
        always: false,
    };
    let [resource_attrs, scope_attrs] = rs_attrs;
    Ok(vec![
        spans,
        resource_attrs,
        scope_attrs,
        attrs_plan(&SPAN_ATTRS, attrs, false, encoding),
        span_events,
        attrs_plan(&SPAN_EVENT_ATTRS, event_attrs, true, encoding),
        span_links,
        attrs_plan(&SPAN_LINK_ATTRS, link_attrs, true, encoding),
    ])
}

/// Producer for the `ArrowTracesService` stream: `SPANS` plus
/// `RESOURCE_ATTRS`, `SCOPE_ATTRS`, `SPAN_ATTRS`, `SPAN_EVENTS`,
/// `SPAN_EVENT_ATTRS`, `SPAN_LINKS` and `SPAN_LINK_ATTRS` when nonempty.
pub struct TraceEncoder {
    set: StreamSet,
}
impl TraceEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        Self {
            set: StreamSet::new(config),
        }
    }
    /// Drops every stream; the next batch starts new streams with new ids.
    pub fn reset(&mut self) {
        self.set.reset();
    }
    /// Number of stream resets so far (dictionary overflow, explicit or error).
    pub fn resets(&self) -> u64 {
        self.set.resets
    }
    /// The batch id the next successful encode will use.
    pub fn next_batch_id(&self) -> i64 {
        self.set.next_batch_id
    }

    /// Encodes one batch under an empty resource and scope. A batch that
    /// does not fit (see [`EncodeError::is_capacity`]) is an error and
    /// leaves the streams unchanged; any other error resets every stream.
    pub fn encode_spans(&mut self, spans: &[OtapSpan]) -> Result<BatchArrowRecords, EncodeError> {
        self.encode(&default_group(spans))
    }
    /// Encodes resource-and-scope grouped spans as one batch.
    pub fn encode(
        &mut self,
        groups: &[ResourceGroup<OtapSpan>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        encode_one(&mut self.set, &flatten(groups), plan_traces)
    }
    /// Like [`encode_spans`](Self::encode_spans), but splits the input into
    /// as many batches as needed to respect every row and dictionary limit.
    /// Fails only if a single span does not fit on its own; batches made
    /// before such a failure are discarded and the streams reset.
    pub fn encode_spans_split(
        &mut self,
        spans: &[OtapSpan],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        self.encode_split(&default_group(spans))
    }
    pub fn encode_split(
        &mut self,
        groups: &[ResourceGroup<OtapSpan>],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        encode_split_all(&mut self.set, &flatten(groups), &plan_traces)
    }
}

// ------------------------------------------------------------------ metrics

/// One cumulative explicit-bucket histogram data point.
///
/// Bucket `i` is Beacon bin `i` for every bin up to the last nonzero one;
/// `explicit_bounds[i]` is bin `i`'s inclusive upper end as `f64`, which is
/// exact below 2^53 and rounded above it. `sum` is the histogram's exact
/// sum rounded to `f64`; `min` and `max` are exact below 2^53. Consecutive
/// points of one scope with the same name and unit share one metric row.
#[derive(Clone)]
pub struct HistogramPoint<'a> {
    pub name: &'static str,
    pub unit: &'static str,
    pub start_unix_ns: u64,
    pub time_unix_ns: u64,
    pub histogram: &'a FastLogHistogram,
    pub attributes: Attributes,
    pub exemplars: Vec<Exemplar>,
}
impl<'a> HistogramPoint<'a> {
    pub fn new(
        name: &'static str,
        unit: &'static str,
        start_unix_ns: u64,
        time_unix_ns: u64,
        histogram: &'a FastLogHistogram,
    ) -> Self {
        Self {
            name,
            unit,
            start_unix_ns,
            time_unix_ns,
            histogram,
            attributes: Attributes::new(),
            exemplars: Vec::new(),
        }
    }
    /// One exemplar per bin that holds a span id (see
    /// `FastLogHistogram::record_with_exemplar`), stamped `time_unix_ns`.
    /// Beacon keeps only the span id per bin, so the value is the bin's
    /// lower end (an estimate within 12.5% of the recorded value, exact
    /// below 16) and the trace id is `trace_id`.
    pub fn with_bin_exemplars(mut self, time_unix_ns: u64, trace_id: Option<u128>) -> Self {
        for i in 0..HISTOGRAM_BINS {
            if let Some(span) = self.histogram.exemplar(i) {
                let lower = FastLogHistogram::bin_range(i).expect("bin index").lower;
                let value = i64::try_from(lower)
                    .map_or(NumberValue::Double(lower as f64), NumberValue::Int);
                self.exemplars.push(Exemplar {
                    span_id: Some(span.get()),
                    trace_id,
                    ..Exemplar::new(time_unix_ns, value)
                });
            }
        }
        self
    }
}

/// Bucket counts and bounds of a histogram, up to its last nonzero bin.
pub(crate) fn buckets(h: &FastLogHistogram) -> (Vec<u64>, Vec<f64>) {
    let used = (0..HISTOGRAM_BINS)
        .rev()
        .find(|i| h.count(*i).unwrap_or(0) != 0)
        .map_or(1, |last| last + 1);
    let counts = (0..used).map(|i| h.count(i).unwrap_or(0)).collect();
    let bounds = (0..used - 1)
        .map(|i| FastLogHistogram::bin_range(i).expect("bin index").upper as f64)
        .collect();
    (counts, bounds)
}

/// A metric data point of any kind Beacon exports.
#[derive(Clone)]
pub enum MetricPoint<'a> {
    /// A gauge or sum point.
    Number(NumberPoint),
    Histogram(HistogramPoint<'a>),
    ExpHistogram(ExpHistogramPoint),
}
impl From<NumberPoint> for MetricPoint<'_> {
    fn from(p: NumberPoint) -> Self {
        Self::Number(p)
    }
}
impl<'a> From<HistogramPoint<'a>> for MetricPoint<'a> {
    fn from(p: HistogramPoint<'a>) -> Self {
        Self::Histogram(p)
    }
}
impl From<ExpHistogramPoint> for MetricPoint<'_> {
    fn from(p: ExpHistogramPoint) -> Self {
        Self::ExpHistogram(p)
    }
}

/// The metric row a point belongs to: consecutive points of one scope with
/// equal keys share a row.
#[derive(Clone, Copy, PartialEq)]
pub struct MetricKey {
    pub name: &'static str,
    pub unit: &'static str,
    pub description: Option<&'static str>,
    pub metric_type: u8,
    pub temporality: Option<i32>,
    pub monotonic: Option<bool>,
}
impl MetricPoint<'_> {
    pub fn key(&self) -> MetricKey {
        match self {
            Self::Number(p) => {
                let (metric_type, temporality, monotonic) = match p.kind {
                    NumberKind::Gauge => (schema::METRIC_TYPE_GAUGE, None, None),
                    NumberKind::Sum {
                        monotonic,
                        temporality,
                    } => (
                        schema::METRIC_TYPE_SUM,
                        Some(temporality as i32),
                        Some(monotonic),
                    ),
                };
                MetricKey {
                    name: p.name,
                    unit: p.unit,
                    description: p.description,
                    metric_type,
                    temporality,
                    monotonic,
                }
            }
            Self::Histogram(p) => MetricKey {
                name: p.name,
                unit: p.unit,
                description: None,
                metric_type: schema::METRIC_TYPE_HISTOGRAM,
                temporality: Some(schema::TEMPORALITY_CUMULATIVE),
                monotonic: None,
            },
            Self::ExpHistogram(p) => MetricKey {
                name: p.name,
                unit: p.unit,
                description: p.description,
                metric_type: schema::METRIC_TYPE_EXP_HISTOGRAM,
                temporality: Some(p.temporality as i32),
                monotonic: None,
            },
        }
    }
    fn exemplars(&self) -> &[Exemplar] {
        match self {
            Self::Number(p) => &p.exemplars,
            Self::Histogram(p) => &p.exemplars,
            Self::ExpHistogram(p) => &p.exemplars,
        }
    }
    fn attributes(&self) -> &Attributes {
        match self {
            Self::Number(p) => &p.attributes,
            Self::Histogram(p) => &p.attributes,
            Self::ExpHistogram(p) => &p.attributes,
        }
    }
}

/// Data-point `parent_id`: the metric id, delta-encoded row by row (upstream
/// number, histogram and exponential histogram decoders).
fn point_parents(metric_of: &[u32], encoding: IdEncoding) -> Col {
    let mut prev = 0;
    Col::U16(
        metric_of
            .iter()
            .map(|p| {
                let v = if encoding == IdEncoding::Plain {
                    *p
                } else {
                    let d = p - prev;
                    prev = *p;
                    d
                };
                Some(v as u16)
            })
            .collect(),
    )
}

/// The payload kinds of one data-point family.
struct Family {
    points: &'static Kind,
    attrs: &'static Kind,
    exemplars: &'static Kind,
    exemplar_attrs: &'static Kind,
}
const NUMBER_FAMILY: Family = Family {
    points: &NUMBER_POINTS,
    attrs: &NUMBER_DP_ATTRS,
    exemplars: &NUMBER_DP_EXEMPLARS,
    exemplar_attrs: &NUMBER_DP_EXEMPLAR_ATTRS,
};
const HISTOGRAM_FAMILY: Family = Family {
    points: &HISTOGRAM_POINTS,
    attrs: &HISTOGRAM_DP_ATTRS,
    exemplars: &HISTOGRAM_DP_EXEMPLARS,
    exemplar_attrs: &HISTOGRAM_DP_EXEMPLAR_ATTRS,
};
const EXP_HISTOGRAM_FAMILY: Family = Family {
    points: &EXP_HISTOGRAM_POINTS,
    attrs: &EXP_HISTOGRAM_DP_ATTRS,
    exemplars: &EXP_HISTOGRAM_DP_EXEMPLARS,
    exemplar_attrs: &EXP_HISTOGRAM_DP_EXEMPLAR_ATTRS,
};

/// Per-family rows collected while planning.
#[derive(Default)]
struct Rows<'p, 'a> {
    points: Vec<(u32, &'p MetricPoint<'a>)>,
}

fn same_exemplar_value(a: NumberValue, b: NumberValue) -> bool {
    // Upstream `ExemplarParentIdDecoder`: same type and value (`f64 ==`).
    match (a, b) {
        (NumberValue::Int(x), NumberValue::Int(y)) => x == y,
        (NumberValue::Double(x), NumberValue::Double(y)) => x == y,
        _ => false,
    }
}

/// The exemplar and exemplar-attribute plans of one family.
fn exemplar_plans(
    family: &Family,
    points: &[(u32, &MetricPoint<'_>)],
    encoding: IdEncoding,
) -> Result<[Plan; 2], EncodeError> {
    let mut parents = Vec::new();
    let mut rows: Vec<&Exemplar> = Vec::new();
    for (dp, (_, p)) in points.iter().enumerate() {
        for e in p.exemplars() {
            parents.push(dp as u32);
            rows.push(e);
        }
    }
    let n = rows.len();
    if n > MAX_BATCH_ROWS {
        return Err(EncodeError::RelatedRowLimit(family.exemplars.ty));
    }
    let parent_ids = quasi_delta(
        &parents,
        |i| same_exemplar_value(rows[i].value, rows[i - 1].value),
        encoding,
    );
    let mut times = Vec::with_capacity(n);
    let mut ints = Vec::with_capacity(n);
    let mut doubles = Vec::with_capacity(n);
    let mut span_ids = Vec::with_capacity(n);
    let mut trace_ids = Vec::with_capacity(n);
    let mut attrs = Vec::new();
    for (i, e) in rows.iter().enumerate() {
        times.push(Some(signed(e.time_unix_ns, "exemplar.time_unix_nano")?));
        let (iv, dv) = match e.value {
            NumberValue::Int(v) => (Some(v), None),
            NumberValue::Double(v) => (None, Some(v)),
        };
        ints.push(iv);
        doubles.push(dv);
        span_ids.push(e.span_id.map(|s| be(s, 8)));
        trace_ids.push(e.trace_id.map(|t| be(t, 16)));
        push_attrs(&mut attrs, i as u32, &e.filtered_attributes);
        if attrs.len() > MAX_BATCH_ROWS {
            return Err(EncodeError::AttributeRowLimit);
        }
    }
    let exemplars = Plan {
        kind: family.exemplars,
        rows: n,
        cols: vec![
            Col::U32(delta_ids((0..n as u32).map(Some).collect(), encoding)),
            Col::U32(parent_ids.into_iter().map(Some).collect()),
            Col::Time(times),
            Col::I64(ints),
            Col::F64(doubles),
            Col::Fixed(span_ids),
            Col::Fixed(trace_ids),
        ],
        always: false,
    };
    Ok([
        exemplars,
        attrs_plan(family.exemplar_attrs, attrs, true, encoding),
    ])
}

/// Data-point attribute rows of one family (parent: the point's id).
fn point_attrs(
    family: &Family,
    points: &[(u32, &MetricPoint<'_>)],
    encoding: IdEncoding,
) -> Result<Plan, EncodeError> {
    let mut rows = Vec::new();
    for (i, (_, p)) in points.iter().enumerate() {
        push_attrs(&mut rows, i as u32, p.attributes());
        if rows.len() > MAX_BATCH_ROWS {
            return Err(EncodeError::AttributeRowLimit);
        }
    }
    Ok(attrs_plan(family.attrs, rows, true, encoding))
}

fn number_plan(
    points: &[(u32, &MetricPoint<'_>)],
    encoding: IdEncoding,
) -> Result<Plan, EncodeError> {
    let n = points.len();
    let metric_of: Vec<u32> = points.iter().map(|(m, _)| *m).collect();
    let mut starts = Vec::with_capacity(n);
    let mut times = Vec::with_capacity(n);
    let mut ints = Vec::with_capacity(n);
    let mut doubles = Vec::with_capacity(n);
    let mut flags = Vec::with_capacity(n);
    for (_, p) in points {
        let MetricPoint::Number(p) = p else {
            unreachable!("number family")
        };
        starts.push(
            p.start_unix_ns
                .map(|t| signed(t, "start_time_unix_nano"))
                .transpose()?,
        );
        times.push(Some(signed(p.time_unix_ns, "time_unix_nano")?));
        let (iv, dv) = match p.value {
            NumberValue::Int(v) => (Some(v), None),
            NumberValue::Double(v) => (None, Some(v)),
        };
        ints.push(iv);
        doubles.push(dv);
        flags.push(nonzero(p.flags));
    }
    Ok(Plan {
        kind: &NUMBER_POINTS,
        rows: n,
        cols: vec![
            Col::U32(delta_ids((0..n as u32).map(Some).collect(), encoding)),
            point_parents(&metric_of, encoding),
            Col::Time(starts),
            Col::Time(times),
            Col::I64(ints),
            Col::F64(doubles),
            Col::U32(flags),
        ],
        always: false,
    })
}

fn histogram_plan(
    points: &[(u32, &MetricPoint<'_>)],
    encoding: IdEncoding,
    always: bool,
) -> Result<Plan, EncodeError> {
    let n = points.len();
    let metric_of: Vec<u32> = points.iter().map(|(m, _)| *m).collect();
    let mut starts = Vec::with_capacity(n);
    let mut times = Vec::with_capacity(n);
    let mut counts = Vec::with_capacity(n);
    let mut sums = Vec::with_capacity(n);
    let mut bucket_counts = Vec::with_capacity(n);
    let mut bounds = Vec::with_capacity(n);
    let mut mins = Vec::with_capacity(n);
    let mut maxs = Vec::with_capacity(n);
    for (_, p) in points {
        let MetricPoint::Histogram(p) = p else {
            unreachable!("histogram family")
        };
        starts.push(Some(signed(p.start_unix_ns, "start_time_unix_nano")?));
        times.push(Some(signed(p.time_unix_ns, "time_unix_nano")?));
        counts.push(Some(p.histogram.total_count()));
        sums.push(Some(p.histogram.sum() as f64));
        mins.push(p.histogram.min().map(|v| v as f64));
        maxs.push(p.histogram.max().map(|v| v as f64));
        let (c, b) = buckets(p.histogram);
        bucket_counts.push(Some(c));
        bounds.push(Some(b));
    }
    Ok(Plan {
        kind: &HISTOGRAM_POINTS,
        rows: n,
        cols: vec![
            Col::U32(delta_ids((0..n as u32).map(Some).collect(), encoding)),
            point_parents(&metric_of, encoding),
            Col::Time(starts),
            Col::Time(times),
            Col::U64(counts),
            Col::F64(sums),
            Col::ListU64(bucket_counts),
            Col::ListF64(bounds),
            Col::U32(vec![None; n]),
            Col::F64(mins),
            Col::F64(maxs),
        ],
        always,
    })
}

fn exp_histogram_plan(
    points: &[(u32, &MetricPoint<'_>)],
    encoding: IdEncoding,
) -> Result<Plan, EncodeError> {
    let n = points.len();
    let metric_of: Vec<u32> = points.iter().map(|(m, _)| *m).collect();
    let mut starts = Vec::with_capacity(n);
    let mut times = Vec::with_capacity(n);
    let mut counts = Vec::with_capacity(n);
    let mut sums = Vec::with_capacity(n);
    let mut scales = Vec::with_capacity(n);
    let mut zero_counts = Vec::with_capacity(n);
    let mut pos_offsets = Vec::with_capacity(n);
    let mut pos_counts = Vec::with_capacity(n);
    let mut neg_offsets = Vec::with_capacity(n);
    let mut neg_counts = Vec::with_capacity(n);
    let mut flags = Vec::with_capacity(n);
    let mut mins = Vec::with_capacity(n);
    let mut maxs = Vec::with_capacity(n);
    let mut thresholds = Vec::with_capacity(n);
    for (_, p) in points {
        let MetricPoint::ExpHistogram(p) = p else {
            unreachable!("exponential histogram family")
        };
        check_exp_point(p)?;
        starts.push(
            p.start_unix_ns
                .map(|t| signed(t, "start_time_unix_nano"))
                .transpose()?,
        );
        times.push(Some(signed(p.time_unix_ns, "time_unix_nano")?));
        counts.push(Some(p.count));
        sums.push(p.sum);
        scales.push(Some(p.scale));
        zero_counts.push(Some(p.zero_count));
        pos_offsets.push(Some(p.positive.offset));
        pos_counts.push(Some(p.positive.counts.clone()));
        neg_offsets.push(Some(p.negative.offset));
        neg_counts.push(Some(p.negative.counts.clone()));
        flags.push(nonzero(p.flags));
        mins.push(p.min);
        maxs.push(p.max);
        thresholds.push((p.zero_threshold != 0.0).then_some(p.zero_threshold));
    }
    Ok(Plan {
        kind: &EXP_HISTOGRAM_POINTS,
        rows: n,
        cols: vec![
            Col::U32(delta_ids((0..n as u32).map(Some).collect(), encoding)),
            point_parents(&metric_of, encoding),
            Col::Time(starts),
            Col::Time(times),
            Col::U64(counts),
            Col::F64(sums),
            Col::I32(scales),
            Col::U64(zero_counts),
            Col::Struct(vec![Col::I32(pos_offsets), Col::ListU64(pos_counts)]),
            Col::Struct(vec![Col::I32(neg_offsets), Col::ListU64(neg_counts)]),
            Col::U32(flags),
            Col::F64(mins),
            Col::F64(maxs),
            Col::F64(thresholds),
        ],
        always: false,
    })
}

/// OTLP exponential histogram range limits Beacon enforces before export.
pub const EXP_SCALE_RANGE: std::ops::RangeInclusive<i32> = -10..=20;
/// Buckets per side of one exported exponential histogram point.
pub const MAX_EXP_BUCKETS: usize = 16_384;

fn check_exp_point(p: &ExpHistogramPoint) -> Result<(), EncodeError> {
    let bad = |what| Err(EncodeError::InvalidPoint(what));
    if !EXP_SCALE_RANGE.contains(&p.scale) {
        return bad("scale");
    }
    let mut total = p.zero_count;
    for side in [&p.positive, &p.negative] {
        if side.counts.len() > MAX_EXP_BUCKETS {
            return bad("bucket_counts");
        }
        if i32::try_from(side.counts.len())
            .ok()
            .and_then(|l| side.offset.checked_add(l))
            .is_none()
        {
            return bad("offset");
        }
        for c in &side.counts {
            total = match total.checked_add(*c) {
                Some(t) => t,
                None => return bad("count"),
            };
        }
    }
    if total != p.count {
        return bad("count");
    }
    if !(p.zero_threshold >= 0.0 && p.zero_threshold.is_finite()) {
        return bad("zero_threshold");
    }
    Ok(())
}

fn plan_metrics(
    entries: &[Entry<'_, MetricPoint<'_>>],
    encoding: IdEncoding,
) -> Result<Vec<Plan>, EncodeError> {
    // Metric rows: consecutive points of one scope with the same key.
    let mut metric_rows: Vec<usize> = Vec::new();
    let mut number = Rows::default();
    let mut histogram = Rows::default();
    let mut exp = Rows::default();
    for (i, e) in entries.iter().enumerate() {
        let same = i > 0 && {
            let p = &entries[i - 1];
            p.group == e.group && p.item.key() == e.item.key()
        };
        if !same {
            metric_rows.push(i);
        }
        let metric = metric_rows.len() as u32 - 1;
        match e.item {
            MetricPoint::Number(_) => number.points.push((metric, e.item)),
            MetricPoint::Histogram(_) => histogram.points.push((metric, e.item)),
            MetricPoint::ExpHistogram(_) => exp.points.push((metric, e.item)),
        }
    }
    let m = metric_rows.len();
    if m > MAX_BATCH_ROWS {
        return Err(EncodeError::RowLimit);
    }
    for (rows, family) in [
        (&number, &NUMBER_FAMILY),
        (&histogram, &HISTOGRAM_FAMILY),
        (&exp, &EXP_HISTOGRAM_FAMILY),
    ] {
        if rows.points.len() > MAX_BATCH_ROWS {
            return Err(EncodeError::RelatedRowLimit(family.points.ty));
        }
    }
    let metric_entries: Vec<Entry<'_, MetricPoint<'_>>> = metric_rows
        .iter()
        .map(|&i| Entry {
            group: entries[i].group,
            resource: entries[i].resource,
            scope: entries[i].scope,
            item: entries[i].item,
        })
        .collect();
    let keys: Vec<MetricKey> = metric_entries.iter().map(|e| e.item.key()).collect();
    let g = groups(&metric_entries);
    let (rs_cols, rs_attrs) = resource_scope(&g, encoding);
    let [resource, scope, schema_url] = rs_cols;
    let metrics = Plan {
        kind: &METRICS,
        rows: m,
        cols: vec![
            Col::U16(to_u16(delta_ids(
                (0..m as u32).map(Some).collect(),
                encoding,
            ))),
            resource,
            scope,
            schema_url,
            Col::U8(keys.iter().map(|k| Some(k.metric_type)).collect()),
            Col::Dict(
                "name",
                Key::U8,
                keys.iter().map(|k| Some(k.name.to_owned())).collect(),
            ),
            Col::Dict(
                "description",
                Key::U8,
                keys.iter().map(|k| opt_str(k.description)).collect(),
            ),
            Col::Dict(
                "unit",
                Key::U8,
                keys.iter()
                    .map(|k| (!k.unit.is_empty()).then(|| k.unit.to_owned()))
                    .collect(),
            ),
            Col::I32(keys.iter().map(|k| k.temporality).collect()),
            Col::Bool(keys.iter().map(|k| k.monotonic).collect()),
        ],
        always: true,
    };
    let [resource_attrs, scope_attrs] = rs_attrs;
    let mut plans = vec![metrics, resource_attrs, scope_attrs];
    // Histogram points keep their earlier behaviour: emitted (possibly
    // empty) whenever no other point kind is present.
    let histogram_always = number.points.is_empty() && exp.points.is_empty();
    for (rows, family) in [
        (&number, &NUMBER_FAMILY),
        (&histogram, &HISTOGRAM_FAMILY),
        (&exp, &EXP_HISTOGRAM_FAMILY),
    ] {
        let points = match family.points.ty {
            ArrowPayloadType::NumberDataPoints => number_plan(&rows.points, encoding)?,
            ArrowPayloadType::HistogramDataPoints => {
                histogram_plan(&rows.points, encoding, histogram_always)?
            }
            _ => exp_histogram_plan(&rows.points, encoding)?,
        };
        let attrs = point_attrs(family, &rows.points, encoding)?;
        let [exemplars, exemplar_attrs] = exemplar_plans(family, &rows.points, encoding)?;
        plans.extend([points, attrs, exemplars, exemplar_attrs]);
    }
    Ok(plans)
}

/// Producer for the `ArrowMetricsService` stream: `UNIVARIATE_METRICS` plus
/// each nonempty data-point, data-point attribute, exemplar and exemplar
/// attribute payload of the number (gauge, sum), histogram and exponential
/// histogram families, and `RESOURCE_ATTRS` and `SCOPE_ATTRS`.
pub struct MetricsEncoder {
    set: StreamSet,
}
impl MetricsEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        Self {
            set: StreamSet::new(config),
        }
    }
    pub fn reset(&mut self) {
        self.set.reset();
    }
    pub fn resets(&self) -> u64 {
        self.set.resets
    }
    pub fn encode_histograms(
        &mut self,
        points: &[HistogramPoint<'_>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        self.encode(&histogram_groups(points))
    }
    /// Histogram points only (see [`Self::encode_metrics`] for every kind).
    pub fn encode(
        &mut self,
        groups: &[ResourceGroup<HistogramPoint<'_>>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        self.encode_metrics(&to_metric_groups(groups))
    }
    /// Points of every kind, grouped by resource and scope.
    pub fn encode_metrics(
        &mut self,
        groups: &[ResourceGroup<MetricPoint<'_>>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        encode_one(&mut self.set, &flatten(groups), plan_metrics)
    }
    /// Points of every kind in one default resource and scope.
    pub fn encode_points(
        &mut self,
        points: &[MetricPoint<'_>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        self.encode_metrics(&default_group(points))
    }
    pub fn encode_histograms_split(
        &mut self,
        points: &[HistogramPoint<'_>],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        self.encode_split(&histogram_groups(points))
    }
    pub fn encode_split(
        &mut self,
        groups: &[ResourceGroup<HistogramPoint<'_>>],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        self.encode_metrics_split(&to_metric_groups(groups))
    }
    pub fn encode_metrics_split(
        &mut self,
        groups: &[ResourceGroup<MetricPoint<'_>>],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        encode_split_all(&mut self.set, &flatten(groups), &plan_metrics)
    }
}

fn histogram_groups<'a>(points: &[HistogramPoint<'a>]) -> [ResourceGroup<HistogramPoint<'a>>; 1] {
    default_group(points)
}
/// Histogram groups as metric groups.
pub fn to_metric_groups<'a>(
    groups: &[ResourceGroup<HistogramPoint<'a>>],
) -> Vec<ResourceGroup<MetricPoint<'a>>> {
    groups
        .iter()
        .map(|g| ResourceGroup {
            resource: g.resource.clone(),
            scopes: g
                .scopes
                .iter()
                .map(|s| ScopeGroup {
                    scope: s.scope.clone(),
                    items: s
                        .items
                        .iter()
                        .cloned()
                        .map(MetricPoint::Histogram)
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

// ------------------------------------------------------------------ logs

fn plan_logs(
    entries: &[Entry<'_, LogRecord>],
    encoding: IdEncoding,
) -> Result<Vec<Plan>, EncodeError> {
    let n = entries.len();
    if n > MAX_BATCH_ROWS {
        return Err(EncodeError::RowLimit);
    }
    let g = groups(entries);
    let (rs_cols, rs_attrs) = resource_scope(&g, encoding);
    let mut times = Vec::with_capacity(n);
    let mut observed = Vec::with_capacity(n);
    let mut trace_ids = Vec::with_capacity(n);
    let mut span_ids = Vec::with_capacity(n);
    let mut severities = Vec::with_capacity(n);
    let mut severity_texts = Vec::with_capacity(n);
    let mut body = Cells::with_capacity(n);
    let mut dropped = Vec::with_capacity(n);
    let mut flags = Vec::with_capacity(n);
    let mut event_names = Vec::with_capacity(n);
    let mut attrs = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let l = e.item;
        times.push(
            l.time_unix_ns
                .map(|t| signed(t, "time_unix_nano"))
                .transpose()?,
        );
        observed.push(
            l.observed_time_unix_ns
                .map(|t| signed(t, "observed_time_unix_nano"))
                .transpose()?,
        );
        trace_ids.push(l.trace_id.map(|t| be(t, 16)));
        span_ids.push(l.span_id.map(|s| be(s, 8)));
        severities.push(Some(i32::from(l.severity.number())));
        severity_texts.push(opt_str(l.severity_text));
        body.push(l.body.as_ref().map(AttrCell::of));
        dropped.push(nonzero(l.dropped_attributes_count));
        flags.push(nonzero(l.flags));
        event_names.push(opt_str(l.event_name));
        push_attrs(&mut attrs, i as u32, &l.attributes);
        if attrs.len() > MAX_BATCH_ROWS {
            return Err(EncodeError::AttributeRowLimit);
        }
    }
    let [resource, scope, schema_url] = rs_cols;
    let logs = Plan {
        kind: &LOGS,
        rows: n,
        cols: vec![
            Col::U16(to_u16(delta_ids(
                (0..n as u32).map(Some).collect(),
                encoding,
            ))),
            resource,
            scope,
            schema_url,
            Col::Time(times),
            Col::Time(observed),
            Col::Fixed(trace_ids),
            Col::Fixed(span_ids),
            Col::I32(severities),
            Col::Dict("severity_text", Key::U8, severity_texts),
            Col::Struct(body.into_cols("body.str").into()),
            Col::U32(dropped),
            Col::U32(flags),
            Col::Dict("event_name", Key::U8, event_names),
        ],
        always: true,
    };
    let [resource_attrs, scope_attrs] = rs_attrs;
    Ok(vec![
        logs,
        resource_attrs,
        scope_attrs,
        attrs_plan(&LOG_ATTRS, attrs, false, encoding),
    ])
}

/// Producer for the `ArrowLogsService` stream: `LOGS` plus `RESOURCE_ATTRS`,
/// `SCOPE_ATTRS` and `LOG_ATTRS` when nonempty.
pub struct LogsEncoder {
    set: StreamSet,
}
impl LogsEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        Self {
            set: StreamSet::new(config),
        }
    }
    pub fn reset(&mut self) {
        self.set.reset();
    }
    pub fn resets(&self) -> u64 {
        self.set.resets
    }
    pub fn encode_logs(&mut self, logs: &[LogRecord]) -> Result<BatchArrowRecords, EncodeError> {
        self.encode(&default_group(logs))
    }
    pub fn encode(
        &mut self,
        groups: &[ResourceGroup<LogRecord>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        encode_one(&mut self.set, &flatten(groups), plan_logs)
    }
    pub fn encode_logs_split(
        &mut self,
        logs: &[LogRecord],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        self.encode_split(&default_group(logs))
    }
    pub fn encode_split(
        &mut self,
        groups: &[ResourceGroup<LogRecord>],
    ) -> Result<Vec<BatchArrowRecords>, EncodeError> {
        encode_split_all(&mut self.set, &flatten(groups), &plan_logs)
    }
}
