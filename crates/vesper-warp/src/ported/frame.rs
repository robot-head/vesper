//! [`crate::frame`]'s ported functions, forwarded to `vesper/src/frame.ves`.
//! A parsed frame comes back with spans of the input, turned back into
//! slices here; an encoded frame passes its byte fields beside its words.

use super::binding::{self as b, repr};
use super::packet_type_to;
use crate::PacketType;
use crate::frame::{
    AckFrame, AckRange, AckRangeIter, ConnectionClose, EcnCounts, Frame, FrameError, Frames,
    RESET_TOKEN_LEN, Ranges, StreamFrame,
};
use crate::varint::Reader;

// The mirror is the crate's `AckRange` in this build; the Rust struct is
// `Eq` as well.
impl Eq for repr::AckRange {}

pub(crate) fn frame_error(e: b::FrameErr) -> FrameError {
    use b::FrameErr as F;
    match e {
        F::Truncated { frame_type } => FrameError::Truncated { frame_type },
        F::Malformed { frame_type } => FrameError::Malformed { frame_type },
        F::OffsetOverflow { frame_type } => FrameError::OffsetOverflow { frame_type },
        F::NonMinimalType { ty } => FrameError::NonMinimalType(ty),
        F::UnknownType { ty } => FrameError::UnknownType(ty),
        F::NotAllowed {
            frame_type,
            packet_type,
        } => FrameError::NotAllowed {
            frame_type,
            packet_type: match packet_type {
                b::PktType::Initial => PacketType::Initial,
                b::PktType::ZeroRtt => PacketType::ZeroRtt,
                b::PktType::Handshake => PacketType::Handshake,
                b::PktType::OneRtt => PacketType::OneRtt,
            },
        },
        F::EmptyPayload => FrameError::EmptyPayload,
        F::Encode => FrameError::Encode,
    }
}

fn len_span(bytes: &[u8]) -> b::Sp {
    b::Sp {
        start: 0,
        len: bytes.len() as u64,
    }
}

/// The frame's words and its byte fields (`a`, `b`) and caller ACK ranges.
fn words<'f>(f: &Frame<'f>) -> (b::FrameW, &'f [u8], &'f [u8], &'f [AckRange]) {
    use b::FrameW as W;
    let none: (&[u8], &[u8], &[AckRange]) = (&[], &[], &[]);
    let (w, (a, rb, ranges)) = match *f {
        Frame::Padding { len } => (W::Padding { len: len as u64 }, none),
        Frame::Ping => (W::Ping, none),
        Frame::Ack(ack) => {
            let ecn = ack.ecn.unwrap_or_default();
            let (decoded, first_range, count, pairs, ranges): (bool, u64, u64, &[u8], &[AckRange]) =
                match ack.ranges {
                    Ranges::Wire {
                        first_range,
                        count,
                        pairs,
                    } => (false, first_range, count, pairs, &[]),
                    Ranges::Decoded(r) => (true, 0, 0, &[], r),
                };
            (
                W::Ack {
                    largest: ack.largest,
                    ack_delay: ack.ack_delay,
                    has_ecn: ack.ecn.is_some(),
                    ect0: ecn.ect0,
                    ect1: ecn.ect1,
                    ce: ecn.ce,
                    decoded,
                    first_range,
                    count,
                    pairs: len_span(pairs),
                },
                (pairs, &[][..], ranges),
            )
        }
        Frame::Crypto { offset, data } => (
            W::Crypto {
                offset,
                data: len_span(data),
            },
            (data, &[][..], &[][..]),
        ),
        Frame::Stream(s) => (
            W::Stream {
                stream_id: s.stream_id,
                offset: s.offset,
                data: len_span(s.data),
                fin: s.fin,
                has_length: s.has_length,
            },
            (s.data, &[][..], &[][..]),
        ),
        Frame::MaxData(max) => (W::MaxData { max }, none),
        Frame::MaxStreamData { stream_id, max } => (W::MaxStreamData { stream_id, max }, none),
        Frame::ResetStream {
            stream_id,
            error_code,
            final_size,
        } => (
            W::ResetStream {
                stream_id,
                error_code,
                final_size,
            },
            none,
        ),
        Frame::StopSending {
            stream_id,
            error_code,
        } => (
            W::StopSending {
                stream_id,
                error_code,
            },
            none,
        ),
        Frame::MaxStreams { uni, max } => (W::MaxStreams { uni, max }, none),
        Frame::DataBlocked(limit) => (W::DataBlocked { limit }, none),
        Frame::StreamDataBlocked { stream_id, limit } => {
            (W::StreamDataBlocked { stream_id, limit }, none)
        }
        Frame::StreamsBlocked { uni, limit } => (W::StreamsBlocked { uni, limit }, none),
        Frame::ConnectionClose(c) => (
            W::ConnectionClose {
                error_code: c.error_code,
                has_frame_type: c.frame_type.is_some(),
                frame_type: c.frame_type.unwrap_or(0),
                reason: len_span(c.reason),
            },
            (c.reason, &[][..], &[][..]),
        ),
        Frame::HandshakeDone => (W::HandshakeDone, none),
        Frame::NewToken { token } => (
            W::NewToken {
                token: len_span(token),
            },
            (token, &[][..], &[][..]),
        ),
        Frame::NewConnectionId {
            seq,
            retire_prior_to,
            cid,
            reset_token,
        } => (
            W::NewConnectionId {
                seq,
                retire_prior_to,
                cid: len_span(cid),
                reset_token: 0,
            },
            (cid, &reset_token[..], &[][..]),
        ),
        Frame::RetireConnectionId { seq } => (W::RetireConnectionId { seq }, none),
        Frame::PathChallenge(d) => (
            W::PathChallenge {
                data: u64::from_be_bytes(d),
            },
            none,
        ),
        Frame::PathResponse(d) => (
            W::PathResponse {
                data: u64::from_be_bytes(d),
            },
            none,
        ),
    };
    (w, a, rb, ranges)
}

fn bytes(buf: &[u8], s: b::Sp) -> &[u8] {
    &buf[s.start as usize..(s.start + s.len) as usize]
}

/// The crate's frame for the words of a frame parsed from `buf`.
fn frame_of(buf: &[u8], w: b::FrameW) -> Result<Frame<'_>, FrameError> {
    use b::FrameW as W;
    Ok(match w {
        W::Padding { len } => Frame::Padding { len: len as usize },
        W::Ping => Frame::Ping,
        W::Ack {
            largest,
            ack_delay,
            has_ecn,
            ect0,
            ect1,
            ce,
            decoded: _,
            first_range,
            count,
            pairs,
        } => Frame::Ack(AckFrame {
            largest,
            ack_delay,
            ecn: has_ecn.then_some(EcnCounts { ect0, ect1, ce }),
            ranges: Ranges::Wire {
                first_range,
                count,
                pairs: bytes(buf, pairs),
            },
        }),
        W::Crypto { offset, data } => Frame::Crypto {
            offset,
            data: bytes(buf, data),
        },
        W::Stream {
            stream_id,
            offset,
            data,
            fin,
            has_length,
        } => Frame::Stream(StreamFrame {
            stream_id,
            offset,
            data: bytes(buf, data),
            fin,
            has_length,
        }),
        W::MaxData { max } => Frame::MaxData(max),
        W::MaxStreamData { stream_id, max } => Frame::MaxStreamData { stream_id, max },
        W::ResetStream {
            stream_id,
            error_code,
            final_size,
        } => Frame::ResetStream {
            stream_id,
            error_code,
            final_size,
        },
        W::StopSending {
            stream_id,
            error_code,
        } => Frame::StopSending {
            stream_id,
            error_code,
        },
        W::MaxStreams { uni, max } => Frame::MaxStreams { uni, max },
        W::DataBlocked { limit } => Frame::DataBlocked(limit),
        W::StreamDataBlocked { stream_id, limit } => Frame::StreamDataBlocked { stream_id, limit },
        W::StreamsBlocked { uni, limit } => Frame::StreamsBlocked { uni, limit },
        W::ConnectionClose {
            error_code,
            has_frame_type,
            frame_type,
            reason,
        } => Frame::ConnectionClose(ConnectionClose {
            error_code,
            frame_type: has_frame_type.then_some(frame_type),
            reason: bytes(buf, reason),
        }),
        W::HandshakeDone => Frame::HandshakeDone,
        W::NewToken { token } => Frame::NewToken {
            token: bytes(buf, token),
        },
        W::NewConnectionId {
            seq,
            retire_prior_to,
            cid,
            reset_token,
        } => {
            let at = reset_token as usize;
            Frame::NewConnectionId {
                seq,
                retire_prior_to,
                cid: bytes(buf, cid),
                reset_token: buf[at..at + RESET_TOKEN_LEN].try_into().map_err(|_| {
                    FrameError::Truncated {
                        frame_type: crate::frame::NEW_CONNECTION_ID,
                    }
                })?,
            }
        }
        W::RetireConnectionId { seq } => Frame::RetireConnectionId { seq },
        W::PathChallenge { data } => Frame::PathChallenge(data.to_be_bytes()),
        W::PathResponse { data } => Frame::PathResponse(data.to_be_bytes()),
    })
}

/// Parses the frame at `buf[at..]`, returning it and the position after it.
fn parse_at(
    buf: &[u8],
    at: usize,
    packet_type: PacketType,
) -> Result<(Frame<'_>, usize), FrameError> {
    let parsed =
        b::frame_parse_at(buf, at as u64, packet_type_to(packet_type)).map_err(frame_error)?;
    Ok((frame_of(buf, parsed.f.get())?, parsed.end as usize))
}

impl<'a> AckFrame<'a> {
    /// Builds an ACK frame from descending, disjoint, non-adjacent ranges.
    pub fn from_ranges(
        ranges: &'a [AckRange],
        ack_delay: u64,
        ecn: Option<EcnCounts>,
    ) -> Result<Self, FrameError> {
        b::frame_check_ranges(ranges, ack_delay).map_err(frame_error)?;
        Ok(Self {
            largest: ranges[0].largest,
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
                // Validated at parse time; `None` only ends iteration.
                let largest = (*next_largest)?;
                if let Some(first) = first_range.take() {
                    let smallest = largest.checked_sub(first)?;
                    *next_largest = Some(smallest);
                    return Some(AckRange { smallest, largest });
                }
                if *remaining == 0 {
                    return None;
                }
                *remaining -= 1;
                let step = b::frame_wire_next(reader.full(), reader.position() as u64, largest)?;
                *reader = advanced(reader.full(), step.next as usize);
                *next_largest = Some(step.smallest);
                Some(AckRange {
                    smallest: step.smallest,
                    largest: step.largest,
                })
            }
        }
    }
}

/// A reader over `buf` positioned at `pos`.
fn advanced(buf: &[u8], pos: usize) -> Reader<'_> {
    let mut r = Reader::new(buf);
    let _ = r.bytes(pos);
    r
}

/// The frame's kind (its variant index) and the bits its wire type adds
/// (see `fr_kind_type` in vesper/src/frame.ves).
fn kind(f: &Frame<'_>) -> (u64, u64) {
    match f {
        Frame::Padding { .. } => (0, 0),
        Frame::Ping => (1, 0),
        Frame::Ack(a) => (2, u64::from(a.ecn.is_some())),
        Frame::Crypto { .. } => (3, 0),
        Frame::Stream(s) => (
            4,
            u64::from(s.fin) | u64::from(s.has_length) << 1 | u64::from(s.offset != 0) << 2,
        ),
        Frame::MaxData(_) => (5, 0),
        Frame::MaxStreamData { .. } => (6, 0),
        Frame::ResetStream { .. } => (7, 0),
        Frame::StopSending { .. } => (8, 0),
        Frame::MaxStreams { uni, .. } => (9, u64::from(*uni)),
        Frame::DataBlocked(_) => (10, 0),
        Frame::StreamDataBlocked { .. } => (11, 0),
        Frame::StreamsBlocked { uni, .. } => (12, u64::from(*uni)),
        Frame::ConnectionClose(c) => (13, u64::from(c.frame_type.is_some())),
        Frame::HandshakeDone => (14, 0),
        Frame::NewToken { .. } => (15, 0),
        Frame::NewConnectionId { .. } => (16, 0),
        Frame::RetireConnectionId { .. } => (17, 0),
        Frame::PathChallenge(_) => (18, 0),
        Frame::PathResponse(_) => (19, 0),
    }
}

impl Frame<'_> {
    /// RFC 9002 §2: everything but ACK, PADDING and CONNECTION_CLOSE.
    pub fn is_ack_eliciting(&self) -> bool {
        b::frame_is_ack_eliciting(kind(self).0)
    }
    /// The frame's wire type (the first type of a PADDING run).
    pub fn frame_type(&self) -> u64 {
        let (kind, bits) = kind(self);
        b::frame_type(kind, bits)
    }

    /// RFC 9000 §9.1 probing frames: a packet with nothing else is a
    /// probing packet and does not move the connection to its path.
    pub fn is_probing(&self) -> bool {
        b::frame_is_probing(kind(self).0)
    }

    /// Encodes the frame, returning the bytes written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, FrameError> {
        let (w, a, rb, ranges) = words(self);
        b::frame_encode(out, w, a, rb, ranges)
            .map(|n| n as usize)
            .map_err(frame_error)
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
    b::frame_encode_stream_header(out, stream_id, offset, len as u64, fin)
        .map(|n| n as usize)
        .map_err(frame_error)
}

/// Like [`encode_stream_header`] for a CRYPTO frame.
pub fn encode_crypto_header(out: &mut [u8], offset: u64, len: usize) -> Result<usize, FrameError> {
    b::frame_encode_crypto_header(out, offset, len as u64)
        .map(|n| n as usize)
        .map_err(frame_error)
}

/// Bytes a STREAM frame header from [`encode_stream_header`] takes.
pub fn stream_header_len(stream_id: u64, offset: u64, len: usize) -> usize {
    b::frame_stream_header_len(stream_id, offset, len as u64) as usize
}

/// Bytes a CRYPTO frame header from [`encode_crypto_header`] takes.
pub fn crypto_header_len(offset: u64, len: usize) -> usize {
    b::frame_crypto_header_len(offset, len as u64) as usize
}

/// Table 3 permissions.
pub fn allowed(frame_type: u64, packet_type: PacketType) -> bool {
    b::frame_allowed(frame_type, packet_type_to(packet_type))
}

/// Parses one frame from the front of `buf`; returns it and its length.
pub fn parse_frame(buf: &[u8], packet_type: PacketType) -> Result<(Frame<'_>, usize), FrameError> {
    parse_at(buf, 0, packet_type)
}

impl<'a> Iterator for Frames<'a> {
    type Item = Result<Frame<'a>, FrameError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.reader.is_empty() {
            return None;
        }
        let buf = self.reader.full();
        let result = parse_at(buf, self.reader.position(), self.packet_type).map(|(f, end)| {
            self.reader = advanced(buf, end);
            f
        });
        self.done = result.is_err();
        Some(result)
    }
}
