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
    /// Bytes of owned strings one batch may decode into. Dictionary columns
    /// expand each referenced value per row, so this bounds the expansion
    /// independently of the encoded size.
    pub max_decoded_string_bytes: usize,
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
            max_decoded_string_bytes: 64 << 20,
        }
    }
}

#[derive(Debug)]
pub enum DecodeError {
    HeadersUnsupported,
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
    UnsupportedMetricType(u8),
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

/// A decoded attribute or body value. Equality on `Double` is `f64 ==`, as
/// in upstream's quasi-delta comparison.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
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
    pub logs: Vec<DecodedLog>,
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
        T::HistogramDataPoints | T::HistogramDpAttrs => (Signal::Metrics, false),
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
        }
    }
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }
    pub fn reset(&mut self) {
        self.streams.clear();
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
        if !batch.headers.is_empty() {
            return Err(DecodeError::HeadersUnsupported);
        }
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
        let reader = &mut stream.reader;
        let mut batches = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.feed(bytes, max_rows)
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
                v.map(|values| {
                    let values = values.as_primitive::<T>();
                    if values.null_count() != 0 {
                        return Err(DecodeError::NullValue(c.name));
                    }
                    Ok(values.values().to_vec())
                })
                .transpose()
            })
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

/// Related rows grouped by parent id; every group must be claimed.
struct ByParent<T> {
    map: HashMap<u32, Vec<T>>,
    claimed: HashSet<u32>,
    name: &'static str,
}
impl<T: Clone> ByParent<T> {
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
    fn claim(&mut self, id: u32) -> Vec<T> {
        self.claimed.insert(id);
        self.map.get(&id).cloned().unwrap_or_default()
    }
    fn finish(self) -> Result<(), DecodeError> {
        if self.map.keys().all(|k| self.claimed.contains(k)) {
            Ok(())
        } else {
            Err(DecodeError::DanglingParent(self.name))
        }
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
    let strs = strings(find(batch, "str"), n, budget)?;
    let ints = opt_prim::<Int64Type>(find(batch, "int"), n, is(DataType::Int64))?;
    let doubles = opt_prim::<Float64Type>(find(batch, "double"), n, is(DataType::Float64))?;
    let bools = bools(find(batch, "bool"), n)?;
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        let key = keys[i].clone().ok_or(DecodeError::NullValue("key"))?;
        let ty = types[i].ok_or(DecodeError::NullValue("type"))?;
        let value = match ty {
            value_type::STR => {
                DecodedValue::Str(strs[i].clone().ok_or(DecodeError::NullValue("str"))?)
            }
            value_type::INT => DecodedValue::Int(ints[i].ok_or(DecodeError::NullValue("int"))?),
            value_type::DOUBLE => {
                DecodedValue::Double(doubles[i].ok_or(DecodeError::NullValue("double"))?)
            }
            value_type::BOOL => DecodedValue::Bool(bools[i].ok_or(DecodeError::NullValue("bool"))?),
            other => return Err(DecodeError::BadAttribute(other)),
        };
        rows.push((key, value));
    }
    // Upstream `AttrsParentIDDecoder`: equal key and value continue a run.
    let parents = quasi_ids(&need(batch, "parent_id")?, |i| rows[i] == rows[i - 1])?;
    for (parent, row) in parents.into_iter().zip(rows) {
        out.push(parent, row);
    }
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
                    Some(_) if r.valid[i] => res_attrs.claim(res_id),
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
                    Some(_) if s.valid[i] => scope_attrs.claim(scope_id),
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
                    (attrs.claim(id), events.claim(id), links.claim(id))
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
                Some(id) => attrs.claim(id),
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
                Some(id) => attrs.claim(id),
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

    fn metrics(&mut self) -> Result<(), DecodeError> {
        let budget = self.budget;
        let metrics = self
            .take(ArrowPayloadType::UnivariateMetrics)
            .expect("main record present");
        let m = metrics.num_rows();
        let place = self.place(&metrics)?;
        let mut dp_attrs = attributes(
            self.take(ArrowPayloadType::HistogramDpAttrs).as_ref(),
            "histogram_dp_attrs.parent_id",
            budget,
        )?;
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
        let mut by_id = HashMap::new();
        for i in 0..m {
            let id = metric_ids[i].ok_or(DecodeError::NullValue("id"))?;
            let ty = types[i].ok_or(DecodeError::NullValue("metric_type"))?;
            if ty != schema::METRIC_TYPE_HISTOGRAM {
                return Err(DecodeError::UnsupportedMetricType(ty));
            }
            let name = names[i].clone().ok_or(DecodeError::NullValue("name"))?;
            if by_id.insert(id, i).is_some() {
                return Err(DecodeError::DuplicateId("id"));
            }
            let _ = name;
        }
        let Some(points) = self.take(ArrowPayloadType::HistogramDataPoints) else {
            // Metrics without histogram points are only valid when empty.
            dp_attrs.finish()?;
            return if m == 0 {
                Ok(())
            } else {
                Err(DecodeError::HistogramShape)
            };
        };
        let n = points.num_rows();
        let dp_ids = match find(&points, "id") {
            Some(c) => delta_ids(&c)?,
            None => vec![None; n],
        };
        // `parent_id` is delta-encoded row by row (upstream histogram decoder).
        let parent_col = need(&points, "parent_id")?;
        let (raw_parents, max) = ids(&parent_col)?;
        let plain_parents = codec(&parent_col, Codec::Delta)? == Codec::Plain;
        let mut acc = 0u32;
        let mut parents = Vec::with_capacity(n);
        for v in raw_parents {
            let v = v.ok_or(DecodeError::NullValue("parent_id"))?;
            acc = if plain_parents {
                v
            } else {
                acc.checked_add(v)
                    .filter(|a| *a <= max)
                    .ok_or(DecodeError::IdOverflow("parent_id"))?
            };
            parents.push(acc);
        }
        let start =
            opt_prim::<TimestampNanosecondType>(find(&points, "start_time_unix_nano"), n, is_time)?;
        let times = prim::<TimestampNanosecondType>(&need(&points, "time_unix_nano")?, is_time)?;
        let counts = opt_prim::<UInt64Type>(find(&points, "count"), n, is(DataType::UInt64))?;
        let sums = opt_prim::<Float64Type>(find(&points, "sum"), n, is(DataType::Float64))?;
        let buckets = lists::<UInt64Type>(find(&points, "bucket_counts"), n, &DataType::UInt64)?;
        let bounds = lists::<Float64Type>(find(&points, "explicit_bounds"), n, &DataType::Float64)?;
        let flags = opt_prim::<UInt32Type>(find(&points, "flags"), n, is(DataType::UInt32))?;
        let mins = opt_prim::<Float64Type>(find(&points, "min"), n, is(DataType::Float64))?;
        let maxs = opt_prim::<Float64Type>(find(&points, "max"), n, is(DataType::Float64))?;
        let mut seen = HashSet::new();
        for i in 0..n {
            let row = *by_id
                .get(&parents[i])
                .ok_or(DecodeError::DanglingParent("parent_id"))?;
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
                let mut sum = 0u64;
                for c in &bucket_counts {
                    sum = sum.checked_add(*c).ok_or(DecodeError::HistogramShape)?;
                }
                if !bucket_counts.is_empty() && sum != total {
                    return Err(DecodeError::HistogramShape);
                }
            }
            let attributes = match dp_ids[i] {
                Some(id) if !seen.insert(id) => {
                    return Err(DecodeError::DuplicateId("histogram_dps.id"))
                }
                Some(id) => dp_attrs.claim(id),
                None => Vec::new(),
            };
            self.out.histograms.push(DecodedHistogram {
                name: names[row].clone().expect("checked"),
                unit: units[row].clone(),
                start_unix_ns: time(start[i], "start_time_unix_nano")?,
                time_unix_ns: time(
                    Some(times[i].ok_or(DecodeError::NullValue("time_unix_nano"))?),
                    "time_unix_nano",
                )?
                .expect("some"),
                count: counts[i],
                bucket_counts,
                explicit_bounds,
                resource: place.resource[row],
                scope: place.scope[row],
                description: descriptions[row].clone(),
                aggregation_temporality: temporality[row],
                sum: sums[i],
                min: mins[i],
                max: maxs[i],
                flags: flags[i],
                attributes,
            });
        }
        dp_attrs.finish()
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
            let strs = strings(child(s, fields, "str"), n, budget)?;
            let ints = opt_prim::<Int64Type>(child(s, fields, "int"), n, is(DataType::Int64))?;
            let doubles =
                opt_prim::<Float64Type>(child(s, fields, "double"), n, is(DataType::Float64))?;
            let bools = bools(child(s, fields, "bool"), n)?;
            for i in 0..n {
                bodies[i] = match ty[i] {
                    None | Some(value_type::EMPTY) => None,
                    Some(value_type::STR) => Some(DecodedValue::Str(
                        strs[i].clone().ok_or(DecodeError::NullValue("body.str"))?,
                    )),
                    Some(value_type::INT) => Some(DecodedValue::Int(
                        ints[i].ok_or(DecodeError::NullValue("body.int"))?,
                    )),
                    Some(value_type::DOUBLE) => Some(DecodedValue::Double(
                        doubles[i].ok_or(DecodeError::NullValue("body.double"))?,
                    )),
                    Some(value_type::BOOL) => Some(DecodedValue::Bool(
                        bools[i].ok_or(DecodeError::NullValue("body.bool"))?,
                    )),
                    Some(other) => return Err(DecodeError::BadAttribute(other)),
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
                Some(id) => attrs.claim(id),
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
