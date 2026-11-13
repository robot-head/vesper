//! HTTP/3 frames (RFC 9114 §7) and unidirectional stream types (§6.2).
//!
//! [`FrameReader`] is incremental: bytes are pushed as they arrive on a
//! stream and complete frames come out. DATA payloads are passed through in
//! chunks and never buffered whole; unknown and reserved frame types are
//! skipped without buffering (§9); every other frame's payload must fit
//! `max_payload`, checked from its length before any byte is retained.

use vesper_warp::varint;

use crate::error::Code;

pub const DATA: u64 = 0x00;
pub const HEADERS: u64 = 0x01;
pub const CANCEL_PUSH: u64 = 0x03;
pub const SETTINGS: u64 = 0x04;
pub const PUSH_PROMISE: u64 = 0x05;
pub const GOAWAY: u64 = 0x07;
pub const MAX_PUSH_ID: u64 = 0x0d;

/// Unidirectional stream types (§6.2, RFC 9204 §4.2).
pub const STREAM_CONTROL: u64 = 0x00;
pub const STREAM_PUSH: u64 = 0x01;
pub const STREAM_QPACK_ENCODER: u64 = 0x02;
pub const STREAM_QPACK_DECODER: u64 = 0x03;

/// SETTINGS identifiers (§7.2.4.1, RFC 9204 §5).
pub const SETTINGS_QPACK_MAX_TABLE_CAPACITY: u64 = 0x01;
pub const SETTINGS_MAX_FIELD_SECTION_SIZE: u64 = 0x06;
pub const SETTINGS_QPACK_BLOCKED_STREAMS: u64 = 0x07;

/// HTTP/2 frame types with no HTTP/3 meaning (§7.2.8): receiving one is
/// H3_FRAME_UNEXPECTED.
pub const fn is_http2_frame(ty: u64) -> bool {
    matches!(ty, 0x02 | 0x06 | 0x08 | 0x09)
}

/// HTTP/2 settings that must not appear (§7.2.4.1): H3_SETTINGS_ERROR.
pub const fn is_http2_setting(id: u64) -> bool {
    matches!(id, 0x00 | 0x02 | 0x03 | 0x04 | 0x05)
}

/// The reserved grease values `0x1f * N + 0x21` (§7.2.8, §6.2.3, §7.2.4.1).
pub const fn is_reserved(value: u64) -> bool {
    value >= 0x21 && (value - 0x21).is_multiple_of(0x1f)
}

/// A frame-level failure, with the code to close or reset with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Bad length, a frame truncated by the end of the stream, or a
    /// payload that does not parse (H3_FRAME_ERROR).
    Malformed,
    /// A payload longer than the reader accepts (H3_EXCESSIVE_LOAD).
    TooLarge { frame_type: u64, len: u64 },
    /// A reserved HTTP/2 frame type (H3_FRAME_UNEXPECTED).
    Http2Frame(u64),
    /// A duplicate or HTTP/2 setting (H3_SETTINGS_ERROR).
    BadSetting(u64),
}

impl FrameError {
    pub fn code(self) -> Code {
        match self {
            Self::Malformed => Code::FRAME_ERROR,
            Self::TooLarge { .. } => Code::EXCESSIVE_LOAD,
            Self::Http2Frame(_) => Code::FRAME_UNEXPECTED,
            Self::BadSetting(_) => Code::SETTINGS_ERROR,
        }
    }
}

/// Received settings, with RFC 9114 defaults for absent identifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub qpack_max_table_capacity: u64,
    pub qpack_blocked_streams: u64,
    /// `None`: unlimited (§7.2.4.1).
    pub max_field_section_size: Option<u64>,
}

impl Settings {
    /// Parses a SETTINGS payload. Unknown and reserved identifiers are
    /// ignored; HTTP/2 identifiers and duplicates are errors.
    pub fn parse(payload: &[u8]) -> Result<Self, FrameError> {
        let mut s = Self::default();
        let mut seen: Vec<u64> = Vec::new();
        let mut at = 0;
        while at < payload.len() {
            let (id, n) = varint::decode(&payload[at..]).map_err(|_| FrameError::Malformed)?;
            at += n;
            let (value, n) = varint::decode(&payload[at..]).map_err(|_| FrameError::Malformed)?;
            at += n;
            if is_http2_setting(id) || seen.contains(&id) {
                return Err(FrameError::BadSetting(id));
            }
            // Bounded: each pair is at least two bytes of a bounded payload.
            seen.push(id);
            match id {
                SETTINGS_QPACK_MAX_TABLE_CAPACITY => s.qpack_max_table_capacity = value,
                SETTINGS_QPACK_BLOCKED_STREAMS => s.qpack_blocked_streams = value,
                SETTINGS_MAX_FIELD_SECTION_SIZE => s.max_field_section_size = Some(value),
                _ => {}
            }
        }
        Ok(s)
    }

    /// The SETTINGS payload, with one reserved identifier as grease.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut payload = Vec::new();
        for (id, value) in [
            (
                SETTINGS_QPACK_MAX_TABLE_CAPACITY,
                Some(self.qpack_max_table_capacity),
            ),
            (
                SETTINGS_QPACK_BLOCKED_STREAMS,
                Some(self.qpack_blocked_streams),
            ),
            (SETTINGS_MAX_FIELD_SECTION_SIZE, self.max_field_section_size),
            // A reserved setting exercises the peer's tolerance (§7.2.4.1).
            (0x1f * 7 + 0x21, Some(0)),
        ] {
            if let Some(v) = value {
                put_varint(&mut payload, id);
                put_varint(&mut payload, v);
            }
        }
        write_frame(out, SETTINGS, &payload);
    }
}

/// Appends a varint (values above 2^62 - 1 are a caller bug).
pub fn put_varint(out: &mut Vec<u8>, value: u64) {
    let mut buf = [0u8; 8];
    let n = varint::encode(value, &mut buf).expect("value fits a varint");
    out.extend_from_slice(&buf[..n]);
}

/// Appends a frame header and payload.
pub fn write_frame(out: &mut Vec<u8>, ty: u64, payload: &[u8]) {
    put_varint(out, ty);
    put_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// Appends a DATA frame header for a payload of `len` bytes.
pub fn write_data_header(out: &mut Vec<u8>, len: usize) {
    put_varint(out, DATA);
    put_varint(out, len as u64);
}

/// Appends a frame whose payload is one varint (GOAWAY, MAX_PUSH_ID, ...).
pub fn write_varint_frame(out: &mut Vec<u8>, ty: u64, value: u64) {
    let mut payload = Vec::with_capacity(8);
    put_varint(&mut payload, value);
    write_frame(out, ty, &payload);
}

/// A decoded frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A chunk of a DATA frame's payload (a frame can arrive as several).
    Data(Vec<u8>),
    Headers(Vec<u8>),
    Settings(Settings),
    GoAway(u64),
    CancelPush(u64),
    MaxPushId(u64),
    /// PUSH_PROMISE: its payload is not parsed (push is not supported).
    PushPromise,
    /// An unknown or reserved frame type; its payload was skipped.
    Unknown(u64),
}

impl Frame {
    pub fn frame_type(&self) -> u64 {
        match self {
            Self::Data(_) => DATA,
            Self::Headers(_) => HEADERS,
            Self::Settings(_) => SETTINGS,
            Self::GoAway(_) => GOAWAY,
            Self::CancelPush(_) => CANCEL_PUSH,
            Self::MaxPushId(_) => MAX_PUSH_ID,
            Self::PushPromise => PUSH_PROMISE,
            Self::Unknown(t) => *t,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Header,
    Data { remaining: u64 },
    Skip { remaining: u64 },
    Payload { ty: u64, len: usize },
}

/// Incremental frame decoder for one stream.
#[derive(Debug)]
pub struct FrameReader {
    buf: Vec<u8>,
    state: State,
    max_payload: usize,
    /// A DATA frame's header was announced (for the caller's state machine).
    data_started: bool,
}

impl FrameReader {
    pub fn new(max_payload: usize) -> Self {
        Self {
            buf: Vec::new(),
            state: State::Header,
            max_payload,
            data_started: false,
        }
    }

    /// Bytes held: at most one frame header or one bounded payload.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// True between frames: the end of the stream is allowed here (§7.1).
    pub fn at_boundary(&self) -> bool {
        self.state == State::Header && self.buf.is_empty()
    }

    /// The next frame or DATA chunk, or `None` until more bytes arrive.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        loop {
            match self.state {
                State::Header => {
                    let Ok((ty, a)) = varint::decode(&self.buf) else {
                        return Ok(None);
                    };
                    let Ok((len, b)) = varint::decode(&self.buf[a..]) else {
                        return Ok(None);
                    };
                    self.buf.drain(..a + b);
                    if is_http2_frame(ty) {
                        return Err(FrameError::Http2Frame(ty));
                    }
                    self.state = match ty {
                        DATA => {
                            self.data_started = true;
                            State::Data { remaining: len }
                        }
                        HEADERS | SETTINGS | GOAWAY | CANCEL_PUSH | MAX_PUSH_ID | PUSH_PROMISE => {
                            let len = usize::try_from(len)
                                .ok()
                                .filter(|l| *l <= self.max_payload)
                                .ok_or(FrameError::TooLarge {
                                    frame_type: ty,
                                    len,
                                })?;
                            State::Payload { ty, len }
                        }
                        _ => {
                            self.state = State::Skip { remaining: len };
                            return Ok(Some(Frame::Unknown(ty)));
                        }
                    };
                    if let State::Data { remaining: 0 } = self.state {
                        self.state = State::Header;
                        return Ok(Some(Frame::Data(Vec::new())));
                    }
                }
                State::Data { remaining } => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    let n = remaining.min(self.buf.len() as u64) as usize;
                    let chunk: Vec<u8> = self.buf.drain(..n).collect();
                    let left = remaining - n as u64;
                    self.state = if left == 0 {
                        State::Header
                    } else {
                        State::Data { remaining: left }
                    };
                    return Ok(Some(Frame::Data(chunk)));
                }
                State::Skip { remaining } => {
                    let n = remaining.min(self.buf.len() as u64) as usize;
                    self.buf.drain(..n);
                    let left = remaining - n as u64;
                    if left > 0 {
                        self.state = State::Skip { remaining: left };
                        return Ok(None);
                    }
                    self.state = State::Header;
                }
                State::Payload { ty, len } => {
                    if self.buf.len() < len {
                        return Ok(None);
                    }
                    let payload: Vec<u8> = self.buf.drain(..len).collect();
                    self.state = State::Header;
                    return parse_payload(ty, payload).map(Some);
                }
            }
        }
    }

    /// The stream ended: an error unless it ended between frames.
    pub fn finish(&self) -> Result<(), FrameError> {
        if self.at_boundary() {
            Ok(())
        } else {
            Err(FrameError::Malformed)
        }
    }
}

fn single_varint(payload: &[u8]) -> Result<u64, FrameError> {
    match varint::decode(payload) {
        Ok((v, n)) if n == payload.len() => Ok(v),
        _ => Err(FrameError::Malformed),
    }
}

fn parse_payload(ty: u64, payload: Vec<u8>) -> Result<Frame, FrameError> {
    Ok(match ty {
        HEADERS => Frame::Headers(payload),
        SETTINGS => Frame::Settings(Settings::parse(&payload)?),
        GOAWAY => Frame::GoAway(single_varint(&payload)?),
        CANCEL_PUSH => Frame::CancelPush(single_varint(&payload)?),
        MAX_PUSH_ID => Frame::MaxPushId(single_varint(&payload)?),
        PUSH_PROMISE => Frame::PushPromise,
        _ => unreachable!("only buffered types are parsed"),
    })
}

/// Reads the stream type at the start of a unidirectional stream.
pub fn stream_type(buf: &[u8]) -> Option<(u64, usize)> {
    varint::decode(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_split_anywhere() {
        let mut wire = Vec::new();
        Settings {
            qpack_max_table_capacity: 4096,
            qpack_blocked_streams: 16,
            max_field_section_size: Some(8192),
        }
        .encode(&mut wire);
        write_frame(&mut wire, 0x21, b"grease");
        write_frame(&mut wire, HEADERS, b"\x00\x00\xd1");
        write_frame(&mut wire, DATA, b"hello");
        write_frame(&mut wire, DATA, b"");
        write_varint_frame(&mut wire, GOAWAY, 8);
        for split in 0..wire.len() {
            let mut r = FrameReader::new(1024);
            let mut frames = Vec::new();
            for part in [&wire[..split], &wire[split..]] {
                r.push(part);
                while let Some(f) = r.next_frame().unwrap() {
                    frames.push(f);
                }
            }
            assert!(r.finish().is_ok());
            // Merge DATA chunks.
            let mut data = Vec::new();
            let kinds: Vec<u64> = frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Data(d) => {
                        data.extend_from_slice(d);
                        None
                    }
                    f => Some(f.frame_type()),
                })
                .collect();
            assert_eq!(kinds, [SETTINGS, 0x21, HEADERS, GOAWAY], "split {split}");
            assert_eq!(data, b"hello");
            assert!(frames.contains(&Frame::GoAway(8)));
        }
    }

    #[test]
    fn malformed_frames_are_typed_errors() {
        let mut r = FrameReader::new(16);
        r.push(&[0x02, 0x00]);
        assert_eq!(r.next_frame(), Err(FrameError::Http2Frame(2)));
        let mut r = FrameReader::new(16);
        r.push(&[HEADERS as u8, 17]);
        assert_eq!(
            r.next_frame(),
            Err(FrameError::TooLarge {
                frame_type: HEADERS,
                len: 17
            })
        );
        // GOAWAY payload with trailing junk.
        let mut r = FrameReader::new(16);
        r.push(&[GOAWAY as u8, 2, 0, 0]);
        assert_eq!(r.next_frame(), Err(FrameError::Malformed));
        // Duplicate and HTTP/2 settings.
        let mut r = FrameReader::new(16);
        r.push(&[SETTINGS as u8, 4, 1, 0, 1, 0]);
        assert_eq!(r.next_frame(), Err(FrameError::BadSetting(1)));
        let mut r = FrameReader::new(16);
        r.push(&[SETTINGS as u8, 2, 2, 0]);
        assert_eq!(r.next_frame(), Err(FrameError::BadSetting(2)));
        // Truncated by the end of the stream.
        let mut r = FrameReader::new(16);
        r.push(&[DATA as u8, 5, b'a']);
        assert!(matches!(r.next_frame(), Ok(Some(Frame::Data(_)))));
        assert_eq!(r.finish(), Err(FrameError::Malformed));
        // An unknown frame of enormous length is skipped, not buffered.
        let mut r = FrameReader::new(16);
        let mut wire = Vec::new();
        put_varint(&mut wire, 0x40);
        put_varint(&mut wire, 1 << 40);
        r.push(&wire);
        assert_eq!(r.next_frame(), Ok(Some(Frame::Unknown(0x40))));
        r.push(&[0u8; 4096]);
        assert_eq!(r.next_frame(), Ok(None));
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn grease_values() {
        assert!(is_reserved(0x21));
        assert!(is_reserved(0x40));
        assert!(!is_reserved(0x20));
        assert!(is_reserved(0x1f * 7 + 0x21));
    }
}
