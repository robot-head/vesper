//! OTAP consumer: `BatchArrowRecords` back to spans, histograms and logs
//! with their resources, scopes, attributes, events and links.
//!
//! Streams are keyed by `schema_id`. A payload with an unknown id starts a
//! new IPC stream; if another stream of the same payload type exists it is
//! dropped (the producer reset it). Each payload must hold exactly one record
//! batch and end on an IPC message boundary.
//!
//! Id columns are decoded as upstream's Go decoder does (see
//! [`schema`](super::schema)): delta and quasi-delta by default, absolute
//! when the field carries `encoding=plain`. Resource and scope boundaries
//! follow the Go decoder too: a new resource (scope) starts whenever the
//! accumulated `resource.id` (`scope.id`) changes. Dictionary-encoded
//! columns of any leaf type are accepted, since upstream producers
//! dictionary-encode adaptively.
//!
//! All input is untrusted. Every limit violation, malformed protobuf or IPC
//! message, schema mismatch, id overflow and dangling or unused reference is a
//! typed error, and any error drops all stream state, mirroring the
//! producer's reset-on-error.

use super::ipc::IpcReader;
use super::model::{ExpBuckets, NumberValue};
use super::proto::{ArrowPayloadType, BatchArrowRecords};
use super::schema::{self, value_type};
use arrow_array::{
    cast::AsArray,
    types::{
        DurationNanosecondType, Float64Type, Int32Type, Int64Type, TimestampNanosecondType,
        UInt16Type, UInt32Type, UInt64Type, UInt8Type,
    },
    Array, ArrayRef, ArrowPrimitiveType, RecordBatch, StructArray,
};
use arrow_schema::{ArrowError, DataType, Field, TimeUnit};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    pub max_payloads: usize,
    pub max_streams: usize,
    pub max_schema_id_bytes: usize,
    /// IPC bytes of one payload.
    pub max_payload_bytes: usize,
    /// IPC bytes accepted over a stream's lifetime (bounds dictionary growth).
    pub max_stream_bytes: usize,
    /// Rows of any one payload.
    pub max_rows: usize,
    /// Bytes one payload's Arrow IPC buffers may decompress to (payloads
    /// with zstd `BodyCompression`).
    pub max_decompressed_bytes: usize,
    /// Bytes of owned strings one batch may decode into. Dictionary columns
    /// expand each referenced value per row, so this bounds the expansion
    /// independently of the encoded size.
    pub max_decoded_string_bytes: usize,
    /// What to do with a batch's `headers` (HPACK-encoded gRPC metadata).
    pub headers: HeaderPolicy,
}
/// Handling of `BatchArrowRecords::headers`.
///
/// Upstream's exporter fills `headers` with its outgoing gRPC metadata
/// (for example W3C trace context), HPACK-encoded with a dynamic table that
/// persists across one stream's batches (see [`HeaderDecoder`]).
///
/// [`HeaderDecoder`]: super::HeaderDecoder
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderPolicy {
    /// Any non-empty `headers` is [`DecodeError::HeadersUnsupported`].
    Reject,
    /// Keep the raw bytes, undecoded, in [`DecodedBatch::headers`], up to
    /// `max_bytes` per batch ([`DecodeError::HeadersTooLarge`] beyond).
    KeepRaw { max_bytes: usize },
    /// HPACK-decode each batch's block, in stream order, into
    /// [`DecodedBatch::header_fields`] (the raw bytes are kept too). At most
    /// `max_bytes` of encoded block and of decoded fields per batch, and a
    /// dynamic table of at most `max_table_size` bytes.
    Decode {
        max_bytes: usize,
        max_table_size: usize,
    },
}
impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_payloads: 16,
            max_streams: 16,
            max_schema_id_bytes: 128,
            max_payload_bytes: 16 << 20,
            max_stream_bytes: 256 << 20,
            max_rows: super::encode::MAX_BATCH_ROWS,
            max_decompressed_bytes: 64 << 20,
            max_decoded_string_bytes: 64 << 20,
            headers: HeaderPolicy::Reject,
        }
    }
}

#[derive(Debug)]
pub enum DecodeError {
    HeadersUnsupported,
    /// Raw or decoded `headers` larger than the [`HeaderPolicy`] allows.
    HeadersTooLarge,
    /// A malformed HPACK block (or one that needs a larger dynamic table).
    BadHeaders(&'static str),
    TooManyPayloads,
    UnknownPayloadType(i32),
    UnsupportedPayloadType(ArrowPayloadType),
    DuplicatePayloadType(ArrowPayloadType),
    /// Main records of more than one signal (spans, metrics, logs) in one
    /// batch.
    MixedSignals,
    BadSchemaId,
    /// A known `schema_id` arrived with a different payload type.
    PayloadTypeChanged,
    TooManyStreams,
    PayloadTooLarge,
    StreamTooLarge,
    /// Compressed Arrow IPC buffers would exceed `max_decompressed_bytes`.
    DecompressedTooLarge,
    /// The payload did not contain exactly one complete record batch.
    RecordBatchCount(usize),
    TruncatedMessage,
    /// An IPC message failed structural validation before Arrow decoding.
    Ipc(&'static str),
    /// Arrow panicked on input that passed validation; the stream is dropped.
    /// Defense in depth: no input known to the tests reaches it.
    DecoderPanic,
    MissingColumn(&'static str),
    ColumnType(&'static str),
    /// An `encoding` field metadata value Beacon does not know for this
    /// column.
    BadIdEncoding(&'static str),
    NullValue(&'static str),
    RowLimit,
    NegativeTime(&'static str),
    DuplicateId(&'static str),
    /// A decoded id does not fit its column's width.
    IdOverflow(&'static str),
    /// An attribute, event, link or data point refers to an id that no row
    /// carries.
    DanglingParent(&'static str),
    /// Related payloads without their main record in the same batch.
    MissingMainRecord,
    BadAttribute(u8),
    /// A `ser` cell that is not one well-formed CBOR value of the supported
    /// kinds (see the `cbor` module), or nests too deep.
    BadSerializedValue,
    /// A metric type Beacon does not decode (summary, empty or unknown).
    UnsupportedMetricType(u8),
    /// A data point whose metric row has another type (the row's type).
    PointKindMismatch(u8),
    /// Histogram or exponential histogram buckets, bounds, counts or scale
    /// that OTLP does not allow.
    HistogramShape,
    /// Decoded strings would exceed `max_decoded_string_bytes`.
    DecodedTooLarge,
    Arrow(ArrowError),
}
impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DecodeError {}

/// A decoded attribute or body value. `PartialEq` is structural (`f64 ==`
/// on doubles); the quasi-delta comparison follows upstream instead (slices
/// and maps never repeat).
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    Slice(Vec<DecodedValue>),
    Map(Vec<(String, DecodedValue)>),
    /// An empty value inside a slice or map (CBOR null).
    Empty,
}
impl DecodedValue {
    /// Upstream `carrow.Equal`.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Double(a), Self::Double(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            _ => false,
        }
    }
}
pub type DecodedAttributes = Vec<(String, DecodedValue)>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedResource {
    pub attributes: DecodedAttributes,
    pub schema_url: Option<String>,
    pub dropped_attributes_count: u32,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedScope {
    pub name: Option<String>,
    pub version: Option<String>,
    pub attributes: DecodedAttributes,
    pub dropped_attributes_count: u32,
    /// The enclosing `Scope*` schema URL (the record's `schema_url` column).
    pub schema_url: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedEvent {
    pub name: String,
    pub time_unix_ns: Option<u64>,
    pub attributes: DecodedAttributes,
    pub dropped_attributes_count: u32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedLink {
    pub trace_id: u128,
    pub span_id: u64,
    pub trace_state: Option<String>,
    pub flags: Option<u32>,
    pub attributes: DecodedAttributes,
    pub dropped_attributes_count: u32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedSpan {
    pub trace_id: u128,
    pub span_id: u64,
    pub parent_span_id: Option<u64>,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
    pub flags: Option<u32>,
    pub name: String,
    pub status_code: Option<i32>,
    pub status_message: Option<String>,
    pub attributes: DecodedAttributes,
    /// Index into [`DecodedBatch::resources`].
    pub resource: usize,
    /// Index into [`DecodedBatch::scopes`].
    pub scope: usize,
    pub kind: Option<i32>,
    pub trace_state: Option<String>,
    pub dropped_attributes_count: u32,
    pub dropped_events_count: u32,
    pub dropped_links_count: u32,
    pub events: Vec<DecodedEvent>,
    pub links: Vec<DecodedLink>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedHistogram {
    pub name: String,
    pub unit: Option<String>,
    pub start_unix_ns: Option<u64>,
    pub time_unix_ns: u64,
    pub count: Option<u64>,
    pub bucket_counts: Vec<u64>,
    pub explicit_bounds: Vec<f64>,
    pub resource: usize,
    pub scope: usize,
    pub description: Option<String>,
    pub aggregation_temporality: Option<i32>,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub flags: Option<u32>,
    pub attributes: DecodedAttributes,
    pub exemplars: Vec<DecodedExemplar>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedExemplar {
    pub time_unix_ns: u64,
    pub value: NumberValue,
    pub span_id: Option<u64>,
    pub trace_id: Option<u128>,
    pub filtered_attributes: DecodedAttributes,
}
/// A gauge (`metric_type` 1) or sum (2) data point.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedNumber {
    pub name: String,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub metric_type: u8,
    pub aggregation_temporality: Option<i32>,
    pub is_monotonic: Option<bool>,
    pub resource: usize,
    pub scope: usize,
    pub start_unix_ns: Option<u64>,
    pub time_unix_ns: u64,
    /// `None` when neither `int_value` nor `double_value` is set.
    pub value: Option<NumberValue>,
    pub flags: Option<u32>,
    pub attributes: DecodedAttributes,
    pub exemplars: Vec<DecodedExemplar>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedExpHistogram {
    pub name: String,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub aggregation_temporality: Option<i32>,
    pub resource: usize,
    pub scope: usize,
    pub start_unix_ns: Option<u64>,
    pub time_unix_ns: u64,
    pub count: u64,
    pub sum: Option<f64>,
    pub scale: i32,
    pub zero_count: u64,
    pub zero_threshold: f64,
    pub positive: ExpBuckets,
    pub negative: ExpBuckets,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub flags: Option<u32>,
    pub attributes: DecodedAttributes,
    pub exemplars: Vec<DecodedExemplar>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedLog {
    pub resource: usize,
    pub scope: usize,
    pub time_unix_ns: Option<u64>,
    pub observed_time_unix_ns: Option<u64>,
    pub trace_id: Option<u128>,
    pub span_id: Option<u64>,
    pub severity_number: Option<i32>,
    pub severity_text: Option<String>,
    pub body: Option<DecodedValue>,
    pub attributes: DecodedAttributes,
    pub dropped_attributes_count: u32,
    pub flags: Option<u32>,
    pub event_name: Option<String>,
}
/// One decoded batch. Resources and scopes are listed in the order they
/// start (as the Go decoder appends `Resource*`/`Scope*` entries); records
/// refer to them by index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedBatch {
    pub batch_id: i64,
    pub resources: Vec<DecodedResource>,
    pub scopes: Vec<DecodedScope>,
    pub spans: Vec<DecodedSpan>,
    pub histograms: Vec<DecodedHistogram>,
    pub numbers: Vec<DecodedNumber>,
    pub exp_histograms: Vec<DecodedExpHistogram>,
    pub logs: Vec<DecodedLog>,
    /// The batch's raw HPACK `headers` under [`HeaderPolicy::KeepRaw`] and
    /// [`HeaderPolicy::Decode`] (empty otherwise).
    pub headers: Vec<u8>,
    /// The decoded header fields under [`HeaderPolicy::Decode`].
    pub header_fields: Vec<super::HeaderField>,
}

struct RxStream {
    ty: ArrowPayloadType,
    reader: IpcReader,
    bytes: usize,
}

pub struct OtapDecoder {
    limits: DecodeLimits,
    streams: HashMap<String, RxStream>,
    string_budget: Cell<usize>,
    hpack: Option<super::HeaderDecoder>,
}
impl Default for OtapDecoder {
    fn default() -> Self {
        Self::new(DecodeLimits::default())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signal {
    Traces,
    Metrics,
    Logs,
}
fn signal_of(ty: ArrowPayloadType) -> Option<(Signal, bool)> {
    use ArrowPayloadType as T;
    Some(match ty {
        T::Spans => (Signal::Traces, true),
        T::SpanAttrs | T::SpanEvents | T::SpanLinks | T::SpanEventAttrs | T::SpanLinkAttrs => {
            (Signal::Traces, false)
        }
        T::UnivariateMetrics => (Signal::Metrics, true),
        T::NumberDataPoints
        | T::NumberDpAttrs
        | T::NumberDpExemplars
        | T::NumberDpExemplarAttrs
        | T::HistogramDataPoints
        | T::HistogramDpAttrs
        | T::HistogramDpExemplars
        | T::HistogramDpExemplarAttrs
        | T::ExpHistogramDataPoints
        | T::ExpHistogramDpAttrs
        | T::ExpHistogramDpExemplars
        | T::ExpHistogramDpExemplarAttrs => (Signal::Metrics, false),
        T::Logs => (Signal::Logs, true),
        T::LogAttrs => (Signal::Logs, false),
        _ => return None,
    })
}

impl OtapDecoder {
    pub fn new(limits: DecodeLimits) -> Self {
        Self {
            limits,
            streams: HashMap::new(),
            string_budget: Cell::new(limits.max_decoded_string_bytes),
            hpack: None,
        }
    }
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }
    pub fn reset(&mut self) {
        self.streams.clear();
        self.hpack = None;
    }

    pub fn decode(&mut self, batch: &BatchArrowRecords) -> Result<DecodedBatch, DecodeError> {
        let result = self.decode_inner(batch);
        if result.is_err() {
            self.reset();
        }
        result
    }

    fn decode_inner(&mut self, batch: &BatchArrowRecords) -> Result<DecodedBatch, DecodeError> {
        self.string_budget.set(self.limits.max_decoded_string_bytes);
        match self.limits.headers {
            HeaderPolicy::Reject if !batch.headers.is_empty() => {
                return Err(DecodeError::HeadersUnsupported)
            }
            HeaderPolicy::KeepRaw { max_bytes } | HeaderPolicy::Decode { max_bytes, .. }
                if batch.headers.len() > max_bytes =>
            {
                return Err(DecodeError::HeadersTooLarge)
            }
            _ => {}
        }
        let header_fields = match self.limits.headers {
            HeaderPolicy::Decode {
                max_bytes,
                max_table_size,
            } if !batch.headers.is_empty() => self
                .hpack
                .get_or_insert_with(|| super::HeaderDecoder::new(max_table_size, max_bytes))
                .decode(&batch.headers)?,
            _ => Vec::new(),
        };
        if batch.arrow_payloads.len() > self.limits.max_payloads {
            return Err(DecodeError::TooManyPayloads);
        }
        let mut records = BTreeMap::new();
        for p in &batch.arrow_payloads {
            let ty = ArrowPayloadType::try_from(p.r#type)
                .map_err(|_| DecodeError::UnknownPayloadType(p.r#type))?;
            if signal_of(ty).is_none()
                && !matches!(
                    ty,
                    ArrowPayloadType::ResourceAttrs | ArrowPayloadType::ScopeAttrs
                )
            {
                return Err(DecodeError::UnsupportedPayloadType(ty));
            }
            if records.contains_key(&ty) {
                return Err(DecodeError::DuplicatePayloadType(ty));
            }
            if p.schema_id.is_empty() || p.schema_id.len() > self.limits.max_schema_id_bytes {
                return Err(DecodeError::BadSchemaId);
            }
            if p.record.len() > self.limits.max_payload_bytes {
                return Err(DecodeError::PayloadTooLarge);
            }
            let record = self.feed(&p.schema_id, ty, &p.record)?;
            records.insert(ty, record);
        }
        // Exactly one main record decides the signal; every related payload
        // must belong to it.
        let mains: Vec<Signal> = records
            .keys()
            .filter_map(|t| signal_of(*t).filter(|(_, main)| *main).map(|(s, _)| s))
            .collect();
        if mains.len() > 1 {
            return Err(DecodeError::MixedSignals);
        }
        let main = mains.first().copied();
        for ty in records.keys() {
            let fits = match signal_of(*ty) {
                Some((s, _)) => Some(s) == main,
                None => main.is_some(), // resource and scope attributes
            };
            if !fits {
                return Err(DecodeError::MissingMainRecord);
            }
        }
        let mut cx = Cx {
            budget: &self.string_budget,
            records: &mut records,
            out: DecodedBatch {
                batch_id: batch.batch_id,
                headers: batch.headers.clone(),
                header_fields,
                ..DecodedBatch::default()
            },
        };
        match main {
            Some(Signal::Traces) => cx.traces()?,
            Some(Signal::Metrics) => cx.metrics()?,
            Some(Signal::Logs) => cx.logs()?,
            None => {}
        }
        Ok(cx.out)
    }

    fn feed(
        &mut self,
        schema_id: &str,
        ty: ArrowPayloadType,
        bytes: &[u8],
    ) -> Result<RecordBatch, DecodeError> {
        if let Some(stream) = self.streams.get(schema_id) {
            if stream.ty != ty {
                return Err(DecodeError::PayloadTypeChanged);
            }
        } else {
            // A new stream of a known type replaces the old one (schema reset).
            self.streams.retain(|_, s| s.ty != ty);
            if self.streams.len() >= self.limits.max_streams {
                return Err(DecodeError::TooManyStreams);
            }
            self.streams.insert(
                schema_id.to_owned(),
                RxStream {
                    ty,
                    reader: IpcReader::default(),
                    bytes: 0,
                },
            );
        }
        let stream = self.streams.get_mut(schema_id).expect("present");
        stream.bytes = stream
            .bytes
            .checked_add(bytes.len())
            .filter(|b| *b <= self.limits.max_stream_bytes)
            .ok_or(DecodeError::StreamTooLarge)?;
        let max_rows = self.limits.max_rows;
        let max_decompressed = self.limits.max_decompressed_bytes;
        let reader = &mut stream.reader;
        let mut batches = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.feed(bytes, max_rows, max_decompressed)
        }))
        .map_err(|_| DecodeError::DecoderPanic)??;
        if batches.len() > 1 {
            return Err(DecodeError::RecordBatchCount(batches.len()));
        }
        match batches.pop() {
            Some(batch) if batch.num_rows() <= self.limits.max_rows => Ok(batch),
            Some(_) => Err(DecodeError::RowLimit),
            None => Err(DecodeError::RecordBatchCount(0)),
        }
    }
}

// ------------------------------------------------------------------ columns

/// A column of a record batch or struct, with its field (for metadata).
struct C<'a> {
    array: ArrayRef,
    field: &'a Field,
    name: &'static str,
}

fn find<'a>(batch: &'a RecordBatch, name: &'static str) -> Option<C<'a>> {
    let (i, field) = batch.schema_ref().column_with_name(name)?;
    Some(C {
        array: batch.column(i).clone(),
        field,
        name,
    })
}
fn need<'a>(batch: &'a RecordBatch, name: &'static str) -> Result<C<'a>, DecodeError> {
    find(batch, name).ok_or(DecodeError::MissingColumn(name))
}
/// A struct child; rows where the struct itself is null read as null.
fn child<'a>(
    s: &'a StructArray,
    fields: &'a arrow_schema::Fields,
    name: &'static str,
) -> Option<C<'a>> {
    let (i, field) = fields.find(name)?;
    let column = s.column(i).clone();
    let array = match s.nulls() {
        None => column,
        Some(parent) => {
            // Combine the struct's validity into the child.
            let data = column.to_data();
            let merged = arrow_buffer::NullBuffer::union(Some(parent), data.nulls());
            let data = data.into_builder().nulls(merged).build().ok()?;
            arrow_array::make_array(data)
        }
    };
    Some(C { array, field, name })
}
fn as_struct<'a>(c: &'a C<'_>) -> Result<(&'a StructArray, &'a arrow_schema::Fields), DecodeError> {
    match c.array.data_type() {
        DataType::Struct(fields) => Ok((c.array.as_struct(), fields)),
        _ => Err(DecodeError::ColumnType(c.name)),
    }
}

/// Materializes dictionary encoding for non-string value types.
fn plain(c: &C<'_>) -> Result<ArrayRef, DecodeError> {
    match c.array.data_type() {
        DataType::Dictionary(_, v) if **v != DataType::Utf8 => {
            let any = c
                .array
                .as_any_dictionary_opt()
                .ok_or(DecodeError::ColumnType(c.name))?;
            let keys = arrow_cast_keys(any.keys(), c.name)?;
            arrow_select::take::take(any.values().as_ref(), &keys, None).map_err(DecodeError::Arrow)
        }
        _ => Ok(c.array.clone()),
    }
}
fn arrow_cast_keys(
    keys: &dyn Array,
    name: &'static str,
) -> Result<arrow_array::UInt32Array, DecodeError> {
    let out: arrow_array::UInt32Array = match keys.data_type() {
        DataType::UInt8 => keys
            .as_primitive::<UInt8Type>()
            .iter()
            .map(|k| k.map(u32::from))
            .collect(),
        DataType::UInt16 => keys
            .as_primitive::<UInt16Type>()
            .iter()
            .map(|k| k.map(u32::from))
            .collect(),
        DataType::UInt32 => keys.as_primitive::<UInt32Type>().clone(),
        _ => return Err(DecodeError::ColumnType(name)),
    };
    Ok(out)
}

fn prim<T: ArrowPrimitiveType>(
    c: &C<'_>,
    ok: impl Fn(&DataType) -> bool,
) -> Result<Vec<Option<T::Native>>, DecodeError> {
    let a = plain(c)?;
    if !ok(a.data_type()) {
        return Err(DecodeError::ColumnType(c.name));
    }
    Ok(a.as_primitive::<T>().iter().collect())
}
fn opt_prim<T: ArrowPrimitiveType>(
    c: Option<C<'_>>,
    n: usize,
    ok: impl Fn(&DataType) -> bool,
) -> Result<Vec<Option<T::Native>>, DecodeError> {
    match c {
        Some(c) => prim::<T>(&c, ok),
        None => Ok(vec![None; n]),
    }
}
fn is(t: DataType) -> impl Fn(&DataType) -> bool {
    move |d| *d == t
}
fn is_time(d: &DataType) -> bool {
    matches!(d, DataType::Timestamp(TimeUnit::Nanosecond, _))
}
fn is_duration(d: &DataType) -> bool {
    matches!(d, DataType::Duration(TimeUnit::Nanosecond))
}
fn bools(c: Option<C<'_>>, n: usize) -> Result<Vec<Option<bool>>, DecodeError> {
    let Some(c) = c else {
        return Ok(vec![None; n]);
    };
    let a = plain(&c)?;
    match a.data_type() {
        DataType::Boolean => Ok(a.as_boolean().iter().collect()),
        _ => Err(DecodeError::ColumnType(c.name)),
    }
}
fn fixed(c: Option<C<'_>>, n: usize, width: i32) -> Result<Vec<Option<Vec<u8>>>, DecodeError> {
    let Some(c) = c else {
        return Ok(vec![None; n]);
    };
    let a = plain(&c)?;
    match a.data_type() {
        DataType::FixedSizeBinary(w) if *w == width => Ok(a
            .as_fixed_size_binary()
            .iter()
            .map(|v| v.map(<[u8]>::to_vec))
            .collect()),
        _ => Err(DecodeError::ColumnType(c.name)),
    }
}
fn take_str(budget: &Cell<usize>, s: &str) -> Result<String, DecodeError> {
    let left = budget
        .get()
        .checked_sub(s.len())
        .ok_or(DecodeError::DecodedTooLarge)?;
    budget.set(left);
    Ok(s.to_owned())
}
/// Strings from plain `Utf8` or `Dictionary<u8|u16|u32, Utf8>` columns,
/// charged against `budget` before each copy.
fn strings(
    c: Option<C<'_>>,
    n: usize,
    budget: &Cell<usize>,
) -> Result<Vec<Option<String>>, DecodeError> {
    let Some(c) = c else {
        return Ok(vec![None; n]);
    };
    let name = c.name;
    match c.array.data_type() {
        DataType::Utf8 => c
            .array
            .as_string::<i32>()
            .iter()
            .map(|s| s.map(|s| take_str(budget, s)).transpose())
            .collect(),
        DataType::Dictionary(_, v) if **v == DataType::Utf8 => {
            let any = c
                .array
                .as_any_dictionary_opt()
                .ok_or(DecodeError::ColumnType(name))?;
            let keys = arrow_cast_keys(any.keys(), name)?;
            let values = any.values().as_string::<i32>();
            keys.iter()
                .map(|k| {
                    k.map(|k| {
                        let k = k as usize;
                        if k >= values.len() || values.is_null(k) {
                            return Err(DecodeError::NullValue(name));
                        }
                        take_str(budget, values.value(k))
                    })
                    .transpose()
                })
                .collect()
        }
        _ => Err(DecodeError::ColumnType(name)),
    }
}
/// Byte strings from plain `Binary` or `Dictionary<_, Binary>` columns,
/// charged against `budget` before each copy.
fn binaries(
    c: Option<C<'_>>,
    n: usize,
    budget: &Cell<usize>,
) -> Result<Vec<Option<Vec<u8>>>, DecodeError> {
    let Some(c) = c else {
        return Ok(vec![None; n]);
    };
    let a = plain(&c)?;
    if a.data_type() != &DataType::Binary {
        return Err(DecodeError::ColumnType(c.name));
    }
    a.as_binary::<i32>()
        .iter()
        .map(|v| {
            v.map(|v| {
                let left = budget
                    .get()
                    .checked_sub(v.len())
                    .ok_or(DecodeError::DecodedTooLarge)?;
                budget.set(left);
                Ok(v.to_vec())
            })
            .transpose()
        })
        .collect()
}
fn lists<T: ArrowPrimitiveType>(
    c: Option<C<'_>>,
    n: usize,
    item: &DataType,
) -> Result<Vec<Option<Vec<T::Native>>>, DecodeError> {
    let Some(c) = c else {
        return Ok(vec![None; n]);
    };
    match c.array.data_type() {
        DataType::List(f) if f.data_type() == item => c
            .array
            .as_list::<i32>()
            .iter()
            .map(|v| {
                // Upstream's Go producer appends zero items as nulls (its
                // `AppendNonZero` builders): a null item is the zero value.
                v.map(|values| {
                    values
                        .as_primitive::<T>()
                        .iter()
                        .map(Option::unwrap_or_default)
                        .collect()
                })
            })
            .map(Ok)
            .collect(),
        _ => Err(DecodeError::ColumnType(c.name)),
    }
}
fn time(v: Option<i64>, name: &'static str) -> Result<Option<u64>, DecodeError> {
    v.map(|t| u64::try_from(t).map_err(|_| DecodeError::NegativeTime(name)))
        .transpose()
}

// ------------------------------------------------------------------ ids

#[derive(Clone, Copy, PartialEq, Eq)]
enum Codec {
    Plain,
    Delta,
    Quasi,
}
fn codec(c: &C<'_>, default: Codec) -> Result<Codec, DecodeError> {
    match c
        .field
        .metadata()
        .get(schema::ENCODING_KEY)
        .map(String::as_str)
    {
        None => Ok(default),
        Some(schema::PLAIN_ENCODING) => Ok(Codec::Plain),
        Some(schema::DELTA_ENCODING) if default == Codec::Delta => Ok(Codec::Delta),
        Some(schema::QUASI_DELTA_ENCODING) if default == Codec::Quasi => Ok(Codec::Quasi),
        Some(_) => Err(DecodeError::BadIdEncoding(c.name)),
    }
}
/// Unsigned id values widened to u32; `max` is the column's width.
fn ids(c: &C<'_>) -> Result<(Vec<Option<u32>>, u32), DecodeError> {
    let a = plain(c)?;
    Ok(match a.data_type() {
        DataType::UInt16 => (
            a.as_primitive::<UInt16Type>()
                .iter()
                .map(|v| v.map(u32::from))
                .collect(),
            u32::from(u16::MAX),
        ),
        DataType::UInt32 => (a.as_primitive::<UInt32Type>().iter().collect(), u32::MAX),
        _ => return Err(DecodeError::ColumnType(c.name)),
    })
}
/// Delta (each non-null value adds to the previous non-null id) or plain.
fn delta_ids(c: &C<'_>) -> Result<Vec<Option<u32>>, DecodeError> {
    let (values, max) = ids(c)?;
    if codec(c, Codec::Delta)? == Codec::Plain {
        return Ok(values);
    }
    let mut acc = 0u32;
    let mut first = true;
    values
        .into_iter()
        .map(|v| {
            v.map(|d| {
                acc = if first {
                    d
                } else {
                    acc.checked_add(d)
                        .filter(|a| *a <= max)
                        .ok_or(DecodeError::IdOverflow(c.name))?
                };
                first = false;
                Ok(acc)
            })
            .transpose()
        })
        .collect()
}
/// Quasi-delta: a difference to the previous row's id when `same(i)`.
fn quasi_ids(c: &C<'_>, same: impl Fn(usize) -> bool) -> Result<Vec<u32>, DecodeError> {
    let (values, max) = ids(c)?;
    let quasi = codec(c, Codec::Quasi)? == Codec::Quasi;
    let mut out: Vec<u32> = Vec::with_capacity(values.len());
    for (i, v) in values.into_iter().enumerate() {
        let v = v.ok_or(DecodeError::NullValue(c.name))?;
        let id = if quasi && i > 0 && same(i) {
            out[i - 1]
                .checked_add(v)
                .filter(|a| *a <= max)
                .ok_or(DecodeError::IdOverflow(c.name))?
        } else {
            v
        };
        out.push(id);
    }
    Ok(out)
}

// ------------------------------------------------------------------ related data

/// Bytes a repeated copy of a decoded item costs against the string budget.
trait Weight {
    fn weight(&self) -> usize;
}
/// Fixed charge per copied entry, so repeats of string-free rows count too.
const ENTRY_WEIGHT: usize = 32;
fn attrs_weight(attrs: &DecodedAttributes) -> usize {
    attrs
        .iter()
        .map(Weight::weight)
        .fold(0, usize::saturating_add)
}
impl Weight for DecodedValue {
    fn weight(&self) -> usize {
        match self {
            DecodedValue::Str(s) => s.len(),
            DecodedValue::Bytes(b) => b.len(),
            DecodedValue::Slice(items) => items
                .iter()
                .map(|v| ENTRY_WEIGHT.saturating_add(v.weight()))
                .fold(0, usize::saturating_add),
            DecodedValue::Map(entries) => entries
                .iter()
                .map(Weight::weight)
                .fold(0, usize::saturating_add),
            _ => 0,
        }
    }
}
impl Weight for (String, DecodedValue) {
    fn weight(&self) -> usize {
        ENTRY_WEIGHT
            .saturating_add(self.0.len())
            .saturating_add(self.1.weight())
    }
}
impl Weight for DecodedEvent {
    fn weight(&self) -> usize {
        ENTRY_WEIGHT
            .saturating_add(self.name.len())
            .saturating_add(attrs_weight(&self.attributes))
    }
}
impl Weight for DecodedExemplar {
    fn weight(&self) -> usize {
        ENTRY_WEIGHT.saturating_add(attrs_weight(&self.filtered_attributes))
    }
}
impl Weight for DecodedLink {
    fn weight(&self) -> usize {
        ENTRY_WEIGHT
            .saturating_add(self.trace_state.as_ref().map_or(0, String::len))
            .saturating_add(attrs_weight(&self.attributes))
    }
}

/// Related rows grouped by parent id; every group must be claimed.
struct ByParent<T> {
    map: HashMap<u32, Vec<T>>,
    claimed: HashSet<u32>,
    name: &'static str,
}
impl<T: Clone + Weight> ByParent<T> {
    fn new(name: &'static str) -> Self {
        Self {
            map: HashMap::new(),
            claimed: HashSet::new(),
            name,
        }
    }
    fn push(&mut self, parent: u32, item: T) {
        self.map.entry(parent).or_default().push(item);
    }
    /// The group for `id`. The first claim's strings were charged when they
    /// were decoded; a repeated claim (the same resource or scope id placed
    /// again, possible with plain ids) copies them again, so it is charged
    /// against the decoded-string budget, with a per-entry overhead, before
    /// the copy.
    fn claim(&mut self, id: u32, budget: &Cell<usize>) -> Result<Vec<T>, DecodeError> {
        let Some(group) = self.map.get(&id) else {
            self.claimed.insert(id);
            return Ok(Vec::new());
        };
        if !self.claimed.insert(id) {
            let cost = group
                .iter()
                .try_fold(0usize, |acc, item| acc.checked_add(item.weight()))
                .ok_or(DecodeError::DecodedTooLarge)?;
            let left = budget
                .get()
                .checked_sub(cost)
                .ok_or(DecodeError::DecodedTooLarge)?;
            budget.set(left);
        }
        Ok(group.clone())
    }
    fn finish(self) -> Result<(), DecodeError> {
        if self.map.keys().all(|k| self.claimed.contains(k)) {
            Ok(())
        } else {
            Err(DecodeError::DanglingParent(self.name))
        }
    }
}

/// The value columns of an attribute payload or of the log `body` struct.
struct Values {
    strs: Vec<Option<String>>,
    ints: Vec<Option<i64>>,
    doubles: Vec<Option<f64>>,
    bools: Vec<Option<bool>>,
    bytes: Vec<Option<Vec<u8>>>,
    sers: Vec<Option<Vec<u8>>>,
    names: [&'static str; 6],
}
impl Values {
    /// Columns `str`, `int`, `double`, `bool`, `bytes`, `ser`.
    fn read(n: usize, budget: &Cell<usize>, cols: [Option<C<'_>>; 6]) -> Result<Self, DecodeError> {
        let [s, i, d, b, y, z] = cols;
        let name = |c: &Option<C<'_>>, default| c.as_ref().map_or(default, |c| c.name);
        let names = [
            name(&s, "str"),
            name(&i, "int"),
            name(&d, "double"),
            name(&b, "bool"),
            name(&y, "bytes"),
            name(&z, "ser"),
        ];
        Ok(Self {
            strs: strings(s, n, budget)?,
            ints: opt_prim::<Int64Type>(i, n, is(DataType::Int64))?,
            doubles: opt_prim::<Float64Type>(d, n, is(DataType::Float64))?,
            bools: bools(b, n)?,
            bytes: binaries(y, n, budget)?,
            sers: binaries(z, n, budget)?,
            names,
        })
    }
    /// Row `i` of type `ty`; `None` for an unknown type, and for `EMPTY`.
    fn take(
        &mut self,
        i: usize,
        ty: u8,
        budget: &Cell<usize>,
    ) -> Result<Option<DecodedValue>, DecodeError> {
        let null = |k: usize| DecodeError::NullValue(self.names[k]);
        Ok(Some(match ty {
            value_type::STR => DecodedValue::Str(self.strs[i].take().ok_or(null(0))?),
            value_type::INT => DecodedValue::Int(self.ints[i].ok_or(null(1))?),
            value_type::DOUBLE => DecodedValue::Double(self.doubles[i].ok_or(null(2))?),
            value_type::BOOL => DecodedValue::Bool(self.bools[i].ok_or(null(3))?),
            value_type::BYTES => DecodedValue::Bytes(self.bytes[i].take().ok_or(null(4))?),
            value_type::SLICE | value_type::MAP => {
                let ser = self.sers[i].take().ok_or(null(5))?;
                let value = super::cbor::decode(&ser, budget)?;
                match (ty, &value) {
                    (value_type::SLICE, DecodedValue::Slice(_))
                    | (value_type::MAP, DecodedValue::Map(_)) => value,
                    _ => return Err(DecodeError::BadSerializedValue),
                }
            }
            _ => return Ok(None),
        }))
    }
}

fn attributes(
    batch: Option<&RecordBatch>,
    name: &'static str,
    budget: &Cell<usize>,
) -> Result<ByParent<(String, DecodedValue)>, DecodeError> {
    let mut out = ByParent::new(name);
    let Some(batch) = batch else {
        return Ok(out);
    };
    let n = batch.num_rows();
    let keys = strings(Some(need(batch, "key")?), n, budget)?;
    let types = prim::<UInt8Type>(&need(batch, "type")?, is(DataType::UInt8))?;
    let mut values = Values::read(
        n,
        budget,
        [
            find(batch, "str"),
            find(batch, "int"),
            find(batch, "double"),
            find(batch, "bool"),
            find(batch, "bytes"),
            find(batch, "ser"),
        ],
    )?;
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        let key = keys[i].clone().ok_or(DecodeError::NullValue("key"))?;
        let ty = types[i].ok_or(DecodeError::NullValue("type"))?;
        let value = values
            .take(i, ty, budget)?
            .ok_or(DecodeError::BadAttribute(ty))?;
        rows.push((key, value));
    }
    // Upstream `AttrsParentIDDecoder`: equal key and value continue a run.
    let parents = quasi_ids(&need(batch, "parent_id")?, |i| {
        rows[i].0 == rows[i - 1].0 && rows[i].1.same(&rows[i - 1].1)
    })?;
    for (parent, row) in parents.into_iter().zip(rows) {
        out.push(parent, row);
    }
    Ok(out)
}

// ------------------------------------------------------------------ metrics helpers

/// Payload types and error names of one data-point family.
#[derive(Clone, Copy)]
struct FamilyNames {
    points: ArrowPayloadType,
    attrs: ArrowPayloadType,
    exemplars: ArrowPayloadType,
    exemplar_attrs: ArrowPayloadType,
    id: &'static str,
    attrs_parent: &'static str,
    exemplar_attrs_parent: &'static str,
}
impl FamilyNames {
    const NUMBER: Self = Self {
        points: ArrowPayloadType::NumberDataPoints,
        attrs: ArrowPayloadType::NumberDpAttrs,
        exemplars: ArrowPayloadType::NumberDpExemplars,
        exemplar_attrs: ArrowPayloadType::NumberDpExemplarAttrs,
        id: "number_dps.id",
        attrs_parent: "number_dp_attrs.parent_id",
        exemplar_attrs_parent: "number_dp_exemplar_attrs.parent_id",
    };
    const HISTOGRAM: Self = Self {
        points: ArrowPayloadType::HistogramDataPoints,
        attrs: ArrowPayloadType::HistogramDpAttrs,
        exemplars: ArrowPayloadType::HistogramDpExemplars,
        exemplar_attrs: ArrowPayloadType::HistogramDpExemplarAttrs,
        id: "histogram_dps.id",
        attrs_parent: "histogram_dp_attrs.parent_id",
        exemplar_attrs_parent: "histogram_dp_exemplar_attrs.parent_id",
    };
    const EXP_HISTOGRAM: Self = Self {
        points: ArrowPayloadType::ExpHistogramDataPoints,
        attrs: ArrowPayloadType::ExpHistogramDpAttrs,
        exemplars: ArrowPayloadType::ExpHistogramDpExemplars,
        exemplar_attrs: ArrowPayloadType::ExpHistogramDpExemplarAttrs,
        id: "exp_histogram_dps.id",
        attrs_parent: "exp_histogram_dp_attrs.parent_id",
        exemplar_attrs_parent: "exp_histogram_dp_exemplar_attrs.parent_id",
    };
}

/// The columns every data-point family shares, for one point.
struct PointCommon {
    row: usize,
    start_unix_ns: Option<u64>,
    time_unix_ns: u64,
    flags: Option<u32>,
    attributes: DecodedAttributes,
    exemplars: Vec<DecodedExemplar>,
}
struct MetricMeta {
    name: String,
    unit: Option<String>,
    description: Option<String>,
    aggregation_temporality: Option<i32>,
    resource: usize,
    scope: usize,
}

/// Data-point `parent_id`: delta-encoded row by row, or plain.
fn row_delta_parents(c: &C<'_>) -> Result<Vec<u32>, DecodeError> {
    let (raw, max) = ids(c)?;
    let plain = codec(c, Codec::Delta)? == Codec::Plain;
    let mut acc = 0u32;
    raw.into_iter()
        .map(|v| {
            let v = v.ok_or(DecodeError::NullValue("parent_id"))?;
            acc = if plain {
                v
            } else {
                acc.checked_add(v)
                    .filter(|a| *a <= max)
                    .ok_or(DecodeError::IdOverflow("parent_id"))?
            };
            Ok(acc)
        })
        .collect()
}

fn checked_total(counts: &[u64], start: u64) -> Result<u64, DecodeError> {
    counts.iter().try_fold(start, |acc, c| {
        acc.checked_add(*c).ok_or(DecodeError::HistogramShape)
    })
}

/// An exponential histogram `positive`/`negative` struct column.
fn exp_buckets(c: Option<C<'_>>, n: usize) -> Result<Vec<ExpBuckets>, DecodeError> {
    let Some(c) = c else {
        return Ok(vec![ExpBuckets::default(); n]);
    };
    let (s, fields) = as_struct(&c)?;
    let offsets = opt_prim::<Int32Type>(child(s, fields, "offset"), n, is(DataType::Int32))?;
    let counts = lists::<UInt64Type>(child(s, fields, "bucket_counts"), n, &DataType::UInt64)?;
    Ok(offsets
        .into_iter()
        .zip(counts)
        .map(|(o, c)| ExpBuckets {
            offset: o.unwrap_or(0),
            counts: c.unwrap_or_default(),
        })
        .collect())
}

fn same_number(a: Option<NumberValue>, b: Option<NumberValue>) -> bool {
    match (a, b) {
        (Some(NumberValue::Int(x)), Some(NumberValue::Int(y))) => x == y,
        (Some(NumberValue::Double(x)), Some(NumberValue::Double(y))) => x == y,
        _ => false,
    }
}

/// Exemplars of one family grouped by data-point id. `parent_id` is
/// quasi-delta keyed on the value (upstream `ExemplarParentIdDecoder`);
/// Beacon requires every exemplar to carry a value.
fn exemplars(
    batch: Option<&RecordBatch>,
    attrs_batch: Option<&RecordBatch>,
    attrs_parent: &'static str,
    budget: &Cell<usize>,
) -> Result<ByParent<DecodedExemplar>, DecodeError> {
    let mut out = ByParent::new("exemplars.parent_id");
    let mut attrs = attributes(attrs_batch, attrs_parent, budget)?;
    let Some(batch) = batch else {
        attrs.finish()?;
        return Ok(out);
    };
    let n = batch.num_rows();
    let ids = match find(batch, "id") {
        Some(c) => delta_ids(&c)?,
        None => vec![None; n],
    };
    let ints = opt_prim::<Int64Type>(find(batch, "int_value"), n, is(DataType::Int64))?;
    let doubles = opt_prim::<Float64Type>(find(batch, "double_value"), n, is(DataType::Float64))?;
    let values: Vec<Option<NumberValue>> = (0..n)
        .map(|i| match (ints[i], doubles[i]) {
            (Some(v), _) => Some(NumberValue::Int(v)),
            (None, Some(v)) => Some(NumberValue::Double(v)),
            (None, None) => None,
        })
        .collect();
    if values.iter().any(Option::is_none) {
        return Err(DecodeError::NullValue("exemplar value"));
    }
    let parents = quasi_ids(&need(batch, "parent_id")?, |i| {
        same_number(values[i], values[i - 1])
    })?;
    let times = prim::<TimestampNanosecondType>(&need(batch, "time_unix_nano")?, is_time)?;
    let span_ids = fixed(find(batch, "span_id"), n, 8)?;
    let trace_ids = fixed(find(batch, "trace_id"), n, 16)?;
    let mut seen = HashSet::new();
    for i in 0..n {
        let filtered_attributes = match ids[i] {
            Some(id) if !seen.insert(id) => return Err(DecodeError::DuplicateId("exemplars.id")),
            Some(id) => attrs.claim(id, budget)?,
            None => Vec::new(),
        };
        out.push(
            parents[i],
            DecodedExemplar {
                time_unix_ns: time(
                    Some(times[i].ok_or(DecodeError::NullValue("time_unix_nano"))?),
                    "time_unix_nano",
                )?
                .expect("some"),
                value: values[i].expect("checked"),
                span_id: span_ids[i]
                    .as_ref()
                    .map(|b| u64::from_be_bytes(b.as_slice().try_into().expect("width 8"))),
                trace_id: trace_ids[i]
                    .as_ref()
                    .map(|b| u128::from_be_bytes(b.as_slice().try_into().expect("width 16"))),
                filtered_attributes,
            },
        );
    }
    attrs.finish()?;
    Ok(out)
}

/// Per-row resource and scope indices into the batch's lists.
struct Placement {
    resource: Vec<usize>,
    scope: Vec<usize>,
}

struct Cx<'a> {
    budget: &'a Cell<usize>,
    records: &'a mut BTreeMap<ArrowPayloadType, RecordBatch>,
    out: DecodedBatch,
}
impl Cx<'_> {
    fn take(&mut self, ty: ArrowPayloadType) -> Option<RecordBatch> {
        self.records.remove(&ty)
    }

    /// Resource and scope boundaries of a main record, as the Go decoder
    /// computes them (`ResourceIDFromRecord`, `UpdateResourceFromRecord`).
    fn place(&mut self, batch: &RecordBatch) -> Result<Placement, DecodeError> {
        let n = batch.num_rows();
        let budget = self.budget;
        let mut res_attrs = attributes(
            self.take(ArrowPayloadType::ResourceAttrs).as_ref(),
            "resource_attrs.parent_id",
            budget,
        )?;
        let mut scope_attrs = attributes(
            self.take(ArrowPayloadType::ScopeAttrs).as_ref(),
            "scope_attrs.parent_id",
            budget,
        )?;

        struct Side {
            ids: Vec<Option<u32>>,
            plain: bool,
            valid: Vec<bool>,
            strs: Vec<Vec<Option<String>>>,
            dropped: Vec<Option<u32>>,
        }
        let side = |name: &'static str,
                    id_name: &'static str,
                    strs: &[&'static str]|
         -> Result<Side, DecodeError> {
            let Some(col) = find(batch, name) else {
                return Ok(Side {
                    ids: vec![None; n],
                    plain: false,
                    valid: vec![false; n],
                    strs: vec![vec![None; n]; strs.len()],
                    dropped: vec![None; n],
                });
            };
            let (s, fields) = as_struct(&col)?;
            let valid = (0..n).map(|i| s.is_valid(i)).collect();
            let (ids, plain) = match child(s, fields, "id") {
                Some(mut c) => {
                    c.name = id_name;
                    let (raw, _) = ids(&c)?;
                    (raw, codec(&c, Codec::Delta)? == Codec::Plain)
                }
                None => (vec![None; n], false),
            };
            let strs = strs
                .iter()
                .map(|f| strings(child(s, fields, f), n, budget))
                .collect::<Result<_, _>>()?;
            let dropped = opt_prim::<UInt32Type>(
                child(s, fields, "dropped_attributes_count"),
                n,
                is(DataType::UInt32),
            )?;
            Ok(Side {
                ids,
                plain,
                valid,
                strs,
                dropped,
            })
        };
        let r = side("resource", "resource.id", &["schema_url"])?;
        let s = side("scope", "scope.id", &["name", "version"])?;
        let schema_urls = strings(find(batch, "schema_url"), n, budget)?;

        let mut placement = Placement {
            resource: Vec::with_capacity(n),
            scope: Vec::with_capacity(n),
        };
        let (mut res_id, mut scope_id) = (0u32, 0u32);
        let (mut prev_res, mut prev_scope): (Option<u32>, Option<u32>) = (None, None);
        for i in 0..n {
            // Null ids read as 0, as `U16FromStruct` does.
            let advance =
                |acc: u32, v: Option<u32>, plain: bool, name| -> Result<u32, DecodeError> {
                    let v = v.unwrap_or(0);
                    if plain {
                        Ok(v)
                    } else {
                        acc.checked_add(v)
                            .filter(|a| *a <= u32::from(u16::MAX))
                            .ok_or(DecodeError::IdOverflow(name))
                    }
                };
            res_id = advance(res_id, r.ids[i], r.plain, "resource.id")?;
            if prev_res != Some(res_id) {
                prev_res = Some(res_id);
                prev_scope = None;
                let attributes = match r.ids[i] {
                    Some(_) if r.valid[i] => res_attrs.claim(res_id, self.budget)?,
                    _ => Vec::new(),
                };
                self.out.resources.push(DecodedResource {
                    attributes,
                    schema_url: r.strs[0][i].clone(),
                    dropped_attributes_count: r.dropped[i].unwrap_or(0),
                });
            }
            scope_id = advance(scope_id, s.ids[i], s.plain, "scope.id")?;
            if prev_scope != Some(scope_id) {
                prev_scope = Some(scope_id);
                let attributes = match s.ids[i] {
                    Some(_) if s.valid[i] => scope_attrs.claim(scope_id, self.budget)?,
                    _ => Vec::new(),
                };
                self.out.scopes.push(DecodedScope {
                    name: s.strs[0][i].clone(),
                    version: s.strs[1][i].clone(),
                    attributes,
                    dropped_attributes_count: s.dropped[i].unwrap_or(0),
                    schema_url: schema_urls[i].clone(),
                });
            }
            placement.resource.push(self.out.resources.len() - 1);
            placement.scope.push(self.out.scopes.len() - 1);
        }
        res_attrs.finish()?;
        scope_attrs.finish()?;
        Ok(placement)
    }

    fn traces(&mut self) -> Result<(), DecodeError> {
        let budget = self.budget;
        let batch = self
            .take(ArrowPayloadType::Spans)
            .expect("main record present");
        let n = batch.num_rows();
        let place = self.place(&batch)?;
        let mut attrs = attributes(
            self.take(ArrowPayloadType::SpanAttrs).as_ref(),
            "span_attrs.parent_id",
            budget,
        )?;
        let event_attrs = attributes(
            self.take(ArrowPayloadType::SpanEventAttrs).as_ref(),
            "span_event_attrs.parent_id",
            budget,
        )?;
        let link_attrs = attributes(
            self.take(ArrowPayloadType::SpanLinkAttrs).as_ref(),
            "span_link_attrs.parent_id",
            budget,
        )?;
        let mut events = self.events(event_attrs)?;
        let mut links = self.links(link_attrs)?;

        let ids = match find(&batch, "id") {
            Some(c) => delta_ids(&c)?,
            None => vec![None; n],
        };
        let start =
            prim::<TimestampNanosecondType>(&need(&batch, "start_time_unix_nano")?, is_time)?;
        let duration =
            prim::<DurationNanosecondType>(&need(&batch, "duration_time_unix_nano")?, is_duration)?;
        let trace_id = fixed(Some(need(&batch, "trace_id")?), n, 16)?;
        let span_id = fixed(Some(need(&batch, "span_id")?), n, 8)?;
        let trace_state = strings(find(&batch, "trace_state"), n, budget)?;
        let parent = fixed(find(&batch, "parent_span_id"), n, 8)?;
        let flags = opt_prim::<UInt32Type>(find(&batch, "flags"), n, is(DataType::UInt32))?;
        let names = strings(Some(need(&batch, "name")?), n, budget)?;
        let kinds = opt_prim::<Int32Type>(find(&batch, "kind"), n, is(DataType::Int32))?;
        let d_attrs = opt_prim::<UInt32Type>(
            find(&batch, "dropped_attributes_count"),
            n,
            is(DataType::UInt32),
        )?;
        let d_events = opt_prim::<UInt32Type>(
            find(&batch, "dropped_events_count"),
            n,
            is(DataType::UInt32),
        )?;
        let d_links =
            opt_prim::<UInt32Type>(find(&batch, "dropped_links_count"), n, is(DataType::UInt32))?;
        let (codes, messages) = match find(&batch, "status") {
            None => (vec![None; n], vec![None; n]),
            Some(status) => {
                let (s, fields) = as_struct(&status)?;
                (
                    opt_prim::<Int32Type>(child(s, fields, "code"), n, is(DataType::Int32))?,
                    strings(child(s, fields, "status_message"), n, budget)?,
                )
            }
        };
        let mut seen = HashSet::new();
        for i in 0..n {
            let (attributes, evs, lks) = match ids[i] {
                Some(id) => {
                    if !seen.insert(id) {
                        return Err(DecodeError::DuplicateId("id"));
                    }
                    (
                        attrs.claim(id, self.budget)?,
                        events.claim(id, self.budget)?,
                        links.claim(id, self.budget)?,
                    )
                }
                None => Default::default(),
            };
            let trace = trace_id[i]
                .as_ref()
                .ok_or(DecodeError::NullValue("trace_id"))?;
            let span = span_id[i]
                .as_ref()
                .ok_or(DecodeError::NullValue("span_id"))?;
            self.out.spans.push(DecodedSpan {
                trace_id: u128::from_be_bytes(trace.as_slice().try_into().expect("width 16")),
                span_id: u64::from_be_bytes(span.as_slice().try_into().expect("width 8")),
                parent_span_id: parent[i]
                    .as_ref()
                    .map(|p| u64::from_be_bytes(p.as_slice().try_into().expect("width 8"))),
                start_unix_ns: time(
                    Some(start[i].ok_or(DecodeError::NullValue("start_time_unix_nano"))?),
                    "start_time_unix_nano",
                )?
                .expect("some"),
                duration_ns: time(
                    Some(duration[i].ok_or(DecodeError::NullValue("duration_time_unix_nano"))?),
                    "duration_time_unix_nano",
                )?
                .expect("some"),
                flags: flags[i],
                name: names[i].clone().ok_or(DecodeError::NullValue("name"))?,
                status_code: codes[i],
                status_message: messages[i].clone(),
                attributes,
                resource: place.resource[i],
                scope: place.scope[i],
                kind: kinds[i],
                trace_state: trace_state[i].clone(),
                dropped_attributes_count: d_attrs[i].unwrap_or(0),
                dropped_events_count: d_events[i].unwrap_or(0),
                dropped_links_count: d_links[i].unwrap_or(0),
                events: evs,
                links: lks,
            });
        }
        attrs.finish()?;
        events.finish()?;
        links.finish()
    }

    fn events(
        &mut self,
        mut attrs: ByParent<(String, DecodedValue)>,
    ) -> Result<ByParent<DecodedEvent>, DecodeError> {
        let budget = self.budget;
        let mut out = ByParent::new("span_events.parent_id");
        let Some(batch) = self.take(ArrowPayloadType::SpanEvents) else {
            attrs.finish()?;
            return Ok(out);
        };
        let n = batch.num_rows();
        let ids = match find(&batch, "id") {
            Some(c) => delta_ids(&c)?,
            None => vec![None; n],
        };
        let names = strings(Some(need(&batch, "name")?), n, budget)?;
        let names: Vec<String> = names
            .into_iter()
            .map(|s| s.ok_or(DecodeError::NullValue("span_events.name")))
            .collect::<Result<_, _>>()?;
        let parents = quasi_ids(&need(&batch, "parent_id")?, |i| names[i] == names[i - 1])?;
        let times =
            opt_prim::<TimestampNanosecondType>(find(&batch, "time_unix_nano"), n, is_time)?;
        let dropped = opt_prim::<UInt32Type>(
            find(&batch, "dropped_attributes_count"),
            n,
            is(DataType::UInt32),
        )?;
        let mut seen = HashSet::new();
        for i in 0..n {
            let attributes = match ids[i] {
                Some(id) if !seen.insert(id) => {
                    return Err(DecodeError::DuplicateId("span_events.id"))
                }
                Some(id) => attrs.claim(id, self.budget)?,
                None => Vec::new(),
            };
            out.push(
                parents[i],
                DecodedEvent {
                    name: names[i].clone(),
                    time_unix_ns: time(times[i], "span_events.time_unix_nano")?,
                    attributes,
                    dropped_attributes_count: dropped[i].unwrap_or(0),
                },
            );
        }
        attrs.finish()?;
        Ok(out)
    }

    fn links(
        &mut self,
        mut attrs: ByParent<(String, DecodedValue)>,
    ) -> Result<ByParent<DecodedLink>, DecodeError> {
        let budget = self.budget;
        let mut out = ByParent::new("span_links.parent_id");
        let Some(batch) = self.take(ArrowPayloadType::SpanLinks) else {
            attrs.finish()?;
            return Ok(out);
        };
        let n = batch.num_rows();
        let ids = match find(&batch, "id") {
            Some(c) => delta_ids(&c)?,
            None => vec![None; n],
        };
        let trace_ids: Vec<Vec<u8>> = fixed(Some(need(&batch, "trace_id")?), n, 16)?
            .into_iter()
            .map(|t| t.ok_or(DecodeError::NullValue("span_links.trace_id")))
            .collect::<Result<_, _>>()?;
        let span_ids = fixed(Some(need(&batch, "span_id")?), n, 8)?;
        let parents = quasi_ids(&need(&batch, "parent_id")?, |i| {
            trace_ids[i] == trace_ids[i - 1]
        })?;
        let states = strings(find(&batch, "trace_state"), n, budget)?;
        let dropped = opt_prim::<UInt32Type>(
            find(&batch, "dropped_attributes_count"),
            n,
            is(DataType::UInt32),
        )?;
        let flags = opt_prim::<UInt32Type>(find(&batch, "flags"), n, is(DataType::UInt32))?;
        let mut seen = HashSet::new();
        for i in 0..n {
            let attributes = match ids[i] {
                Some(id) if !seen.insert(id) => {
                    return Err(DecodeError::DuplicateId("span_links.id"))
                }
                Some(id) => attrs.claim(id, self.budget)?,
                None => Vec::new(),
            };
            let span = span_ids[i]
                .as_ref()
                .ok_or(DecodeError::NullValue("span_links.span_id"))?;
            out.push(
                parents[i],
                DecodedLink {
                    trace_id: u128::from_be_bytes(
                        trace_ids[i].as_slice().try_into().expect("width 16"),
                    ),
                    span_id: u64::from_be_bytes(span.as_slice().try_into().expect("width 8")),
                    trace_state: states[i].clone(),
                    flags: flags[i],
                    attributes,
                    dropped_attributes_count: dropped[i].unwrap_or(0),
                },
            );
        }
        attrs.finish()?;
        Ok(out)
    }

    /// One data-point family's payloads: rows, their metric row, id, times,
    /// flags, attributes and exemplars, in point order.
    fn family(
        &mut self,
        names: FamilyNames,
        by_id: &HashMap<u32, usize>,
    ) -> Result<Option<(RecordBatch, Vec<PointCommon>)>, DecodeError> {
        let budget = self.budget;
        let mut dp_attrs = attributes(self.take(names.attrs).as_ref(), names.attrs_parent, budget)?;
        let exemplar_attrs = self.take(names.exemplar_attrs);
        let mut exemplars = exemplars(
            self.take(names.exemplars).as_ref(),
            exemplar_attrs.as_ref(),
            names.exemplar_attrs_parent,
            budget,
        )?;
        let Some(points) = self.take(names.points) else {
            dp_attrs.finish()?;
            exemplars.finish()?;
            return Ok(None);
        };
        let n = points.num_rows();
        let dp_ids = match find(&points, "id") {
            Some(c) => delta_ids(&c)?,
            None => vec![None; n],
        };
        let parents = row_delta_parents(&need(&points, "parent_id")?)?;
        let start =
            opt_prim::<TimestampNanosecondType>(find(&points, "start_time_unix_nano"), n, is_time)?;
        let times = prim::<TimestampNanosecondType>(&need(&points, "time_unix_nano")?, is_time)?;
        let flags = opt_prim::<UInt32Type>(find(&points, "flags"), n, is(DataType::UInt32))?;
        let mut seen = HashSet::new();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let row = *by_id
                .get(&parents[i])
                .ok_or(DecodeError::DanglingParent("parent_id"))?;
            let (attributes, exemplars) = match dp_ids[i] {
                Some(id) if !seen.insert(id) => return Err(DecodeError::DuplicateId(names.id)),
                Some(id) => (dp_attrs.claim(id, budget)?, exemplars.claim(id, budget)?),
                None => (Vec::new(), Vec::new()),
            };
            out.push(PointCommon {
                row,
                start_unix_ns: time(start[i], "start_time_unix_nano")?,
                time_unix_ns: time(
                    Some(times[i].ok_or(DecodeError::NullValue("time_unix_nano"))?),
                    "time_unix_nano",
                )?
                .expect("some"),
                flags: flags[i],
                attributes,
                exemplars,
            });
        }
        dp_attrs.finish()?;
        exemplars.finish()?;
        Ok(Some((points, out)))
    }

    fn metrics(&mut self) -> Result<(), DecodeError> {
        let budget = self.budget;
        let metrics = self
            .take(ArrowPayloadType::UnivariateMetrics)
            .expect("main record present");
        let m = metrics.num_rows();
        let place = self.place(&metrics)?;
        let metric_ids = delta_ids(&need(&metrics, "id")?)?;
        let types = prim::<UInt8Type>(&need(&metrics, "metric_type")?, is(DataType::UInt8))?;
        let names = strings(Some(need(&metrics, "name")?), m, budget)?;
        let descriptions = strings(find(&metrics, "description"), m, budget)?;
        let units = strings(find(&metrics, "unit"), m, budget)?;
        let temporality = opt_prim::<Int32Type>(
            find(&metrics, "aggregation_temporality"),
            m,
            is(DataType::Int32),
        )?;
        let monotonic = bools(find(&metrics, "is_monotonic"), m)?;
        let mut by_id = HashMap::new();
        let mut kinds = Vec::with_capacity(m);
        for i in 0..m {
            let id = metric_ids[i].ok_or(DecodeError::NullValue("id"))?;
            let ty = types[i].ok_or(DecodeError::NullValue("metric_type"))?;
            if !matches!(
                ty,
                schema::METRIC_TYPE_GAUGE
                    | schema::METRIC_TYPE_SUM
                    | schema::METRIC_TYPE_HISTOGRAM
                    | schema::METRIC_TYPE_EXP_HISTOGRAM
            ) {
                return Err(DecodeError::UnsupportedMetricType(ty));
            }
            if names[i].is_none() {
                return Err(DecodeError::NullValue("name"));
            }
            if by_id.insert(id, i).is_some() {
                return Err(DecodeError::DuplicateId("id"));
            }
            kinds.push(ty);
        }
        let check_kind = |row: usize, allowed: &[u8]| {
            if allowed.contains(&kinds[row]) {
                Ok(())
            } else {
                Err(DecodeError::PointKindMismatch(kinds[row]))
            }
        };
        let meta = |row: usize| MetricMeta {
            name: names[row].clone().expect("checked"),
            unit: units[row].clone(),
            description: descriptions[row].clone(),
            aggregation_temporality: temporality[row],
            resource: place.resource[row],
            scope: place.scope[row],
        };

        if let Some((points, common)) = self.family(FamilyNames::NUMBER, &by_id)? {
            let n = points.num_rows();
            let ints = opt_prim::<Int64Type>(find(&points, "int_value"), n, is(DataType::Int64))?;
            let doubles =
                opt_prim::<Float64Type>(find(&points, "double_value"), n, is(DataType::Float64))?;
            for (i, c) in common.into_iter().enumerate() {
                check_kind(c.row, &[schema::METRIC_TYPE_GAUGE, schema::METRIC_TYPE_SUM])?;
                let value = match (ints[i], doubles[i]) {
                    (Some(v), _) => Some(NumberValue::Int(v)),
                    (None, Some(v)) => Some(NumberValue::Double(v)),
                    (None, None) => None,
                };
                let meta = meta(c.row);
                self.out.numbers.push(DecodedNumber {
                    name: meta.name,
                    unit: meta.unit,
                    description: meta.description,
                    metric_type: kinds[c.row],
                    aggregation_temporality: meta.aggregation_temporality,
                    is_monotonic: monotonic[c.row],
                    resource: meta.resource,
                    scope: meta.scope,
                    start_unix_ns: c.start_unix_ns,
                    time_unix_ns: c.time_unix_ns,
                    value,
                    flags: c.flags,
                    attributes: c.attributes,
                    exemplars: c.exemplars,
                });
            }
        }

        if let Some((points, common)) = self.family(FamilyNames::HISTOGRAM, &by_id)? {
            let n = points.num_rows();
            let counts = opt_prim::<UInt64Type>(find(&points, "count"), n, is(DataType::UInt64))?;
            let sums = opt_prim::<Float64Type>(find(&points, "sum"), n, is(DataType::Float64))?;
            let buckets =
                lists::<UInt64Type>(find(&points, "bucket_counts"), n, &DataType::UInt64)?;
            let bounds =
                lists::<Float64Type>(find(&points, "explicit_bounds"), n, &DataType::Float64)?;
            let mins = opt_prim::<Float64Type>(find(&points, "min"), n, is(DataType::Float64))?;
            let maxs = opt_prim::<Float64Type>(find(&points, "max"), n, is(DataType::Float64))?;
            for (i, c) in common.into_iter().enumerate() {
                check_kind(c.row, &[schema::METRIC_TYPE_HISTOGRAM])?;
                let bucket_counts = buckets[i].clone().unwrap_or_default();
                let explicit_bounds = bounds[i].clone().unwrap_or_default();
                // OTLP: N bounds describe N + 1 buckets; bounds strictly increase.
                if !bucket_counts.is_empty() && explicit_bounds.len() + 1 != bucket_counts.len()
                    || explicit_bounds
                        .windows(2)
                        .any(|w| w[0].partial_cmp(&w[1]) != Some(std::cmp::Ordering::Less))
                    || explicit_bounds.iter().any(|b| !b.is_finite())
                {
                    return Err(DecodeError::HistogramShape);
                }
                if let Some(total) = counts[i] {
                    let sum = checked_total(&bucket_counts, 0)?;
                    if !bucket_counts.is_empty() && sum != total {
                        return Err(DecodeError::HistogramShape);
                    }
                }
                let meta = meta(c.row);
                self.out.histograms.push(DecodedHistogram {
                    name: meta.name,
                    unit: meta.unit,
                    start_unix_ns: c.start_unix_ns,
                    time_unix_ns: c.time_unix_ns,
                    count: counts[i],
                    bucket_counts,
                    explicit_bounds,
                    resource: meta.resource,
                    scope: meta.scope,
                    description: meta.description,
                    aggregation_temporality: meta.aggregation_temporality,
                    sum: sums[i],
                    min: mins[i],
                    max: maxs[i],
                    flags: c.flags,
                    attributes: c.attributes,
                    exemplars: c.exemplars,
                });
            }
        }

        if let Some((points, common)) = self.family(FamilyNames::EXP_HISTOGRAM, &by_id)? {
            let n = points.num_rows();
            let counts = opt_prim::<UInt64Type>(find(&points, "count"), n, is(DataType::UInt64))?;
            let sums = opt_prim::<Float64Type>(find(&points, "sum"), n, is(DataType::Float64))?;
            let scales = opt_prim::<Int32Type>(find(&points, "scale"), n, is(DataType::Int32))?;
            let zero_counts =
                opt_prim::<UInt64Type>(find(&points, "zero_count"), n, is(DataType::UInt64))?;
            let positive = exp_buckets(find(&points, "positive"), n)?;
            let negative = exp_buckets(find(&points, "negative"), n)?;
            let mins = opt_prim::<Float64Type>(find(&points, "min"), n, is(DataType::Float64))?;
            let maxs = opt_prim::<Float64Type>(find(&points, "max"), n, is(DataType::Float64))?;
            let thresholds =
                opt_prim::<Float64Type>(find(&points, "zero_threshold"), n, is(DataType::Float64))?;
            for (i, c) in common.into_iter().enumerate() {
                check_kind(c.row, &[schema::METRIC_TYPE_EXP_HISTOGRAM])?;
                let scale = scales[i].unwrap_or(0);
                let zero_count = zero_counts[i].unwrap_or(0);
                let zero_threshold = thresholds[i].unwrap_or(0.0);
                let (pos, neg) = (positive[i].clone(), negative[i].clone());
                if !(-10..=20).contains(&scale)
                    || !(zero_threshold >= 0.0 && zero_threshold.is_finite())
                    || [&pos, &neg].iter().any(|b| {
                        i32::try_from(b.counts.len())
                            .ok()
                            .and_then(|l| b.offset.checked_add(l))
                            .is_none()
                    })
                {
                    return Err(DecodeError::HistogramShape);
                }
                let total = checked_total(&neg.counts, checked_total(&pos.counts, zero_count)?)?;
                if counts[i].is_some_and(|c| c != total) {
                    return Err(DecodeError::HistogramShape);
                }
                let meta = meta(c.row);
                self.out.exp_histograms.push(DecodedExpHistogram {
                    name: meta.name,
                    unit: meta.unit,
                    description: meta.description,
                    aggregation_temporality: meta.aggregation_temporality,
                    resource: meta.resource,
                    scope: meta.scope,
                    start_unix_ns: c.start_unix_ns,
                    time_unix_ns: c.time_unix_ns,
                    count: counts[i].unwrap_or(total),
                    sum: sums[i],
                    scale,
                    zero_count,
                    zero_threshold,
                    positive: pos,
                    negative: neg,
                    min: mins[i],
                    max: maxs[i],
                    flags: c.flags,
                    attributes: c.attributes,
                    exemplars: c.exemplars,
                });
            }
        }
        Ok(())
    }

    fn logs(&mut self) -> Result<(), DecodeError> {
        let budget = self.budget;
        let batch = self
            .take(ArrowPayloadType::Logs)
            .expect("main record present");
        let n = batch.num_rows();
        let place = self.place(&batch)?;
        let mut attrs = attributes(
            self.take(ArrowPayloadType::LogAttrs).as_ref(),
            "log_attrs.parent_id",
            budget,
        )?;
        let ids = match find(&batch, "id") {
            Some(c) => delta_ids(&c)?,
            None => vec![None; n],
        };
        let times =
            opt_prim::<TimestampNanosecondType>(find(&batch, "time_unix_nano"), n, is_time)?;
        let observed = opt_prim::<TimestampNanosecondType>(
            find(&batch, "observed_time_unix_nano"),
            n,
            is_time,
        )?;
        let trace_ids = fixed(find(&batch, "trace_id"), n, 16)?;
        let span_ids = fixed(find(&batch, "span_id"), n, 8)?;
        let severity =
            opt_prim::<Int32Type>(find(&batch, "severity_number"), n, is(DataType::Int32))?;
        let severity_text = strings(find(&batch, "severity_text"), n, budget)?;
        let dropped = opt_prim::<UInt32Type>(
            find(&batch, "dropped_attributes_count"),
            n,
            is(DataType::UInt32),
        )?;
        let flags = opt_prim::<UInt32Type>(find(&batch, "flags"), n, is(DataType::UInt32))?;
        let event_names = strings(find(&batch, "event_name"), n, budget)?;
        let mut bodies = vec![None; n];
        if let Some(body) = find(&batch, "body") {
            let (s, fields) = as_struct(&body)?;
            let ty = opt_prim::<UInt8Type>(child(s, fields, "type"), n, is(DataType::UInt8))?;
            let mut values = Values::read(
                n,
                budget,
                [
                    child(s, fields, "str").map(|c| C {
                        name: "body.str",
                        ..c
                    }),
                    child(s, fields, "int").map(|c| C {
                        name: "body.int",
                        ..c
                    }),
                    child(s, fields, "double").map(|c| C {
                        name: "body.double",
                        ..c
                    }),
                    child(s, fields, "bool").map(|c| C {
                        name: "body.bool",
                        ..c
                    }),
                    child(s, fields, "bytes").map(|c| C {
                        name: "body.bytes",
                        ..c
                    }),
                    child(s, fields, "ser").map(|c| C {
                        name: "body.ser",
                        ..c
                    }),
                ],
            )?;
            for i in 0..n {
                bodies[i] = match ty[i] {
                    None | Some(value_type::EMPTY) => None,
                    Some(t) => Some(
                        values
                            .take(i, t, budget)?
                            .ok_or(DecodeError::BadAttribute(t))?,
                    ),
                };
            }
        }
        let to_u128 = |v: &Option<Vec<u8>>| {
            v.as_ref()
                .map(|b| u128::from_be_bytes(b.as_slice().try_into().expect("width 16")))
        };
        let to_u64 = |v: &Option<Vec<u8>>| {
            v.as_ref()
                .map(|b| u64::from_be_bytes(b.as_slice().try_into().expect("width 8")))
        };
        let mut seen = HashSet::new();
        for i in 0..n {
            let attributes = match ids[i] {
                Some(id) if !seen.insert(id) => return Err(DecodeError::DuplicateId("id")),
                Some(id) => attrs.claim(id, self.budget)?,
                None => Vec::new(),
            };
            self.out.logs.push(DecodedLog {
                resource: place.resource[i],
                scope: place.scope[i],
                time_unix_ns: time(times[i], "time_unix_nano")?,
                observed_time_unix_ns: time(observed[i], "observed_time_unix_nano")?,
                trace_id: to_u128(&trace_ids[i]),
                span_id: to_u64(&span_ids[i]),
                severity_number: severity[i],
                severity_text: severity_text[i].clone(),
                body: bodies[i].take(),
                attributes,
                dropped_attributes_count: dropped[i].unwrap_or(0),
                flags: flags[i],
                event_name: event_names[i].clone(),
            });
        }
        attrs.finish()
    }
}
