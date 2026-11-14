//! OTAP Arrow schemas emitted by Beacon, following the upstream otel-arrow
//! Go schemas (`TracesSchema`, `EventSchema`, `LinkSchema`, `ResourceDT`,
//! `ScopeDT`, `AttrsSchema16`/`AttrsSchema32`, `MetricsSchema`,
//! `HistogramDataPointSchema`, `LogsSchema`). Names, physical types and
//! column order follow upstream. Beacon omits only the columns it never
//! fills: attribute `bytes`/`ser` values, exemplars and the non-histogram
//! metric payloads.
//!
//! **Id encodings.** Beacon emits upstream's default transport encodings and
//! no `encoding` metadata, because the upstream Go decoder (used by the
//! collector's `otelarrow` receiver) ignores the metadata and always applies
//! them:
//!
//! * delta: main-record `id` columns, `resource.id`, `scope.id`, event and
//!   link `id`, data-point `id` (each non-null value is the difference to the
//!   previous non-null value), and data-point `parent_id` (difference to the
//!   previous row's parent);
//! * quasi-delta: attribute `parent_id` (a difference when the row's key and
//!   value equal the previous row's, absolute otherwise), event `parent_id`
//!   (keyed on the event name) and link `parent_id` (keyed on the trace id).
//!
//! The decoder also accepts the wave-six `encoding=plain` marker (absolute
//! ids). See [`IdEncoding`].

use arrow_schema::{DataType, Field, Fields, Schema, TimeUnit};
use std::{collections::HashMap, sync::Arc};

pub const ENCODING_KEY: &str = "encoding";
pub const PLAIN_ENCODING: &str = "plain";
pub const DELTA_ENCODING: &str = "delta";
pub const QUASI_DELTA_ENCODING: &str = "quasidelta";

/// How an id column of an emitted schema is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdEncoding {
    /// Upstream transport encodings (delta and quasi-delta), no metadata.
    /// This is what the collector's `otelarrow` receiver understands.
    Transport,
    /// Absolute ids marked `encoding=plain` (the wave-six format). Readers
    /// that ignore the metadata, such as the upstream Go decoder, misread
    /// these ids, so use this only between Beacon peers.
    Plain,
}

fn with_encoding(field: Field, encoding: IdEncoding) -> Field {
    match encoding {
        IdEncoding::Transport => field,
        IdEncoding::Plain => field.with_metadata(HashMap::from([(
            ENCODING_KEY.to_owned(),
            PLAIN_ENCODING.to_owned(),
        )])),
    }
}
pub(crate) fn dict(key: DataType) -> DataType {
    DataType::Dictionary(Box::new(key), Box::new(DataType::Utf8))
}
pub(crate) fn timestamp() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, None)
}
pub(crate) fn list_of(item: DataType) -> DataType {
    DataType::List(Arc::new(Field::new_list_field(item, true)))
}

pub(crate) fn status_fields() -> Fields {
    Fields::from(vec![
        Field::new("code", DataType::Int32, true),
        Field::new("status_message", dict(DataType::UInt8), true),
    ])
}

/// Upstream `ResourceDT`.
pub fn resource_fields(encoding: IdEncoding) -> Fields {
    Fields::from(vec![
        with_encoding(Field::new("id", DataType::UInt16, true), encoding),
        Field::new("schema_url", dict(DataType::UInt8), true),
        Field::new("dropped_attributes_count", DataType::UInt32, true),
    ])
}

/// Upstream `ScopeDT`.
pub fn scope_fields(encoding: IdEncoding) -> Fields {
    Fields::from(vec![
        with_encoding(Field::new("id", DataType::UInt16, true), encoding),
        Field::new("name", dict(DataType::UInt8), true),
        Field::new("version", dict(DataType::UInt8), true),
        Field::new("dropped_attributes_count", DataType::UInt32, true),
    ])
}

fn resource_scope(encoding: IdEncoding) -> [Field; 3] {
    [
        Field::new(
            "resource",
            DataType::Struct(resource_fields(encoding)),
            true,
        ),
        Field::new("scope", DataType::Struct(scope_fields(encoding)), true),
        Field::new("schema_url", dict(DataType::UInt8), true),
    ]
}

/// `SPANS` payload schema.
pub fn spans_schema_with(encoding: IdEncoding) -> Schema {
    let [resource, scope, schema_url] = resource_scope(encoding);
    Schema::new(vec![
        with_encoding(Field::new("id", DataType::UInt16, true), encoding),
        resource,
        scope,
        schema_url,
        Field::new("start_time_unix_nano", timestamp(), false),
        Field::new(
            "duration_time_unix_nano",
            DataType::Duration(TimeUnit::Nanosecond),
            false,
        ),
        Field::new("trace_id", DataType::FixedSizeBinary(16), false),
        Field::new("span_id", DataType::FixedSizeBinary(8), false),
        Field::new("trace_state", DataType::Utf8, true),
        Field::new("parent_span_id", DataType::FixedSizeBinary(8), true),
        Field::new("flags", DataType::UInt32, true),
        Field::new("name", dict(DataType::UInt8), false),
        Field::new("kind", DataType::Int32, true),
        Field::new("dropped_attributes_count", DataType::UInt32, true),
        Field::new("dropped_events_count", DataType::UInt32, true),
        Field::new("dropped_links_count", DataType::UInt32, true),
        Field::new("status", DataType::Struct(status_fields()), true),
    ])
}
pub fn spans_schema() -> Schema {
    spans_schema_with(IdEncoding::Transport)
}

fn attrs(parent: DataType, encoding: IdEncoding) -> Schema {
    Schema::new(vec![
        with_encoding(Field::new("parent_id", parent, false), encoding),
        Field::new("key", dict(DataType::UInt8), false),
        Field::new("type", DataType::UInt8, false),
        Field::new("str", dict(DataType::UInt16), true),
        Field::new("int", DataType::Int64, true),
        Field::new("double", DataType::Float64, true),
        Field::new("bool", DataType::Boolean, true),
    ])
}
/// Upstream `AttrsSchema16` without `bytes`/`ser`: `RESOURCE_ATTRS`,
/// `SCOPE_ATTRS`, `SPAN_ATTRS`, `LOG_ATTRS`.
pub fn attrs16_schema_with(encoding: IdEncoding) -> Schema {
    attrs(DataType::UInt16, encoding)
}
pub fn attrs16_schema() -> Schema {
    attrs16_schema_with(IdEncoding::Transport)
}
/// `SPAN_ATTRS` payload schema (the same as [`attrs16_schema`]).
pub fn span_attrs_schema() -> Schema {
    attrs16_schema()
}
/// Upstream `AttrsSchema32` without `bytes`/`ser`: `SPAN_EVENT_ATTRS`,
/// `SPAN_LINK_ATTRS`, `HISTOGRAM_DP_ATTRS`.
pub fn attrs32_schema_with(encoding: IdEncoding) -> Schema {
    attrs(DataType::UInt32, encoding)
}
pub fn attrs32_schema() -> Schema {
    attrs32_schema_with(IdEncoding::Transport)
}

/// `SPAN_EVENTS` payload schema.
pub fn span_events_schema_with(encoding: IdEncoding) -> Schema {
    Schema::new(vec![
        with_encoding(Field::new("id", DataType::UInt32, true), encoding),
        with_encoding(Field::new("parent_id", DataType::UInt16, false), encoding),
        Field::new("time_unix_nano", timestamp(), true),
        Field::new("name", dict(DataType::UInt8), false),
        Field::new("dropped_attributes_count", DataType::UInt32, true),
    ])
}
pub fn span_events_schema() -> Schema {
    span_events_schema_with(IdEncoding::Transport)
}

/// `SPAN_LINKS` payload schema.
pub fn span_links_schema_with(encoding: IdEncoding) -> Schema {
    Schema::new(vec![
        with_encoding(Field::new("id", DataType::UInt32, true), encoding),
        with_encoding(Field::new("parent_id", DataType::UInt16, false), encoding),
        Field::new("trace_id", DataType::FixedSizeBinary(16), false),
        Field::new("span_id", DataType::FixedSizeBinary(8), false),
        Field::new("trace_state", DataType::Utf8, true),
        Field::new("dropped_attributes_count", DataType::UInt32, true),
        Field::new("flags", DataType::UInt32, true),
    ])
}
pub fn span_links_schema() -> Schema {
    span_links_schema_with(IdEncoding::Transport)
}

/// `UNIVARIATE_METRICS` payload schema.
pub fn metrics_schema_with(encoding: IdEncoding) -> Schema {
    let [resource, scope, schema_url] = resource_scope(encoding);
    Schema::new(vec![
        with_encoding(Field::new("id", DataType::UInt16, false), encoding),
        resource,
        scope,
        schema_url,
        Field::new("metric_type", DataType::UInt8, false),
        Field::new("name", dict(DataType::UInt8), false),
        Field::new("description", dict(DataType::UInt8), true),
        Field::new("unit", dict(DataType::UInt8), true),
        Field::new("aggregation_temporality", DataType::Int32, true),
        Field::new("is_monotonic", DataType::Boolean, true),
    ])
}
pub fn metrics_schema() -> Schema {
    metrics_schema_with(IdEncoding::Transport)
}

/// `HISTOGRAM_DATA_POINTS` payload schema (no exemplars).
pub fn histogram_points_schema_with(encoding: IdEncoding) -> Schema {
    Schema::new(vec![
        with_encoding(Field::new("id", DataType::UInt32, true), encoding),
        with_encoding(Field::new("parent_id", DataType::UInt16, false), encoding),
        Field::new("start_time_unix_nano", timestamp(), true),
        Field::new("time_unix_nano", timestamp(), false),
        Field::new("count", DataType::UInt64, true),
        Field::new("sum", DataType::Float64, true),
        Field::new("bucket_counts", list_of(DataType::UInt64), true),
        Field::new("explicit_bounds", list_of(DataType::Float64), true),
        Field::new("flags", DataType::UInt32, true),
        Field::new("min", DataType::Float64, true),
        Field::new("max", DataType::Float64, true),
    ])
}
pub fn histogram_points_schema() -> Schema {
    histogram_points_schema_with(IdEncoding::Transport)
}

/// Children of the `LOGS` `body` struct (no `bytes`/`ser`).
pub(crate) fn body_fields() -> Fields {
    Fields::from(vec![
        Field::new("type", DataType::UInt8, true),
        Field::new("str", dict(DataType::UInt16), true),
        Field::new("int", DataType::Int64, true),
        Field::new("double", DataType::Float64, true),
        Field::new("bool", DataType::Boolean, true),
    ])
}

/// `LOGS` payload schema.
pub fn logs_schema_with(encoding: IdEncoding) -> Schema {
    let [resource, scope, schema_url] = resource_scope(encoding);
    Schema::new(vec![
        with_encoding(Field::new("id", DataType::UInt16, true), encoding),
        resource,
        scope,
        schema_url,
        Field::new("time_unix_nano", timestamp(), true),
        Field::new("observed_time_unix_nano", timestamp(), true),
        Field::new("trace_id", DataType::FixedSizeBinary(16), true),
        Field::new("span_id", DataType::FixedSizeBinary(8), true),
        Field::new("severity_number", DataType::Int32, true),
        Field::new("severity_text", dict(DataType::UInt8), true),
        Field::new("body", DataType::Struct(body_fields()), true),
        Field::new("dropped_attributes_count", DataType::UInt32, true),
        Field::new("flags", DataType::UInt32, true),
        Field::new("event_name", dict(DataType::UInt8), true),
    ])
}
pub fn logs_schema() -> Schema {
    logs_schema_with(IdEncoding::Transport)
}

/// OTLP `AnyValue` type codes used in the attribute `type` column
/// (pcommon.ValueType numbering).
pub mod value_type {
    pub const EMPTY: u8 = 0;
    pub const STR: u8 = 1;
    pub const INT: u8 = 2;
    pub const DOUBLE: u8 = 3;
    pub const BOOL: u8 = 4;
    pub const MAP: u8 = 5;
    pub const SLICE: u8 = 6;
    pub const BYTES: u8 = 7;
}
/// OTLP `metric_type` for an explicit-bucket histogram.
pub const METRIC_TYPE_HISTOGRAM: u8 = 3;
/// OTLP `AGGREGATION_TEMPORALITY_CUMULATIVE`.
pub const TEMPORALITY_CUMULATIVE: i32 = 2;
/// OTLP `Status.code` values.
pub const STATUS_CODE_OK: i32 = 1;
pub const STATUS_CODE_ERROR: i32 = 2;
