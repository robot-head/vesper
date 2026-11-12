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
    pub fn new(trace_id: u128, span_id: u64, trace_flags: u8) -> Result<Self, TraceError> {
        if trace_id == 0 {
            return Err(TraceError::ZeroTraceId);
        }
        if span_id == 0 {
            return Err(TraceError::ZeroSpanId);
        }
        Ok(Self {
            trace_id,
            span_id,
            trace_flags,
        })
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
        let mut out = *b"00-00000000000000000000000000000000-0000000000000000-00";
        write_hex(self.trace_id, &mut out[3..35]);
        write_hex(u128::from(self.span_id), &mut out[36..52]);
        write_hex(u128::from(self.trace_flags), &mut out[53..55]);
        out
    }
}
fn nibble(byte: u8, offset: usize) -> Result<u8, TraceError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(TraceError::InvalidHex { offset }),
    }
}
fn hex(bytes: &[u8], start: usize) -> Result<u128, TraceError> {
    bytes.iter().enumerate().try_fold(0, |value, (i, byte)| {
        Ok((value << 4) | u128::from(nibble(*byte, start + i)?))
    })
}
fn write_hex(mut value: u128, bytes: &mut [u8]) {
    for byte in bytes.iter_mut().rev() {
        *byte = b"0123456789abcdef"[(value & 15) as usize];
        value >>= 4;
    }
}
/// Deterministic errors: length, separators left-to-right, version, trace ID, span ID, flags.
/// Version `ff` and all future versions are outside this v00-only API.
pub fn parse_traceparent_55(raw: &[u8]) -> Result<W3cSpanContext, TraceError> {
    if raw.len() != 55 {
        return Err(TraceError::InvalidLength { actual: raw.len() });
    }
    for offset in [2, 35, 52] {
        if raw[offset] != b'-' {
            return Err(TraceError::MissingHyphen { offset });
        }
    }
    let version = hex(&raw[..2], 0)? as u8;
    if version != 0 {
        return Err(TraceError::UnsupportedVersion { version });
    }
    let trace_id = hex(&raw[3..35], 3)?;
    if trace_id == 0 {
        return Err(TraceError::ZeroTraceId);
    }
    let span_id = hex(&raw[36..52], 36)? as u64;
    if span_id == 0 {
        return Err(TraceError::ZeroSpanId);
    }
    let flags = hex(&raw[53..55], 53)? as u8;
    W3cSpanContext::new(trace_id, span_id, flags)
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
