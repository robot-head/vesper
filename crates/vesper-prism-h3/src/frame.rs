//! HTTP/3 frames (RFC 9114 §7) and unidirectional stream types (§6.2).
//!
//! [`FrameReader`] is incremental: bytes are pushed as they arrive on a
//! stream and complete frames come out. DATA payloads are passed through in
//! chunks and never buffered whole; unknown and reserved frame types are
//! skipped without buffering (§9); every other frame's payload must fit
//! `max_payload`, checked from its length before any byte is retained.

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
/// RFC 9220 §3 (extended CONNECT).
pub const SETTINGS_ENABLE_CONNECT_PROTOCOL: u64 = 0x08;
/// RFC 9297 §2.1.1 (HTTP Datagrams).
pub const SETTINGS_H3_DATAGRAM: u64 = 0x33;
/// draft-ietf-webtrans-http3: the draft-02 enable flag and the max-sessions
/// identifiers of drafts 07-08 and 09 and later. All three are sent and any
/// is accepted, so peers on different drafts interoperate.
pub const SETTINGS_ENABLE_WEBTRANSPORT: u64 = 0x2b60_3742;
pub const SETTINGS_WEBTRANSPORT_MAX_SESSIONS_DRAFT07: u64 = 0xc671_706a;
pub const SETTINGS_WT_MAX_SESSIONS: u64 = 0x14e9_cd29;

/// WEBTRANSPORT_STREAM: the signal at the start of a WebTransport
/// bidirectional stream (a frame type with a session ID and no length).
pub const WEBTRANSPORT_STREAM: u64 = 0x41;
/// The WebTransport unidirectional stream type.
pub const STREAM_WEBTRANSPORT: u64 = 0x54;

/// HTTP/2 frame types with no HTTP/3 meaning (§7.2.8, §11.2.1).
const H2_PRIORITY: u64 = 0x02;
const H2_PING: u64 = 0x06;
const H2_WINDOW_UPDATE: u64 = 0x08;
const H2_CONTINUATION: u64 = 0x09;

/// HTTP/2 frame types with no HTTP/3 meaning (§7.2.8): receiving one is
/// H3_FRAME_UNEXPECTED.
pub const fn is_http2_frame(ty: u64) -> bool {
    matches!(
        ty,
        H2_PRIORITY | H2_PING | H2_WINDOW_UPDATE | H2_CONTINUATION
    )
}

/// HTTP/2 settings with no HTTP/3 meaning (§7.2.4.1, §11.2.2): 0x00 is
/// reserved, then SETTINGS_ENABLE_PUSH, SETTINGS_MAX_CONCURRENT_STREAMS,
/// SETTINGS_INITIAL_WINDOW_SIZE and SETTINGS_MAX_FRAME_SIZE.
const H2_SETTINGS_RESERVED: u64 = 0x00;
const H2_SETTINGS_ENABLE_PUSH: u64 = 0x02;
const H2_SETTINGS_MAX_CONCURRENT_STREAMS: u64 = 0x03;
const H2_SETTINGS_INITIAL_WINDOW_SIZE: u64 = 0x04;
const H2_SETTINGS_MAX_FRAME_SIZE: u64 = 0x05;

/// HTTP/2 settings that must not appear (§7.2.4.1): H3_SETTINGS_ERROR.
pub const fn is_http2_setting(id: u64) -> bool {
    matches!(
        id,
        H2_SETTINGS_RESERVED
            | H2_SETTINGS_ENABLE_PUSH
            | H2_SETTINGS_MAX_CONCURRENT_STREAMS
            | H2_SETTINGS_INITIAL_WINDOW_SIZE
            | H2_SETTINGS_MAX_FRAME_SIZE
    )
}

/// The first reserved grease value and the step between them (§7.2.8,
/// §6.2.3, §7.2.4.1, §8.1).
pub const GREASE_BASE: u64 = 0x21;
pub const GREASE_STEP: u64 = 0x1f;

/// The reserved grease values `GREASE_STEP * N + GREASE_BASE`.
pub const fn is_reserved(value: u64) -> bool {
    value >= GREASE_BASE && (value - GREASE_BASE).is_multiple_of(GREASE_STEP)
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
    /// SETTINGS_ENABLE_CONNECT_PROTOCOL = 1 (RFC 9220).
    pub enable_connect_protocol: bool,
    /// SETTINGS_H3_DATAGRAM = 1 (RFC 9297). Sent only when the transport
    /// negotiated QUIC DATAGRAM frames (RFC 9221).
    pub h3_datagram: bool,
    /// WebTransport sessions allowed at once; `None`: WebTransport is not
    /// offered.
    pub webtransport_max_sessions: Option<u64>,
    /// SETTINGS_NO_RFC7540_PRIORITIES = 1 (RFC 9218 §2.1).
    pub no_rfc7540_priorities: bool,
}

impl Settings {
    /// Parses a SETTINGS payload (the Vesper build).
    pub fn parse(payload: &[u8]) -> Result<Self, FrameError> {
        port::h3_settings_parse(payload)
    }

    /// The SETTINGS frame (the Vesper build).
    pub fn encode(&self, out: &mut Vec<u8>) {
        written(out, port::h3_settings_encode(*self));
    }
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
    /// PRIORITY_UPDATE (RFC 9218 §7.2).
    PriorityUpdate(crate::priority::PriorityUpdate),
    /// WEBTRANSPORT_STREAM with its session ID: everything after it on the
    /// stream is WebTransport data, not frames.
    WebTransportStream(u64),
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
            Self::PriorityUpdate(u) => {
                if u.push {
                    crate::priority::PRIORITY_UPDATE_PUSH
                } else {
                    crate::priority::PRIORITY_UPDATE_REQUEST
                }
            }
            Self::WebTransportStream(_) => WEBTRANSPORT_STREAM,
            Self::Unknown(t) => *t,
        }
    }
}

/// Reads the stream type at the start of a unidirectional stream.
pub fn stream_type(buf: &[u8]) -> Option<(u64, usize)> {
    vesper_warp::varint::decode(buf).ok()
}

// ---------------------------------------------------------------------
// The Vesper build (port wave P3b lane 5, #1067): the writers and the
// reader forward to vesper/src/frame.ves through the binding.
// ---------------------------------------------------------------------

use crate::qpack::port::binding as port;

/// The bytes a port writer returned: empty when a value does not fit a
/// varint, which is a caller bug.
pub(crate) fn written(out: &mut Vec<u8>, bytes: Vec<u8>) {
    assert!(!bytes.is_empty(), "value fits a varint");
    out.extend_from_slice(&bytes);
}

/// Appends a varint (values above 2^62 - 1 are a caller bug).
pub fn put_varint(out: &mut Vec<u8>, value: u64) {
    written(out, port::h3_put_varint(value));
}

/// Appends a frame header and payload.
pub fn write_frame(out: &mut Vec<u8>, ty: u64, payload: &[u8]) {
    written(out, port::h3_record_header(ty, payload.len() as u64));
    out.extend_from_slice(payload);
}

/// Appends a DATA frame header for a payload of `len` bytes.
pub fn write_data_header(out: &mut Vec<u8>, len: usize) {
    written(out, port::h3_record_header(DATA, len as u64));
}

/// Appends a frame whose payload is one varint (GOAWAY, MAX_PUSH_ID, ...).
pub fn write_varint_frame(out: &mut Vec<u8>, ty: u64, value: u64) {
    written(out, port::h3_varint_frame(ty, value));
}

/// Incremental frame decoder for one stream (the Vesper build).
pub struct FrameReader {
    core: port::FrameReaderCore,
}

impl std::fmt::Debug for FrameReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReader")
            .field("buffered", &self.buffered())
            .finish_non_exhaustive()
    }
}

impl FrameReader {
    pub fn new(max_payload: usize) -> Self {
        Self {
            core: port::FrameReaderCore::new(max_payload as u64, false),
        }
    }

    /// A reader for a stream that may start with WEBTRANSPORT_STREAM.
    pub fn with_webtransport(max_payload: usize) -> Self {
        Self {
            core: port::FrameReaderCore::new(max_payload as u64, true),
        }
    }

    /// Bytes held: at most one frame header or one bounded payload.
    pub fn buffered(&self) -> usize {
        self.core.buffered() as usize
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.core.push(bytes);
    }

    /// The bytes held after a [`Frame::WebTransportStream`] signal.
    pub fn take_rest(&mut self) -> Vec<u8> {
        self.core.take_rest()
    }

    /// True between frames: the end of the stream is allowed here (§7.1).
    pub fn at_boundary(&self) -> bool {
        self.core.at_boundary()
    }

    /// The next frame or DATA chunk, or `None` until more bytes arrive.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        use crate::qpack::port::abi::FrameOut;
        let out = self.core.next()?;
        let bytes = || self.core.as_ref().out().as_slice().to_vec();
        Ok(Some(match out {
            FrameOut::None => return Ok(None),
            FrameOut::Data => Frame::Data(bytes()),
            FrameOut::Headers => Frame::Headers(bytes()),
            FrameOut::SettingsFrame(s) => Frame::Settings(s),
            FrameOut::GoAway(v) => Frame::GoAway(v),
            FrameOut::CancelPush(v) => Frame::CancelPush(v),
            FrameOut::MaxPushId(v) => Frame::MaxPushId(v),
            FrameOut::PushPromise => Frame::PushPromise,
            FrameOut::PriorityUpdateFrame(u) => Frame::PriorityUpdate(u),
            FrameOut::WebTransportStream(id) => Frame::WebTransportStream(id),
            FrameOut::Unknown(t) => Frame::Unknown(t),
        }))
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
            ..Settings::default()
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
