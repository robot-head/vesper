//! OTAP consumer: `BatchArrowRecords` back to spans and histograms.
//!
//! Streams are keyed by `schema_id`. A payload with an unknown id starts a
//! new IPC stream; if another stream of the same payload type exists it is
//! dropped (the producer reset it). Each payload must hold exactly one record
//! batch and end on an IPC message boundary.
//!
//! All input is untrusted. Every limit violation, malformed protobuf or IPC
//! message, schema mismatch and dangling reference is a typed error, and any
//! error drops all stream state, mirroring the producer's reset-on-error.

use super::ipc::IpcReader;
use super::proto::{ArrowPayloadType, BatchArrowRecords};
use super::schema::{self, value_type};
use arrow_array::{
    cast::AsArray,
    types::{
        DurationNanosecondType, Float64Type, Int32Type, Int64Type, TimestampNanosecondType,
        UInt16Type, UInt32Type, UInt64Type, UInt8Type,
    },
    Array, ArrayRef, RecordBatch,
};
use arrow_schema::{ArrowError, DataType, TimeUnit};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    pub max_payloads: usize,
    pub max_streams: usize,
    pub max_schema_id_bytes: usize,
    /// IPC bytes of one payload.
    pub max_payload_bytes: usize,
    /// IPC bytes accepted over a stream's lifetime (bounds dictionary growth).
    pub max_stream_bytes: usize,
    pub max_rows: usize,
}
impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_payloads: 8,
            max_streams: 8,
            max_schema_id_bytes: 128,
            max_payload_bytes: 16 << 20,
            max_stream_bytes: 256 << 20,
            max_rows: super::encode::MAX_BATCH_ROWS,
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
    NullValue(&'static str),
    RowLimit,
    NegativeTime(&'static str),
    DuplicateId(&'static str),
    /// An attribute or data point refers to a row that does not exist.
    DanglingParent(&'static str),
    /// Attribute rows without a `SPANS` payload in the same batch, or data
    /// points without `UNIVARIATE_METRICS`.
    MissingMainRecord,
    BadAttribute(u8),
    UnsupportedMetricType(u8),
    HistogramShape,
    Arrow(ArrowError),
}
impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DecodeError {}

#[derive(Clone, Debug, PartialEq)]
pub enum DecodedValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
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
    pub attributes: Vec<(String, DecodedValue)>,
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
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedBatch {
    pub batch_id: i64,
    pub spans: Vec<DecodedSpan>,
    pub histograms: Vec<DecodedHistogram>,
}

struct RxStream {
    ty: ArrowPayloadType,
    reader: IpcReader,
    bytes: usize,
}

pub struct OtapDecoder {
    limits: DecodeLimits,
    streams: HashMap<String, RxStream>,
}
impl Default for OtapDecoder {
    fn default() -> Self {
        Self::new(DecodeLimits::default())
    }
}
impl OtapDecoder {
    pub fn new(limits: DecodeLimits) -> Self {
        Self {
            limits,
            streams: HashMap::new(),
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
            if !matches!(
                ty,
                ArrowPayloadType::Spans
                    | ArrowPayloadType::SpanAttrs
                    | ArrowPayloadType::UnivariateMetrics
                    | ArrowPayloadType::HistogramDataPoints
            ) {
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
        let mut out = DecodedBatch {
            batch_id: batch.batch_id,
            ..DecodedBatch::default()
        };
        let mut span_ids = None;
        if let Some(spans) = records.get(&ArrowPayloadType::Spans) {
            let (spans, ids) = self.spans(spans)?;
            out.spans = spans;
            span_ids = Some(ids);
        }
        if let Some(attrs) = records.get(&ArrowPayloadType::SpanAttrs) {
            let ids = span_ids.ok_or(DecodeError::MissingMainRecord)?;
            attach_attributes(&mut out.spans, ids, attrs)?;
        }
        match (
            records.get(&ArrowPayloadType::UnivariateMetrics),
            records.get(&ArrowPayloadType::HistogramDataPoints),
        ) {
            (Some(metrics), Some(points)) => out.histograms = self.histograms(metrics, points)?,
            (None, Some(_)) => return Err(DecodeError::MissingMainRecord),
            (Some(metrics), None) => {
                // Metrics without histogram points are only valid when empty.
                if metrics.num_rows() != 0 {
                    return Err(DecodeError::HistogramShape);
                }
            }
            (None, None) => {}
        }
        Ok(out)
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

    #[allow(clippy::type_complexity)]
    fn spans(
        &self,
        batch: &RecordBatch,
    ) -> Result<(Vec<DecodedSpan>, Option<Vec<u16>>), DecodeError> {
        let n = batch.num_rows();
        let ids = u16_ids(batch, "id")?;
        let start = column(batch, "start_time_unix_nano")?;
        let start = match start.data_type() {
            DataType::Timestamp(TimeUnit::Nanosecond, _) => {
                start.as_primitive::<TimestampNanosecondType>()
            }
            _ => return Err(DecodeError::ColumnType("start_time_unix_nano")),
        };
        let duration = column(batch, "duration_time_unix_nano")?;
        let duration = match duration.data_type() {
            DataType::Duration(TimeUnit::Nanosecond) => {
                duration.as_primitive::<DurationNanosecondType>()
            }
            _ => return Err(DecodeError::ColumnType("duration_time_unix_nano")),
        };
        let trace_id = fixed(batch, "trace_id", 16)?;
        let span_id = fixed(batch, "span_id", 8)?;
        let parent = optional(batch, "parent_span_id")
            .map(|_| fixed(batch, "parent_span_id", 8))
            .transpose()?;
        let flags = optional(batch, "flags")
            .map(|c| match c.data_type() {
                DataType::UInt32 => Ok(c.as_primitive::<UInt32Type>()),
                _ => Err(DecodeError::ColumnType("flags")),
            })
            .transpose()?;
        let names = strings(column(batch, "name")?, "name")?;
        let (codes, messages) = match optional(batch, "status") {
            None => (None, vec![None; n]),
            Some(status) => {
                let status = match status.data_type() {
                    DataType::Struct(_) => status.as_struct(),
                    _ => return Err(DecodeError::ColumnType("status")),
                };
                let code = status
                    .column_by_name("code")
                    .map(|c| match c.data_type() {
                        DataType::Int32 => Ok(c.as_primitive::<Int32Type>().clone()),
                        _ => Err(DecodeError::ColumnType("status.code")),
                    })
                    .transpose()?;
                let message = match status.column_by_name("status_message") {
                    Some(c) => strings(c, "status.status_message")?,
                    None => vec![None; n],
                };
                // A null status struct row nulls both children.
                let code = code.map(|c| {
                    (0..n)
                        .map(|i| (status.is_valid(i) && c.is_valid(i)).then(|| c.value(i)))
                        .collect::<Vec<_>>()
                });
                let message = message
                    .into_iter()
                    .enumerate()
                    .map(|(i, m)| m.filter(|_| status.is_valid(i)))
                    .collect();
                (code, message)
            }
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(id) = ids.as_ref().map(|ids| ids[i]) {
                let id = id.ok_or(DecodeError::NullValue("id"))?;
                if !seen.insert(id) {
                    return Err(DecodeError::DuplicateId("id"));
                }
            }
            let start_ns = required(start, i, "start_time_unix_nano")?;
            let duration_ns = required(duration, i, "duration_time_unix_nano")?;
            let trace = trace_id[i].ok_or(DecodeError::NullValue("trace_id"))?;
            let span = span_id[i].ok_or(DecodeError::NullValue("span_id"))?;
            out.push(DecodedSpan {
                trace_id: u128::from_be_bytes(trace.try_into().expect("width 16")),
                span_id: u64::from_be_bytes(span.try_into().expect("width 8")),
                parent_span_id: parent
                    .as_ref()
                    .and_then(|p| p[i])
                    .map(|p| u64::from_be_bytes(p.try_into().expect("width 8"))),
                start_unix_ns: u64::try_from(start_ns)
                    .map_err(|_| DecodeError::NegativeTime("start_time_unix_nano"))?,
                duration_ns: u64::try_from(duration_ns)
                    .map_err(|_| DecodeError::NegativeTime("duration_time_unix_nano"))?,
                flags: flags.and_then(|f| f.is_valid(i).then(|| f.value(i))),
                name: names[i].clone().ok_or(DecodeError::NullValue("name"))?,
                status_code: codes.as_ref().and_then(|c| c[i]),
                status_message: messages[i].clone(),
                attributes: Vec::new(),
            });
        }
        // Span ids in row order, so attributes can be resolved by `id`.
        let ids = ids.map(|ids| ids.into_iter().flatten().collect());
        Ok((out, ids))
    }

    fn histograms(
        &self,
        metrics: &RecordBatch,
        points: &RecordBatch,
    ) -> Result<Vec<DecodedHistogram>, DecodeError> {
        let metric_ids = u16_ids(metrics, "id")?.ok_or(DecodeError::MissingColumn("id"))?;
        let types = column(metrics, "metric_type")?;
        let types = match types.data_type() {
            DataType::UInt8 => types.as_primitive::<UInt8Type>(),
            _ => return Err(DecodeError::ColumnType("metric_type")),
        };
        let names = strings(column(metrics, "name")?, "name")?;
        let units = match optional(metrics, "unit") {
            Some(c) => strings(c, "unit")?,
            None => vec![None; metrics.num_rows()],
        };
        let mut by_id = HashMap::new();
        for i in 0..metrics.num_rows() {
            let id = metric_ids[i].ok_or(DecodeError::NullValue("id"))?;
            let ty = required(types, i, "metric_type")?;
            if ty != schema::METRIC_TYPE_HISTOGRAM {
                return Err(DecodeError::UnsupportedMetricType(ty));
            }
            let name = names[i].clone().ok_or(DecodeError::NullValue("name"))?;
            if by_id.insert(id, (name, units[i].clone())).is_some() {
                return Err(DecodeError::DuplicateId("id"));
            }
        }
        let parents =
            u16_ids(points, "parent_id")?.ok_or(DecodeError::MissingColumn("parent_id"))?;
        let start = optional(points, "start_time_unix_nano")
            .map(|c| match c.data_type() {
                DataType::Timestamp(TimeUnit::Nanosecond, _) => {
                    Ok(c.as_primitive::<TimestampNanosecondType>())
                }
                _ => Err(DecodeError::ColumnType("start_time_unix_nano")),
            })
            .transpose()?;
        let time = column(points, "time_unix_nano")?;
        let time = match time.data_type() {
            DataType::Timestamp(TimeUnit::Nanosecond, _) => {
                time.as_primitive::<TimestampNanosecondType>()
            }
            _ => return Err(DecodeError::ColumnType("time_unix_nano")),
        };
        let count = optional(points, "count")
            .map(|c| match c.data_type() {
                DataType::UInt64 => Ok(c.as_primitive::<UInt64Type>()),
                _ => Err(DecodeError::ColumnType("count")),
            })
            .transpose()?;
        let buckets = list(points, "bucket_counts", &DataType::UInt64)?;
        let bounds = list(points, "explicit_bounds", &DataType::Float64)?;
        let mut out = Vec::with_capacity(points.num_rows());
        for i in 0..points.num_rows() {
            let parent = parents[i].ok_or(DecodeError::NullValue("parent_id"))?;
            let (name, unit) = by_id
                .get(&parent)
                .cloned()
                .ok_or(DecodeError::DanglingParent("parent_id"))?;
            let bucket_counts: Vec<u64> = match buckets.as_ref().and_then(|b| b[i].clone()) {
                Some(values) => {
                    let values = values.as_primitive::<UInt64Type>();
                    if values.null_count() != 0 {
                        return Err(DecodeError::NullValue("bucket_counts"));
                    }
                    values.values().to_vec()
                }
                None => Vec::new(),
            };
            let explicit_bounds: Vec<f64> = match bounds.as_ref().and_then(|b| b[i].clone()) {
                Some(values) => {
                    let values = values.as_primitive::<Float64Type>();
                    if values.null_count() != 0 {
                        return Err(DecodeError::NullValue("explicit_bounds"));
                    }
                    values.values().to_vec()
                }
                None => Vec::new(),
            };
            // OTLP: N bounds describe N + 1 buckets; bounds strictly increase.
            if !bucket_counts.is_empty() && explicit_bounds.len() + 1 != bucket_counts.len()
                || explicit_bounds
                    .windows(2)
                    .any(|w| w[0].partial_cmp(&w[1]) != Some(std::cmp::Ordering::Less))
                || explicit_bounds.iter().any(|b| !b.is_finite())
            {
                return Err(DecodeError::HistogramShape);
            }
            let count = count.and_then(|c| c.is_valid(i).then(|| c.value(i)));
            if let Some(total) = count {
                let mut sum = 0u64;
                for c in &bucket_counts {
                    sum = sum.checked_add(*c).ok_or(DecodeError::HistogramShape)?;
                }
                if !bucket_counts.is_empty() && sum != total {
                    return Err(DecodeError::HistogramShape);
                }
            }
            let time_ns = required(time, i, "time_unix_nano")?;
            out.push(DecodedHistogram {
                name,
                unit,
                start_unix_ns: start
                    .and_then(|s| s.is_valid(i).then(|| s.value(i)))
                    .map(|s| {
                        u64::try_from(s)
                            .map_err(|_| DecodeError::NegativeTime("start_time_unix_nano"))
                    })
                    .transpose()?,
                time_unix_ns: u64::try_from(time_ns)
                    .map_err(|_| DecodeError::NegativeTime("time_unix_nano"))?,
                count,
                bucket_counts,
                explicit_bounds,
            });
        }
        Ok(out)
    }
}

fn attach_attributes(
    spans: &mut [DecodedSpan],
    ids: Option<Vec<u16>>,
    attrs: &RecordBatch,
) -> Result<(), DecodeError> {
    let index: HashMap<u16, usize> = ids
        .ok_or(DecodeError::MissingColumn("id"))?
        .into_iter()
        .enumerate()
        .map(|(row, id)| (id, row))
        .collect();
    let parents = u16_ids(attrs, "parent_id")?.ok_or(DecodeError::MissingColumn("parent_id"))?;
    let keys = strings(column(attrs, "key")?, "key")?;
    let types = column(attrs, "type")?;
    let types = match types.data_type() {
        DataType::UInt8 => types.as_primitive::<UInt8Type>(),
        _ => return Err(DecodeError::ColumnType("type")),
    };
    let n = attrs.num_rows();
    let strs = match optional(attrs, "str") {
        Some(c) => strings(c, "str")?,
        None => vec![None; n],
    };
    let ints = optional(attrs, "int")
        .map(|c| match c.data_type() {
            DataType::Int64 => Ok(c.as_primitive::<Int64Type>()),
            _ => Err(DecodeError::ColumnType("int")),
        })
        .transpose()?;
    let doubles = optional(attrs, "double")
        .map(|c| match c.data_type() {
            DataType::Float64 => Ok(c.as_primitive::<Float64Type>()),
            _ => Err(DecodeError::ColumnType("double")),
        })
        .transpose()?;
    let bools = optional(attrs, "bool")
        .map(|c| match c.data_type() {
            DataType::Boolean => Ok(c.as_boolean()),
            _ => Err(DecodeError::ColumnType("bool")),
        })
        .transpose()?;
    for i in 0..n {
        let parent = parents[i].ok_or(DecodeError::NullValue("parent_id"))?;
        let row = *index
            .get(&parent)
            .ok_or(DecodeError::DanglingParent("parent_id"))?;
        let key = keys[i].clone().ok_or(DecodeError::NullValue("key"))?;
        let ty = required(types, i, "type")?;
        let value = match ty {
            value_type::STR => {
                DecodedValue::Str(strs[i].clone().ok_or(DecodeError::NullValue("str"))?)
            }
            value_type::INT => DecodedValue::Int(
                ints.filter(|c| c.is_valid(i))
                    .map(|c| c.value(i))
                    .ok_or(DecodeError::NullValue("int"))?,
            ),
            value_type::DOUBLE => DecodedValue::Double(
                doubles
                    .filter(|c| c.is_valid(i))
                    .map(|c| c.value(i))
                    .ok_or(DecodeError::NullValue("double"))?,
            ),
            value_type::BOOL => DecodedValue::Bool(
                bools
                    .filter(|c| c.is_valid(i))
                    .map(|c| c.value(i))
                    .ok_or(DecodeError::NullValue("bool"))?,
            ),
            other => return Err(DecodeError::BadAttribute(other)),
        };
        spans[row].attributes.push((key, value));
    }
    Ok(())
}

fn optional<'a>(batch: &'a RecordBatch, name: &str) -> Option<&'a ArrayRef> {
    batch.column_by_name(name)
}
fn column<'a>(batch: &'a RecordBatch, name: &'static str) -> Result<&'a ArrayRef, DecodeError> {
    batch
        .column_by_name(name)
        .ok_or(DecodeError::MissingColumn(name))
}
fn required<T: arrow_array::ArrowPrimitiveType>(
    array: &arrow_array::PrimitiveArray<T>,
    i: usize,
    name: &'static str,
) -> Result<T::Native, DecodeError> {
    if array.is_valid(i) {
        Ok(array.value(i))
    } else {
        Err(DecodeError::NullValue(name))
    }
}
/// `None` if the column is absent; plain encoding only.
fn u16_ids(
    batch: &RecordBatch,
    name: &'static str,
) -> Result<Option<Vec<Option<u16>>>, DecodeError> {
    let Some(c) = batch.column_by_name(name) else {
        return Ok(None);
    };
    let field = batch
        .schema_ref()
        .field_with_name(name)
        .map_err(DecodeError::Arrow)?
        .clone();
    match field.metadata().get(schema::ENCODING_KEY) {
        Some(e) if e == schema::PLAIN_ENCODING => {}
        // Upstream defaults to delta/quasi-delta id encodings; Beacon decodes
        // only explicitly plain ids.
        _ => return Err(DecodeError::ColumnType(name)),
    }
    match c.data_type() {
        DataType::UInt16 => Ok(Some(c.as_primitive::<UInt16Type>().iter().collect())),
        _ => Err(DecodeError::ColumnType(name)),
    }
}
fn fixed<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
    width: i32,
) -> Result<Vec<Option<&'a [u8]>>, DecodeError> {
    let c = column(batch, name)?;
    match c.data_type() {
        DataType::FixedSizeBinary(w) if *w == width => {
            Ok(c.as_fixed_size_binary().iter().collect())
        }
        _ => Err(DecodeError::ColumnType(name)),
    }
}
/// Strings from plain `Utf8` or `Dictionary<u8|u16, Utf8>` columns.
fn strings(c: &ArrayRef, name: &'static str) -> Result<Vec<Option<String>>, DecodeError> {
    fn from_dict<K: arrow_array::types::ArrowDictionaryKeyType>(
        c: &ArrayRef,
        name: &'static str,
    ) -> Result<Vec<Option<String>>, DecodeError> {
        let d = c.as_dictionary::<K>();
        let values = match d.values().data_type() {
            DataType::Utf8 => d.values().as_string::<i32>(),
            _ => return Err(DecodeError::ColumnType(name)),
        };
        d.keys()
            .iter()
            .map(|k| {
                k.map(|k| {
                    let k = arrow_buffer::ArrowNativeType::as_usize(k);
                    if k >= values.len() || values.is_null(k) {
                        return Err(DecodeError::NullValue(name));
                    }
                    Ok(values.value(k).to_owned())
                })
                .transpose()
            })
            .collect()
    }
    match c.data_type() {
        DataType::Utf8 => Ok(c
            .as_string::<i32>()
            .iter()
            .map(|s| s.map(str::to_owned))
            .collect()),
        DataType::Dictionary(k, v) if **v == DataType::Utf8 => match **k {
            DataType::UInt8 => from_dict::<UInt8Type>(c, name),
            DataType::UInt16 => from_dict::<UInt16Type>(c, name),
            _ => Err(DecodeError::ColumnType(name)),
        },
        _ => Err(DecodeError::ColumnType(name)),
    }
}
fn list(
    batch: &RecordBatch,
    name: &'static str,
    item: &DataType,
) -> Result<Option<Vec<Option<ArrayRef>>>, DecodeError> {
    let Some(c) = batch.column_by_name(name) else {
        return Ok(None);
    };
    match c.data_type() {
        DataType::List(f) if f.data_type() == item => Ok(Some(c.as_list::<i32>().iter().collect())),
        _ => Err(DecodeError::ColumnType(name)),
    }
}
