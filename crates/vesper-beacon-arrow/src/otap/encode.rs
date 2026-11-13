//! OTAP producer: Beacon spans and histograms to `BatchArrowRecords`.
//!
//! Each payload type owns one long-lived Arrow IPC stream. The first payload
//! of a stream carries the schema message; later payloads carry only the new
//! messages (record batch, plus a dictionary batch when a dictionary grew).
//! Dictionaries are append-only per stream, so growth is sent as an IPC
//! **delta** dictionary (`isDelta = true`) holding only the new values.
//!
//! A stream is reset (new `schema_id`, schema message and full dictionaries
//! again) when a dictionary would exceed its key width or the configured
//! entry limit, on [`TraceEncoder::reset`], and after any encoding error,
//! because a failed write may leave the IPC writer and the receiver out of
//! step.

use super::attributes::{AttributeValue, SpanAttributes};
use super::proto::{ArrowPayload, ArrowPayloadType, BatchArrowRecords};
use super::schema::{self, value_type};
use arrow_array::{
    builder::{Float64Builder, ListBuilder, UInt64Builder},
    types::{UInt16Type, UInt8Type},
    ArrayRef, BooleanArray, DictionaryArray, DurationNanosecondArray, FixedSizeBinaryArray,
    Float64Array, Int32Array, Int64Array, RecordBatch, StringArray, StructArray,
    TimestampNanosecondArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use arrow_ipc::writer::{DictionaryHandling, IpcWriteOptions, StreamWriter};
use arrow_ipc::MetadataVersion;
use arrow_schema::{ArrowError, Schema, SchemaRef};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use vesper_beacon::{
    ErrorCode, FastLogHistogram, PrimaryAttribute, SpanRecord, SpanSchema, SpanStatus,
    HISTOGRAM_BINS,
};

/// Hard ceiling: span and metric ids are `u16`.
pub const MAX_BATCH_ROWS: usize = 65_536;

#[derive(Clone, Copy, Debug)]
pub struct EncoderConfig {
    /// Rows (spans or histogram points) per batch, at most [`MAX_BATCH_ROWS`].
    pub max_rows: usize,
    /// Entries per dictionary column before the stream is reset. The key
    /// width (256 for `u8` keys, 65,536 for `u16`) caps it further.
    pub max_dictionary_entries: usize,
}
impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_dictionary_entries: 4096,
        }
    }
}

#[derive(Debug)]
pub enum EncodeError {
    RowLimit,
    /// The batch's spans carry more than [`MAX_BATCH_ROWS`] attributes in
    /// total; split it into smaller batches.
    AttributeRowLimit,
    /// A batch alone needs more distinct values than one dictionary can hold.
    DictionaryOverflow {
        column: &'static str,
    },
    /// An unsigned Beacon value does not fit the signed OTLP/Arrow column.
    ValueOutOfRange {
        column: &'static str,
    },
    BatchIdExhausted,
    Arrow(ArrowError),
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

/// A span with its exported attributes. `From<SpanRecord>` carries only the
/// record's primary attribute.
#[derive(Clone, Debug, PartialEq)]
pub struct OtapSpan {
    pub record: SpanRecord,
    pub attributes: SpanAttributes,
}
impl From<SpanRecord> for OtapSpan {
    fn from(record: SpanRecord) -> Self {
        Self {
            record,
            attributes: SpanAttributes::new(),
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
    fn u8_array(&mut self, items: &[Option<&str>]) -> Result<ArrayRef, ArrowError> {
        let keys: UInt8Array = items
            .iter()
            .map(|s| s.map(|s| self.intern(s) as u8))
            .collect();
        Ok(Arc::new(DictionaryArray::<UInt8Type>::try_new(
            keys,
            self.values(),
        )?))
    }
    fn u16_array(&mut self, items: &[Option<&str>]) -> Result<ArrayRef, ArrowError> {
        let keys: UInt16Array = items
            .iter()
            .map(|s| s.map(|s| self.intern(s) as u16))
            .collect();
        Ok(Arc::new(DictionaryArray::<UInt16Type>::try_new(
            keys,
            self.values(),
        )?))
    }
}

/// One payload type's live IPC stream.
struct TxStream {
    schema_id: String,
    writer: StreamWriter<Vec<u8>>,
    dicts: Vec<Dict>,
}
impl TxStream {
    fn write(&mut self, batch: &RecordBatch) -> Result<Vec<u8>, ArrowError> {
        self.writer.write(batch)?;
        Ok(std::mem::take(self.writer.get_mut()))
    }
}

/// Static description of a payload stream.
struct Kind {
    ty: ArrowPayloadType,
    prefix: &'static str,
    schema: fn() -> Schema,
    /// Key capacity of each dictionary column, in column order.
    dict_caps: &'static [usize],
}
const SPANS: Kind = Kind {
    ty: ArrowPayloadType::Spans,
    prefix: "spans",
    schema: schema::spans_schema,
    dict_caps: &[256, 256], // name, status.status_message
};
const SPAN_ATTRS: Kind = Kind {
    ty: ArrowPayloadType::SpanAttrs,
    prefix: "span_attrs",
    schema: schema::span_attrs_schema,
    dict_caps: &[256, 65_536], // key, str
};
const METRICS: Kind = Kind {
    ty: ArrowPayloadType::UnivariateMetrics,
    prefix: "metrics",
    schema: schema::metrics_schema,
    dict_caps: &[256, 256], // name, unit
};
const HISTOGRAM_POINTS: Kind = Kind {
    ty: ArrowPayloadType::HistogramDataPoints,
    prefix: "histogram_dps",
    schema: schema::histogram_points_schema,
    dict_caps: &[],
};

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
    fn check_rows(&self, rows: usize) -> Result<(), EncodeError> {
        if rows > self.config.max_rows.min(MAX_BATCH_ROWS) {
            return Err(EncodeError::RowLimit);
        }
        Ok(())
    }
    fn open(&mut self, kind: &Kind) -> Result<(), EncodeError> {
        let schema: SchemaRef = Arc::new((kind.schema)());
        let options = IpcWriteOptions::try_new(8, false, MetadataVersion::V5)?
            .with_dictionary_handling(DictionaryHandling::Delta);
        let writer = StreamWriter::try_new_with_options(Vec::new(), &schema, options)?;
        let generation = self.next_generation;
        self.next_generation += 1;
        let cap = self.config.max_dictionary_entries;
        let stream = TxStream {
            schema_id: format!("{}:{generation}", kind.prefix),
            writer,
            dicts: kind
                .dict_caps
                .iter()
                .map(|c| Dict::new(cap.min(*c)))
                .collect(),
        };
        if self.streams.insert(kind.ty, stream).is_some() {
            self.resets += 1;
        }
        Ok(())
    }
    /// Returns the stream for `kind`, opened or reset so that every column's
    /// distinct values fit its dictionary.
    fn prepare(
        &mut self,
        kind: &Kind,
        columns: &[(&'static str, BTreeSet<&str>)],
    ) -> Result<&mut TxStream, EncodeError> {
        let cap = self.config.max_dictionary_entries;
        for ((column, distinct), key_cap) in columns.iter().zip(kind.dict_caps) {
            if distinct.len() > cap.min(*key_cap) {
                return Err(EncodeError::DictionaryOverflow { column });
            }
        }
        let fits = self.streams.get(&kind.ty).is_some_and(|s| {
            s.dicts
                .iter()
                .zip(columns)
                .all(|(dict, (_, distinct))| dict.fits(distinct))
        });
        if !fits {
            self.open(kind)?;
        }
        Ok(self.streams.get_mut(&kind.ty).expect("opened above"))
    }
    fn take_batch_id(&mut self) -> Result<i64, EncodeError> {
        let id = self.next_batch_id;
        self.next_batch_id = id.checked_add(1).ok_or(EncodeError::BatchIdExhausted)?;
        Ok(id)
    }
    fn reset(&mut self) {
        if !self.streams.is_empty() {
            self.resets += 1;
        }
        self.streams.clear();
    }
}
fn payload(stream: &TxStream, ty: ArrowPayloadType, record: Vec<u8>) -> ArrowPayload {
    ArrowPayload {
        schema_id: stream.schema_id.clone(),
        r#type: ty as i32,
        record,
    }
}
fn signed(value: u64, column: &'static str) -> Result<i64, EncodeError> {
    i64::try_from(value).map_err(|_| EncodeError::ValueOutOfRange { column })
}

/// One flattened attribute row.
struct AttrRow {
    parent: u16,
    key: &'static str,
    value: AttrCell,
}
enum AttrCell {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
}

/// Producer for the `ArrowTracesService` stream (`SPANS` + `SPAN_ATTRS`).
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
    /// The batch id the next successful `encode_spans` will use.
    pub fn next_batch_id(&self) -> i64 {
        self.set.next_batch_id
    }

    /// Encodes one batch. On any error every stream is reset, so the next
    /// batch starts from a fresh schema on both ends.
    pub fn encode_spans(&mut self, spans: &[OtapSpan]) -> Result<BatchArrowRecords, EncodeError> {
        let result = self.encode_inner(spans);
        if result.is_err() {
            self.set.reset();
        }
        result
    }

    fn encode_inner(&mut self, spans: &[OtapSpan]) -> Result<BatchArrowRecords, EncodeError> {
        self.set.check_rows(spans.len())?;
        let n = spans.len();
        let mut start = Vec::with_capacity(n);
        let mut duration = Vec::with_capacity(n);
        let mut names = Vec::with_capacity(n);
        let mut codes = Vec::with_capacity(n);
        let mut messages = Vec::with_capacity(n);
        let mut attrs = Vec::new();
        for (i, span) in spans.iter().enumerate() {
            let r = &span.record;
            let id = i as u16; // check_rows bounds n by 65,536.
            start.push(signed(r.start_unix_ns, "start_time_unix_nano")?);
            duration.push(signed(r.duration_ns, "duration_time_unix_nano")?);
            names.push(Some(span_name(r.schema)));
            match r.status {
                SpanStatus::Ok => {
                    codes.push(schema::STATUS_CODE_OK);
                    messages.push(None);
                }
                SpanStatus::Error(code) => {
                    codes.push(schema::STATUS_CODE_ERROR);
                    messages.push(Some(error_message(code)));
                }
            }
            let mut all = span.attributes.clone();
            match r.attribute {
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
            for (key, value) in all.iter() {
                let value = match value {
                    AttributeValue::Bool(b) => AttrCell::Bool(b),
                    AttributeValue::Int(v) => AttrCell::Int(v),
                    AttributeValue::Double(v) => AttrCell::Double(v),
                    other => AttrCell::Str(other.export_str().expect("string forms")),
                };
                attrs.push(AttrRow {
                    parent: id,
                    key,
                    value,
                });
            }
        }

        // Attribute rows are bounded like span rows, so every batch this
        // encoder produces fits a default decoder's row limit.
        if attrs.len() > MAX_BATCH_ROWS {
            return Err(EncodeError::AttributeRowLimit);
        }
        let mut payloads = Vec::with_capacity(2);
        let spans_columns = [
            ("name", names.iter().flatten().copied().collect()),
            (
                "status_message",
                messages.iter().flatten().copied().collect(),
            ),
        ];
        let stream = self.set.prepare(&SPANS, &spans_columns)?;
        let ids: UInt16Array = (0..n).map(|i| Some(i as u16)).collect();
        let name_array = stream.dicts[0].u8_array(&names)?;
        let status = StructArray::try_new(
            schema::status_fields(),
            vec![
                Arc::new(Int32Array::from(codes)),
                stream.dicts[1].u8_array(&messages)?,
            ],
            None,
        )?;
        let columns: Vec<ArrayRef> = vec![
            Arc::new(ids),
            Arc::new(TimestampNanosecondArray::from(start)),
            Arc::new(DurationNanosecondArray::from(duration)),
            Arc::new(fixed(
                spans
                    .iter()
                    .map(|s| Some(s.record.context.trace_id().to_be_bytes().to_vec())),
                16,
                n,
            )?),
            Arc::new(fixed(
                spans
                    .iter()
                    .map(|s| Some(s.record.context.span_id().to_be_bytes().to_vec())),
                8,
                n,
            )?),
            Arc::new(fixed(
                spans.iter().map(|s| {
                    (s.record.parent_span_id != 0)
                        .then(|| s.record.parent_span_id.to_be_bytes().to_vec())
                }),
                8,
                n,
            )?),
            Arc::new(UInt32Array::from_iter_values(
                spans
                    .iter()
                    .map(|s| u32::from(s.record.context.trace_flags())),
            )),
            name_array,
            Arc::new(status),
        ];
        let batch = RecordBatch::try_new(Arc::new(schema::spans_schema()), columns)?;
        let bytes = stream.write(&batch)?;
        payloads.push(payload(stream, ArrowPayloadType::Spans, bytes));

        if !attrs.is_empty() {
            let keys: Vec<Option<&str>> = attrs.iter().map(|a| Some(a.key)).collect();
            let strs: Vec<Option<&str>> = attrs
                .iter()
                .map(|a| match &a.value {
                    AttrCell::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect();
            let attr_columns = [
                ("key", keys.iter().flatten().copied().collect()),
                ("str", strs.iter().flatten().copied().collect()),
            ];
            let stream = self.set.prepare(&SPAN_ATTRS, &attr_columns)?;
            let key_array = stream.dicts[0].u8_array(&keys)?;
            let str_array = stream.dicts[1].u16_array(&strs)?;
            let columns: Vec<ArrayRef> = vec![
                Arc::new(UInt16Array::from_iter_values(
                    attrs.iter().map(|a| a.parent),
                )),
                key_array,
                Arc::new(UInt8Array::from_iter_values(attrs.iter().map(
                    |a| match a.value {
                        AttrCell::Str(_) => value_type::STR,
                        AttrCell::Int(_) => value_type::INT,
                        AttrCell::Double(_) => value_type::DOUBLE,
                        AttrCell::Bool(_) => value_type::BOOL,
                    },
                ))),
                str_array,
                Arc::new(Int64Array::from_iter(attrs.iter().map(|a| match a.value {
                    AttrCell::Int(v) => Some(v),
                    _ => None,
                }))),
                Arc::new(Float64Array::from_iter(attrs.iter().map(
                    |a| match a.value {
                        AttrCell::Double(v) => Some(v),
                        _ => None,
                    },
                ))),
                Arc::new(BooleanArray::from_iter(attrs.iter().map(
                    |a| match a.value {
                        AttrCell::Bool(v) => Some(v),
                        _ => None,
                    },
                ))),
            ];
            let batch = RecordBatch::try_new(Arc::new(schema::span_attrs_schema()), columns)?;
            let bytes = stream.write(&batch)?;
            payloads.push(payload(stream, ArrowPayloadType::SpanAttrs, bytes));
        }
        Ok(BatchArrowRecords {
            batch_id: self.set.take_batch_id()?,
            arrow_payloads: payloads,
            headers: Vec::new(),
        })
    }
}

fn fixed(
    values: impl Iterator<Item = Option<Vec<u8>>>,
    width: i32,
    rows: usize,
) -> Result<FixedSizeBinaryArray, ArrowError> {
    if rows == 0 {
        return Ok(FixedSizeBinaryArray::new_null(width, 0));
    }
    let values: Vec<Option<Vec<u8>>> = values.collect();
    if values.iter().all(Option::is_some) {
        FixedSizeBinaryArray::try_from_iter(values.into_iter().flatten())
    } else {
        FixedSizeBinaryArray::try_from_sparse_iter_with_size(values.into_iter(), width)
    }
}

/// One cumulative explicit-bucket histogram data point.
///
/// Bucket `i` is Beacon bin `i` for every bin up to the last nonzero one;
/// `explicit_bounds[i]` is bin `i`'s inclusive upper end as `f64`, which is
/// exact below 2^53 and rounded above it. Exemplars are not exported.
pub struct HistogramPoint<'a> {
    pub name: &'static str,
    pub unit: &'static str,
    pub start_unix_ns: u64,
    pub time_unix_ns: u64,
    pub histogram: &'a FastLogHistogram,
}

/// Producer for the `ArrowMetricsService` stream
/// (`UNIVARIATE_METRICS` + `HISTOGRAM_DATA_POINTS`).
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
        let result = self.encode_inner(points);
        if result.is_err() {
            self.set.reset();
        }
        result
    }
    fn encode_inner(
        &mut self,
        points: &[HistogramPoint<'_>],
    ) -> Result<BatchArrowRecords, EncodeError> {
        self.set.check_rows(points.len())?;
        let n = points.len();
        let names: Vec<Option<&str>> = points.iter().map(|p| Some(p.name)).collect();
        let units: Vec<Option<&str>> = points
            .iter()
            .map(|p| (!p.unit.is_empty()).then_some(p.unit))
            .collect();
        let columns = [
            ("name", names.iter().flatten().copied().collect()),
            ("unit", units.iter().flatten().copied().collect()),
        ];
        let mut payloads = Vec::with_capacity(2);
        let stream = self.set.prepare(&METRICS, &columns)?;
        let name_array = stream.dicts[0].u8_array(&names)?;
        let unit_array = stream.dicts[1].u8_array(&units)?;
        let metric_columns: Vec<ArrayRef> = vec![
            Arc::new(UInt16Array::from_iter_values((0..n).map(|i| i as u16))),
            Arc::new(UInt8Array::from_iter_values(
                (0..n).map(|_| schema::METRIC_TYPE_HISTOGRAM),
            )),
            name_array,
            unit_array,
            Arc::new(Int32Array::from_iter_values(
                (0..n).map(|_| schema::TEMPORALITY_CUMULATIVE),
            )),
        ];
        let batch = RecordBatch::try_new(Arc::new(schema::metrics_schema()), metric_columns)?;
        let bytes = stream.write(&batch)?;
        payloads.push(payload(stream, ArrowPayloadType::UnivariateMetrics, bytes));

        let mut starts = Vec::with_capacity(n);
        let mut times = Vec::with_capacity(n);
        let mut counts = ListBuilder::new(UInt64Builder::new());
        let mut bounds = ListBuilder::new(Float64Builder::new());
        for p in points {
            starts.push(signed(p.start_unix_ns, "start_time_unix_nano")?);
            times.push(signed(p.time_unix_ns, "time_unix_nano")?);
            let used = (0..HISTOGRAM_BINS)
                .rev()
                .find(|i| p.histogram.count(*i).unwrap_or(0) != 0)
                .map_or(1, |last| last + 1);
            for i in 0..used {
                counts
                    .values()
                    .append_value(p.histogram.count(i).unwrap_or(0));
            }
            counts.append(true);
            for i in 0..used - 1 {
                let upper = FastLogHistogram::bin_range(i).expect("bin index").upper;
                bounds.values().append_value(upper as f64);
            }
            bounds.append(true);
        }
        let stream = self.set.prepare(&HISTOGRAM_POINTS, &[])?;
        let point_columns: Vec<ArrayRef> = vec![
            Arc::new(UInt32Array::from_iter_values((0..n).map(|i| i as u32))),
            Arc::new(UInt16Array::from_iter_values((0..n).map(|i| i as u16))),
            Arc::new(TimestampNanosecondArray::from(starts)),
            Arc::new(TimestampNanosecondArray::from(times)),
            Arc::new(UInt64Array::from_iter_values(
                points.iter().map(|p| p.histogram.total_count()),
            )),
            Arc::new(counts.finish()),
            Arc::new(bounds.finish()),
        ];
        let batch =
            RecordBatch::try_new(Arc::new(schema::histogram_points_schema()), point_columns)?;
        let bytes = stream.write(&batch)?;
        payloads.push(payload(
            stream,
            ArrowPayloadType::HistogramDataPoints,
            bytes,
        ));
        Ok(BatchArrowRecords {
            batch_id: self.set.take_batch_id()?,
            arrow_payloads: payloads,
            headers: Vec::new(),
        })
    }
}
