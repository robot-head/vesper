//! Exactly the 55-byte v00 traceparent field value, without HTTP whitespace or list syntax.
//! Unknown flag bits are accepted and preserved as opaque bits, never interpreted.
//! `sampled` only interprets bit 0. No tracestate or future-version fallback is provided.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceError {
    InvalidLength { actual: usize },
    MissingHyphen { offset: usize },
    InvalidHex { offset: usize },
    UnsupportedVersion { version: u8 },
    ZeroTraceId,
    ZeroSpanId,
}

/// Validated nonzero trace/span IDs. Flags are an opaque byte except for sampled bit 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct W3cSpanContext {
    trace_id: u128,
    span_id: u64,
    trace_flags: u8,
}
impl W3cSpanContext {
    /// The port build validates in Vesper (`span_context_new`).
    pub fn new(trace_id: u128, span_id: u64, trace_flags: u8) -> Result<Self, TraceError> {
        crate::port::span_context_new(
            (trace_id >> 64) as u64,
            trace_id as u64,
            span_id,
            trace_flags,
        )
        .map(|context| crate::port::validated(&context))
    }
    /// A context whose IDs the port already validated.
    pub(crate) const fn from_validated(trace_id: u128, span_id: u64, trace_flags: u8) -> Self {
        Self {
            trace_id,
            span_id,
            trace_flags,
        }
    }
    pub const fn trace_id(self) -> u128 {
        self.trace_id
    }
    pub const fn span_id(self) -> u64 {
        self.span_id
    }
    pub const fn trace_flags(self) -> u8 {
        self.trace_flags
    }
    pub const fn sampled(self) -> bool {
        self.trace_flags & 1 != 0
    }
    /// Writes exactly 55 canonical lowercase ASCII bytes, with no allocation or terminator.
    pub fn to_traceparent(self) -> [u8; 55] {
        let mut out = [0; 55];
        crate::port::format_traceparent(&crate::port::context_mirror(self), &mut out);
        out
    }
}
/// Deterministic errors: length, separators left-to-right, version, trace ID, span ID, flags.
/// Version `ff` and all future versions are outside this v00-only API.
pub fn parse_traceparent_55(raw: &[u8]) -> Result<W3cSpanContext, TraceError> {
    crate::port::parse_traceparent_55(raw).map(|context| crate::port::validated(&context))
}

impl core::fmt::Display for TraceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidLength { actual } => {
                write!(f, "traceparent v00 requires 55 bytes, received {actual}")
            }
            Self::MissingHyphen { offset } => {
                write!(f, "traceparent missing hyphen at byte {offset}")
            }
            Self::InvalidHex { offset } => {
                write!(f, "traceparent requires lowercase hex at byte {offset}")
            }
            Self::UnsupportedVersion { version } => write!(
                f,
                "unsupported traceparent version {version:02x}; only 00 is supported"
            ),
            Self::ZeroTraceId => f.write_str("traceparent trace ID must be nonzero"),
            Self::ZeroSpanId => f.write_str("traceparent span ID must be nonzero"),
        }
    }
}
impl core::error::Error for TraceError {}
