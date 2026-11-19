//! RFC 9000 §19 frames: zero-copy parse and encode for the supported subset.
//!
//! Supported: every RFC 9000 frame: PADDING, PING, ACK (with ranges and
//! ECN counts), RESET_STREAM, STOP_SENDING, CRYPTO, NEW_TOKEN, STREAM (all
//! OFF/LEN/FIN variants), MAX_DATA, MAX_STREAM_DATA, MAX_STREAMS,
//! DATA_BLOCKED, STREAM_DATA_BLOCKED, STREAMS_BLOCKED, NEW_CONNECTION_ID,
//! RETIRE_CONNECTION_ID, PATH_CHALLENGE, PATH_RESPONSE, CONNECTION_CLOSE
//! (both variants) and HANDSHAKE_DONE (the last six frame types joined in
//! wave six, issue #247). Data-carrying frames borrow their bytes from the
//! payload. [`FrameError::Unsupported`] is kept for API stability and no
//! longer produced; unregistered types are
//! [`FrameError::UnknownType`]; frames not permitted in the packet type
//! (RFC 9000 §12.4, Table 3) are [`FrameError::NotAllowed`].

// Constants, helpers and layouts that only the Rust implementation
// (src/reference/) reads.
#![cfg_attr(feature = "vesper-port", allow(dead_code))]

use crate::varint::Reader;
use crate::{PacketType, TransportErrorCode};

pub(crate) const PADDING: u64 = 0x00;
pub(crate) const PING: u64 = 0x01;
pub(crate) const ACK: u64 = 0x02;
pub(crate) const ACK_ECN: u64 = 0x03;
pub(crate) const RESET_STREAM: u64 = 0x04;
pub(crate) const STOP_SENDING: u64 = 0x05;
pub(crate) const CRYPTO: u64 = 0x06;
pub(crate) const NEW_TOKEN: u64 = 0x07;
pub(crate) const STREAM: u64 = 0x08;
pub(crate) const STREAM_OFF: u64 = 0x04;
pub(crate) const STREAM_LEN: u64 = 0x02;
pub(crate) const STREAM_FIN: u64 = 0x01;
pub(crate) const MAX_DATA: u64 = 0x10;
pub(crate) const MAX_STREAM_DATA: u64 = 0x11;
pub(crate) const MAX_STREAMS_BIDI: u64 = 0x12;
pub(crate) const MAX_STREAMS_UNI: u64 = 0x13;
pub(crate) const DATA_BLOCKED: u64 = 0x14;
pub(crate) const STREAM_DATA_BLOCKED: u64 = 0x15;
pub(crate) const STREAMS_BLOCKED_BIDI: u64 = 0x16;
pub(crate) const STREAMS_BLOCKED_UNI: u64 = 0x17;

/// Largest stream count a MAX_STREAMS or STREAMS_BLOCKED frame may carry
/// (RFC 9000 §19.11: a stream ID must stay below 2^62).
pub const MAX_STREAM_COUNT: u64 = 1 << 60;
pub(crate) const NEW_CONNECTION_ID: u64 = 0x18;
pub(crate) const RETIRE_CONNECTION_ID: u64 = 0x19;
pub(crate) const PATH_CHALLENGE: u64 = 0x1a;
pub(crate) const PATH_RESPONSE: u64 = 0x1b;
/// Stateless reset token length (RFC 9000 §10.3).
pub const RESET_TOKEN_LEN: usize = 16;
pub(crate) const CLOSE_TRANSPORT: u64 = 0x1c;
pub(crate) const CLOSE_APPLICATION: u64 = 0x1d;
pub(crate) const HANDSHAKE_DONE: u64 = 0x1e;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The frame ended early.
    Truncated { frame_type: u64 },
    /// A field value is malformed (an ACK range below zero, ...).
    Malformed { frame_type: u64 },
    /// `offset + length` exceeds 2^62 − 1.
    OffsetOverflow { frame_type: u64 },
    /// The frame type is not minimally encoded (RFC 9000 §12.4).
    NonMinimalType(u64),
    /// Not a frame type registered by RFC 9000.
    UnknownType(u64),
    /// An RFC 9000 frame this crate does not implement yet.
    Unsupported(u64),
    /// The frame may not appear in this packet type (Table 3).
    NotAllowed {
        frame_type: u64,
        packet_type: PacketType,
    },
    /// A packet payload must contain at least one frame (RFC 9000 §12.4).
    EmptyPayload,
    /// Encoding: output buffer too small, or a value out of range.
    Encode,
}

impl FrameError {
    pub const fn transport_code(&self) -> TransportErrorCode {
        match self {
            Self::NonMinimalType(_) | Self::NotAllowed { .. } | Self::EmptyPayload => {
                TransportErrorCode::ProtocolViolation
            }
            _ => TransportErrorCode::FrameEncodingError,
        }
    }
}

#[cfg(not(feature = "vesper-port"))]
/// An inclusive range of acknowledged packet numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckRange {
    pub smallest: u64,
    pub largest: u64,
}
/// An inclusive range of acknowledged packet numbers (the export ABI
/// mirror of the Vesper `AckRange`, so ranges are lent to Vesper in place).
#[cfg(feature = "vesper-port")]
pub use crate::ported::binding::repr::AckRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EcnCounts {
    pub ect0: u64,
    pub ect1: u64,
    pub ce: u64,
}

/// ACK ranges either still in wire form (parsed and validated, decoded
/// lazily without allocation) or supplied by the caller for encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ranges<'a> {
    /// Bytes of the `(gap, length)` pairs following First ACK Range.
    Wire {
        first_range: u64,
        count: u64,
        pairs: &'a [u8],
    },
    /// Descending, disjoint, non-adjacent ranges.
    Decoded(&'a [AckRange]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckFrame<'a> {
    pub largest: u64,
    /// Encoded ACK Delay (scaled by the peer's `ack_delay_exponent`).
    pub ack_delay: u64,
    pub ecn: Option<EcnCounts>,
    pub(crate) ranges: Ranges<'a>,
}

impl<'a> AckFrame<'a> {
    /// Number of ranges, including the first.
    pub fn range_count(&self) -> u64 {
        match self.ranges {
            Ranges::Wire { count, .. } => count + 1,
            Ranges::Decoded(r) => r.len() as u64,
        }
    }
    /// Ranges in descending packet-number order.
    pub fn ranges(&self) -> AckRangeIter<'a> {
        match self.ranges {
            Ranges::Wire {
                first_range,
                count,
                pairs,
            } => AckRangeIter::Wire {
                next_largest: Some(self.largest),
                first_range: Some(first_range),
                remaining: count,
                reader: Reader::new(pairs),
            },
            Ranges::Decoded(r) => AckRangeIter::Decoded(r.iter()),
        }
    }
    /// Smallest acknowledged packet number.
    pub fn smallest(&self) -> u64 {
        self.ranges().last().map_or(self.largest, |r| r.smallest)
    }
}

#[derive(Debug, Clone)]
pub enum AckRangeIter<'a> {
    Wire {
        next_largest: Option<u64>,
        first_range: Option<u64>,
        remaining: u64,
        reader: Reader<'a>,
    },
    Decoded(std::slice::Iter<'a, AckRange>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFrame<'a> {
    pub stream_id: u64,
    pub offset: u64,
    pub data: &'a [u8],
    pub fin: bool,
    /// Whether the Length field is present (when absent, the frame runs to
    /// the end of the packet and must be encoded last).
    pub has_length: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionClose<'a> {
    pub error_code: u64,
    /// `Some` for the transport variant (0x1c), `None` for application (0x1d).
    pub frame_type: Option<u64>,
    pub reason: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    /// A run of consecutive PADDING bytes.
    Padding {
        len: usize,
    },
    Ping,
    Ack(AckFrame<'a>),
    Crypto {
        offset: u64,
        data: &'a [u8],
    },
    Stream(StreamFrame<'a>),
    MaxData(u64),
    MaxStreamData {
        stream_id: u64,
        max: u64,
    },
    ResetStream {
        stream_id: u64,
        error_code: u64,
        final_size: u64,
    },
    StopSending {
        stream_id: u64,
        error_code: u64,
    },
    /// MAX_STREAMS: `uni` selects 0x13, else 0x12 (bidirectional).
    MaxStreams {
        uni: bool,
        max: u64,
    },
    DataBlocked(u64),
    StreamDataBlocked {
        stream_id: u64,
        limit: u64,
    },
    StreamsBlocked {
        uni: bool,
        limit: u64,
    },
    ConnectionClose(ConnectionClose<'a>),
    HandshakeDone,
    /// NEW_TOKEN (§19.7): an address-validation token for a future
    /// connection; never empty.
    NewToken {
        token: &'a [u8],
    },
    /// NEW_CONNECTION_ID (§19.15).
    NewConnectionId {
        seq: u64,
        retire_prior_to: u64,
        cid: &'a [u8],
        reset_token: &'a [u8; RESET_TOKEN_LEN],
    },
    /// RETIRE_CONNECTION_ID (§19.16).
    RetireConnectionId {
        seq: u64,
    },
    /// PATH_CHALLENGE (§19.17).
    PathChallenge([u8; 8]),
    /// PATH_RESPONSE (§19.18).
    PathResponse([u8; 8]),
}

pub use crate::imp::frame::{
    allowed, crypto_header_len, encode_crypto_header, encode_stream_header, parse_frame,
    stream_header_len,
};

/// Iterates the frames of an opened packet payload. Stops after the first
/// error. Use [`scan_payload`] to also enforce the non-empty rule.
#[derive(Debug, Clone)]
pub struct Frames<'a> {
    pub(crate) reader: Reader<'a>,
    pub(crate) packet_type: PacketType,
    pub(crate) done: bool,
}

pub fn frames(payload: &[u8], packet_type: PacketType) -> Frames<'_> {
    Frames {
        reader: Reader::new(payload),
        packet_type,
        done: false,
    }
}

/// What RFC 9002 needs to know about a received or sent payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PayloadInfo {
    pub frames: usize,
    pub ack_eliciting: bool,
    /// Ack-eliciting or containing PADDING (RFC 9002 §2 "in flight").
    pub in_flight: bool,
}

/// Validates every frame in `payload` and summarises it, calling `each` on
/// every frame. Allocation-free.
pub fn scan_payload<'a>(
    payload: &'a [u8],
    packet_type: PacketType,
    mut each: impl FnMut(&Frame<'a>),
) -> Result<PayloadInfo, FrameError> {
    let mut info = PayloadInfo::default();
    for frame in frames(payload, packet_type) {
        let frame = frame?;
        info.frames += 1;
        info.ack_eliciting |= frame.is_ack_eliciting();
        info.in_flight |= frame.is_ack_eliciting() || matches!(frame, Frame::Padding { .. });
        each(&frame);
    }
    if info.frames == 0 {
        return Err(FrameError::EmptyPayload);
    }
    Ok(info)
}
