//! A sans-I/O HTTP/3 connection (RFC 9114) over any [`Transport`].
//!
//! [`Core`] is the part both roles share: our control stream with SETTINGS,
//! the QPACK encoder and decoder streams in both directions, the peer's
//! unidirectional streams (§6.2), GOAWAY, per-stream send queues and the
//! connection-error latch. [`ServerConnection`] adds request streams and
//! the [`Responder`] obligation; [`crate::client::ClientConnection`] adds
//! the client side.
//!
//! A response is an obligation: each request is handed out with a linear
//! [`Responder<HeadersPending>`] whose only exits are consuming calls that
//! complete the response ([`Responder::respond`], [`Responder::finish`]) or
//! reset the stream ([`Responder::reset`]). Dropping one is detected: the
//! connection resets that stream with H3_INTERNAL_ERROR on its next poll
//! and counts it in [`ConnectionStats::leaked_responders`]. This is
//! host-library discipline (Rust ownership plus a runtime check), not the
//! Vesper source typing rules of design §17.2.

use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use crate::error::Code;
use crate::frame::{self, Frame, FrameError, FrameReader, Settings};
use crate::message::{self, MessageError, RequestHead};
use crate::priority::{self, Priority};
use crate::qpack::{
    Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field, FieldSection, QpackError,
};
use crate::session::{
    DatagramPath, SessionError, SessionEvent, SessionKind, SessionStats, Sessions,
};
use crate::transport::{ReadOutcome, Transport, TransportError, is_server_initiated, is_uni};

/// Connection limits and the SETTINGS we advertise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H3Config {
    /// Our SETTINGS: QPACK decoder limits and the field section limit.
    pub settings: Settings,
    pub encoder: EncoderConfig,
    /// Largest non-DATA frame payload accepted (HEADERS, SETTINGS, ...).
    pub max_frame_payload: usize,
    /// Largest message body accepted (requests on a server, responses on a
    /// client); bodies are delivered whole.
    pub max_body_bytes: usize,
    /// Request streams open at once; more are refused with
    /// H3_REQUEST_REJECTED.
    pub max_concurrent_requests: usize,
    /// Peer unidirectional streams tracked at once (control, QPACK and
    /// unknown types awaiting their type byte).
    pub max_peer_uni_streams: usize,
    /// Encoded QPACK bytes held for blocked request streams.
    pub max_blocked_bytes: usize,
    /// Unsent bytes queued per stream before writes are refused.
    pub max_stream_buffer: usize,
    /// Deliver request heads as soon as their HEADERS arrive
    /// ([`Event::StreamingRequest`]) and let the application pull the body
    /// ([`ServerConnection::read_body`]); otherwise bodies are delivered
    /// whole ([`Event::Request`]).
    pub streaming_request_bodies: bool,
    /// Body bytes held per streaming request before the connection stops
    /// reading that stream (QUIC flow control then holds the peer back).
    pub stream_body_window: usize,
    /// Request bytes held by the connection across all streams (bodies
    /// being assembled, streaming buffers and frame buffers). When it is
    /// reached no request stream is read; if every held byte belongs to
    /// whole bodies that cannot complete, the newest such request is reset
    /// with H3_EXCESSIVE_LOAD so the others can.
    pub max_connection_buffer: usize,
    /// Bytes an incremental response may write before the next incremental
    /// response of the same urgency gets a turn (RFC 9218 §10).
    pub incremental_quantum: usize,
    /// When false (the default), a less urgent (or later non-incremental)
    /// response gets no bytes in a flush in which the transport accepted
    /// bytes of a more urgent one that still has more queued; a response
    /// the transport took nothing from (flow control) holds nobody back.
    /// When true, less urgent responses always fill what the transport
    /// still takes.
    pub work_conserving: bool,
}

impl Default for H3Config {
    fn default() -> Self {
        Self {
            settings: Settings {
                qpack_max_table_capacity: 4096,
                qpack_blocked_streams: 16,
                max_field_section_size: Some(16 * 1024),
                no_rfc7540_priorities: true,
                ..Settings::default()
            },
            encoder: EncoderConfig::default(),
            max_frame_payload: 32 * 1024,
            max_body_bytes: 1024 * 1024,
            max_concurrent_requests: 64,
            max_peer_uni_streams: 8,
            max_blocked_bytes: 64 * 1024,
            max_stream_buffer: 2 * 1024 * 1024,
            streaming_request_bodies: false,
            stream_body_window: 64 * 1024,
            max_connection_buffer: 4 * 1024 * 1024,
            incremental_quantum: 16 * 1024,
            work_conserving: false,
        }
    }
}

impl H3Config {
    /// Offers extended CONNECT, HTTP Datagrams and WebTransport with up to
    /// `max_sessions` sessions at once. SETTINGS_H3_DATAGRAM is sent only
    /// if the transport has QUIC DATAGRAM frames.
    pub fn with_webtransport(mut self, max_sessions: u64) -> Self {
        self.settings.enable_connect_protocol = true;
        self.settings.h3_datagram = true;
        self.settings.webtransport_max_sessions = Some(max_sessions.max(1));
        self
    }
}

/// Why the connection failed (it was closed with `code`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    Frame {
        stream_id: u64,
        error: FrameError,
    },
    Qpack(QpackError),
    /// A frame not allowed on this stream or in this state.
    FrameUnexpected {
        stream_id: u64,
        frame_type: u64,
    },
    /// The control stream did not start with SETTINGS.
    MissingSettings,
    /// A second control, encoder or decoder stream, or a push stream.
    StreamCreation {
        stream_id: u64,
    },
    /// A control or QPACK stream was closed or reset.
    ClosedCriticalStream {
        stream_id: u64,
    },
    /// GOAWAY with an invalid or increasing ID.
    BadGoaway(u64),
    /// CANCEL_PUSH or MAX_PUSH_ID that does not fit the push state.
    BadPushId(u64),
    /// The peer's request stream ID is not one a client may open.
    BadStreamId(u64),
    /// The transport refused a write on a critical stream.
    Transport(TransportError),
    /// A malformed HTTP Datagram, or one we never offered (RFC 9297).
    Datagram,
}

impl ConnectionError {
    pub fn code(self) -> Code {
        match self {
            Self::Frame { error, .. } => error.code(),
            Self::Qpack(e) => e.code(),
            Self::FrameUnexpected { .. } => Code::FRAME_UNEXPECTED,
            Self::MissingSettings => Code::MISSING_SETTINGS,
            Self::StreamCreation { .. } => Code::STREAM_CREATION_ERROR,
            Self::ClosedCriticalStream { .. } => Code::CLOSED_CRITICAL_STREAM,
            Self::BadGoaway(_) | Self::BadPushId(_) | Self::BadStreamId(_) => Code::ID_ERROR,
            Self::Transport(_) => Code::INTERNAL_ERROR,
            Self::Datagram => Code::DATAGRAM_ERROR,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionStats {
    pub requests: u64,
    pub responses: u64,
    pub resets_sent: u64,
    pub resets_received: u64,
    pub rejected: u64,
    pub malformed: u64,
    pub unknown_frames: u64,
    pub unknown_streams: u64,
    pub blocked_sections: u64,
    pub leaked_responders: u64,
    /// Requests reset because the connection buffer was exhausted by whole
    /// bodies that could not complete.
    pub budget_resets: u64,
    pub priority_updates: u64,
    pub interim_responses: u64,
}

enum Uni {
    /// Waiting for the stream-type varint.
    Pending(Vec<u8>),
    Control(FrameReader),
    QpackEncoder,
    QpackDecoder,
    /// Unknown or reserved type: STOP_SENDING was sent, bytes are discarded.
    Ignored,
}

#[derive(Default)]
struct Outgoing {
    buf: VecDeque<u8>,
    fin: bool,
}

#[derive(Clone, Copy)]
struct Local {
    control: u64,
    encoder: u64,
    decoder: u64,
}

/// What both roles share.
pub(crate) struct Core {
    pub(crate) cfg: H3Config,
    /// The SETTINGS we sent (H3_DATAGRAM cleared if the transport has no
    /// QUIC datagrams).
    pub(crate) sent_settings: Settings,
    pub(crate) server: bool,
    /// The server's next expected request stream ID (session IDs below it
    /// that are not sessions are gone).
    pub(crate) next_peer_bidi: u64,
    pub(crate) sessions: Sessions,
    /// RFC 9218 priorities of request streams (server: of responses).
    pub(crate) priorities: BTreeMap<u64, Priority>,
    /// Streams whose priority came from PRIORITY_UPDATE (it wins over the
    /// header).
    pub(crate) priority_updated: std::collections::BTreeSet<u64>,
    /// The priority last hinted to the transport, per stream.
    hinted: BTreeMap<u64, Priority>,
    rr: usize,
    pub(crate) encoder: Encoder,
    pub(crate) decoder: Decoder,
    local: Option<Local>,
    /// Critical streams opened so far while stream credit is short.
    opening: Vec<u64>,
    pub(crate) peer_settings: Option<Settings>,
    peer_control: Option<u64>,
    peer_encoder: Option<u64>,
    peer_decoder: Option<u64>,
    uni: BTreeMap<u64, Uni>,
    out: BTreeMap<u64, Outgoing>,
    pub(crate) error: Option<ConnectionError>,
    pub(crate) goaway_received: Option<u64>,
    /// The client's MAX_PUSH_ID (a server never pushes).
    max_push_id: Option<u64>,
    pub(crate) stats: ConnectionStats,
    pub(crate) scratch: Vec<u8>,
    /// Non-critical streams whose writes failed (peer STOP_SENDING).
    pub(crate) write_failures: Vec<(u64, u64)>,
}

impl Core {
    pub(crate) fn new(cfg: H3Config, server: bool) -> Self {
        let s = cfg.settings;
        Self {
            cfg,
            sent_settings: s,
            server,
            next_peer_bidi: 0,
            sessions: Sessions::default(),
            priorities: BTreeMap::new(),
            priority_updated: Default::default(),
            hinted: BTreeMap::new(),
            rr: 0,
            encoder: Encoder::new(cfg.encoder),
            decoder: Decoder::new(DecoderConfig {
                max_table_capacity: s.qpack_max_table_capacity,
                max_blocked_streams: s.qpack_blocked_streams,
                max_field_section_size: s.max_field_section_size.unwrap_or(u64::MAX),
                max_blocked_bytes: cfg.max_blocked_bytes,
            }),
            local: None,
            opening: Vec::new(),
            peer_settings: None,
            peer_control: None,
            peer_encoder: None,
            peer_decoder: None,
            uni: BTreeMap::new(),
            out: BTreeMap::new(),
            error: None,
            goaway_received: None,
            max_push_id: None,
            stats: ConnectionStats::default(),
            scratch: vec![0; 16 * 1024],
            write_failures: Vec::new(),
        }
    }

    /// Latches a connection error and closes the transport with its code.
    pub(crate) fn fail(&mut self, t: &mut impl Transport, e: ConnectionError) {
        if self.error.is_none() {
            self.error = Some(e);
            t.close(e.code());
        }
    }

    /// Opens our control and QPACK streams once the transport allows it.
    pub(crate) fn open_local(&mut self, t: &mut impl Transport) -> Result<bool, ConnectionError> {
        if self.local.is_some() {
            return Ok(true);
        }
        // Out of stream credit is not an error: the streams opened so far
        // are kept and the rest are opened on a later poll.
        while self.opening.len() < 3 {
            match t.open_uni() {
                Ok(id) => self.opening.push(id),
                Err(TransportError::NotReady) => return Ok(false),
                Err(e) => return Err(ConnectionError::Transport(e)),
            }
        }
        let (control, encoder, decoder) = (self.opening[0], self.opening[1], self.opening[2]);
        self.opening.clear();
        self.local = Some(Local {
            control,
            encoder,
            decoder,
        });
        let mut sent = self.cfg.settings;
        // RFC 9297 §2.1.1: H3_DATAGRAM needs QUIC DATAGRAM support.
        sent.h3_datagram &= t.max_datagram_size().is_some();
        self.sent_settings = sent;
        let mut c = Vec::new();
        frame::put_varint(&mut c, frame::STREAM_CONTROL);
        sent.encode(&mut c);
        self.queue(control, &c);
        let mut e = Vec::new();
        frame::put_varint(&mut e, frame::STREAM_QPACK_ENCODER);
        self.queue(encoder, &e);
        let mut d = Vec::new();
        frame::put_varint(&mut d, frame::STREAM_QPACK_DECODER);
        self.queue(decoder, &d);
        Ok(true)
    }

    pub(crate) fn queued(&self, stream_id: u64) -> usize {
        self.out.get(&stream_id).map_or(0, |o| o.buf.len())
    }

    /// Queues bytes for a stream (unbounded: callers check the budget).
    pub(crate) fn queue(&mut self, stream_id: u64, bytes: &[u8]) {
        self.out.entry(stream_id).or_default().buf.extend(bytes);
    }

    pub(crate) fn queue_fin(&mut self, stream_id: u64) {
        self.out.entry(stream_id).or_default().fin = true;
    }

    /// Drops a stream's unsent bytes (it was reset).
    pub(crate) fn discard(&mut self, stream_id: u64) {
        self.out.remove(&stream_id);
    }

    /// Queues a frame on our control stream (false before it is open).
    pub(crate) fn queue_control(&mut self, bytes: &[u8]) -> bool {
        match self.local {
            Some(l) => {
                self.queue(l.control, bytes);
                true
            }
            None => false,
        }
    }

    /// Sends GOAWAY on our control stream.
    pub(crate) fn send_goaway(&mut self, id: u64) {
        if let Some(l) = self.local {
            let mut g = Vec::new();
            frame::write_varint_frame(&mut g, frame::GOAWAY, id);
            self.queue(l.control, &g);
        }
    }

    /// Writes queued bytes; finishes streams whose queue drained.
    ///
    /// Our unidirectional streams go first (QPACK instructions before the
    /// sections that use them), then request streams in RFC 9218 order
    /// ([`priority::schedule`]).
    pub(crate) fn flush(&mut self, t: &mut impl Transport) {
        if let Some(l) = self.local {
            let enc = self.encoder.take_encoder_stream();
            if !enc.is_empty() {
                self.queue(l.encoder, &enc);
            }
            let dec = self.decoder.take_decoder_stream();
            if !dec.is_empty() {
                self.queue(l.decoder, &dec);
            }
        }
        let critical: Vec<u64> = self
            .local
            .map(|l| vec![l.control, l.encoder, l.decoder])
            .unwrap_or_default();
        let mut failure = None;
        let ids: Vec<u64> = self.out.keys().copied().collect();
        let mut bidi = Vec::new();
        for id in ids {
            if is_uni(id) {
                self.write_out(t, id, usize::MAX, &critical, &mut failure);
            } else {
                let p = self.priorities.get(&id).copied().unwrap_or_default();
                // The transport's scheduler orders what it already holds.
                if self.hinted.get(&id) != Some(&p) {
                    t.set_priority(id, p.urgency, p.incremental);
                    self.hinted.insert(id, p);
                }
                bidi.push((id, p));
            }
        }
        self.rr = self.rr.wrapping_add(1);
        let quantum = self.cfg.incremental_quantum.max(1);
        for group in priority::schedule(&bidi, self.rr) {
            let mut wrote = 0;
            if let [id] = group[..] {
                wrote = self.write_out(t, id, usize::MAX, &critical, &mut failure);
            } else {
                // Incremental responses take turns (§10).
                loop {
                    let mut progress = 0;
                    for &id in &group {
                        progress += self.write_out(t, id, quantum, &critical, &mut failure);
                    }
                    wrote += progress;
                    if progress == 0 {
                        break;
                    }
                }
            }
            // While the transport is taking this group's bytes and some
            // remain, later groups wait, so the transport's own stream
            // scheduling cannot undo the order. A group the transport took
            // nothing from (flow control) does not hold the others back,
            // and a work-conserving configuration never waits.
            if !self.cfg.work_conserving && wrote > 0 && group.iter().any(|id| self.queued(*id) > 0)
            {
                break;
            }
        }
        let ids: Vec<u64> = self.out.keys().copied().collect();
        for id in ids {
            let o = self.out.get(&id).expect("listed");
            if !o.buf.is_empty() {
                continue;
            }
            if o.fin {
                let _ = t.finish(id);
                self.out.remove(&id);
            } else if !critical.contains(&id) {
                self.out.remove(&id);
            }
        }
        // Hints only matter while bytes are queued; a stream that queues
        // again is hinted again.
        let out = &self.out;
        self.hinted.retain(|id, _| out.contains_key(id));
        if let Some(e) = failure {
            self.fail(t, e);
        }
    }

    /// Writes up to `limit` queued bytes of one stream; returns how many
    /// were written. A failed write drops the queue (critical streams fail
    /// the connection).
    fn write_out(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        limit: usize,
        critical: &[u64],
        failure: &mut Option<ConnectionError>,
    ) -> usize {
        let Some(o) = self.out.get_mut(&id) else {
            return 0;
        };
        let mut written = 0;
        while !o.buf.is_empty() && written < limit {
            let (a, _) = o.buf.as_slices();
            let a = &a[..a.len().min(limit - written)];
            match t.write(id, a) {
                Ok(0) => break,
                Ok(n) => {
                    o.buf.drain(..n);
                    written += n;
                }
                Err(e) => {
                    if critical.contains(&id) {
                        *failure = Some(ConnectionError::Transport(e));
                    } else if let TransportError::Reset(code) = e {
                        self.write_failures.push((id, code));
                    }
                    self.out.remove(&id);
                    break;
                }
            }
        }
        written
    }

    /// Sets a stream's priority from its `priority` field, unless a
    /// PRIORITY_UPDATE already set it (§7.1: the frame wins).
    pub(crate) fn header_priority(&mut self, id: u64, fields: &[Field]) {
        if self.priority_updated.contains(&id) {
            return;
        }
        // Repeated field lines are combined with commas (RFC 9110 §5.3).
        let mut value = Vec::new();
        for f in fields.iter().filter(|f| f.name == b"priority") {
            if !value.is_empty() {
                value.extend_from_slice(b", ");
            }
            value.extend_from_slice(&f.value);
        }
        if !value.is_empty() {
            self.priorities.insert(id, Priority::parse(&value));
        }
    }

    pub(crate) fn forget_priority(&mut self, id: u64) {
        self.priorities.remove(&id);
        self.priority_updated.remove(&id);
        self.hinted.remove(&id);
    }

    /// Routes a newly accepted peer unidirectional stream.
    pub(crate) fn accept_uni(&mut self, t: &mut impl Transport, id: u64) {
        if self.uni.len() >= self.cfg.max_peer_uni_streams {
            t.stop_sending(id, Code::EXCESSIVE_LOAD);
            self.stats.unknown_streams += 1;
            return;
        }
        self.uni.insert(id, Uni::Pending(Vec::new()));
    }

    /// Reads every peer unidirectional stream.
    pub(crate) fn read_uni(
        &mut self,
        t: &mut impl Transport,
        server: bool,
    ) -> Result<(), ConnectionError> {
        let ids: Vec<u64> = self.uni.keys().copied().collect();
        let mut buf = std::mem::take(&mut self.scratch);
        let r = (|| {
            for id in ids {
                loop {
                    let r = match t.read(id, &mut buf) {
                        Ok(r) => r,
                        Err(TransportError::Reset(_)) | Err(TransportError::UnknownStream) => {
                            self.uni_closed(id)?;
                            break;
                        }
                        Err(TransportError::NotReady) => break,
                        Err(e) => return Err(ConnectionError::Transport(e)),
                    };
                    if r.len > 0 {
                        self.uni_bytes(t, id, &buf[..r.len], server)?;
                    }
                    if r.fin {
                        self.uni_closed(id)?;
                        break;
                    }
                    if r.len == 0 {
                        break;
                    }
                }
            }
            Ok(())
        })();
        self.scratch = buf;
        r
    }

    fn uni_closed(&mut self, id: u64) -> Result<(), ConnectionError> {
        match self.uni.remove(&id) {
            Some(Uni::Control(_) | Uni::QpackEncoder | Uni::QpackDecoder) => {
                Err(ConnectionError::ClosedCriticalStream { stream_id: id })
            }
            _ => Ok(()),
        }
    }

    fn uni_bytes(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        bytes: &[u8],
        server: bool,
    ) -> Result<(), ConnectionError> {
        if let Some(Uni::Pending(held)) = self.uni.get_mut(&id) {
            held.extend_from_slice(bytes);
            let Some((ty, n)) = frame::stream_type(held) else {
                return Ok(());
            };
            if ty == frame::STREAM_WEBTRANSPORT
                && self.sent_settings.webtransport_max_sessions.is_some()
            {
                let Ok((session, m)) = vesper_warp::varint::decode(&held[n..]) else {
                    return Ok(());
                };
                let rest = held[n + m..].to_vec();
                self.uni.remove(&id);
                let gone = self.session_known_gone(session);
                return self.incoming_wt_stream(t, id, session, false, rest, gone);
            }
            let rest = held.split_off(n);
            let slot = match ty {
                frame::STREAM_CONTROL => &mut self.peer_control,
                frame::STREAM_QPACK_ENCODER => &mut self.peer_encoder,
                frame::STREAM_QPACK_DECODER => &mut self.peer_decoder,
                // Only servers push; a client never accepts push here
                // (push is out of scope, so no MAX_PUSH_ID is ever sent).
                frame::STREAM_PUSH => {
                    return Err(ConnectionError::StreamCreation { stream_id: id });
                }
                _ => {
                    t.stop_sending(id, Code::STREAM_CREATION_ERROR);
                    self.uni.insert(id, Uni::Ignored);
                    self.stats.unknown_streams += 1;
                    return Ok(());
                }
            };
            if slot.is_some() {
                return Err(ConnectionError::StreamCreation { stream_id: id });
            }
            *slot = Some(id);
            let kind = match ty {
                frame::STREAM_CONTROL => Uni::Control(FrameReader::new(self.cfg.max_frame_payload)),
                frame::STREAM_QPACK_ENCODER => Uni::QpackEncoder,
                _ => Uni::QpackDecoder,
            };
            self.uni.insert(id, kind);
            if rest.is_empty() {
                return Ok(());
            }
            return self.uni_bytes(t, id, &rest, server);
        }
        match self.uni.get_mut(&id) {
            Some(Uni::Control(reader)) => {
                reader.push(bytes);
                loop {
                    let f = match self.uni.get_mut(&id) {
                        Some(Uni::Control(reader)) => reader.next_frame(),
                        _ => unreachable!("control stream"),
                    };
                    match f {
                        Ok(Some(f)) => self.control_frame(id, f, server)?,
                        Ok(None) => break,
                        Err(error) => {
                            return Err(ConnectionError::Frame {
                                stream_id: id,
                                error,
                            });
                        }
                    }
                }
            }
            Some(Uni::QpackEncoder) => {
                self.decoder
                    .on_encoder_stream(bytes)
                    .map_err(ConnectionError::Qpack)?;
            }
            Some(Uni::QpackDecoder) => {
                self.encoder
                    .on_decoder_stream(bytes)
                    .map_err(ConnectionError::Qpack)?;
            }
            Some(Uni::Ignored) | None => {}
            Some(Uni::Pending(_)) => unreachable!("handled above"),
        }
        Ok(())
    }

    /// A session ID that names no live or future session.
    pub(crate) fn session_known_gone(&self, session: u64) -> bool {
        if self.sessions.map.contains_key(&session) {
            return false;
        }
        // A client knows every session it opened; a server knows the
        // request streams the client opened so far.
        !self.server || session < self.next_peer_bidi
    }

    fn control_frame(&mut self, id: u64, f: Frame, server: bool) -> Result<(), ConnectionError> {
        let unexpected = |f: &Frame| ConnectionError::FrameUnexpected {
            stream_id: id,
            frame_type: f.frame_type(),
        };
        if self.peer_settings.is_none() {
            let Frame::Settings(s) = f else {
                return Err(ConnectionError::MissingSettings);
            };
            self.peer_settings = Some(s);
            self.encoder
                .apply_peer_settings(s.qpack_max_table_capacity, s.qpack_blocked_streams);
            return Ok(());
        }
        match f {
            Frame::Unknown(_) => self.stats.unknown_frames += 1,
            Frame::GoAway(v) => {
                // From a server: a client-initiated bidi stream ID. From a
                // client: a push ID. It must never increase (§5.2).
                if !server && (is_uni(v) || is_server_initiated(v)) {
                    return Err(ConnectionError::BadGoaway(v));
                }
                if self.goaway_received.is_some_and(|prev| v > prev) {
                    return Err(ConnectionError::BadGoaway(v));
                }
                self.goaway_received = Some(v);
            }
            Frame::MaxPushId(v) if server => {
                if self.max_push_id.is_some_and(|prev| v < prev) {
                    return Err(ConnectionError::BadPushId(v));
                }
                self.max_push_id = Some(v);
            }
            Frame::PriorityUpdate(u) if server => {
                // Push is never enabled, so no push ID is valid (§7.2).
                if u.push {
                    return Err(ConnectionError::BadPushId(u.element_id));
                }
                if is_uni(u.element_id) || is_server_initiated(u.element_id) {
                    return Err(ConnectionError::BadStreamId(u.element_id));
                }
                self.stats.priority_updates += 1;
                // Bounded: updates for streams not yet open are kept only
                // while the table is small.
                let bound = self.cfg.max_concurrent_requests * 2 + 16;
                if self.priorities.contains_key(&u.element_id) || self.priorities.len() < bound {
                    self.priorities.insert(u.element_id, u.priority);
                    self.priority_updated.insert(u.element_id);
                }
            }
            Frame::CancelPush(v) => {
                // Push is never used: any ID past MAX_PUSH_ID is an error,
                // others refer to pushes that never happened (ignored).
                if !server || self.max_push_id.is_none_or(|m| v > m) {
                    return Err(ConnectionError::BadPushId(v));
                }
            }
            f => return Err(unexpected(&f)),
        }
        Ok(())
    }
}

/// A request stream ID the server will (or will not) process after GOAWAY.
fn goaway_allows(goaway: Option<u64>, id: u64) -> bool {
    goaway.is_none_or(|g| id < g)
}

/// Response typestate: no response fields sent yet.
#[derive(Debug)]
pub struct HeadersPending;
/// Response typestate: fields sent, body (DATA) open.
#[derive(Debug)]
pub struct StreamingBody;

/// The receipt that a response was completed or its stream reset.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct ResponseSent {
    pub stream_id: u64,
    pub reset: Option<Code>,
}

type LeakSink = Arc<Mutex<Vec<u64>>>;

fn leaked(sink: &LeakSink, id: u64) {
    if let Ok(mut v) = sink.lock() {
        v.push(id);
    }
}

/// The obligation to answer one request. Not `Clone`; every exit consumes
/// it. Dropping it is detected (see the module documentation).
#[must_use = "a request must be answered: respond, finish or reset"]
#[derive(Debug)]
pub struct Responder<S> {
    stream_id: u64,
    conn: u64,
    sink: LeakSink,
    armed: bool,
    _state: PhantomData<S>,
}

impl<S> Responder<S> {
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    fn transition<N>(mut self) -> Responder<N> {
        self.armed = false;
        Responder {
            stream_id: self.stream_id,
            conn: self.conn,
            sink: self.sink.clone(),
            armed: true,
            _state: PhantomData,
        }
    }

    fn discharge(mut self) -> ResponseSent {
        self.armed = false;
        ResponseSent {
            stream_id: self.stream_id,
            reset: None,
        }
    }

    /// Abandons the response: RESET_STREAM (and STOP_SENDING) with `code`.
    /// Works after the stream is gone; refused only with
    /// [`SendError::Foreign`] when `conn` is not the connection the
    /// request came from, in which case the responder comes back armed.
    pub fn reset<T: Transport>(
        self,
        conn: &mut ServerConnection,
        t: &mut T,
        code: Code,
    ) -> Result<ResponseSent, (Self, SendError)> {
        if self.conn != conn.id {
            return Err((self, SendError::Foreign));
        }
        conn.reset_request(t, self.stream_id, code);
        let mut sent = self.discharge();
        sent.reset = Some(code);
        Ok(sent)
    }
}

impl<S> Drop for Responder<S> {
    fn drop(&mut self) {
        if self.armed {
            // Never silently released: the connection resets the stream.
            leaked(&self.sink, self.stream_id);
        }
    }
}

/// Why a response step was refused. The responder comes back with it.
#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// The response would exceed the stream's send budget.
    BufferFull,
    /// The responder belongs to a different connection.
    Foreign,
    /// The stream was reset by the peer (STOP_SENDING) or the connection
    /// failed; the responder can only be [`Responder::reset`] now.
    StreamGone,
    /// Status out of range for the step (final: 200..=599; interim:
    /// 100..=199 except 101), or a pseudo/connection-specific field was
    /// supplied.
    InvalidResponse(MessageError),
    /// [`Responder::accept_session`] on a request that is not an extended
    /// CONNECT.
    NotExtendedConnect,
    /// A session close reason over 1024 bytes or not UTF-8.
    BadCloseReason,
}

/// An application response, as fields plus a whole body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub fields: Vec<Field>,
    pub body: Vec<u8>,
}

impl Responder<HeadersPending> {
    /// Sends a complete response: HEADERS, the body as one DATA frame,
    /// then FIN. `content-length` is added from the body (omitted for 204
    /// and 304); for a HEAD request the length is sent and the body is not.
    /// All or nothing: on error nothing was queued.
    pub fn respond<T: Transport>(
        self,
        conn: &mut ServerConnection,
        t: &mut T,
        response: Response,
    ) -> Result<ResponseSent, (Self, SendError)> {
        if let Err(e) = conn.check_responder(self.conn, self.stream_id) {
            return Err((self, e));
        }
        match conn.send_whole_response(t, self.stream_id, response) {
            Ok(()) => Ok(self.discharge()),
            Err(e) => Err((self, e)),
        }
    }

    /// Sends an interim (1xx) response: 100 Continue, 103 Early Hints and
    /// so on (RFC 9110 §15.2). 101 is refused: HTTP/3 has no Upgrade
    /// (RFC 9114 §4.5). The final response is still owed.
    pub fn send_interim<T: Transport>(
        &mut self,
        conn: &mut ServerConnection,
        t: &mut T,
        status: u16,
        fields: Vec<Field>,
    ) -> Result<(), SendError> {
        conn.check_responder(self.conn, self.stream_id)?;
        conn.send_interim(t, self.stream_id, status, fields)
    }

    /// Sends the response fields; the body follows with
    /// [`Responder::send_data`]. `content_length`, when given, is sent as
    /// the content-length field (and checked by [`Responder::finish`]).
    pub fn send_headers<T: Transport>(
        self,
        conn: &mut ServerConnection,
        t: &mut T,
        status: u16,
        fields: Vec<Field>,
        content_length: Option<u64>,
    ) -> Result<Responder<StreamingBody>, (Self, SendError)> {
        if let Err(e) = conn.check_responder(self.conn, self.stream_id) {
            return Err((self, e));
        }
        match conn.send_response_headers(t, self.stream_id, status, fields, content_length, 0) {
            Ok(()) => Ok(self.transition()),
            Err(e) => Err((self, e)),
        }
    }

    /// Accepts an extended CONNECT: sends 200 and establishes the session.
    /// The request obligation becomes the [`Session`] obligation. To refuse
    /// a session, answer with [`Responder::respond`] and a 4xx/5xx status.
    pub fn accept_session<T: Transport>(
        self,
        conn: &mut ServerConnection,
        t: &mut T,
        fields: Vec<Field>,
    ) -> Result<Session, (Self, SendError)> {
        if let Err(e) = conn.check_responder(self.conn, self.stream_id) {
            return Err((self, e));
        }
        if !conn
            .requests
            .get(&self.stream_id)
            .is_some_and(|rs| rs.phase == Phase::Tunnel)
        {
            return Err((self, SendError::NotExtendedConnect));
        }
        if let Err(e) = conn.send_response_headers(t, self.stream_id, 200, fields, None, 0) {
            return Err((self, e));
        }
        conn.core.establish_session(self.stream_id);
        conn.forward_session_events();
        let mut me = self;
        me.armed = false;
        Ok(Session {
            id: me.stream_id,
            conn: me.conn,
            sink: me.sink.clone(),
            armed: true,
        })
    }
}

impl Responder<StreamingBody> {
    /// Queues body bytes as one DATA frame; all or nothing.
    pub fn send_data<T: Transport>(
        &mut self,
        conn: &mut ServerConnection,
        t: &mut T,
        data: &[u8],
    ) -> Result<(), SendError> {
        conn.check_responder(self.conn, self.stream_id)?;
        conn.send_response_data(t, self.stream_id, data)
    }

    /// Ends the response, with optional trailers, and discharges the
    /// obligation.
    pub fn finish<T: Transport>(
        self,
        conn: &mut ServerConnection,
        t: &mut T,
        trailers: Vec<Field>,
    ) -> Result<ResponseSent, (Self, SendError)> {
        if let Err(e) = conn.check_responder(self.conn, self.stream_id) {
            return Err((self, e));
        }
        match conn.finish_response(t, self.stream_id, trailers) {
            Ok(()) => Ok(self.discharge()),
            Err(e) => Err((self, e)),
        }
    }
}

/// An established extended CONNECT session (WebTransport or another
/// protocol): the obligation that replaced the request's responder. Not
/// `Clone`; its only exit is [`Session::close`]. Dropping it is detected:
/// the connection resets the CONNECT stream with H3_INTERNAL_ERROR (which
/// ends the session and resets its streams) and counts it in
/// [`ConnectionStats::leaked_responders`].
#[must_use = "a session must be closed"]
#[derive(Debug)]
pub struct Session {
    id: u64,
    conn: u64,
    sink: LeakSink,
    armed: bool,
}

impl Session {
    /// The session ID: the CONNECT request's stream ID.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Closes the session: a CLOSE_WEBTRANSPORT_SESSION capsule with
    /// `code` and `reason` (WebTransport only), then FIN. The session's
    /// streams are reset with WT_SESSION_GONE. Also the exit after the
    /// peer closed the session ([`SessionEvent::Closed`]): then only our
    /// FIN is sent. Refused (and handed back) for a reason over 1024 bytes
    /// or not UTF-8, and for another connection.
    pub fn close<T: Transport>(
        mut self,
        conn: &mut ServerConnection,
        t: &mut T,
        code: u32,
        reason: &[u8],
    ) -> Result<ResponseSent, (Self, SendError)> {
        if self.conn != conn.id {
            return Err((self, SendError::Foreign));
        }
        match conn.close_session(t, self.id, code, reason) {
            Ok(reset) => {
                self.armed = false;
                Ok(ResponseSent {
                    stream_id: self.id,
                    reset,
                })
            }
            Err(e) => Err((self, e)),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.armed {
            leaked(&self.sink, self.id);
        }
    }
}

/// A complete request handed to the application.
#[derive(Debug)]
pub struct Request {
    pub stream_id: u64,
    pub head: RequestHead,
    pub body: Vec<u8>,
    pub trailers: Vec<Field>,
    pub responder: Responder<HeadersPending>,
}

/// A request whose body is still arriving
/// ([`H3Config::streaming_request_bodies`]): read it with
/// [`ServerConnection::read_body`].
#[derive(Debug)]
pub struct StreamingRequest {
    pub stream_id: u64,
    pub head: RequestHead,
    pub responder: Responder<HeadersPending>,
}

/// An extended CONNECT (RFC 9220): accept it with
/// [`Responder::accept_session`] or refuse it with a response.
#[derive(Debug)]
pub struct ConnectRequest {
    pub stream_id: u64,
    pub head: RequestHead,
    pub kind: SessionKind,
    pub responder: Responder<HeadersPending>,
}

/// Something the server application should know.
#[derive(Debug)]
pub enum Event {
    Request(Box<Request>),
    StreamingRequest(Box<StreamingRequest>),
    /// Body bytes (or the end of the body) are ready for
    /// [`ServerConnection::read_body`].
    BodyReadable {
        stream_id: u64,
    },
    Connect(Box<ConnectRequest>),
    Session(SessionEvent),
    /// A request stream failed before a complete request: it was reset
    /// with `code` (malformed, too large, refused, or reset by the peer).
    RequestFailed {
        stream_id: u64,
        code: Code,
    },
    /// The peer sent STOP_SENDING or reset its side after the request was
    /// handed out; the responder can still be consumed (it is a no-op).
    ResponseAbandoned {
        stream_id: u64,
        code: u64,
    },
    /// The client sent GOAWAY.
    GoAway(u64),
}

/// The result of [`ServerConnection::read_body`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyRead {
    pub len: usize,
    /// The whole body has been read; trailers, if any, are available from
    /// [`ServerConnection::take_trailers`].
    pub end: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Waiting for the request HEADERS.
    Headers,
    /// QPACK-blocked on the request (or trailer) section.
    Blocked {
        trailers: bool,
    },
    Body,
    /// Trailers received; only unknown frames may follow.
    Trailers,
    /// Handed to the application whole.
    Delivered,
    /// Handed to the application with its body still arriving.
    Streaming,
    /// An extended CONNECT handed to the application; DATA is capsules.
    Tunnel,
}

impl Phase {
    /// The application holds the responder (or session).
    fn handed_out(self) -> bool {
        matches!(self, Self::Delivered | Self::Streaming | Self::Tunnel)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    None,
    Headers {
        content_length: Option<u64>,
        sent: u64,
    },
    Finished,
    Gone,
}

struct RequestStream {
    reader: FrameReader,
    phase: Phase,
    head: Option<RequestHead>,
    /// Whole mode: the body so far. Streaming mode: bytes not yet read by
    /// the application.
    body: Vec<u8>,
    /// Body bytes received in total (content-length checks).
    body_total: u64,
    trailers: Vec<Field>,
    fin: bool,
    reply: Reply,
    head_request: bool,
    /// Any frame was seen (WEBTRANSPORT_STREAM must come first).
    framed: bool,
    /// Streaming mode: the application pulls the body.
    streaming: bool,
    /// Trailers came (streaming mode): only unknown frames may follow.
    trailers_seen: bool,
    /// Streaming mode: FIN received and checked.
    body_end: bool,
    /// Streaming mode: a BodyReadable event is outstanding.
    notified: bool,
}

impl RequestStream {
    /// Request bytes the connection holds for this stream (budgeted).
    fn held(&self) -> usize {
        match self.phase {
            Phase::Delivered | Phase::Tunnel => self.reader.buffered(),
            _ => self.body.len() + self.reader.buffered(),
        }
    }
}

/// A server-side HTTP/3 connection.
pub struct ServerConnection {
    core: Core,
    id: u64,
    requests: BTreeMap<u64, RequestStream>,
    ready: VecDeque<Event>,
    leaks: LeakSink,
    /// GOAWAY we sent: requests at or above this ID are refused.
    goaway_sent: Option<u64>,
    /// The next client bidi stream ID we expect, for GOAWAY.
    next_request_id: u64,
    goaway_reported: Option<u64>,
    /// A request stream could not be read for lack of connection budget.
    budget_blocked: bool,
}

fn next_conn_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl ServerConnection {
    pub fn new(cfg: H3Config) -> Self {
        Self {
            core: Core::new(cfg, true),
            id: next_conn_id(),
            requests: BTreeMap::new(),
            ready: VecDeque::new(),
            leaks: Arc::new(Mutex::new(Vec::new())),
            goaway_sent: None,
            next_request_id: 0,
            goaway_reported: None,
            budget_blocked: false,
        }
    }

    pub fn stats(&self) -> ConnectionStats {
        self.core.stats
    }
    pub fn session_stats(&self) -> SessionStats {
        self.core.sessions.stats
    }
    pub fn error(&self) -> Option<ConnectionError> {
        self.core.error
    }
    pub fn peer_settings(&self) -> Option<Settings> {
        self.core.peer_settings
    }
    /// The SETTINGS sent (after transport capability adjustments).
    pub fn sent_settings(&self) -> Settings {
        self.core.sent_settings
    }
    pub fn encoder(&self) -> &Encoder {
        &self.core.encoder
    }
    pub fn decoder(&self) -> &Decoder {
        &self.core.decoder
    }
    /// Request streams not yet finished (received or answered).
    pub fn open_requests(&self) -> usize {
        self.requests.len()
    }
    /// The RFC 9218 priority the scheduler uses for a request's response.
    pub fn priority(&self, stream_id: u64) -> Priority {
        self.core
            .priorities
            .get(&stream_id)
            .copied()
            .unwrap_or_default()
    }
    /// Request bytes the connection holds now, against
    /// [`H3Config::max_connection_buffer`].
    pub fn buffered_request_bytes(&self) -> usize {
        self.requests.values().map(RequestStream::held).sum()
    }
    /// WebTransport streams open in any session.
    pub fn webtransport_streams(&self) -> usize {
        self.core.wt_streams()
    }

    /// Drives the connection: opens our streams, reads every stream,
    /// processes frames, delivers complete requests, writes queued bytes.
    pub fn poll<T: Transport>(&mut self, t: &mut T) {
        if self.core.error.is_some() {
            return;
        }
        if let Err(e) = self.poll_inner(t) {
            self.core.fail(t, e);
        }
        self.core.flush(t);
        for (id, code) in std::mem::take(&mut self.core.write_failures) {
            if let Some(rs) = self.requests.get_mut(&id)
                && rs.reply != Reply::Gone
            {
                rs.reply = Reply::Gone;
                rs.fin = true;
                if rs.phase == Phase::Tunnel {
                    self.core
                        .end_session(t, id, 0, Vec::new(), Some(code), true);
                }
                self.ready.push_back(Event::ResponseAbandoned {
                    stream_id: id,
                    code,
                });
            }
        }
        self.forward_session_events();
        self.retire();
    }

    fn poll_inner<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        if !self.core.open_local(t)? {
            return Ok(());
        }
        while let Some(id) = t.accept() {
            if is_uni(id) {
                self.core.accept_uni(t, id);
            } else {
                self.accept_request(t, id)?;
            }
        }
        self.core.read_uni(t, true)?;
        if let Some(g) = self.core.goaway_received
            && self.goaway_reported != Some(g)
        {
            self.goaway_reported = Some(g);
            self.ready.push_back(Event::GoAway(g));
        }
        while let Some(r) = self.core.decoder.poll_unblocked() {
            self.unblocked(t, r)?;
        }
        self.budget_blocked = false;
        let ids: Vec<u64> = self.requests.keys().copied().collect();
        for id in ids {
            self.read_request(t, id)?;
        }
        if self.budget_blocked {
            self.relieve_budget(t);
        }
        self.core.poll_datagrams(t)?;
        self.drain_leaks(t);
        Ok(())
    }

    /// The connection buffer is exhausted. If nothing the application can
    /// drain holds it (streaming bodies), whole bodies are deadlocked:
    /// reset the newest one holding bytes.
    fn relieve_budget<T: Transport>(&mut self, t: &mut T) {
        if self.buffered_request_bytes() < self.core.cfg.max_connection_buffer {
            return;
        }
        let streaming: usize = self
            .requests
            .values()
            .filter(|rs| rs.phase == Phase::Streaming)
            .map(RequestStream::held)
            .sum();
        if streaming > 0 {
            return;
        }
        let victim = self
            .requests
            .iter()
            .rev()
            .find(|(_, rs)| !rs.phase.handed_out() && rs.held() > 0)
            .map(|(id, _)| *id);
        if let Some(id) = victim {
            self.core.stats.budget_resets += 1;
            self.malformed(t, id, Code::EXCESSIVE_LOAD);
        }
    }

    /// Moves session events into the application's queue.
    fn forward_session_events(&mut self) {
        while let Some(e) = self.core.sessions.events.pop_front() {
            self.ready.push_back(Event::Session(e));
        }
    }

    /// The next event.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.forward_session_events();
        self.ready.pop_front()
    }

    /// Starts a graceful shutdown: GOAWAY with the first request stream ID
    /// that will not be processed. Requests already received continue.
    pub fn shutdown<T: Transport>(&mut self, t: &mut T) {
        let id = self.next_request_id;
        if self.goaway_sent.is_none_or(|g| id < g) {
            self.goaway_sent = Some(id);
            self.core.send_goaway(id);
            self.core.flush(t);
        }
    }

    fn accept_request<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        if is_server_initiated(id) {
            return Err(ConnectionError::BadStreamId(id));
        }
        self.next_request_id = self.next_request_id.max(id + 4);
        self.core.next_peer_bidi = self.next_request_id;
        if !goaway_allows(self.goaway_sent, id)
            || self.requests.len() >= self.core.cfg.max_concurrent_requests
        {
            t.reset(id, Code::REQUEST_REJECTED);
            t.stop_sending(id, Code::REQUEST_REJECTED);
            self.core.stats.rejected += 1;
            self.core.drop_held_for(t, id);
            self.ready.push_back(Event::RequestFailed {
                stream_id: id,
                code: Code::REQUEST_REJECTED,
            });
            return Ok(());
        }
        let max = self.core.cfg.max_frame_payload;
        self.requests.insert(
            id,
            RequestStream {
                reader: if self.core.sent_settings.webtransport_max_sessions.is_some() {
                    FrameReader::with_webtransport(max)
                } else {
                    FrameReader::new(max)
                },
                phase: Phase::Headers,
                head: None,
                body: Vec::new(),
                body_total: 0,
                trailers: Vec::new(),
                fin: false,
                reply: Reply::None,
                head_request: false,
                framed: false,
                streaming: false,
                trailers_seen: false,
                body_end: false,
                notified: false,
            },
        );
        Ok(())
    }

    /// How many bytes stream `id` may read now: the scratch buffer, the
    /// streaming window and the connection budget.
    fn read_allowance(&self, id: u64, scratch: usize) -> usize {
        let Some(rs) = self.requests.get(&id) else {
            return 0;
        };
        // Capsules and trailers on handed-out streams are small and
        // bounded by the readers; they never wait for the budget.
        if matches!(rs.phase, Phase::Delivered | Phase::Tunnel) {
            return scratch;
        }
        let cfg = &self.core.cfg;
        let mut n = scratch.min(
            cfg.max_connection_buffer
                .saturating_sub(self.buffered_request_bytes()),
        );
        if rs.phase == Phase::Streaming {
            n = n.min(cfg.stream_body_window.saturating_sub(rs.held()));
        }
        n
    }

    fn read_request<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        let mut buf = std::mem::take(&mut self.core.scratch);
        let r = (|| {
            loop {
                let Some(rs) = self.requests.get(&id) else {
                    return Ok(());
                };
                // Blocked streams stay in flow control (RFC 9204 §2.1.2);
                // finished request halves need no reads.
                if matches!(rs.phase, Phase::Blocked { .. }) || rs.fin {
                    return Ok(());
                }
                let allowance = self.read_allowance(id, buf.len());
                let r = match t.read(id, &mut buf[..allowance]) {
                    Ok(r) => r,
                    Err(TransportError::Reset(code)) => {
                        self.peer_reset(t, id, code);
                        return Ok(());
                    }
                    Err(TransportError::NotReady) => return Ok(()),
                    Err(TransportError::UnknownStream) => {
                        // The transport retired it (both halves done).
                        return Ok(());
                    }
                    Err(e) => return Err(ConnectionError::Transport(e)),
                };
                if r.len > 0 {
                    let rs = self.requests.get_mut(&id).expect("present");
                    rs.reader.push(&buf[..r.len]);
                    self.request_frames(t, id)?;
                }
                if r.fin {
                    self.request_fin(t, id)?;
                    return Ok(());
                }
                if allowance == 0 {
                    if self
                        .requests
                        .get(&id)
                        .is_some_and(|rs| rs.phase != Phase::Streaming)
                    {
                        self.budget_blocked = true;
                    }
                    return Ok(());
                }
                if r.len == 0 {
                    return Ok(());
                }
            }
        })();
        self.core.scratch = buf;
        r
    }

    fn request_frames<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        loop {
            let Some(rs) = self.requests.get_mut(&id) else {
                return Ok(());
            };
            if matches!(rs.phase, Phase::Blocked { .. }) {
                return Ok(());
            }
            let f = match rs.reader.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => return Ok(()),
                Err(error @ FrameError::Http2Frame(_)) | Err(error @ FrameError::Malformed) => {
                    return Err(ConnectionError::Frame {
                        stream_id: id,
                        error,
                    });
                }
                Err(FrameError::TooLarge { .. }) => {
                    self.malformed(t, id, Code::EXCESSIVE_LOAD);
                    return Ok(());
                }
                Err(error) => {
                    return Err(ConnectionError::Frame {
                        stream_id: id,
                        error,
                    });
                }
            };
            let first = !rs.framed;
            rs.framed = true;
            let phase = rs.phase;
            match (f, phase) {
                (Frame::WebTransportStream(session), Phase::Headers)
                    if first && self.core.sent_settings.webtransport_max_sessions.is_some() =>
                {
                    let mut rs = self.requests.remove(&id).expect("present");
                    let rest = rs.reader.take_rest();
                    let gone = self.core.session_known_gone(session);
                    return self
                        .core
                        .incoming_wt_stream(t, id, session, true, rest, gone);
                }
                (Frame::Unknown(_), _) => self.core.stats.unknown_frames += 1,
                (Frame::Headers(block), Phase::Headers) => self.section(t, id, &block, false)?,
                (f @ (Frame::Headers(_) | Frame::Data(_)), Phase::Streaming)
                    if rs.trailers_seen =>
                {
                    return Err(ConnectionError::FrameUnexpected {
                        stream_id: id,
                        frame_type: f.frame_type(),
                    });
                }
                (Frame::Headers(block), Phase::Body | Phase::Streaming) => {
                    self.section(t, id, &block, true)?
                }
                (Frame::Data(d), Phase::Body | Phase::Streaming) => {
                    let cfg = &self.core.cfg;
                    if phase == Phase::Body
                        && d.len() > cfg.max_body_bytes.saturating_sub(rs.body.len())
                    {
                        self.malformed(t, id, Code::EXCESSIVE_LOAD);
                        return Ok(());
                    }
                    rs.body_total += d.len() as u64;
                    rs.body.extend_from_slice(&d);
                    if let Some(cl) = rs.head.as_ref().and_then(|h| h.content_length)
                        && rs.body_total > cl
                    {
                        self.malformed(t, id, Code::MESSAGE_ERROR);
                        return Ok(());
                    }
                    if phase == Phase::Streaming && !d.is_empty() && !rs.notified {
                        rs.notified = true;
                        self.ready.push_back(Event::BodyReadable { stream_id: id });
                    }
                }
                (Frame::Data(d), Phase::Tunnel) => {
                    if let Err(code) = self.core.session_data(t, id, &d) {
                        self.malformed(t, id, code);
                        return Ok(());
                    }
                }
                // Trailers on a CONNECT stream carry nothing we use.
                (Frame::Headers(_), Phase::Tunnel) => self.core.stats.unknown_frames += 1,
                (f, Phase::Delivered) if matches!(f, Frame::Data(_) | Frame::Headers(_)) => {
                    // Bytes after the request ended (FIN) cannot occur;
                    // after delivery only via a protocol violation.
                    return Err(ConnectionError::FrameUnexpected {
                        stream_id: id,
                        frame_type: f.frame_type(),
                    });
                }
                (f, _) => {
                    return Err(ConnectionError::FrameUnexpected {
                        stream_id: id,
                        frame_type: f.frame_type(),
                    });
                }
            }
        }
    }

    fn section<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        block: &[u8],
        trailers: bool,
    ) -> Result<(), ConnectionError> {
        match self.core.decoder.decode_section(id, block) {
            Ok(Decoded::Fields(s)) => self.fields(t, id, s, trailers),
            Ok(Decoded::Blocked) => {
                self.core.stats.blocked_sections += 1;
                if let Some(rs) = self.requests.get_mut(&id) {
                    rs.phase = Phase::Blocked { trailers };
                }
                Ok(())
            }
            Err(QpackError::FieldSectionTooLarge { .. }) => {
                self.malformed(t, id, Code::EXCESSIVE_LOAD);
                Ok(())
            }
            Err(e) => Err(ConnectionError::Qpack(e)),
        }
    }

    fn unblocked<T: Transport>(
        &mut self,
        t: &mut T,
        r: Result<FieldSection, QpackError>,
    ) -> Result<(), ConnectionError> {
        match r {
            Ok(s) => {
                let id = s.stream_id;
                let Some(rs) = self.requests.get_mut(&id) else {
                    return Ok(());
                };
                let Phase::Blocked { trailers } = rs.phase else {
                    return Ok(());
                };
                rs.phase = if !trailers {
                    Phase::Headers
                } else if rs.streaming {
                    Phase::Streaming
                } else {
                    Phase::Body
                };
                self.fields(t, id, s, trailers)?;
                // Frames that arrived behind the blocked section.
                self.request_frames(t, id)?;
                match self.requests.get(&id) {
                    Some(rs) if rs.fin && !matches!(rs.phase, Phase::Blocked { .. }) => {
                        self.request_fin(t, id)
                    }
                    _ => Ok(()),
                }
            }
            Err(QpackError::FieldSectionTooLarge { stream_id }) => {
                self.malformed(t, stream_id, Code::EXCESSIVE_LOAD);
                Ok(())
            }
            Err(e) => Err(ConnectionError::Qpack(e)),
        }
    }

    fn new_responder(&self, id: u64) -> Responder<HeadersPending> {
        Responder {
            stream_id: id,
            conn: self.id,
            sink: self.leaks.clone(),
            armed: true,
            _state: PhantomData,
        }
    }

    fn fields<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        s: FieldSection,
        trailers: bool,
    ) -> Result<(), ConnectionError> {
        let Some(rs) = self.requests.get_mut(&id) else {
            return Ok(());
        };
        if trailers {
            if message::check_trailers(&s.fields).is_err() {
                self.malformed(t, id, Code::MESSAGE_ERROR);
                return Ok(());
            }
            rs.trailers = s.fields;
            if rs.phase == Phase::Streaming {
                // Only unknown frames may follow; FIN ends the body.
                rs.trailers_seen = true;
            } else {
                rs.phase = Phase::Trailers;
            }
            return Ok(());
        }
        let head = match message::parse_request(s.fields) {
            Ok(head) => head,
            Err(MessageError::UnsupportedConnect) => {
                self.malformed(t, id, Code::REQUEST_REJECTED);
                return Ok(());
            }
            Err(_) => {
                self.malformed(t, id, Code::MESSAGE_ERROR);
                return Ok(());
            }
        };
        self.core.header_priority(id, &head.fields);
        if let Some(protocol) = &head.protocol {
            // RFC 9220 §3: :protocol only after we enabled it.
            if !self.core.sent_settings.enable_connect_protocol {
                self.malformed(t, id, Code::MESSAGE_ERROR);
                return Ok(());
            }
            let wt = protocol == "webtransport"
                && self.core.sent_settings.webtransport_max_sessions.is_some();
            if wt
                && self.core.live_webtransport_sessions() as u64
                    >= self
                        .core
                        .sent_settings
                        .webtransport_max_sessions
                        .unwrap_or(0)
            {
                self.malformed(t, id, Code::REQUEST_REJECTED);
                return Ok(());
            }
            let kind = if wt {
                SessionKind::WebTransport
            } else {
                SessionKind::Tunnel
            };
            self.core.add_session(id, kind);
            let rs = self.requests.get_mut(&id).expect("present");
            rs.phase = Phase::Tunnel;
            self.core.stats.requests += 1;
            let responder = self.new_responder(id);
            self.ready
                .push_back(Event::Connect(Box::new(ConnectRequest {
                    stream_id: id,
                    head,
                    kind,
                    responder,
                })));
            return Ok(());
        }
        // Streams that named this request stream as their session.
        self.core.drop_held_for(t, id);
        if head
            .content_length
            .is_some_and(|cl| cl > self.core.cfg.max_body_bytes as u64)
            && !self.core.cfg.streaming_request_bodies
        {
            self.malformed(t, id, Code::EXCESSIVE_LOAD);
            return Ok(());
        }
        let rs = self.requests.get_mut(&id).expect("present");
        rs.head_request = head.method == "HEAD";
        if self.core.cfg.streaming_request_bodies {
            rs.phase = Phase::Streaming;
            rs.streaming = true;
            rs.head = Some(RequestHead {
                fields: Vec::new(),
                ..head.clone()
            });
            self.core.stats.requests += 1;
            let responder = self.new_responder(id);
            self.ready
                .push_back(Event::StreamingRequest(Box::new(StreamingRequest {
                    stream_id: id,
                    head,
                    responder,
                })));
        } else {
            rs.head = Some(head);
            rs.phase = Phase::Body;
        }
        Ok(())
    }

    fn request_fin<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        let Some(rs) = self.requests.get_mut(&id) else {
            return Ok(());
        };
        rs.fin = true;
        // A blocked section holds frames behind it in the reader; the
        // frame boundary is checked once they are processed on unblock.
        if matches!(rs.phase, Phase::Blocked { .. }) {
            return Ok(());
        }
        if rs.reader.finish().is_err() {
            return Err(ConnectionError::Frame {
                stream_id: id,
                error: FrameError::Malformed,
            });
        }
        match rs.phase {
            // A blocked section completes later; FIN is remembered.
            Phase::Blocked { .. } => Ok(()),
            Phase::Headers => {
                // FIN before any request HEADERS: incomplete.
                self.malformed(t, id, Code::REQUEST_INCOMPLETE);
                Ok(())
            }
            Phase::Body | Phase::Trailers => self.deliver(t, id),
            Phase::Streaming => {
                if rs
                    .head
                    .as_ref()
                    .and_then(|h| h.content_length)
                    .is_some_and(|cl| cl != rs.body_total)
                {
                    self.malformed(t, id, Code::MESSAGE_ERROR);
                    return Ok(());
                }
                rs.body_end = true;
                if !rs.notified {
                    rs.notified = true;
                    self.ready.push_back(Event::BodyReadable { stream_id: id });
                }
                Ok(())
            }
            Phase::Tunnel => {
                // FIN on the CONNECT stream ends the session (a capsule
                // may have ended it already).
                let capsules_ok = self
                    .core
                    .sessions
                    .map
                    .get(&id)
                    .is_none_or(|s| s.capsules.finish().is_ok());
                if !capsules_ok {
                    self.malformed(t, id, Code::MESSAGE_ERROR);
                    return Ok(());
                }
                self.core.end_session(t, id, 0, Vec::new(), None, true);
                Ok(())
            }
            Phase::Delivered => Ok(()),
        }
    }

    fn deliver<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        let rs = self.requests.get_mut(&id).expect("present");
        let head = rs.head.take().expect("headers received");
        if head.content_length.is_some_and(|cl| cl != rs.body_total) {
            rs.head = Some(head);
            self.malformed(t, id, Code::MESSAGE_ERROR);
            return Ok(());
        }
        rs.phase = Phase::Delivered;
        let body = std::mem::take(&mut rs.body);
        let trailers = std::mem::take(&mut rs.trailers);
        self.core.stats.requests += 1;
        let responder = self.new_responder(id);
        self.ready.push_back(Event::Request(Box::new(Request {
            stream_id: id,
            responder,
            head,
            body,
            trailers,
        })));
        Ok(())
    }

    /// Reads body bytes of a streaming request. Reading frees window and
    /// budget, so the connection reads more from the stream at once.
    /// `Err(StreamGone)` if the request failed or was reset (an event said
    /// so) or is not a streaming request.
    pub fn read_body<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        out: &mut [u8],
    ) -> Result<BodyRead, SendError> {
        let rs = match self.requests.get_mut(&stream_id) {
            Some(rs) if rs.phase == Phase::Streaming && rs.reply != Reply::Gone => rs,
            _ => return Err(SendError::StreamGone),
        };
        let n = out.len().min(rs.body.len());
        out[..n].copy_from_slice(&rs.body[..n]);
        rs.body.drain(..n);
        if rs.body.is_empty() {
            rs.notified = false;
        }
        let end = rs.body_end && rs.body.is_empty();
        if n > 0 && !rs.fin {
            if let Err(e) = self.read_request(t, stream_id) {
                self.core.fail(t, e);
            }
            // New data after this read gets a new event.
            if let Some(rs) = self.requests.get_mut(&stream_id)
                && rs.body.is_empty()
                && !rs.body_end
            {
                rs.notified = false;
            }
            self.core.flush(t);
        }
        Ok(BodyRead { len: n, end })
    }

    /// The trailers of a streaming request, once its body ended.
    pub fn take_trailers(&mut self, stream_id: u64) -> Vec<Field> {
        self.requests
            .get_mut(&stream_id)
            .map(|rs| std::mem::take(&mut rs.trailers))
            .unwrap_or_default()
    }

    /// A stream error: reset and stop the stream, cancel its QPACK state.
    fn malformed<T: Transport>(&mut self, t: &mut T, id: u64, code: Code) {
        self.core.stats.malformed += 1;
        self.core.decoder.cancel_stream(id);
        t.stop_sending(id, code);
        t.reset(id, code);
        self.core.stats.resets_sent += 1;
        self.core.discard(id);
        if let Some(rs) = self.requests.remove(&id) {
            // Cannot happen: whole requests are not parsed after delivery.
            debug_assert!(rs.phase != Phase::Delivered, "malformed after delivery");
            if rs.phase == Phase::Tunnel {
                self.core
                    .end_session(t, id, 0, Vec::new(), Some(code.0), true);
                self.core.forget_session(id);
            }
        }
        self.core.forget_priority(id);
        self.ready.push_back(Event::RequestFailed {
            stream_id: id,
            code,
        });
    }

    fn peer_reset<T: Transport>(&mut self, t: &mut T, id: u64, code: u64) {
        self.core.stats.resets_received += 1;
        self.core.decoder.cancel_stream(id);
        let Some(rs) = self.requests.get_mut(&id) else {
            return;
        };
        if rs.phase == Phase::Tunnel {
            self.core
                .end_session(t, id, 0, Vec::new(), Some(code), true);
        }
        if rs.phase.handed_out() {
            // The application holds the responder; let it finish (no-op).
            rs.fin = true;
            rs.body.clear();
            if !matches!(rs.reply, Reply::Finished) {
                rs.reply = Reply::Gone;
                self.core.discard(id);
                t.reset(id, Code::REQUEST_CANCELLED);
                self.ready.push_back(Event::ResponseAbandoned {
                    stream_id: id,
                    code,
                });
            }
        } else {
            self.requests.remove(&id);
            t.reset(id, Code::REQUEST_CANCELLED);
            self.ready.push_back(Event::RequestFailed {
                stream_id: id,
                code: Code(code),
            });
        }
    }

    fn drain_leaks<T: Transport>(&mut self, t: &mut T) {
        let leaked: Vec<u64> = match self.leaks.lock() {
            Ok(mut v) => std::mem::take(&mut *v),
            Err(_) => Vec::new(),
        };
        for id in leaked {
            self.core.stats.leaked_responders += 1;
            self.reset_request(t, id, Code::INTERNAL_ERROR);
        }
    }

    /// Forgets request streams whose response is complete and flushed.
    fn retire(&mut self) {
        let core = &self.core;
        let mut gone = Vec::new();
        self.requests.retain(|id, rs| {
            let done = rs.fin
                && matches!(rs.reply, Reply::Finished | Reply::Gone)
                && core.queued(*id) == 0;
            if done {
                gone.push(*id);
            }
            !done
        });
        for id in gone {
            self.core.forget_session(id);
            self.core.forget_priority(id);
        }
    }

    fn check_responder(&self, conn: u64, id: u64) -> Result<(), SendError> {
        if conn != self.id {
            return Err(SendError::Foreign);
        }
        match self.requests.get(&id) {
            Some(rs) if rs.reply != Reply::Gone && self.core.error.is_none() => Ok(()),
            _ => Err(SendError::StreamGone),
        }
    }

    fn check_fields(fields: &[Field]) -> Result<(), SendError> {
        for f in fields {
            if f.name.first() == Some(&b':') {
                return Err(SendError::InvalidResponse(
                    MessageError::UnknownPseudoHeader,
                ));
            }
            if f.name == b"content-length" {
                return Err(SendError::InvalidResponse(
                    MessageError::InvalidContentLength,
                ));
            }
        }
        message::check_trailers(fields).map_err(SendError::InvalidResponse)
    }

    fn send_interim<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        status: u16,
        fields: Vec<Field>,
    ) -> Result<(), SendError> {
        if !(100..=199).contains(&status) || status == 101 {
            return Err(SendError::InvalidResponse(MessageError::InvalidStatus));
        }
        Self::check_fields(&fields)?;
        let mut list = Vec::with_capacity(fields.len() + 1);
        list.push(Field::new(":status", status.to_string()));
        list.extend(fields);
        let budget = self
            .core
            .cfg
            .max_stream_buffer
            .saturating_sub(self.core.queued(id));
        if headers_frame_bound(&list) > budget {
            return Err(SendError::BufferFull);
        }
        let block = self.core.encoder.encode(id, &list);
        let mut out = Vec::with_capacity(block.len() + 8);
        frame::write_frame(&mut out, frame::HEADERS, &block);
        self.core.queue(id, &out);
        self.core.stats.interim_responses += 1;
        self.core.flush(t);
        Ok(())
    }

    fn send_response_headers<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        status: u16,
        fields: Vec<Field>,
        content_length: Option<u64>,
        reserve: usize,
    ) -> Result<(), SendError> {
        if !(200..=599).contains(&status) {
            return Err(SendError::InvalidResponse(MessageError::InvalidStatus));
        }
        let mut list = Vec::with_capacity(fields.len() + 2);
        list.push(Field::new(":status", status.to_string()));
        Self::check_fields(&fields)?;
        list.extend(fields);
        let bodyless = matches!(status, 204 | 304);
        if let Some(cl) = content_length
            && !(status == 204 || (status == 304 && cl == 0))
        {
            list.push(Field::new("content-length", cl.to_string()));
        }
        if bodyless && content_length.is_some_and(|cl| cl > 0) {
            return Err(SendError::InvalidResponse(
                MessageError::ContentLengthMismatch,
            ));
        }
        // Checked before encoding: an encoded section is registered with the
        // QPACK encoder (it pins dynamic entries until acknowledged), so it
        // must not be discarded. `reserve` is room the caller needs after
        // the HEADERS frame, so a whole response is refused up front.
        let budget = self
            .core
            .cfg
            .max_stream_buffer
            .saturating_sub(self.core.queued(id));
        if headers_frame_bound(&list).saturating_add(reserve) > budget {
            return Err(SendError::BufferFull);
        }
        let block = self.core.encoder.encode(id, &list);
        let mut out = Vec::with_capacity(block.len() + 8);
        frame::write_frame(&mut out, frame::HEADERS, &block);
        self.core.queue(id, &out);
        let rs = self.requests.get_mut(&id).expect("checked");
        rs.reply = Reply::Headers {
            content_length: if bodyless { Some(0) } else { content_length },
            sent: 0,
        };
        self.core.flush(t);
        Ok(())
    }

    fn send_whole_response<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        response: Response,
    ) -> Result<(), SendError> {
        let head = self.requests.get(&id).is_some_and(|rs| rs.head_request);
        let wire_body = if head || matches!(response.status, 204 | 304) {
            0
        } else {
            response.body.len()
        };
        if matches!(response.status, 204 | 304) && !response.body.is_empty() {
            return Err(SendError::InvalidResponse(
                MessageError::ContentLengthMismatch,
            ));
        }
        let reserve = if wire_body > 0 { wire_body + 9 } else { 0 };
        self.send_response_headers(
            t,
            id,
            response.status,
            response.fields,
            Some(response.body.len() as u64),
            reserve,
        )?;
        if wire_body > 0 {
            self.send_response_data(t, id, &response.body)?;
        }
        self.finish_response(t, id, Vec::new())
    }

    fn send_response_data<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        data: &[u8],
    ) -> Result<(), SendError> {
        let rs = self.requests.get_mut(&id).expect("checked");
        if rs.head_request && !data.is_empty() {
            return Err(SendError::InvalidResponse(
                MessageError::ContentLengthMismatch,
            ));
        }
        let Reply::Headers {
            content_length,
            sent,
        } = &mut rs.reply
        else {
            return Err(SendError::StreamGone);
        };
        if data.is_empty() {
            return Ok(());
        }
        let total = *sent + data.len() as u64;
        if content_length.is_some_and(|cl| total > cl) {
            return Err(SendError::InvalidResponse(
                MessageError::ContentLengthMismatch,
            ));
        }
        let budget = self
            .core
            .cfg
            .max_stream_buffer
            .saturating_sub(self.core.queued(id));
        if data.len() + 9 > budget {
            return Err(SendError::BufferFull);
        }
        *sent = total;
        let mut hdr = Vec::with_capacity(9);
        frame::write_data_header(&mut hdr, data.len());
        self.core.queue(id, &hdr);
        self.core.queue(id, data);
        self.core.flush(t);
        Ok(())
    }

    fn finish_response<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        trailers: Vec<Field>,
    ) -> Result<(), SendError> {
        let rs = self.requests.get_mut(&id).expect("checked");
        let Reply::Headers {
            content_length,
            sent,
            ..
        } = rs.reply
        else {
            return Err(SendError::StreamGone);
        };
        if !rs.head_request && content_length.is_some_and(|cl| cl != sent) {
            return Err(SendError::InvalidResponse(
                MessageError::ContentLengthMismatch,
            ));
        }
        if !trailers.is_empty() {
            message::check_trailers(&trailers).map_err(SendError::InvalidResponse)?;
            let block = self.core.encoder.encode(id, &trailers);
            let mut out = Vec::new();
            frame::write_frame(&mut out, frame::HEADERS, &block);
            self.core.queue(id, &out);
        }
        rs.reply = Reply::Finished;
        let phase = rs.phase;
        if !rs.fin && phase != Phase::Tunnel {
            // Answered before the request ended: the rest of the body is
            // not needed (RFC 9114 §4.1: STOP_SENDING with H3_NO_ERROR).
            rs.fin = true;
            rs.body.clear();
            t.stop_sending(id, Code::NO_ERROR);
            self.core.decoder.cancel_stream(id);
        }
        if phase == Phase::Tunnel {
            // A refused extended CONNECT: the session never started.
            self.core.end_session(t, id, 0, Vec::new(), None, false);
            if !self.requests.get(&id).is_some_and(|rs| rs.fin) {
                t.stop_sending(id, Code::NO_ERROR);
                if let Some(rs) = self.requests.get_mut(&id) {
                    rs.fin = true;
                }
            }
        }
        self.core.queue_fin(id);
        self.core.stats.responses += 1;
        self.core.flush(t);
        Ok(())
    }

    fn close_session<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        code: u32,
        reason: &[u8],
    ) -> Result<Option<Code>, SendError> {
        if reason.len() > crate::capsule::MAX_CLOSE_REASON || std::str::from_utf8(reason).is_err() {
            return Err(SendError::BadCloseReason);
        }
        let live = self
            .requests
            .get(&id)
            .is_some_and(|rs| rs.reply != Reply::Gone && rs.reply != Reply::Finished)
            && self.core.error.is_none();
        if !live {
            // The stream is gone (reset by the peer, or the connection
            // failed): nothing to send; the obligation is discharged.
            self.core.end_session(t, id, 0, Vec::new(), None, false);
            return Ok(Some(Code::REQUEST_CANCELLED));
        }
        self.core
            .queue_session_close(id, code, reason)
            .map_err(|_| SendError::BadCloseReason)?;
        self.core.end_session(t, id, code, Vec::new(), None, false);
        let rs = self.requests.get_mut(&id).expect("live");
        rs.reply = Reply::Finished;
        self.core.stats.responses += 1;
        self.core.flush(t);
        Ok(None)
    }

    fn reset_request<T: Transport>(&mut self, t: &mut T, id: u64, code: Code) {
        self.core.discard(id);
        if let Some(rs) = self.requests.get_mut(&id) {
            if rs.reply != Reply::Gone {
                t.reset(id, code);
                if !rs.fin {
                    t.stop_sending(id, code);
                    self.core.decoder.cancel_stream(id);
                }
                self.core.stats.resets_sent += 1;
            }
            rs.reply = Reply::Gone;
            rs.fin = true;
            rs.body.clear();
            if rs.phase == Phase::Tunnel {
                self.core
                    .end_session(t, id, 0, Vec::new(), Some(code.0), false);
            }
        }
    }

    fn session_check(&self, s: &Session) -> Result<u64, SessionError> {
        if s.conn != self.id {
            return Err(SessionError::Foreign);
        }
        Ok(s.id)
    }

    /// Opens a WebTransport stream in `session`.
    pub fn open_stream<T: Transport>(
        &mut self,
        t: &mut T,
        session: &Session,
        bidi: bool,
    ) -> Result<u64, SessionError> {
        let id = self.session_check(session)?;
        self.core.open_wt_stream(t, id, bidi)
    }

    /// Sends an HTTP Datagram in `session`.
    pub fn send_datagram<T: Transport>(
        &mut self,
        t: &mut T,
        session: &Session,
        payload: &[u8],
    ) -> Result<DatagramPath, SessionError> {
        let id = self.session_check(session)?;
        self.core.send_session_datagram(t, id, payload)
    }

    /// Writes WebTransport stream data; returns how many bytes the
    /// transport took (0 under flow control).
    pub fn write_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        data: &[u8],
    ) -> Result<usize, SessionError> {
        self.core.wt_write(t, stream_id, data)
    }

    pub fn finish_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
    ) -> Result<(), SessionError> {
        self.core.wt_finish(t, stream_id)
    }

    pub fn read_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        out: &mut [u8],
    ) -> Result<ReadOutcome, SessionError> {
        self.core.wt_read(t, stream_id, out)
    }

    /// RESET_STREAM with WebTransport application error `code`.
    pub fn reset_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        self.core.wt_reset(t, stream_id, code)
    }

    /// STOP_SENDING with WebTransport application error `code`.
    pub fn stop_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        self.core.wt_stop_sending(t, stream_id, code)
    }

    /// The session a WebTransport stream belongs to.
    pub fn stream_session(&self, stream_id: u64) -> Option<u64> {
        self.core.wt_session_of(stream_id)
    }
}

/// An upper bound on the HEADERS frame the QPACK encoder produces for
/// `fields`: every representation is at most a literal name and value
/// (one prefix byte, two varint lengths of at most 10 bytes, raw bytes;
/// Huffman is used only when shorter), behind a section prefix of two
/// integers and a frame header of type and 8-byte length.
fn headers_frame_bound(fields: &[Field]) -> usize {
    fields.iter().fold(20 + 9, |n, f| {
        n.saturating_add(21 + f.name.len() + f.value.len())
    })
}
