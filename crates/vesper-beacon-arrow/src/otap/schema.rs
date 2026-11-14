//! OTAP Arrow schemas emitted by Beacon: the subset of the upstream
//! otel-arrow `TracesSchema`, `AttrsSchema16`, `MetricsSchema` and
//! `HistogramDataPointSchema` columns that Beacon has data for. Names and
//! physical types follow upstream; upstream marks most columns optional, and
//! Beacon omits the ones it never fills (resource, scope, schema_url,
//! trace_state, kind, dropped counts, bytes/ser attribute values, sum, min,
//! max, exemplars).
//!
//! `id`/`parent_id` columns carry field metadata `encoding=plain`: Beacon
//! sends absolute ids, not upstream's default delta/quasi-delta encoding.

use arrow_schema::{DataType, Field, Fields, Schema, TimeUnit};
use std::{collections::HashMap, sync::Arc};

pub const ENCODING_KEY: &str = "encoding";
pub const PLAIN_ENCODING: &str = "plain";

fn plain(field: Field) -> Field {
    field.with_metadata(HashMap::from([(
        ENCODING_KEY.to_owned(),
        PLAIN_ENCODING.to_owned(),
    )]))
}
pub(crate) fn dict(key: DataType) -> DataType {
    DataType::Dictionary(Box::new(key), Box::new(DataType::Utf8))
}
pub(crate) fn timestamp() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, None)
}

pub(crate) fn status_fields() -> Fields {
    Fields::from(vec![
        Field::new("code", DataType::Int32, true),
        Field::new("status_message", dict(DataType::UInt8), true),
    ])
}

/// `SPANS` payload schema.
pub fn spans_schema() -> Schema {
    Schema::new(vec![
        plain(Field::new("id", DataType::UInt16, true)),
        Field::new("start_time_unix_nano", timestamp(), false),
        Field::new(
            "duration_time_unix_nano",
            DataType::Duration(TimeUnit::Nanosecond),
            false,
        ),
        Field::new("trace_id", DataType::FixedSizeBinary(16), false),
        Field::new("span_id", DataType::FixedSizeBinary(8), false),
        Field::new("parent_span_id", DataType::FixedSizeBinary(8), true),
        Field::new("flags", DataType::UInt32, true),
        Field::new("name", dict(DataType::UInt8), false),
        Field::new("status", DataType::Struct(status_fields()), true),
    ])
}

/// `SPAN_ATTRS` payload schema (upstream `AttrsSchema16`, without `bytes`/`ser`).
pub fn span_attrs_schema() -> Schema {
    Schema::new(vec![
        plain(Field::new("parent_id", DataType::UInt16, false)),
        Field::new("key", dict(DataType::UInt8), false),
        Field::new("type", DataType::UInt8, false),
        Field::new("str", dict(DataType::UInt16), true),
        Field::new("int", DataType::Int64, true),
        Field::new("double", DataType::Float64, true),
        Field::new("bool", DataType::Boolean, true),
    ])
}

/// `UNIVARIATE_METRICS` payload schema.
pub fn metrics_schema() -> Schema {
    Schema::new(vec![
        plain(Field::new("id", DataType::UInt16, false)),
        Field::new("metric_type", DataType::UInt8, false),
        Field::new("name", dict(DataType::UInt8), false),
        Field::new("unit", dict(DataType::UInt8), true),
        Field::new("aggregation_temporality", DataType::Int32, true),
    ])
}

pub(crate) fn list_of(item: DataType) -> DataType {
    DataType::List(Arc::new(Field::new_list_field(item, true)))
}

/// `HISTOGRAM_DATA_POINTS` payload schema.
pub fn histogram_points_schema() -> Schema {
    Schema::new(vec![
        plain(Field::new("id", DataType::UInt32, true)),
        plain(Field::new("parent_id", DataType::UInt16, false)),
        Field::new("start_time_unix_nano", timestamp(), true),
        Field::new("time_unix_nano", timestamp(), false),
        Field::new("count", DataType::UInt64, true),
        Field::new("bucket_counts", list_of(DataType::UInt64), true),
        Field::new("explicit_bounds", list_of(DataType::Float64), true),
    ])
}

/// OTLP `AnyValue` type codes used in the attribute `type` column
/// (pcommon.ValueType numbering).
pub mod value_type {
    pub const EMPTY: u8 = 0;
    pub const STR: u8 = 1;
    pub const INT: u8 = 2;
    pub const DOUBLE: u8 = 3;
    pub const BOOL: u8 = 4;
}
/// OTLP `metric_type` for an explicit-bucket histogram.
pub const METRIC_TYPE_HISTOGRAM: u8 = 3;
/// OTLP `AGGREGATION_TEMPORALITY_CUMULATIVE`.
pub const TEMPORALITY_CUMULATIVE: i32 = 2;
/// OTLP `Status.code` values.
pub const STATUS_CODE_OK: i32 = 1;
pub const STATUS_CODE_ERROR: i32 = 2;
