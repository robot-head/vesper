//! Messages of `opentelemetry.proto.experimental.arrow.v1` (the OTel Arrow
//! protocol service file `arrow_service.proto`), written by hand with
//! `prost` derives so no `protoc` is needed at build time. Field numbers,
//! types and enum values follow the upstream file; this module carries no
//! gRPC service stubs.

/// One message of the `ArrowTraces`/`ArrowMetrics`/`ArrowLogs` bidirectional
/// stream: a batch id and the Arrow IPC bytes of each payload stream.
#[derive(Clone, PartialEq, prost::Message)]
pub struct BatchArrowRecords {
    #[prost(int64, tag = "1")]
    pub batch_id: i64,
    #[prost(message, repeated, tag = "2")]
    pub arrow_payloads: Vec<ArrowPayload>,
    /// HPACK-encoded headers in upstream; Beacon always sends it empty and
    /// rejects nonempty headers on receipt (no HPACK decoder is provided).
    #[prost(bytes = "vec", tag = "3")]
    pub headers: Vec<u8>,
}

/// The next bytes of one Arrow IPC stream. `schema_id` names the stream:
/// a new id starts a new IPC stream (schema reset); the same id continues it.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ArrowPayload {
    #[prost(string, tag = "1")]
    pub schema_id: String,
    #[prost(enumeration = "ArrowPayloadType", tag = "2")]
    pub r#type: i32,
    #[prost(bytes = "vec", tag = "3")]
    pub record: Vec<u8>,
}

/// Acknowledgement returned on the response stream for one batch id.
#[derive(Clone, PartialEq, prost::Message)]
pub struct BatchStatus {
    #[prost(int64, tag = "1")]
    pub batch_id: i64,
    #[prost(enumeration = "StatusCode", tag = "2")]
    pub status_code: i32,
    #[prost(string, tag = "3")]
    pub status_message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ArrowPayloadType {
    Unknown = 0,
    ResourceAttrs = 1,
    ScopeAttrs = 2,
    UnivariateMetrics = 10,
    NumberDataPoints = 11,
    SummaryDataPoints = 12,
    HistogramDataPoints = 13,
    ExpHistogramDataPoints = 14,
    NumberDpAttrs = 15,
    SummaryDpAttrs = 16,
    HistogramDpAttrs = 17,
    ExpHistogramDpAttrs = 18,
    NumberDpExemplars = 19,
    HistogramDpExemplars = 20,
    ExpHistogramDpExemplars = 21,
    NumberDpExemplarAttrs = 22,
    HistogramDpExemplarAttrs = 23,
    ExpHistogramDpExemplarAttrs = 24,
    MultivariateMetrics = 25,
    MetricAttrs = 26,
    Logs = 30,
    LogAttrs = 31,
    Spans = 40,
    SpanAttrs = 41,
    SpanEvents = 42,
    SpanLinks = 43,
    SpanEventAttrs = 44,
    SpanLinkAttrs = 45,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum StatusCode {
    Ok = 0,
    Canceled = 1,
    InvalidArgument = 3,
    DeadlineExceeded = 4,
    PermissionDenied = 7,
    ResourceExhausted = 8,
    Aborted = 10,
    Internal = 13,
    Unavailable = 14,
    Unauthenticated = 16,
}

impl BatchStatus {
    pub fn ok(batch_id: i64) -> Self {
        Self {
            batch_id,
            status_code: StatusCode::Ok as i32,
            status_message: String::new(),
        }
    }
    pub fn is_ok(&self) -> bool {
        self.status_code == StatusCode::Ok as i32
    }
}
