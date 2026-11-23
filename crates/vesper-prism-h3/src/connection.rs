//! A sans-I/O HTTP/3 connection (RFC 9114) over any [`Transport`].
//!
//! All shared connection state, control/QPACK unidirectional stream routing,
//! outgoing flush scheduling, session/WebTransport/datagram machinery, and
//! request-stream state live in Vesper (`vesper/src/{core,server,session}.ves`).
//! [`ServerConnection`] holds the Vesper handle alongside [`Encoder`]/[`Decoder`]
//! and drives [`Transport`] I/O through `port/binding.rs`.
//!
//! A response is an obligation: each request is handed out with a linear
//! [`Responder<HeadersPending>`] whose only exits are consuming calls that
//! complete the response ([`Responder::respond`], [`Responder::finish`]) or
//! reset the stream ([`Responder::reset`]). Dropping one is detected: the
//! connection resets that stream with H3_INTERNAL_ERROR on its next poll
//! and counts it in [`ConnectionStats::leaked_responders`].

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use crate::error::Code;
use crate::frame::{FrameError, Settings};
use crate::message::{MessageError, RequestHead};
use crate::priority::Priority;
use crate::qpack::port::{abi, binding, huff};
use crate::qpack::{Decoder, DecoderConfig, Encoder, EncoderConfig, Field, QpackError};
use crate::session::{DatagramPath, SessionError, SessionEvent, SessionKind, SessionStats};
use crate::transport::{ReadOutcome, Transport, TransportError};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// then FIN. `content_length` is added from the body (omitted for 204
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
        if let Err(e) = conn.accept_session_send(t, self.stream_id, &fields) {
            return Err((self, e));
        }
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

// ---------------------------------------------------------------------
// Shared Vesper port drivers used by both ServerConnection and ClientConnection
// ---------------------------------------------------------------------

const CMD_STOP_SENDING: u64 = 1;
const CMD_RESET: u64 = 2;
const CMD_FINISH: u64 = 3;
const CMD_CLOSE: u64 = 4;
const CMD_SET_PRIORITY: u64 = 5;

const SEV_STREAM: u64 = 1;
const SEV_DATAGRAM: u64 = 2;
const SEV_DRAINING: u64 = 3;
const SEV_CLOSED: u64 = 4;
const SEV_STRIDE: usize = 6;

const FLUSH_OK: u64 = 0;
const FLUSH_BLOCKED: u64 = 1;
const FLUSH_RESET: u64 = 2;
const FLUSH_NOT_READY: u64 = 3;
const FLUSH_UNKNOWN: u64 = 4;
const FLUSH_CLOSED: u64 = 5;
const FLUSH_UNSUPPORTED: u64 = 6;
const FLUSH_TOO_LARGE: u64 = 7;

pub(crate) fn cfg_to_abi(cfg: H3Config) -> abi::H3ConfigAbi {
    abi::H3ConfigAbi {
        settings: cfg.settings,
        encoder: cfg.encoder,
        max_frame_payload: cfg.max_frame_payload as u64,
        max_body_bytes: cfg.max_body_bytes as u64,
        max_concurrent_requests: cfg.max_concurrent_requests as u64,
        max_peer_uni_streams: cfg.max_peer_uni_streams as u64,
        max_blocked_bytes: cfg.max_blocked_bytes as u64,
        max_stream_buffer: cfg.max_stream_buffer as u64,
        streaming_request_bodies: cfg.streaming_request_bodies,
        stream_body_window: cfg.stream_body_window as u64,
        max_connection_buffer: cfg.max_connection_buffer as u64,
        incremental_quantum: cfg.incremental_quantum as u64,
        work_conserving: cfg.work_conserving,
    }
}

pub(crate) fn decoder_config_of(cfg: &H3Config) -> DecoderConfig {
    let s = cfg.settings;
    DecoderConfig {
        max_table_capacity: s.qpack_max_table_capacity,
        max_blocked_streams: s.qpack_blocked_streams,
        max_field_section_size: s.max_field_section_size.unwrap_or(u64::MAX),
        max_blocked_bytes: cfg.max_blocked_bytes,
    }
}

pub(crate) fn flatten_fields(fields: &[Field], bytes: &mut Vec<u8>, meta: &mut Vec<u64>) {
    bytes.clear();
    meta.clear();
    meta.reserve(fields.len() * 3);
    for f in fields {
        bytes.extend_from_slice(&f.name);
        bytes.extend_from_slice(&f.value);
        meta.extend_from_slice(&[
            f.name.len() as u64,
            f.value.len() as u64,
            u64::from(f.sensitive),
        ]);
    }
}

pub(crate) fn decode_fields(
    bytes: &[u8],
    meta: &[u64],
    moff: usize,
    count: usize,
    mut at: usize,
) -> Vec<Field> {
    let mut out = Vec::with_capacity(count);
    for m in meta[moff * 3..(moff + count) * 3].as_chunks::<3>().0 {
        let nl = m[0] as usize;
        let vl = m[1] as usize;
        out.push(Field {
            name: bytes[at..at + nl].to_vec(),
            value: bytes[at + nl..at + nl + vl].to_vec(),
            sensitive: m[2] != 0,
        });
        at += nl + vl;
    }
    out
}

pub(crate) fn text(bytes: &[u8], lo: u64, hi: u64) -> String {
    String::from_utf8(bytes[lo as usize..hi as usize].to_vec()).expect("vesper validated UTF-8")
}

pub(crate) fn execute_cmds<T: Transport>(conn: &binding::ConnCoreRef<'_>, t: &mut T) -> bool {
    let cmd = conn.cmd();
    let kinds = cmd.kinds().as_slice();
    if kinds.is_empty() {
        return false;
    }
    let a = cmd.a().as_slice();
    let b = cmd.b().as_slice();
    for i in 0..kinds.len() {
        match kinds[i] {
            CMD_STOP_SENDING => t.stop_sending(a[i], Code(b[i])),
            CMD_RESET => t.reset(a[i], Code(b[i])),
            CMD_FINISH => {
                let _ = t.finish(a[i]);
            }
            CMD_CLOSE => t.close(Code(a[i])),
            CMD_SET_PRIORITY => t.set_priority(a[i], b[i] as u8, (b[i] >> 8) != 0),
            _ => {}
        }
    }
    true
}

pub(crate) fn decode_session_events(
    sev: &binding::vesper__prism_2h3__pool__SevBufRef<'_>,
    mut emit: impl FnMut(SessionEvent),
) {
    let kinds = sev.kinds().as_slice();
    if kinds.is_empty() {
        return;
    }
    let words = sev.words().as_slice();
    let bytes = sev.bytes().as_slice();
    for (i, &kind) in kinds.iter().enumerate() {
        let w = &words[i * SEV_STRIDE..(i + 1) * SEV_STRIDE];
        match kind {
            SEV_STREAM => emit(SessionEvent::Stream {
                session: w[0],
                stream_id: w[1],
                bidi: w[2] != 0,
            }),
            SEV_DATAGRAM => emit(SessionEvent::Datagram {
                session: w[0],
                payload: bytes[w[1] as usize..(w[1] + w[2]) as usize].to_vec(),
            }),
            SEV_DRAINING => emit(SessionEvent::Draining { session: w[0] }),
            SEV_CLOSED => emit(SessionEvent::Closed {
                session: w[0],
                code: w[1] as u32,
                reason: bytes[w[2] as usize..(w[2] + w[3]) as usize].to_vec(),
                reset: (w[4] != 0).then_some(w[5]),
            }),
            _ => {}
        }
    }
}

pub(crate) fn transport_err_to_flush(e: TransportError) -> (u64, u64, u64) {
    match e {
        TransportError::Reset(code) => (FLUSH_RESET, 0, code),
        TransportError::NotReady => (FLUSH_NOT_READY, 0, 0),
        TransportError::UnknownStream => (FLUSH_UNKNOWN, 0, 0),
        TransportError::Closed => (FLUSH_CLOSED, 0, 0),
        TransportError::Unsupported => (FLUSH_UNSUPPORTED, 0, 0),
        TransportError::TooLarge => (FLUSH_TOO_LARGE, 0, 0),
    }
}

pub(crate) enum RoleCore<'a> {
    Server(&'a mut binding::ServerCore),
    Client(&'a mut binding::ClientCore),
}

impl RoleCore<'_> {
    fn conn(&self) -> binding::ConnCoreRef<'_> {
        match self {
            Self::Server(s) => s.as_ref().conn(),
            Self::Client(c) => c.as_ref().conn(),
        }
    }

    fn clear_cmds(&mut self) {
        match self {
            Self::Server(s) => s.clear_cmds(),
            Self::Client(c) => c.clear_cmds(),
        }
    }

    fn run_cmds<T: Transport>(&mut self, t: &mut T) {
        if execute_cmds(&self.conn(), t) {
            self.clear_cmds();
        }
    }

    pub(crate) fn flush<T: Transport>(&mut self, t: &mut T, enc: &mut Encoder, dec: &mut Decoder) {
        let mut step = match self {
            Self::Server(s) => s.flush_begin(&mut enc.core, &mut dec.core),
            Self::Client(c) => c.flush_begin(&mut enc.core, &mut dec.core),
        };
        self.run_cmds(t);
        while let abi::FlushStep::Write {
            stream_id,
            off,
            len,
        } = step
        {
            let (status, wrote, err_code) = {
                let conn = self.conn();
                let data = &conn.pool().data().as_slice()[off as usize..(off + len) as usize];
                match t.write(stream_id, data) {
                    Ok(0) => (FLUSH_BLOCKED, 0, 0),
                    Ok(n) => (FLUSH_OK, n as u64, 0),
                    Err(e) => transport_err_to_flush(e),
                }
            };
            step = match self {
                Self::Server(s) => s.flush_step(status, wrote, err_code),
                Self::Client(c) => c.flush_step(status, wrote, err_code),
            };
        }
        self.run_cmds(t);
    }

    pub(crate) fn open_local<T: Transport>(&mut self, t: &mut T) -> Result<bool, ConnectionError> {
        if self.conn().has_local() {
            return Ok(true);
        }
        while !self.conn().has_local() {
            match t.open_uni() {
                Ok(id) => {
                    let has_dgram = t.max_datagram_size().is_some();
                    let ready = match self {
                        Self::Server(s) => s.open_local_push(id, has_dgram),
                        Self::Client(c) => c.open_local_push(id, has_dgram),
                    };
                    if ready {
                        return Ok(true);
                    }
                }
                Err(TransportError::NotReady) => return Ok(false),
                Err(e) => return Err(ConnectionError::Transport(e)),
            }
        }
        Ok(true)
    }

    pub(crate) fn read_uni<T: Transport>(
        &mut self,
        t: &mut T,
        enc: &mut Encoder,
        dec: &mut Decoder,
        rx_scratch: &mut [u8],
        tmp_ids: &mut Vec<u64>,
    ) -> Result<(), ConnectionError> {
        tmp_ids.clear();
        tmp_ids.extend_from_slice(self.conn().uni().ids().as_slice());
        for &id in tmp_ids.iter() {
            loop {
                let r = match t.read(id, rx_scratch) {
                    Ok(r) => r,
                    Err(TransportError::Reset(_)) | Err(TransportError::UnknownStream) => {
                        let res = match self {
                            Self::Server(s) => s.uni_closed(id),
                            Self::Client(c) => c.uni_closed(id),
                        };
                        self.run_cmds(t);
                        res?;
                        break;
                    }
                    Err(TransportError::NotReady) => break,
                    Err(e) => return Err(ConnectionError::Transport(e)),
                };
                if r.len > 0 || r.fin {
                    let res = match self {
                        Self::Server(s) => s.uni_bytes(
                            &mut enc.core,
                            &mut dec.core,
                            huff(),
                            id,
                            &rx_scratch[..r.len],
                            r.fin,
                        ),
                        Self::Client(c) => c.uni_bytes(
                            &mut enc.core,
                            &mut dec.core,
                            huff(),
                            id,
                            &rx_scratch[..r.len],
                            r.fin,
                        ),
                    };
                    self.run_cmds(t);
                    res?;
                    if r.len > 0 && !self.conn().uni().ids().as_slice().contains(&id) {
                        break;
                    }
                }
                if r.fin || r.len == 0 {
                    break;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn wt_open<T: Transport>(
        &mut self,
        t: &mut T,
        enc: &mut Encoder,
        dec: &mut Decoder,
        session: u64,
        bidi: bool,
    ) -> Result<u64, SessionError> {
        match self {
            Self::Server(s) => s.wt_can_open(session)?,
            Self::Client(c) => c.wt_can_open(session)?,
        }
        let id = if bidi { t.open_bidi() } else { t.open_uni() }.map_err(|e| match e {
            TransportError::NotReady => SessionError::NotReady,
            _ => SessionError::Connection,
        })?;
        match self {
            Self::Server(s) => s.wt_register_opened(id, session, bidi),
            Self::Client(c) => c.wt_register_opened(id, session, bidi),
        }
        self.flush(t, enc, dec);
        Ok(id)
    }

    pub(crate) fn wt_send_datagram<T: Transport>(
        &mut self,
        t: &mut T,
        enc: &mut Encoder,
        dec: &mut Decoder,
        session: u64,
        payload: &[u8],
    ) -> Result<DatagramPath, SessionError> {
        let max_dgram = t.max_datagram_size().map(|m| m as u64);
        let plan = match self {
            Self::Server(s) => s.send_datagram_prepare(max_dgram, session, payload)?,
            Self::Client(c) => c.send_datagram_prepare(max_dgram, session, payload)?,
        };
        match plan {
            abi::DgramSendPlan::SendQuic => {
                send_quic_datagram(t, self.conn().scratch().as_slice())?;
                match self {
                    Self::Server(s) => s.on_quic_datagram_sent(),
                    Self::Client(c) => c.on_quic_datagram_sent(),
                }
                Ok(DatagramPath::Quic)
            }
            abi::DgramSendPlan::SentCapsule => {
                self.flush(t, enc, dec);
                Ok(DatagramPath::Capsule)
            }
        }
    }

    pub(crate) fn wt_write<T: Transport>(
        &mut self,
        t: &mut T,
        enc: &mut Encoder,
        dec: &mut Decoder,
        stream_id: u64,
        data: &[u8],
    ) -> Result<usize, SessionError> {
        let queued = match self {
            Self::Server(s) => s.wt_can_write(stream_id)?,
            Self::Client(c) => c.wt_can_write(stream_id)?,
        };
        if queued {
            self.flush(t, enc, dec);
            let still = match self {
                Self::Server(s) => s.wt_can_write(stream_id)?,
                Self::Client(c) => c.wt_can_write(stream_id)?,
            };
            if still {
                return Ok(0);
            }
        }
        match t.write(stream_id, data) {
            Ok(n) => Ok(n),
            Err(TransportError::Reset(code)) => Err(match self {
                Self::Server(s) => s.wt_on_write_reset(stream_id, code).unwrap_err(),
                Self::Client(c) => c.wt_on_write_reset(stream_id, code).unwrap_err(),
            }),
            Err(TransportError::NotReady) => Ok(0),
            Err(_) => Err(SessionError::Connection),
        }
    }

    pub(crate) fn wt_finish<T: Transport>(
        &mut self,
        t: &mut T,
        enc: &mut Encoder,
        dec: &mut Decoder,
        stream_id: u64,
    ) -> Result<(), SessionError> {
        let need_flush = match self {
            Self::Server(s) => s.wt_finish(stream_id)?,
            Self::Client(c) => c.wt_finish(stream_id)?,
        };
        if need_flush {
            self.flush(t, enc, dec);
        } else {
            let _ = t.finish(stream_id);
        }
        Ok(())
    }

    pub(crate) fn wt_read<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        out: &mut [u8],
    ) -> Result<ReadOutcome, SessionError> {
        let pre = match self {
            Self::Server(s) => s.wt_read_pre(stream_id, out.len() as u64)?,
            Self::Client(c) => c.wt_read_pre(stream_id, out.len() as u64)?,
        };
        match pre {
            abi::WtReadPre::Done { len, fin } => {
                let n = len as usize;
                if n > 0 {
                    out[..n].copy_from_slice(&self.conn().scratch().as_slice()[..n]);
                }
                Ok(ReadOutcome { len: n, fin })
            }
            abi::WtReadPre::TryTransport => {
                let res = t.read(stream_id, out);
                let mut mark_fin = || match self {
                    Self::Server(s) => s.wt_on_read_fin(stream_id),
                    Self::Client(c) => c.wt_on_read_fin(stream_id),
                };
                match res {
                    Ok(r) => {
                        if r.fin {
                            let _ = mark_fin();
                        }
                        Ok(r)
                    }
                    Err(TransportError::Reset(code)) => {
                        let _ = mark_fin();
                        Err(SessionError::StreamReset(code))
                    }
                    Err(TransportError::NotReady) => Ok(ReadOutcome { len: 0, fin: false }),
                    Err(TransportError::UnknownStream) => {
                        let _ = mark_fin();
                        Ok(ReadOutcome { len: 0, fin: true })
                    }
                    Err(_) => Err(SessionError::Connection),
                }
            }
        }
    }

    pub(crate) fn wt_reset<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        match self {
            Self::Server(s) => s.wt_reset(stream_id, code)?,
            Self::Client(c) => c.wt_reset(stream_id, code)?,
        }
        self.run_cmds(t);
        Ok(())
    }

    pub(crate) fn wt_stop_sending<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        match self {
            Self::Server(s) => s.wt_stop_sending(stream_id, code)?,
            Self::Client(c) => c.wt_stop_sending(stream_id, code)?,
        }
        self.run_cmds(t);
        Ok(())
    }
}

pub(crate) fn drive_poll_datagrams<T: Transport>(
    t: &mut T,
    mut on_dgram: impl FnMut(&[u8]) -> Result<(), ConnectionError>,
) -> Result<(), ConnectionError> {
    while let Some(d) = t.recv_datagram() {
        on_dgram(&d)?;
    }
    Ok(())
}

fn send_quic_datagram<T: Transport>(t: &mut T, bytes: &[u8]) -> Result<(), SessionError> {
    match t.send_datagram(bytes) {
        Ok(()) => Ok(()),
        Err(TransportError::TooLarge) => Err(SessionError::TooLarge),
        Err(_) => Err(SessionError::Connection),
    }
}

fn next_conn_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A server-side HTTP/3 connection.
pub struct ServerConnection {
    core: binding::ServerCore,
    encoder: Encoder,
    decoder: Decoder,
    id: u64,
    ready: VecDeque<Event>,
    leaks: LeakSink,
    rx_scratch: Vec<u8>,
    tmp_ids: Vec<u64>,
    tmp_bytes: Vec<u8>,
    tmp_meta: Vec<u64>,
}

impl ServerConnection {
    pub fn new(cfg: H3Config) -> Self {
        Self {
            core: binding::ServerCore::new(cfg_to_abi(cfg)),
            encoder: Encoder::new(cfg.encoder),
            decoder: Decoder::new(decoder_config_of(&cfg)),
            id: next_conn_id(),
            ready: VecDeque::new(),
            leaks: Arc::new(Mutex::new(Vec::new())),
            rx_scratch: vec![0u8; 16 * 1024],
            tmp_ids: Vec::new(),
            tmp_bytes: Vec::new(),
            tmp_meta: Vec::new(),
        }
    }

    pub fn stats(&self) -> ConnectionStats {
        self.core.as_ref().conn().stats()
    }
    pub fn session_stats(&self) -> SessionStats {
        self.core.as_ref().conn().sess().stats()
    }
    pub fn error(&self) -> Option<ConnectionError> {
        self.core.as_ref().conn().error()
    }
    pub fn peer_settings(&self) -> Option<Settings> {
        self.core.as_ref().conn().peer_settings()
    }
    /// The SETTINGS sent (after transport capability adjustments).
    pub fn sent_settings(&self) -> Settings {
        self.core.as_ref().conn().sent_settings()
    }
    pub fn encoder(&self) -> &Encoder {
        &self.encoder
    }
    pub fn decoder(&self) -> &Decoder {
        &self.decoder
    }
    /// Request streams not yet finished (received or answered).
    pub fn open_requests(&self) -> usize {
        self.core.as_ref().r_id().as_slice().len()
    }
    /// How many streams have a non-default priority recorded (a bound the
    /// tests check: entries leave with their streams).
    pub fn tracked_priorities(&self) -> usize {
        self.core.as_ref().conn().prio_id().as_slice().len()
    }
    /// The RFC 9218 priority the scheduler uses for a request's response.
    pub fn priority(&self, stream_id: u64) -> Priority {
        self.core.priority(stream_id)
    }
    /// Request bytes the connection holds now, against
    /// [`H3Config::max_connection_buffer`].
    pub fn buffered_request_bytes(&self) -> usize {
        self.core.buffered_request_bytes() as usize
    }
    /// WebTransport streams open in any session.
    pub fn webtransport_streams(&self) -> usize {
        self.core.as_ref().conn().sess().w_id().as_slice().len()
    }

    fn run_cmds<T: Transport>(&mut self, t: &mut T) {
        if execute_cmds(&self.core.as_ref().conn(), t) {
            self.core.clear_cmds();
        }
    }

    fn drain_events(&mut self) {
        {
            let core_ref = self.core.as_ref();
            let conn = core_ref.conn();
            let hev = conn.hev();
            let sev = conn.sev();
            let hkinds = hev.kinds().as_slice();
            let skinds = sev.kinds().as_slice();
            if hkinds.is_empty() && skinds.is_empty() {
                return;
            }
            let hwpos = hev.wpos().as_slice();
            let hwords = hev.words().as_slice();
            let hmeta = hev.meta().as_slice();
            let hbytes = hev.bytes().as_slice();
            for (i, &kind) in hkinds.iter().enumerate() {
                let w = &hwords[hwpos[i] as usize..];
                match kind {
                    1 => {
                        let stream_id = w[0];
                        let head = decode_req_head(hbytes, hmeta, w);
                        let body = hbytes[w[19] as usize..(w[19] + w[20]) as usize].to_vec();
                        let trailers = decode_fields(
                            hbytes,
                            hmeta,
                            w[21] as usize,
                            w[22] as usize,
                            w[23] as usize,
                        );
                        let responder = self.new_responder(stream_id);
                        self.ready.push_back(Event::Request(Box::new(Request {
                            stream_id,
                            head,
                            body,
                            trailers,
                            responder,
                        })));
                    }
                    2 => {
                        let stream_id = w[0];
                        let head = decode_req_head(hbytes, hmeta, w);
                        let responder = self.new_responder(stream_id);
                        self.ready
                            .push_back(Event::StreamingRequest(Box::new(StreamingRequest {
                                stream_id,
                                head,
                                responder,
                            })));
                    }
                    3 => self
                        .ready
                        .push_back(Event::BodyReadable { stream_id: w[0] }),
                    4 => {
                        let stream_id = w[0];
                        let kind = if w[1] == 0 {
                            SessionKind::WebTransport
                        } else {
                            SessionKind::Tunnel
                        };
                        let head = decode_req_head(hbytes, hmeta, w);
                        let responder = self.new_responder(stream_id);
                        self.ready
                            .push_back(Event::Connect(Box::new(ConnectRequest {
                                stream_id,
                                head,
                                kind,
                                responder,
                            })));
                    }
                    5 => self.ready.push_back(Event::RequestFailed {
                        stream_id: w[0],
                        code: Code(w[1]),
                    }),
                    6 => self.ready.push_back(Event::ResponseAbandoned {
                        stream_id: w[0],
                        code: w[1],
                    }),
                    7 => self.ready.push_back(Event::GoAway(w[0])),
                    _ => {}
                }
            }
            decode_session_events(&sev, |ev| self.ready.push_back(Event::Session(ev)));
        }
        self.core.clear_events();
    }

    fn flush<T: Transport>(&mut self, t: &mut T) {
        RoleCore::Server(&mut self.core).flush(t, &mut self.encoder, &mut self.decoder);
    }

    fn open_local<T: Transport>(&mut self, t: &mut T) -> Result<bool, ConnectionError> {
        RoleCore::Server(&mut self.core).open_local(t)
    }

    fn read_uni<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        RoleCore::Server(&mut self.core).read_uni(
            t,
            &mut self.encoder,
            &mut self.decoder,
            &mut self.rx_scratch,
            &mut self.tmp_ids,
        )
    }

    /// Drives the connection: opens our streams, reads every stream,
    /// processes frames, delivers complete requests, writes queued bytes.
    pub fn poll<T: Transport>(&mut self, t: &mut T) {
        if self.error().is_some() {
            return;
        }
        if let Err(e) = self.poll_inner(t) {
            self.core.fail(e);
            self.run_cmds(t);
        }
        self.flush(t);
        self.core.on_write_failures();
        self.run_cmds(t);
        self.core.retire();
        self.drain_events();
    }

    fn poll_inner<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        if !self.open_local(t)? {
            return Ok(());
        }
        while let Some(id) = t.accept() {
            if crate::transport::is_uni(id) {
                self.core
                    .accept_uni(&mut self.encoder.core, &mut self.decoder.core, huff(), id);
                self.run_cmds(t);
            } else {
                let res = self.core.accept(id);
                self.run_cmds(t);
                res?;
            }
        }
        self.read_uni(t)?;
        self.core.report_goaway();
        let unb = self.core.drain_unblocked(&mut self.decoder.core, huff());
        self.run_cmds(t);
        unb?;
        let _ = self.core.clear_budget_blocked();
        self.tmp_ids.clear();
        self.tmp_ids
            .extend_from_slice(self.core.as_ref().r_id().as_slice());
        for i in 0..self.tmp_ids.len() {
            let id = self.tmp_ids[i];
            self.read_request(t, id)?;
        }
        if self.core.clear_budget_blocked() {
            self.core.relieve_budget(&mut self.decoder.core);
            self.run_cmds(t);
        }
        drive_poll_datagrams(t, |d| self.core.on_quic_datagram(d))?;
        self.drain_leaks(t);
        Ok(())
    }

    fn drain_leaks<T: Transport>(&mut self, t: &mut T) {
        let leaked: Vec<u64> = match self.leaks.lock() {
            Ok(mut v) => std::mem::take(&mut *v),
            Err(_) => Vec::new(),
        };
        for id in leaked {
            self.core.on_leaked(&mut self.decoder.core, id);
            self.run_cmds(t);
        }
    }

    fn read_request<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        loop {
            let plan = self.core.read_plan(id, self.rx_scratch.len() as u64);
            if plan.skip {
                return Ok(());
            }
            let allowance = plan.allowance as usize;
            let r = match t.read(id, &mut self.rx_scratch[..allowance]) {
                Ok(r) => r,
                Err(TransportError::Reset(code)) => {
                    self.core.on_peer_reset(&mut self.decoder.core, id, code);
                    self.run_cmds(t);
                    return Ok(());
                }
                Err(TransportError::NotReady | TransportError::UnknownStream) => return Ok(()),
                Err(e) => return Err(ConnectionError::Transport(e)),
            };
            if r.len > 0 || r.fin {
                let res = self.core.on_read_data(
                    &mut self.decoder.core,
                    huff(),
                    id,
                    &self.rx_scratch[..r.len],
                    r.fin,
                );
                self.run_cmds(t);
                res?;
                if !self.core.as_ref().r_id().as_slice().contains(&id) {
                    return Ok(());
                }
            }
            if r.fin {
                return Ok(());
            }
            if allowance == 0 {
                self.core.on_zero_allowance(id);
                return Ok(());
            }
            if r.len == 0 {
                return Ok(());
            }
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

    /// The next event.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.drain_events();
        self.ready.pop_front()
    }

    /// Starts a graceful shutdown: GOAWAY with the first request stream ID
    /// that will not be processed. Requests already received continue.
    pub fn shutdown<T: Transport>(&mut self, t: &mut T) {
        self.core.shutdown();
        self.flush(t);
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
        let step = self.core.read_body_pre(stream_id, out.len() as u64)?;
        let n = step.len as usize;
        if n > 0 {
            let src = &self.core.as_ref().conn().scratch().as_slice()[..n];
            out[..n].copy_from_slice(src);
        }
        if step.need_read {
            if let Err(e) = self.read_request(t, stream_id) {
                self.core.fail(e);
                self.run_cmds(t);
            }
            self.core.read_body_post(stream_id);
            self.flush(t);
            self.drain_events();
        }
        Ok(BodyRead {
            len: n,
            end: step.end,
        })
    }

    /// The trailers of a streaming request, once its body ended.
    pub fn take_trailers(&mut self, stream_id: u64) -> Vec<Field> {
        let u = self.core.take_trailers(stream_id);
        let fields = if u.count == 0 {
            Vec::new()
        } else {
            let core_ref = self.core.as_ref();
            let hev = core_ref.conn().hev();
            decode_fields(
                hev.bytes().as_slice(),
                hev.meta().as_slice(),
                u.meta_off as usize,
                u.count as usize,
                u.byte_off as usize,
            )
        };
        if u.count > 0 {
            self.core.clear_events();
        }
        fields
    }

    fn check_responder(&self, conn: u64, id: u64) -> Result<(), SendError> {
        if conn != self.id {
            return Err(SendError::Foreign);
        }
        self.core.check_responder(id)
    }

    fn send_interim<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        status: u16,
        fields: Vec<Field>,
    ) -> Result<(), SendError> {
        flatten_fields(&fields, &mut self.tmp_bytes, &mut self.tmp_meta);
        self.core.send_interim(
            &mut self.encoder.core,
            huff(),
            id,
            status,
            &self.tmp_bytes,
            &self.tmp_meta,
        )?;
        self.flush(t);
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
        flatten_fields(&fields, &mut self.tmp_bytes, &mut self.tmp_meta);
        self.core.send_headers(
            &mut self.encoder.core,
            huff(),
            id,
            status,
            &self.tmp_bytes,
            &self.tmp_meta,
            content_length.is_some(),
            content_length.unwrap_or(0),
            reserve as u64,
        )?;
        self.flush(t);
        Ok(())
    }

    fn accept_session_send<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        fields: &[Field],
    ) -> Result<(), SendError> {
        flatten_fields(fields, &mut self.tmp_bytes, &mut self.tmp_meta);
        self.core.accept_session_send(
            &mut self.encoder.core,
            huff(),
            id,
            &self.tmp_bytes,
            &self.tmp_meta,
        )?;
        self.flush(t);
        self.core.accept_session_establish(id);
        self.drain_events();
        Ok(())
    }

    fn send_whole_response<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        response: Response,
    ) -> Result<(), SendError> {
        flatten_fields(&response.fields, &mut self.tmp_bytes, &mut self.tmp_meta);
        self.core.send_whole(
            &mut self.encoder.core,
            &mut self.decoder.core,
            huff(),
            id,
            response.status,
            &self.tmp_bytes,
            &self.tmp_meta,
            &response.body,
        )?;
        self.run_cmds(t);
        self.flush(t);
        Ok(())
    }

    fn send_response_data<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        data: &[u8],
    ) -> Result<(), SendError> {
        self.core.send_data(id, data)?;
        self.flush(t);
        Ok(())
    }

    fn finish_response<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        trailers: Vec<Field>,
    ) -> Result<(), SendError> {
        flatten_fields(&trailers, &mut self.tmp_bytes, &mut self.tmp_meta);
        self.core.finish_response(
            &mut self.encoder.core,
            &mut self.decoder.core,
            huff(),
            id,
            &self.tmp_bytes,
            &self.tmp_meta,
        )?;
        self.run_cmds(t);
        self.flush(t);
        Ok(())
    }

    fn close_session<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        code: u32,
        reason: &[u8],
    ) -> Result<Option<Code>, SendError> {
        let reset = self.core.close_session(id, code, reason)?;
        self.run_cmds(t);
        if reset.is_none() {
            self.flush(t);
        }
        Ok(reset.map(Code))
    }

    fn reset_request<T: Transport>(&mut self, t: &mut T, id: u64, code: Code) {
        self.core.reset_request(&mut self.decoder.core, id, code.0);
        self.run_cmds(t);
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
        let sid = self.session_check(session)?;
        RoleCore::Server(&mut self.core).wt_open(t, &mut self.encoder, &mut self.decoder, sid, bidi)
    }

    /// Sends an HTTP Datagram in `session`.
    pub fn send_datagram<T: Transport>(
        &mut self,
        t: &mut T,
        session: &Session,
        payload: &[u8],
    ) -> Result<DatagramPath, SessionError> {
        let sid = self.session_check(session)?;
        RoleCore::Server(&mut self.core).wt_send_datagram(
            t,
            &mut self.encoder,
            &mut self.decoder,
            sid,
            payload,
        )
    }

    /// Writes WebTransport stream data; returns how many bytes the
    /// transport took (0 under flow control).
    pub fn write_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        data: &[u8],
    ) -> Result<usize, SessionError> {
        RoleCore::Server(&mut self.core).wt_write(
            t,
            &mut self.encoder,
            &mut self.decoder,
            stream_id,
            data,
        )
    }

    pub fn finish_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
    ) -> Result<(), SessionError> {
        RoleCore::Server(&mut self.core).wt_finish(
            t,
            &mut self.encoder,
            &mut self.decoder,
            stream_id,
        )
    }

    pub fn read_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        out: &mut [u8],
    ) -> Result<ReadOutcome, SessionError> {
        RoleCore::Server(&mut self.core).wt_read(t, stream_id, out)
    }

    /// RESET_STREAM with WebTransport application error `code`.
    pub fn reset_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        RoleCore::Server(&mut self.core).wt_reset(t, stream_id, code)
    }

    /// STOP_SENDING with WebTransport application error `code`.
    pub fn stop_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        RoleCore::Server(&mut self.core).wt_stop_sending(t, stream_id, code)
    }

    /// The session a WebTransport stream belongs to.
    pub fn stream_session(&self, stream_id: u64) -> Option<u64> {
        self.core.wt_session_of(stream_id)
    }
}

fn decode_req_head(bytes: &[u8], meta: &[u64], w: &[u64]) -> RequestHead {
    RequestHead {
        method: text(bytes, w[2], w[3]),
        scheme: text(bytes, w[4], w[5]),
        authority: (w[6] != 0).then(|| text(bytes, w[7], w[8])),
        path: text(bytes, w[9], w[10]),
        protocol: (w[11] != 0).then(|| text(bytes, w[12], w[13])),
        fields: decode_fields(bytes, meta, w[16] as usize, w[17] as usize, w[18] as usize),
        content_length: (w[14] != 0).then_some(w[15]),
    }
}
