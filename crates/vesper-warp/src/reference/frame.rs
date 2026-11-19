//! The Rust reference implementation of the ported functions of
//! [`crate::frame`], compiled without `vesper-port` (the Vesper build is
//! `vesper/src/frame.ves`). Frozen: bug fixes only (#1066).

use crate::PacketType;
use crate::frame::*;
use crate::varint::{self, Reader, VarIntError, Writer};

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
            Self::NewToken { .. } => NEW_TOKEN,
            Self::NewConnectionId { .. } => NEW_CONNECTION_ID,
            Self::RetireConnectionId { .. } => RETIRE_CONNECTION_ID,
            Self::PathChallenge(_) => PATH_CHALLENGE,
            Self::PathResponse(_) => PATH_RESPONSE,
        }
    }

    /// RFC 9000 §9.1 probing frames: a packet with nothing else is a
    /// probing packet and does not move the connection to its path.
    pub const fn is_probing(&self) -> bool {
        matches!(
            self,
            Self::Padding { .. }
                | Self::PathChallenge(_)
                | Self::PathResponse(_)
                | Self::NewConnectionId { .. }
        )
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
            Self::NewToken { token } => {
                if token.is_empty() {
                    return Err(VarIntError::BadLength(0));
                }
                w.varint(token.len() as u64)?;
                w.bytes(token)?;
            }
            Self::NewConnectionId {
                seq,
                retire_prior_to,
                cid,
                reset_token,
            } => {
                if retire_prior_to > seq || cid.is_empty() || cid.len() > 20 {
                    return Err(VarIntError::BadLength(0));
                }
                w.varint(*seq)?;
                w.varint(*retire_prior_to)?;
                w.u8(cid.len() as u8)?;
                w.bytes(cid)?;
                w.bytes(&reset_token[..])?;
            }
            Self::RetireConnectionId { seq } => w.varint(*seq)?,
            Self::PathChallenge(d) | Self::PathResponse(d) => w.bytes(d)?,
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
        NEW_TOKEN | PATH_RESPONSE | HANDSHAKE_DONE => matches!(packet_type, OneRtt),
        NEW_CONNECTION_ID | RETIRE_CONNECTION_ID | PATH_CHALLENGE => {
            matches!(packet_type, ZeroRtt | OneRtt)
        }
        RESET_STREAM
        | STOP_SENDING
        | 0x08..=0x0f
        | MAX_DATA..=STREAMS_BLOCKED_UNI
        | CLOSE_APPLICATION => {
            matches!(packet_type, ZeroRtt | OneRtt)
        }
        _ => false,
    }
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
        NEW_TOKEN => {
            let len = vi(r, truncated)?;
            if len == 0 {
                return Err(malformed);
            }
            Frame::NewToken {
                token: take(r, len, truncated)?,
            }
        }
        NEW_CONNECTION_ID => {
            let seq = vi(r, truncated)?;
            let retire_prior_to = vi(r, truncated)?;
            let len = r.u8().ok_or(truncated)?;
            // §19.15: a length outside 1..=20, or Retire Prior To above the
            // sequence number, is a FRAME_ENCODING_ERROR.
            if len == 0 || len > 20 || retire_prior_to > seq {
                return Err(malformed);
            }
            let cid = take(r, u64::from(len), truncated)?;
            let token = take(r, RESET_TOKEN_LEN as u64, truncated)?;
            Frame::NewConnectionId {
                seq,
                retire_prior_to,
                cid,
                reset_token: token.try_into().map_err(|_| truncated)?,
            }
        }
        RETIRE_CONNECTION_ID => Frame::RetireConnectionId {
            seq: vi(r, truncated)?,
        },
        PATH_CHALLENGE | PATH_RESPONSE => {
            let d: [u8; 8] = take(r, 8, truncated)?.try_into().map_err(|_| truncated)?;
            if ty == PATH_CHALLENGE {
                Frame::PathChallenge(d)
            } else {
                Frame::PathResponse(d)
            }
        }
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
