//! The 16-byte `ChromaTokenSpanWire` record (§43.2), little-endian, no padding:
//!
//! | offset | field            | type    |
//! |--------|------------------|---------|
//! | 0      | `byte_start`     | u32le   |
//! | 4      | `byte_len`       | u32le   |
//! | 8      | `token_class`    | u16le   |
//! | 10     | `semantic_flags` | u16le   |
//! | 12     | `hover_sym_id`   | u32le   |
//!
//! `hover_sym_id` is [`NO_HOVER`] when the span has no hover-table row.
use crate::{ChromaError, HighlightSpan, VesperTokenClass, flags};

/// Record size in bytes.
pub const RECORD_BYTES: usize = 16;
/// `hover_sym_id` of a span without a hover row.
pub const NO_HOVER: u32 = u32::MAX;
/// Field offsets, for layout tests and foreign readers.
pub const OFFSET_BYTE_START: usize = 0;
pub const OFFSET_BYTE_LEN: usize = 4;
pub const OFFSET_TOKEN_CLASS: usize = 8;
pub const OFFSET_SEMANTIC_FLAGS: usize = 10;
pub const OFFSET_HOVER_SYM_ID: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChromaTokenSpanWire {
    pub byte_start: u32,
    pub byte_len: u32,
    pub token_class: u16,
    pub semantic_flags: u16,
    pub hover_sym_id: u32,
}

impl ChromaTokenSpanWire {
    pub fn from_span(span: &HighlightSpan) -> Result<Self, ChromaError> {
        let overflow = ChromaError::WireFieldOverflow {
            start: span.start,
            end: span.end,
        };
        if span.end < span.start {
            return Err(overflow);
        }
        let byte_start = u32::try_from(span.start).map_err(|_| overflow.clone())?;
        let byte_len = u32::try_from(span.end - span.start).map_err(|_| overflow.clone())?;
        byte_start.checked_add(byte_len).ok_or(overflow)?;
        Ok(Self {
            byte_start,
            byte_len,
            token_class: span.class as u16,
            semantic_flags: span.semantic_flags(),
            hover_sym_id: span.hover_sym_id.unwrap_or(NO_HOVER),
        })
    }

    pub fn to_span(&self) -> Result<HighlightSpan, ChromaError> {
        let class = VesperTokenClass::from_u16(self.token_class)?;
        if self.semantic_flags & !flags::KNOWN != 0 {
            return Err(ChromaError::UnknownSemanticFlags {
                raw: self.semantic_flags,
            });
        }
        let start = self.byte_start as usize;
        let end =
            self.byte_start
                .checked_add(self.byte_len)
                .ok_or(ChromaError::WireFieldOverflow {
                    start,
                    end: usize::MAX,
                })? as usize;
        Ok(HighlightSpan {
            start,
            end,
            class,
            is_live_linear: self.semantic_flags & flags::LINEAR_UNDERLINE != 0,
            flags: self.semantic_flags & !flags::LINEAR_UNDERLINE,
            hover_sym_id: (self.hover_sym_id != NO_HOVER).then_some(self.hover_sym_id),
        })
    }

    pub fn to_bytes(&self) -> [u8; RECORD_BYTES] {
        let mut b = [0u8; RECORD_BYTES];
        b[OFFSET_BYTE_START..4].copy_from_slice(&self.byte_start.to_le_bytes());
        b[OFFSET_BYTE_LEN..8].copy_from_slice(&self.byte_len.to_le_bytes());
        b[OFFSET_TOKEN_CLASS..10].copy_from_slice(&self.token_class.to_le_bytes());
        b[OFFSET_SEMANTIC_FLAGS..12].copy_from_slice(&self.semantic_flags.to_le_bytes());
        b[OFFSET_HOVER_SYM_ID..16].copy_from_slice(&self.hover_sym_id.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; RECORD_BYTES]) -> Self {
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        Self {
            byte_start: u32_at(OFFSET_BYTE_START),
            byte_len: u32_at(OFFSET_BYTE_LEN),
            token_class: u16_at(OFFSET_TOKEN_CLASS),
            semantic_flags: u16_at(OFFSET_SEMANTIC_FLAGS),
            hover_sym_id: u32_at(OFFSET_HOVER_SYM_ID),
        }
    }
}

/// Encodes spans as concatenated 16-byte records.
pub fn encode(spans: &[HighlightSpan]) -> Result<Vec<u8>, ChromaError> {
    let mut out = Vec::with_capacity(spans.len() * RECORD_BYTES);
    encode_into(spans, &mut out)?;
    Ok(out)
}

/// Appends spans to `out` as 16-byte records. On error `out` is unchanged.
pub fn encode_into(spans: &[HighlightSpan], out: &mut Vec<u8>) -> Result<(), ChromaError> {
    let mark = out.len();
    out.reserve(spans.len() * RECORD_BYTES);
    for s in spans {
        match ChromaTokenSpanWire::from_span(s) {
            Ok(w) => out.extend_from_slice(&w.to_bytes()),
            Err(e) => {
                out.truncate(mark);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Decodes concatenated records, rejecting truncated buffers, unknown token
/// classes, unknown flag bits and overflowing ranges.
pub fn decode(bytes: &[u8]) -> Result<Vec<HighlightSpan>, ChromaError> {
    let (records, rest) = bytes.as_chunks::<RECORD_BYTES>();
    if !rest.is_empty() {
        return Err(ChromaError::InvalidWireLength { len: bytes.len() });
    }
    records
        .iter()
        .map(|rec| ChromaTokenSpanWire::from_bytes(rec).to_span())
        .collect()
}
