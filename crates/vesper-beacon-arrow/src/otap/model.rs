//! The OTLP data model Beacon exports beyond bare spans: resources,
//! instrumentation scopes, span kinds, W3C `tracestate`, span events and
//! links, and log records.
//!
//! Every string a producer supplies is `&'static str` vocabulary or a stored
//! [`AttributeValue`] (public, redacted or fingerprinted), as for span
//! attributes, so runtime input reaches an exporter only through the PII
//! boundary. The one exception is [`TraceState`]: W3C propagation metadata
//! received from a peer, validated against the `tracestate` grammar and
//! exported as is.

use super::attributes::{AttributeValue, Attributes, PiiPolicy};
use vesper_beacon::{Sensitive, Telemetrable};

/// The entity producing telemetry (OTLP `Resource`). `schema_url` is the
/// enclosing `ResourceSpans`/`ResourceMetrics`/`ResourceLogs` schema URL.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resource {
    pub attributes: Attributes,
    pub schema_url: Option<&'static str>,
    pub dropped_attributes_count: u32,
}

/// The instrumentation scope (OTLP `InstrumentationScope`). `schema_url` is
/// the enclosing `ScopeSpans`/`ScopeMetrics`/`ScopeLogs` schema URL.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    pub name: Option<&'static str>,
    pub version: Option<&'static str>,
    pub attributes: Attributes,
    pub dropped_attributes_count: u32,
    pub schema_url: Option<&'static str>,
}

/// Items of one scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopeGroup<T> {
    pub scope: Scope,
    pub items: Vec<T>,
}
/// Scopes of one resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceGroup<T> {
    pub resource: Resource,
    pub scopes: Vec<ScopeGroup<T>>,
}
impl<T> ResourceGroup<T> {
    /// One resource with one scope holding `items`.
    pub fn single(resource: Resource, scope: Scope, items: Vec<T>) -> Self {
        Self {
            resource,
            scopes: vec![ScopeGroup { scope, items }],
        }
    }
    pub fn item_count(&self) -> usize {
        self.scopes.iter().map(|s| s.items.len()).sum()
    }
}

/// OTLP `Span.SpanKind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SpanKind {
    #[default]
    Unspecified = 0,
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}
impl SpanKind {
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::Unspecified,
            1 => Self::Internal,
            2 => Self::Server,
            3 => Self::Client,
            4 => Self::Producer,
            5 => Self::Consumer,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceStateError {
    TooLong,
    TooManyMembers,
    EmptyMember,
    BadKey,
    BadValue,
    DuplicateKey,
}

/// A W3C Trace Context `tracestate` value: at most 32 `key=value` members
/// and 512 bytes, keys `[a-z0-9][a-z0-9_*/@-]*` (at most 256 bytes, at most
/// one `@`), values of printable ASCII without `,` and `=` that do not end
/// in a space (at most 256 bytes), no duplicate keys. Members are stored
/// without the optional whitespace around commas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceState(String);
impl TraceState {
    pub const MAX_BYTES: usize = 512;
    pub const MAX_MEMBERS: usize = 32;
    pub fn parse(raw: &str) -> Result<Self, TraceStateError> {
        if raw.len() > Self::MAX_BYTES {
            return Err(TraceStateError::TooLong);
        }
        let mut members: Vec<&str> = Vec::new();
        let mut keys: Vec<&str> = Vec::new();
        for member in raw.split(',') {
            let member = member.trim_matches([' ', '\t']);
            if member.is_empty() {
                // The grammar allows empty list elements; they carry nothing.
                continue;
            }
            let (key, value) = member.split_once('=').ok_or(TraceStateError::EmptyMember)?;
            if !valid_key(key) {
                return Err(TraceStateError::BadKey);
            }
            if !valid_value(value) {
                return Err(TraceStateError::BadValue);
            }
            if keys.contains(&key) {
                return Err(TraceStateError::DuplicateKey);
            }
            keys.push(key);
            members.push(member);
            if members.len() > Self::MAX_MEMBERS {
                return Err(TraceStateError::TooManyMembers);
            }
        }
        Ok(Self(members.join(",")))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
fn valid_key(key: &str) -> bool {
    let b = key.as_bytes();
    if b.is_empty() || b.len() > 256 || b.iter().filter(|c| **c == b'@').count() > 1 {
        return false;
    }
    let first = b[0];
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || matches!(c, b'_' | b'-' | b'*' | b'/' | b'@')
        })
        && !key.ends_with('@')
}
fn valid_value(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 256
        && b.iter()
            .all(|c| (0x20..=0x7e).contains(c) && *c != b',' && *c != b'=')
        && b[b.len() - 1] != b' '
}

/// A timestamped event inside a span (OTLP `Span.Event`).
#[derive(Clone, Debug, PartialEq)]
pub struct SpanEvent {
    pub name: &'static str,
    pub time_unix_ns: u64,
    pub attributes: Attributes,
    pub dropped_attributes_count: u32,
}
impl SpanEvent {
    pub fn new(name: &'static str, time_unix_ns: u64) -> Self {
        Self {
            name,
            time_unix_ns,
            attributes: Attributes::new(),
            dropped_attributes_count: 0,
        }
    }
}

/// A reference to another span (OTLP `Span.Link`).
#[derive(Clone, Debug, PartialEq)]
pub struct SpanLink {
    pub trace_id: u128,
    pub span_id: u64,
    pub trace_state: Option<TraceState>,
    /// W3C trace flags in bits 0-7, plus the OTLP `SPAN_FLAGS_CONTEXT_*`
    /// bits 8 and 9; `None` leaves the column null.
    pub flags: Option<u32>,
    pub attributes: Attributes,
    pub dropped_attributes_count: u32,
}
impl SpanLink {
    pub fn new(trace_id: u128, span_id: u64) -> Self {
        Self {
            trace_id,
            span_id,
            trace_state: None,
            flags: None,
            attributes: Attributes::new(),
            dropped_attributes_count: 0,
        }
    }
}

/// OTLP `SeverityNumber` (0 unspecified, 1-24 TRACE..FATAL4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Severity(u8);
impl Severity {
    pub const UNSPECIFIED: Self = Self(0);
    pub const TRACE: Self = Self(1);
    pub const DEBUG: Self = Self(5);
    pub const INFO: Self = Self(9);
    pub const WARN: Self = Self(13);
    pub const ERROR: Self = Self(17);
    pub const FATAL: Self = Self(21);
    pub fn new(number: u8) -> Option<Self> {
        (number <= 24).then_some(Self(number))
    }
    pub fn number(self) -> u8 {
        self.0
    }
}

/// One log record (OTLP `LogRecord`). The body is a stored attribute value,
/// so a sensitive body goes through a [`PiiPolicy`] like any attribute.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogRecord {
    pub time_unix_ns: Option<u64>,
    pub observed_time_unix_ns: Option<u64>,
    pub trace_id: Option<u128>,
    pub span_id: Option<u64>,
    pub severity: Severity,
    pub severity_text: Option<&'static str>,
    pub body: Option<AttributeValue>,
    pub attributes: Attributes,
    pub dropped_attributes_count: u32,
    /// W3C trace flags of the associated span (bits 0-7).
    pub flags: u32,
    pub event_name: Option<&'static str>,
}
impl LogRecord {
    pub fn set_body(&mut self, value: impl Telemetrable) -> &mut Self {
        self.body = Some(AttributeValue::public(value));
        self
    }
    /// Stores only the policy's output; the sensitive owner is dropped.
    pub fn set_body_pii(&mut self, value: impl Sensitive, policy: &PiiPolicy) -> &mut Self {
        self.body = Some(policy.apply(value));
        self
    }
}

/// A measurement value: OTLP `as_int` or `as_double`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumberValue {
    Int(i64),
    Double(f64),
}

/// A sampled measurement attached to a data point (OTLP `Exemplar`).
/// `filtered_attributes` go through the same PII boundary as every other
/// attribute set.
#[derive(Clone, Debug, PartialEq)]
pub struct Exemplar {
    pub time_unix_ns: u64,
    pub value: NumberValue,
    pub span_id: Option<u64>,
    pub trace_id: Option<u128>,
    pub filtered_attributes: Attributes,
}
impl Exemplar {
    pub fn new(time_unix_ns: u64, value: NumberValue) -> Self {
        Self {
            time_unix_ns,
            value,
            span_id: None,
            trace_id: None,
            filtered_attributes: Attributes::new(),
        }
    }
}

/// OTLP `AggregationTemporality`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Temporality {
    Delta = 1,
    #[default]
    Cumulative = 2,
}
impl Temporality {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            1 => Some(Self::Delta),
            2 => Some(Self::Cumulative),
            _ => None,
        }
    }
}

/// Whether a number point belongs to a gauge or a sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NumberKind {
    Gauge,
    Sum {
        monotonic: bool,
        temporality: Temporality,
    },
}

/// One gauge or sum data point (OTLP `NumberDataPoint`). Consecutive points
/// of one scope with the same name, unit, description and kind share one
/// metric row.
#[derive(Clone, Debug, PartialEq)]
pub struct NumberPoint {
    pub name: &'static str,
    pub unit: &'static str,
    pub description: Option<&'static str>,
    pub kind: NumberKind,
    pub start_unix_ns: Option<u64>,
    pub time_unix_ns: u64,
    pub value: NumberValue,
    pub attributes: Attributes,
    pub exemplars: Vec<Exemplar>,
    /// OTLP `DataPointFlags` (bit 0: no recorded value).
    pub flags: u32,
}
impl NumberPoint {
    pub fn gauge(
        name: &'static str,
        unit: &'static str,
        time_unix_ns: u64,
        value: NumberValue,
    ) -> Self {
        Self {
            name,
            unit,
            description: None,
            kind: NumberKind::Gauge,
            start_unix_ns: None,
            time_unix_ns,
            value,
            attributes: Attributes::new(),
            exemplars: Vec::new(),
            flags: 0,
        }
    }
    pub fn sum(
        name: &'static str,
        unit: &'static str,
        monotonic: bool,
        temporality: Temporality,
        start_unix_ns: u64,
        time_unix_ns: u64,
        value: NumberValue,
    ) -> Self {
        Self {
            kind: NumberKind::Sum {
                monotonic,
                temporality,
            },
            start_unix_ns: Some(start_unix_ns),
            ..Self::gauge(name, unit, time_unix_ns, value)
        }
    }
}

/// One side (positive or negative) of an exponential histogram.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpBuckets {
    pub offset: i32,
    pub counts: Vec<u64>,
}

/// One exponential histogram data point (OTLP
/// `ExponentialHistogramDataPoint`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExpHistogramPoint {
    pub name: &'static str,
    pub unit: &'static str,
    pub description: Option<&'static str>,
    pub temporality: Temporality,
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
    pub attributes: Attributes,
    pub exemplars: Vec<Exemplar>,
    pub flags: u32,
}
impl ExpHistogramPoint {
    /// A cumulative point from a Beacon [`ExponentialHistogram`]. `sum`,
    /// `min` and `max` are rounded to `f64` (exact below 2^53).
    ///
    /// [`ExponentialHistogram`]: vesper_beacon::ExponentialHistogram
    pub fn from_histogram(
        name: &'static str,
        unit: &'static str,
        start_unix_ns: u64,
        time_unix_ns: u64,
        h: &vesper_beacon::ExponentialHistogram,
    ) -> Self {
        Self {
            name,
            unit,
            description: None,
            temporality: Temporality::Cumulative,
            start_unix_ns: Some(start_unix_ns),
            time_unix_ns,
            count: h.total_count(),
            sum: Some(h.sum() as f64),
            scale: h.scale(),
            zero_count: h.zero_count(),
            zero_threshold: 0.0,
            positive: ExpBuckets {
                offset: h.offset(),
                counts: h.positive_counts().to_vec(),
            },
            negative: ExpBuckets::default(),
            min: h.min().map(|v| v as f64),
            max: h.max().map(|v| v as f64),
            attributes: Attributes::new(),
            exemplars: Vec::new(),
            flags: 0,
        }
    }
}
