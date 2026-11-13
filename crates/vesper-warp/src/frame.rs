//! RFC 9000 §19 frames: zero-copy parse and encode for the supported subset.
//!
//! Supported: PADDING, PING, ACK (with ranges and ECN counts), RESET_STREAM,
//! STOP_SENDING, CRYPTO, STREAM (all OFF/LEN/FIN variants), MAX_DATA,
//! MAX_STREAM_DATA, MAX_STREAMS, DATA_BLOCKED, STREAM_DATA_BLOCKED,
//! STREAMS_BLOCKED, CONNECTION_CLOSE (both variants) and HANDSHAKE_DONE. Data-carrying frames borrow their bytes
//! from the payload. Other RFC 9000 frame types are
//! [`FrameError::Unsupported`]; unregistered types are
//! [`FrameError::UnknownType`]; frames not permitted in the packet type
//! (RFC 9000 §12.4, Table 3) are [`FrameError::NotAllowed`].

use crate::varint::{self, Reader, VarIntError, Writer};
use crate::{PacketType, TransportErrorCode};

const PADDING: u64 = 0x00;
const PING: u64 = 0x01;
const ACK: u64 = 0x02;
const ACK_ECN: u64 = 0x03;
const RESET_STREAM: u64 = 0x04;
const STOP_SENDING: u64 = 0x05;
const CRYPTO: u64 = 0x06;
const STREAM: u64 = 0x08;
const STREAM_OFF: u64 = 0x04;
const STREAM_LEN: u64 = 0x02;
const STREAM_FIN: u64 = 0x01;
const MAX_DATA: u64 = 0x10;
const MAX_STREAM_DATA: u64 = 0x11;
const MAX_STREAMS_BIDI: u64 = 0x12;
const MAX_STREAMS_UNI: u64 = 0x13;
const DATA_BLOCKED: u64 = 0x14;
const STREAM_DATA_BLOCKED: u64 = 0x15;
const STREAMS_BLOCKED_BIDI: u64 = 0x16;
const STREAMS_BLOCKED_UNI: u64 = 0x17;

/// Largest stream count a MAX_STREAMS or STREAMS_BLOCKED frame may carry
/// (RFC 9000 §19.11: a stream ID must stay below 2^62).
pub const MAX_STREAM_COUNT: u64 = 1 << 60;
const CLOSE_TRANSPORT: u64 = 0x1c;
const CLOSE_APPLICATION: u64 = 0x1d;
const HANDSHAKE_DONE: u64 = 0x1e;

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

/// An inclusive range of acknowledged packet numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckRange {
    pub smallest: u64,
    pub largest: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EcnCounts {
    pub ect0: u64,
    pub ect1: u64,
    pub ce: u64,
}

/// ACK ranges either still in wire form (parsed and validated, decoded
/// lazily without allocation) or supplied by the caller for encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ranges<'a> {
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
    ranges: Ranges<'a>,
}

impl<'a> AckFrame<'a> {
    /// Builds an ACK frame from descending, disjoint, non-adjacent ranges.
    pub fn from_ranges(
        ranges: &'a [AckRange],
        ack_delay: u64,
        ecn: Option<EcnCounts>,
    ) -> Result<Self, FrameError> {
        let malformed = FrameError::Malformed { frame_type: ACK };
        let first = ranges.first().ok_or(malformed)?;
        for r in ranges {
            if r.smallest > r.largest || r.largest > varint::MAX {
                return Err(malformed);
            }
        }
        for pair in ranges.windows(2) {
            // A gap of at least one unacknowledged packet: the encoded Gap is
            // `prev.smallest - next.largest - 2`.
            if pair[1]
                .largest
                .checked_add(2)
                .is_none_or(|n| n > pair[0].smallest)
            {
                return Err(malformed);
            }
        }
        if ack_delay > varint::MAX {
            return Err(malformed);
        }
        Ok(Self {
            largest: first.largest,
            ack_delay,
            ecn,
            ranges: Ranges::Decoded(ranges),
        })
    }
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

impl Iterator for AckRangeIter<'_> {
    type Item = AckRange;
    fn next(&mut self) -> Option<AckRange> {
        match self {
            Self::Decoded(it) => it.next().copied(),
            Self::Wire {
                next_largest,
                first_range,
                remaining,
                reader,
            } => {
                // Validated at parse time; `?` only ends iteration.
                let largest = (*next_largest)?;
                let len = if let Some(first) = first_range.take() {
                    first
                } else {
                    if *remaining == 0 {
                        return None;
                    }
                    *remaining -= 1;
                    let gap = reader.varint().ok()?;
                    let len = reader.varint().ok()?;
                    let prev_smallest = largest;
                    let largest = prev_smallest.checked_sub(gap)?.checked_sub(2)?;
                    let smallest = largest.checked_sub(len)?;
                    *next_largest = Some(smallest);
                    return Some(AckRange { smallest, largest });
                };
                let smallest = largest.checked_sub(len)?;
                *next_largest = Some(smallest);
                Some(AckRange { smallest, largest })
            }
        }
    }
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
}

impl Frame<'_> {
    /// RFC 9002 §2: everything but ACK, PADDING and CONNECTION_CLOSE.
    pub const fn is_ack_eliciting(&self) -> bool {
        !matches!(
            self,
            Self::Padding { .. } | Self::Ack(_) | Self::ConnectionClose(_)
        )
    }
    /// The frame's wire type (the first type of a PADDING run).
    pub fn frame_type(&self) -> u64 {
        match self {
            Self::Padding { .. } => PADDING,
            Self::Ping => PING,
            Self::Ack(a) => {
                if a.ecn.is_some() {
                    ACK_ECN
                } else {
                    ACK
                }
            }
            Self::Crypto { .. } => CRYPTO,
            Self::Stream(s) => {
                STREAM
                    | if s.offset != 0 { STREAM_OFF } else { 0 }
                    | if s.has_length { STREAM_LEN } else { 0 }
                    | if s.fin { STREAM_FIN } else { 0 }
            }
            Self::MaxData(_) => MAX_DATA,
            Self::MaxStreamData { .. } => MAX_STREAM_DATA,
            Self::ResetStream { .. } => RESET_STREAM,
            Self::StopSending { .. } => STOP_SENDING,
            Self::MaxStreams { uni, .. } => {
                if *uni {
                    MAX_STREAMS_UNI
                } else {
                    MAX_STREAMS_BIDI
                }
            }
            Self::DataBlocked(_) => DATA_BLOCKED,
            Self::StreamDataBlocked { .. } => STREAM_DATA_BLOCKED,
            Self::StreamsBlocked { uni, .. } => {
                if *uni {
                    STREAMS_BLOCKED_UNI
                } else {
                    STREAMS_BLOCKED_BIDI
                }
            }
            Self::ConnectionClose(c) => {
                if c.frame_type.is_some() {
                    CLOSE_TRANSPORT
                } else {
                    CLOSE_APPLICATION
                }
            }
            Self::HandshakeDone => HANDSHAKE_DONE,
        }
    }

    /// Encodes the frame, returning the bytes written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, FrameError> {
        let mut w = Writer::new(out);
        self.encode_into(&mut w).map_err(|_| FrameError::Encode)?;
        Ok(w.position())
    }

    fn encode_into(&self, w: &mut Writer<'_>) -> Result<(), VarIntError> {
        match self {
            Self::Padding { len } => {
                for _ in 0..*len {
                    w.u8(0)?;
                }
                return Ok(());
            }
            _ => w.varint(self.frame_type())?,
        }
        match self {
            Self::Padding { .. } | Self::Ping | Self::HandshakeDone => {}
            Self::Ack(a) => {
                let mut ranges = a.ranges();
                let first = ranges.next().ok_or(VarIntError::BadLength(0))?;
                w.varint(a.largest)?;
                w.varint(a.ack_delay)?;
                w.varint(a.range_count() - 1)?;
                w.varint(first.largest - first.smallest)?;
                let mut prev_smallest = first.smallest;
                for r in ranges {
                    w.varint(prev_smallest - r.largest - 2)?;
                    w.varint(r.largest - r.smallest)?;
                    prev_smallest = r.smallest;
                }
                if let Some(ecn) = a.ecn {
                    w.varint(ecn.ect0)?;
                    w.varint(ecn.ect1)?;
                    w.varint(ecn.ce)?;
                }
            }
            Self::Crypto { offset, data } => {
                check_end(*offset, data.len()).ok_or(VarIntError::BadLength(0))?;
                w.varint(*offset)?;
                w.varint(data.len() as u64)?;
                w.bytes(data)?;
            }
            Self::Stream(s) => {
                check_end(s.offset, s.data.len()).ok_or(VarIntError::BadLength(0))?;
                w.varint(s.stream_id)?;
                if s.offset != 0 {
                    w.varint(s.offset)?;
                }
                if s.has_length {
                    w.varint(s.data.len() as u64)?;
                }
                w.bytes(s.data)?;
            }
            Self::MaxData(max) => w.varint(*max)?,
            Self::MaxStreamData { stream_id, max } => {
                w.varint(*stream_id)?;
                w.varint(*max)?;
            }
            Self::ResetStream {
                stream_id,
                error_code,
                final_size,
            } => {
                w.varint(*stream_id)?;
                w.varint(*error_code)?;
                w.varint(*final_size)?;
            }
            Self::StopSending {
                stream_id,
                error_code,
            } => {
                w.varint(*stream_id)?;
                w.varint(*error_code)?;
            }
            Self::MaxStreams { max: n, .. } | Self::StreamsBlocked { limit: n, .. } => {
                if *n > MAX_STREAM_COUNT {
                    return Err(VarIntError::BadLength(0));
                }
                w.varint(*n)?;
            }
            Self::DataBlocked(limit) => w.varint(*limit)?,
            Self::StreamDataBlocked { stream_id, limit } => {
                w.varint(*stream_id)?;
                w.varint(*limit)?;
            }
            Self::ConnectionClose(c) => {
                w.varint(c.error_code)?;
                if let Some(ty) = c.frame_type {
                    w.varint(ty)?;
                }
                w.varint(c.reason.len() as u64)?;
                w.bytes(c.reason)?;
            }
        }
        Ok(())
    }
}

/// Writes the header of a STREAM frame (type with LEN, and OFF when the
/// offset is non-zero) whose `len` data bytes the caller copies right
/// after it; returns the header length. Lets data come from a ring buffer
/// in two pieces.
pub fn encode_stream_header(
    out: &mut [u8],
    stream_id: u64,
    offset: u64,
    len: usize,
    fin: bool,
) -> Result<usize, FrameError> {
    check_end(offset, len).ok_or(FrameError::Encode)?;
    let ty = STREAM
        | STREAM_LEN
        | if offset != 0 { STREAM_OFF } else { 0 }
        | if fin { STREAM_FIN } else { 0 };
    let mut w = Writer::new(out);
    let res: Result<(), VarIntError> = (|| {
        w.varint(ty)?;
        w.varint(stream_id)?;
        if offset != 0 {
            w.varint(offset)?;
        }
        w.varint(len as u64)
    })();
    res.map_err(|_| FrameError::Encode)?;
    Ok(w.position())
}

/// Like [`encode_stream_header`] for a CRYPTO frame.
pub fn encode_crypto_header(out: &mut [u8], offset: u64, len: usize) -> Result<usize, FrameError> {
    check_end(offset, len).ok_or(FrameError::Encode)?;
    let mut w = Writer::new(out);
    let res: Result<(), VarIntError> = (|| {
        w.varint(CRYPTO)?;
        w.varint(offset)?;
        w.varint(len as u64)
    })();
    res.map_err(|_| FrameError::Encode)?;
    Ok(w.position())
}

/// Bytes a STREAM frame header from [`encode_stream_header`] takes.
pub fn stream_header_len(stream_id: u64, offset: u64, len: usize) -> usize {
    let vl = |v: u64| varint::encoded_len(v).unwrap_or(8);
    1 + vl(stream_id) + if offset != 0 { vl(offset) } else { 0 } + vl(len as u64)
}

/// Bytes a CRYPTO frame header from [`encode_crypto_header`] takes.
pub fn crypto_header_len(offset: u64, len: usize) -> usize {
    let vl = |v: u64| varint::encoded_len(v).unwrap_or(8);
    1 + vl(offset) + vl(len as u64)
}

fn check_end(offset: u64, len: usize) -> Option<u64> {
    offset
        .checked_add(u64::try_from(len).ok()?)
        .filter(|end| *end <= varint::MAX)
}

/// Table 3 permissions.
pub const fn allowed(frame_type: u64, packet_type: PacketType) -> bool {
    use PacketType::*;
    match frame_type {
        PADDING | PING => true,
        ACK | ACK_ECN | CRYPTO => !matches!(packet_type, ZeroRtt),
        CLOSE_TRANSPORT => true,
        RESET_STREAM
        | STOP_SENDING
        | 0x08..=0x0f
        | MAX_DATA..=STREAMS_BLOCKED_UNI
        | CLOSE_APPLICATION => {
            matches!(packet_type, ZeroRtt | OneRtt)
        }
        HANDSHAKE_DONE => matches!(packet_type, OneRtt),
        _ => false,
    }
}

const fn is_registered_unsupported(frame_type: u64) -> bool {
    matches!(frame_type, 0x07 | 0x18..=0x1b)
}

/// Parses one frame from the front of `buf`; returns it and its length.
pub fn parse_frame(buf: &[u8], packet_type: PacketType) -> Result<(Frame<'_>, usize), FrameError> {
    let mut r = Reader::new(buf);
    let frame = parse_one(&mut r, packet_type)?;
    Ok((frame, r.position()))
}

fn parse_one<'a>(r: &mut Reader<'a>, packet_type: PacketType) -> Result<Frame<'a>, FrameError> {
    let start = r.position();
    let ty = r
        .varint()
        .map_err(|_| FrameError::Truncated { frame_type: 0 })?;
    let type_len = r.position() - start;
    if type_len != varint::encoded_len(ty).unwrap_or(8) {
        return Err(FrameError::NonMinimalType(ty));
    }
    if ty > HANDSHAKE_DONE {
        return Err(FrameError::UnknownType(ty));
    }
    if is_registered_unsupported(ty) {
        return Err(FrameError::Unsupported(ty));
    }
    if !allowed(ty, packet_type) {
        return Err(FrameError::NotAllowed {
            frame_type: ty,
            packet_type,
        });
    }
    let truncated = FrameError::Truncated { frame_type: ty };
    let malformed = FrameError::Malformed { frame_type: ty };
    let frame = match ty {
        PADDING => {
            let mut len = 1;
            while r.remaining().first() == Some(&0) {
                r.u8();
                len += 1;
            }
            Frame::Padding { len }
        }
        PING => Frame::Ping,
        ACK | ACK_ECN => {
            let largest = vi(r, truncated)?;
            let ack_delay = vi(r, truncated)?;
            let count = vi(r, truncated)?;
            let first_range = vi(r, truncated)?;
            let mut smallest = largest.checked_sub(first_range).ok_or(malformed)?;
            let pairs_start = r.position();
            for _ in 0..count {
                let gap = vi(r, truncated)?;
                let len = vi(r, truncated)?;
                let next_largest = smallest
                    .checked_sub(gap)
                    .and_then(|v| v.checked_sub(2))
                    .ok_or(malformed)?;
                smallest = next_largest.checked_sub(len).ok_or(malformed)?;
            }
            let pairs = &r.full()[pairs_start..r.position()];
            let ecn = if ty == ACK_ECN {
                Some(EcnCounts {
                    ect0: vi(r, truncated)?,
                    ect1: vi(r, truncated)?,
                    ce: vi(r, truncated)?,
                })
            } else {
                None
            };
            Frame::Ack(AckFrame {
                largest,
                ack_delay,
                ecn,
                ranges: Ranges::Wire {
                    first_range,
                    count,
                    pairs,
                },
            })
        }
        CRYPTO => {
            let offset = vi(r, truncated)?;
            let len = vi(r, truncated)?;
            let data = take(r, len, truncated)?;
            check_end(offset, data.len()).ok_or(FrameError::OffsetOverflow { frame_type: ty })?;
            Frame::Crypto { offset, data }
        }
        0x08..=0x0f => {
            let stream_id = vi(r, truncated)?;
            let offset = if ty & STREAM_OFF != 0 {
                vi(r, truncated)?
            } else {
                0
            };
            let has_length = ty & STREAM_LEN != 0;
            let data = if has_length {
                let len = vi(r, truncated)?;
                take(r, len, truncated)?
            } else {
                let rest = r.remaining();
                r.bytes(rest.len()).ok_or(truncated)?
            };
            check_end(offset, data.len()).ok_or(FrameError::OffsetOverflow { frame_type: ty })?;
            Frame::Stream(StreamFrame {
                stream_id,
                offset,
                data,
                fin: ty & STREAM_FIN != 0,
                has_length,
            })
        }
        MAX_DATA => Frame::MaxData(vi(r, truncated)?),
        MAX_STREAM_DATA => Frame::MaxStreamData {
            stream_id: vi(r, truncated)?,
            max: vi(r, truncated)?,
        },
        RESET_STREAM => Frame::ResetStream {
            stream_id: vi(r, truncated)?,
            error_code: vi(r, truncated)?,
            final_size: vi(r, truncated)?,
        },
        STOP_SENDING => Frame::StopSending {
            stream_id: vi(r, truncated)?,
            error_code: vi(r, truncated)?,
        },
        MAX_STREAMS_BIDI | MAX_STREAMS_UNI | STREAMS_BLOCKED_BIDI | STREAMS_BLOCKED_UNI => {
            let n = vi(r, truncated)?;
            // RFC 9000 §19.11 and §19.14: a count above 2^60 is a
            // FRAME_ENCODING_ERROR.
            if n > MAX_STREAM_COUNT {
                return Err(malformed);
            }
            let uni = ty & 1 == 1;
            if ty <= MAX_STREAMS_UNI {
                Frame::MaxStreams { uni, max: n }
            } else {
                Frame::StreamsBlocked { uni, limit: n }
            }
        }
        DATA_BLOCKED => Frame::DataBlocked(vi(r, truncated)?),
        STREAM_DATA_BLOCKED => Frame::StreamDataBlocked {
            stream_id: vi(r, truncated)?,
            limit: vi(r, truncated)?,
        },
        CLOSE_TRANSPORT | CLOSE_APPLICATION => {
            let error_code = vi(r, truncated)?;
            let frame_type = if ty == CLOSE_TRANSPORT {
                Some(vi(r, truncated)?)
            } else {
                None
            };
            let len = vi(r, truncated)?;
            let reason = take(r, len, truncated)?;
            Frame::ConnectionClose(ConnectionClose {
                error_code,
                frame_type,
                reason,
            })
        }
        HANDSHAKE_DONE => Frame::HandshakeDone,
        _ => return Err(FrameError::UnknownType(ty)),
    };
    Ok(frame)
}

fn vi(r: &mut Reader<'_>, err: FrameError) -> Result<u64, FrameError> {
    r.varint().map_err(|_| err)
}

fn take<'a>(r: &mut Reader<'a>, len: u64, err: FrameError) -> Result<&'a [u8], FrameError> {
    let len = usize::try_from(len).map_err(|_| err)?;
    r.bytes(len).ok_or(err)
}

/// Iterates the frames of an opened packet payload. Stops after the first
/// error. Use [`scan_payload`] to also enforce the non-empty rule.
#[derive(Debug, Clone)]
pub struct Frames<'a> {
    reader: Reader<'a>,
    packet_type: PacketType,
    done: bool,
}

pub fn frames(payload: &[u8], packet_type: PacketType) -> Frames<'_> {
    Frames {
        reader: Reader::new(payload),
        packet_type,
        done: false,
    }
}

impl<'a> Iterator for Frames<'a> {
    type Item = Result<Frame<'a>, FrameError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.reader.is_empty() {
            return None;
        }
        let result = parse_one(&mut self.reader, self.packet_type);
        self.done = result.is_err();
        Some(result)
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
