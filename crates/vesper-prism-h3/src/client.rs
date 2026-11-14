//! A small HTTP/3 client connection.
//!
//! It shares the control and QPACK machinery with the server
//! ([`crate::connection::Core`]) and exists so the server can be tested end
//! to end over Warp with the same wire behaviour in both directions. It
//! sends whole requests ([`ClientConnection::send_request`]) or streams a
//! request body under a send budget
//! ([`ClientConnection::send_request_streaming`]), reports interim (1xx)
//! responses, sends PRIORITY_UPDATE (RFC 9218), and opens extended CONNECT
//! sessions: WebTransport (streams and datagrams) or another protocol with
//! HTTP Datagrams (RFC 9297). Server push is never enabled (no MAX_PUSH_ID
//! is sent, so a push stream is a connection error).

use std::collections::{BTreeMap, VecDeque};

use crate::connection::{ConnectionError, ConnectionStats, Core, H3Config};
use crate::error::Code;
use crate::frame::{self, Frame, FrameError, FrameReader, Settings};
use crate::message::{self, ResponseHead};
use crate::priority::{Priority, PriorityUpdate};
use crate::qpack::{Decoded, Field, FieldSection, QpackError};
use crate::session::{DatagramPath, SessionError, SessionEvent, SessionKind, SessionStats};
use crate::transport::{ReadOutcome, Transport, TransportError, is_server_initiated, is_uni};

/// A request to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientRequest {
    pub method: String,
    pub scheme: String,
    pub authority: String,
    pub path: String,
    /// `:protocol` for an extended CONNECT (RFC 9220).
    pub protocol: Option<String>,
    pub fields: Vec<Field>,
    pub body: Vec<u8>,
}

impl ClientRequest {
    pub fn get(authority: &str, path: &str) -> Self {
        Self {
            method: "GET".into(),
            scheme: "https".into(),
            authority: authority.into(),
            path: path.into(),
            protocol: None,
            fields: Vec::new(),
            body: Vec::new(),
        }
    }

    /// An extended CONNECT for `protocol` (`webtransport` for a
    /// WebTransport session).
    pub fn connect(authority: &str, path: &str, protocol: &str) -> Self {
        Self {
            method: "CONNECT".into(),
            protocol: Some(protocol.into()),
            ..Self::get(authority, path)
        }
    }

    /// The field list as sent (pseudo-headers first).
    pub fn field_list(&self) -> Vec<Field> {
        let mut list = vec![Field::new(":method", self.method.as_bytes())];
        if let Some(p) = &self.protocol {
            list.push(Field::new(":protocol", p.as_bytes()));
        }
        list.extend([
            Field::new(":scheme", self.scheme.as_bytes()),
            Field::new(":authority", self.authority.as_bytes()),
            Field::new(":path", self.path.as_bytes()),
        ]);
        list.extend(self.fields.iter().cloned());
        if !self.body.is_empty() {
            list.push(Field::new("content-length", self.body.len().to_string()));
        }
        list
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientResponse {
    pub stream_id: u64,
    pub head: ResponseHead,
    pub body: Vec<u8>,
    pub trailers: Vec<Field>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClientEvent {
    Response(ClientResponse),
    /// An interim (1xx) response; the final one follows.
    Interim {
        stream_id: u64,
        head: ResponseHead,
    },
    /// An extended CONNECT got a 2xx: the session is established. (A
    /// non-2xx answer arrives as an ordinary [`ClientEvent::Response`].)
    SessionEstablished {
        session_id: u64,
        head: ResponseHead,
    },
    Session(SessionEvent),
    /// The response stream was reset by the server, or was malformed and
    /// we reset it.
    Failed {
        stream_id: u64,
        code: Code,
    },
    GoAway(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// The transport cannot open a stream yet (handshake or stream limit),
    /// or the server's SETTINGS have not arrived (extended CONNECT needs
    /// them).
    NotReady,
    /// The server sent GOAWAY; new requests would not be processed.
    GoingAway,
    /// The server did not offer what the request needs (extended CONNECT,
    /// WebTransport).
    Unsupported,
    /// No such request stream, or its body is already finished.
    UnknownStream,
    /// The body bytes would exceed the stream's send budget; retry after
    /// the queue drains ([`ClientConnection::queued`]).
    BufferFull,
    Failed(ConnectionError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Headers,
    Blocked {
        trailers: bool,
    },
    Body,
    Trailers,
    /// An established extended CONNECT: DATA is capsules.
    Tunnel,
}

struct ResponseStream {
    reader: FrameReader,
    phase: Phase,
    head: Option<ResponseHead>,
    body: Vec<u8>,
    trailers: Vec<Field>,
    head_request: bool,
    connect: bool,
    fin: bool,
    /// The request body is still open (streaming upload).
    sending: bool,
}

impl ResponseStream {
    /// 204 and 304 responses never carry content (RFC 9110 §15.3.5, §15.4.5).
    fn bodyless(&self) -> bool {
        self.head
            .as_ref()
            .is_some_and(|h| matches!(h.status, 204 | 304))
    }
}

/// Server-initiated bidirectional streams waiting for their first frame
/// (only WebTransport streams are allowed).
const MAX_SNIFFED: usize = 16;

pub struct ClientConnection {
    core: Core,
    streams: BTreeMap<u64, ResponseStream>,
    sniff: BTreeMap<u64, FrameReader>,
    ready: VecDeque<ClientEvent>,
    /// The lowest GOAWAY cutoff already acted on.
    goaway_seen: Option<u64>,
}

impl ClientConnection {
    pub fn new(cfg: H3Config) -> Self {
        Self {
            core: Core::new(cfg, false),
            streams: BTreeMap::new(),
            sniff: BTreeMap::new(),
            ready: VecDeque::new(),
            goaway_seen: None,
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
    pub fn sent_settings(&self) -> Settings {
        self.core.sent_settings
    }
    pub fn in_flight(&self) -> usize {
        self.streams.len()
    }
    /// Bytes queued on a stream and not yet taken by the transport.
    pub fn queued(&self, stream_id: u64) -> usize {
        self.core.queued(stream_id)
    }

    fn ready_to_send<T: Transport>(&mut self, t: &mut T) -> Result<(), ClientError> {
        if let Some(e) = self.core.error {
            return Err(ClientError::Failed(e));
        }
        if self.core.goaway_received.is_some() {
            return Err(ClientError::GoingAway);
        }
        match self.core.open_local(t) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ClientError::NotReady),
            Err(e) => {
                self.core.fail(t, e);
                Err(ClientError::Failed(e))
            }
        }
    }

    fn open_request<T: Transport>(
        &mut self,
        t: &mut T,
        req: &ClientRequest,
        fin: bool,
    ) -> Result<u64, ClientError> {
        let id = match t.open_bidi() {
            Ok(id) => id,
            Err(TransportError::NotReady) => return Err(ClientError::NotReady),
            Err(e) => return Err(ClientError::Failed(ConnectionError::Transport(e))),
        };
        let block = self.core.encoder.encode(id, &req.field_list());
        let mut out = Vec::with_capacity(block.len() + req.body.len() + 16);
        frame::write_frame(&mut out, frame::HEADERS, &block);
        if !req.body.is_empty() {
            frame::write_frame(&mut out, frame::DATA, &req.body);
        }
        self.core.queue(id, &out);
        if fin {
            self.core.queue_fin(id);
        }
        self.streams.insert(
            id,
            ResponseStream {
                reader: FrameReader::new(self.core.cfg.max_frame_payload),
                phase: Phase::Headers,
                head: None,
                body: Vec::new(),
                trailers: Vec::new(),
                head_request: req.method == "HEAD",
                connect: req.protocol.is_some(),
                fin: false,
                sending: !fin,
            },
        );
        self.core.flush(t);
        Ok(id)
    }

    /// Opens a request stream and queues the whole request.
    pub fn send_request<T: Transport>(
        &mut self,
        t: &mut T,
        req: &ClientRequest,
    ) -> Result<u64, ClientError> {
        if req.protocol.is_some() {
            return self.open_session(t, req);
        }
        self.ready_to_send(t)?;
        self.open_request(t, req, true)
    }

    /// Opens a request whose body follows with
    /// [`ClientConnection::send_body`] and ends with
    /// [`ClientConnection::finish_body`]. `req.body`, if any, is sent
    /// first; put a `content-length` in `req.fields` if it is known (none
    /// is added).
    pub fn send_request_streaming<T: Transport>(
        &mut self,
        t: &mut T,
        req: &ClientRequest,
    ) -> Result<u64, ClientError> {
        self.ready_to_send(t)?;
        let mut req = req.clone();
        let first = std::mem::take(&mut req.body);
        // Checked before the request opens, so a refusal leaves nothing in
        // flight.
        if !first.is_empty() && first.len() + 9 > self.core.cfg.max_stream_buffer {
            return Err(ClientError::BufferFull);
        }
        let id = self.open_request(t, &req, false)?;
        if !first.is_empty()
            && let Err(e) = self.send_body(t, id, &first)
        {
            // The HEADERS could not leave yet: cancel rather than orphan.
            self.cancel(t, id);
            return Err(e);
        }
        Ok(id)
    }

    /// Queues request body bytes as one DATA frame; all or nothing.
    pub fn send_body<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        data: &[u8],
    ) -> Result<(), ClientError> {
        if !self.streams.get(&id).is_some_and(|s| s.sending) {
            return Err(ClientError::UnknownStream);
        }
        if self.core.queued(id) + data.len() + 9 > self.core.cfg.max_stream_buffer {
            return Err(ClientError::BufferFull);
        }
        let mut f = Vec::with_capacity(data.len() + 9);
        frame::write_frame(&mut f, frame::DATA, data);
        self.core.queue(id, &f);
        self.core.flush(t);
        Ok(())
    }

    /// Ends a streaming request body (FIN).
    pub fn finish_body<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ClientError> {
        match self.streams.get_mut(&id) {
            Some(s) if s.sending && !s.connect => s.sending = false,
            _ => return Err(ClientError::UnknownStream),
        }
        self.core.queue_fin(id);
        self.core.flush(t);
        Ok(())
    }

    /// Opens an extended CONNECT session (RFC 9220). Needs the server's
    /// SETTINGS with SETTINGS_ENABLE_CONNECT_PROTOCOL (and, for
    /// `webtransport`, a WebTransport max-sessions setting).
    pub fn open_session<T: Transport>(
        &mut self,
        t: &mut T,
        req: &ClientRequest,
    ) -> Result<u64, ClientError> {
        self.ready_to_send(t)?;
        let Some(peer) = self.core.peer_settings else {
            return Err(ClientError::NotReady);
        };
        let Some(protocol) = &req.protocol else {
            return Err(ClientError::Unsupported);
        };
        if !peer.enable_connect_protocol || req.method != "CONNECT" {
            return Err(ClientError::Unsupported);
        }
        let wt = protocol == "webtransport";
        if wt && !self.core.webtransport_negotiated() {
            return Err(ClientError::Unsupported);
        }
        let mut req = req.clone();
        req.body.clear();
        let id = self.open_request(t, &req, false)?;
        self.core.add_session(
            id,
            if wt {
                SessionKind::WebTransport
            } else {
                SessionKind::Tunnel
            },
        );
        Ok(id)
    }

    /// Closes a session: CLOSE_WEBTRANSPORT_SESSION (WebTransport) and FIN.
    pub fn close_session<T: Transport>(
        &mut self,
        t: &mut T,
        session: u64,
        code: u32,
        reason: &[u8],
    ) -> Result<(), SessionError> {
        if !self
            .streams
            .get(&session)
            .is_some_and(|s| s.connect && s.sending)
        {
            return Err(SessionError::NotEstablished);
        }
        // Validated and queued first; the state changes only on success.
        self.core.queue_session_close(session, code, reason)?;
        if let Some(s) = self.streams.get_mut(&session) {
            s.sending = false;
        }
        self.core
            .end_session(t, session, code, Vec::new(), None, false);
        self.core.flush(t);
        Ok(())
    }

    pub fn open_stream<T: Transport>(
        &mut self,
        t: &mut T,
        session: u64,
        bidi: bool,
    ) -> Result<u64, SessionError> {
        self.core.open_wt_stream(t, session, bidi)
    }

    pub fn send_datagram<T: Transport>(
        &mut self,
        t: &mut T,
        session: u64,
        payload: &[u8],
    ) -> Result<DatagramPath, SessionError> {
        self.core.send_session_datagram(t, session, payload)
    }

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

    pub fn reset_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        self.core.wt_reset(t, stream_id, code)
    }

    pub fn stop_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        self.core.wt_stop_sending(t, stream_id, code)
    }

    /// Changes a request's priority with PRIORITY_UPDATE (RFC 9218 §7.2).
    pub fn update_priority<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        priority: Priority,
    ) -> Result<(), ClientError> {
        if is_uni(stream_id) || is_server_initiated(stream_id) {
            return Err(ClientError::UnknownStream);
        }
        self.ready_to_send(t)?;
        let mut f = Vec::new();
        PriorityUpdate {
            push: false,
            element_id: stream_id,
            priority,
        }
        .encode(&mut f);
        if !self.core.queue_control(&f) {
            return Err(ClientError::NotReady);
        }
        self.core.flush(t);
        Ok(())
    }

    /// Cancels a request: STOP_SENDING and RESET_STREAM with
    /// H3_REQUEST_CANCELLED.
    pub fn cancel<T: Transport>(&mut self, t: &mut T, id: u64) {
        if let Some(s) = self.streams.remove(&id) {
            self.core.decoder.cancel_stream(id);
            self.core.discard(id);
            t.stop_sending(id, Code::REQUEST_CANCELLED);
            t.reset(id, Code::REQUEST_CANCELLED);
            if s.connect {
                self.core.end_session(t, id, 0, Vec::new(), None, false);
                self.core.forget_session(id);
            }
            self.core.flush(t);
        }
    }

    pub fn poll<T: Transport>(&mut self, t: &mut T) {
        if self.core.error.is_some() {
            return;
        }
        if let Err(e) = self.poll_inner(t) {
            self.core.fail(t, e);
        }
        self.core.flush(t);
        self.forward_session_events();
    }

    fn forward_session_events(&mut self) {
        while let Some(e) = self.core.sessions.events.pop_front() {
            self.ready.push_back(ClientEvent::Session(e));
        }
    }

    fn poll_inner<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        if !self.core.open_local(t)? {
            return Ok(());
        }
        while let Some(id) = t.accept() {
            if is_uni(id) {
                self.core.accept_uni(t, id);
            } else if self.core.sent_settings.webtransport_max_sessions.is_some() {
                // A server bidi stream can only be a WebTransport stream.
                // Streams whose headers already arrived leave the bound
                // first; past it, a stream is refused on its own.
                if self.sniff.len() >= MAX_SNIFFED {
                    self.read_sniffed(t)?;
                }
                if self.sniff.len() < MAX_SNIFFED {
                    self.sniff.insert(
                        id,
                        FrameReader::with_webtransport(self.core.cfg.max_frame_payload),
                    );
                } else {
                    self.core.sessions.stats.streams_rejected += 1;
                    t.stop_sending(id, Code::WT_BUFFERED_STREAM_REJECTED);
                    t.reset(id, Code::WT_BUFFERED_STREAM_REJECTED);
                }
            } else {
                // Servers cannot open bidirectional streams in HTTP/3.
                return Err(ConnectionError::StreamCreation { stream_id: id });
            }
        }
        self.core.read_uni(t, false)?;
        self.read_sniffed(t)?;
        // A server may lower its cutoff in later GOAWAY frames; each
        // decrease rejects the requests it newly excludes.
        if let Some(g) = self.core.goaway_received
            && self.goaway_seen.is_none_or(|prev| g < prev)
        {
            self.goaway_seen = Some(g);
            self.ready.push_back(ClientEvent::GoAway(g));
            // Requests at or above the ID were not processed (§5.2).
            let gone: Vec<u64> = self.streams.range(g..).map(|(id, _)| *id).collect();
            for id in gone {
                self.streams.remove(&id);
                self.core.end_session(t, id, 0, Vec::new(), None, false);
                self.ready.push_back(ClientEvent::Failed {
                    stream_id: id,
                    code: Code::REQUEST_REJECTED,
                });
            }
        }
        while let Some(r) = self.core.decoder.poll_unblocked() {
            self.unblocked(t, r)?;
        }
        let ids: Vec<u64> = self.streams.keys().copied().collect();
        for id in ids {
            self.read_stream_frames(t, id)?;
        }
        self.core.poll_datagrams(t)?;
        Ok(())
    }

    /// Reads server bidi streams up to their WEBTRANSPORT_STREAM signal.
    fn read_sniffed<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        let ids: Vec<u64> = self.sniff.keys().copied().collect();
        let mut buf = [0u8; 64];
        for id in ids {
            let r = match t.read(id, &mut buf) {
                Ok(r) => r,
                Err(TransportError::NotReady) => continue,
                Err(_) => {
                    self.sniff.remove(&id);
                    continue;
                }
            };
            let reader = self.sniff.get_mut(&id).expect("listed");
            reader.push(&buf[..r.len]);
            match reader.next_frame() {
                Ok(Some(Frame::WebTransportStream(session))) => {
                    let mut reader = self.sniff.remove(&id).expect("listed");
                    let rest = reader.take_rest();
                    let gone = self.core.session_known_gone(session);
                    self.core
                        .incoming_wt_stream_fin(t, id, session, true, rest, gone, r.fin)?;
                }
                Ok(None) if !r.fin => {}
                _ => return Err(ConnectionError::StreamCreation { stream_id: id }),
            }
        }
        Ok(())
    }

    pub fn poll_event(&mut self) -> Option<ClientEvent> {
        self.forward_session_events();
        self.ready.pop_front()
    }

    fn read_stream_frames<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
    ) -> Result<(), ConnectionError> {
        let mut buf = std::mem::take(&mut self.core.scratch);
        let r = (|| {
            loop {
                let Some(rs) = self.streams.get(&id) else {
                    return Ok(());
                };
                if matches!(rs.phase, Phase::Blocked { .. }) || rs.fin {
                    return Ok(());
                }
                let r = match t.read(id, &mut buf) {
                    Ok(r) => r,
                    Err(TransportError::Reset(code)) => {
                        self.core.stats.resets_received += 1;
                        self.core.decoder.cancel_stream(id);
                        if let Some(s) = self.streams.remove(&id)
                            && s.connect
                        {
                            self.core
                                .end_session(t, id, 0, Vec::new(), Some(code), true);
                            self.core.forget_session(id);
                        }
                        self.core.discard(id);
                        self.ready.push_back(ClientEvent::Failed {
                            stream_id: id,
                            code: Code(code),
                        });
                        return Ok(());
                    }
                    Err(TransportError::NotReady | TransportError::UnknownStream) => return Ok(()),
                    Err(e) => return Err(ConnectionError::Transport(e)),
                };
                if r.len > 0 {
                    self.streams
                        .get_mut(&id)
                        .expect("present")
                        .reader
                        .push(&buf[..r.len]);
                    self.frames(t, id)?;
                }
                if r.fin {
                    return self.fin(t, id);
                }
                if r.len == 0 {
                    return Ok(());
                }
            }
        })();
        self.core.scratch = buf;
        r
    }

    fn frames<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        loop {
            let Some(rs) = self.streams.get_mut(&id) else {
                return Ok(());
            };
            if matches!(rs.phase, Phase::Blocked { .. }) {
                return Ok(());
            }
            let f = match rs.reader.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => return Ok(()),
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
            match (f, rs.phase) {
                (Frame::Unknown(_), _) => self.core.stats.unknown_frames += 1,
                (Frame::Headers(b), Phase::Headers) => self.section(t, id, &b, false)?,
                (Frame::Headers(b), Phase::Body) => self.section(t, id, &b, true)?,
                (Frame::Data(d), Phase::Body) => {
                    if !d.is_empty() && (rs.head_request || rs.bodyless()) {
                        self.malformed(t, id, Code::MESSAGE_ERROR);
                        return Ok(());
                    }
                    if d.len() > self.core.cfg.max_body_bytes - rs.body.len() {
                        self.malformed(t, id, Code::EXCESSIVE_LOAD);
                        return Ok(());
                    }
                    rs.body.extend_from_slice(&d);
                }
                (Frame::Data(d), Phase::Tunnel) => {
                    if let Err(code) = self.core.session_data(t, id, &d) {
                        self.malformed(t, id, code);
                        return Ok(());
                    }
                }
                (Frame::Headers(_), Phase::Tunnel) => self.core.stats.unknown_frames += 1,
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
        b: &[u8],
        trailers: bool,
    ) -> Result<(), ConnectionError> {
        match self.core.decoder.decode_section(id, b) {
            Ok(Decoded::Fields(s)) => self.fields(t, id, s, trailers),
            Ok(Decoded::Blocked) => {
                self.core.stats.blocked_sections += 1;
                if let Some(rs) = self.streams.get_mut(&id) {
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
                let Some(rs) = self.streams.get_mut(&id) else {
                    return Ok(());
                };
                let Phase::Blocked { trailers } = rs.phase else {
                    return Ok(());
                };
                rs.phase = if trailers {
                    Phase::Body
                } else {
                    Phase::Headers
                };
                self.fields(t, id, s, trailers)?;
                self.frames(t, id)?;
                // The FIN may have been read before the section unblocked.
                match self.streams.get(&id) {
                    Some(rs) if rs.fin && !matches!(rs.phase, Phase::Blocked { .. }) => {
                        self.fin(t, id)
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

    fn fields<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
        s: FieldSection,
        trailers: bool,
    ) -> Result<(), ConnectionError> {
        let Some(rs) = self.streams.get_mut(&id) else {
            return Ok(());
        };
        if trailers {
            if message::check_trailers(&s.fields).is_err() {
                self.malformed(t, id, Code::MESSAGE_ERROR);
                return Ok(());
            }
            rs.trailers = s.fields;
            rs.phase = Phase::Trailers;
            return Ok(());
        }
        match message::parse_response(s.fields) {
            // HTTP/3 has no 101 (RFC 9114 §4.5).
            Ok(h) if h.status == 101 => {
                self.malformed(t, id, Code::MESSAGE_ERROR);
                Ok(())
            }
            // Interim responses: reported; the final one follows.
            Ok(h) if h.status < 200 => {
                self.core.stats.interim_responses += 1;
                self.ready.push_back(ClientEvent::Interim {
                    stream_id: id,
                    head: h,
                });
                Ok(())
            }
            Ok(h) if rs.connect && (200..300).contains(&h.status) => {
                rs.phase = Phase::Tunnel;
                self.core.establish_session(id);
                self.ready.push_back(ClientEvent::SessionEstablished {
                    session_id: id,
                    head: h,
                });
                Ok(())
            }
            Ok(h) => {
                if rs.connect {
                    // Refused: the session never started.
                    rs.connect = false;
                    rs.sending = false;
                    self.core.end_session(t, id, 0, Vec::new(), None, false);
                    self.core.forget_session(id);
                    self.core.queue_fin(id);
                }
                let rs = self.streams.get_mut(&id).expect("present");
                rs.head = Some(h);
                rs.phase = Phase::Body;
                Ok(())
            }
            Err(_) => {
                self.malformed(t, id, Code::MESSAGE_ERROR);
                Ok(())
            }
        }
    }

    fn fin<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
        let Some(rs) = self.streams.get_mut(&id) else {
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
            Phase::Blocked { .. } => Ok(()),
            Phase::Headers => {
                self.malformed(t, id, Code::REQUEST_INCOMPLETE);
                Ok(())
            }
            Phase::Tunnel => {
                // The server ended the session (FIN on the CONNECT stream),
                // which must not end inside a capsule (RFC 9297 §3.3).
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
                let sending = rs.sending;
                self.streams.remove(&id);
                self.core.end_session(t, id, 0, Vec::new(), None, true);
                if sending {
                    self.core.queue_fin(id);
                }
                self.core.forget_session(id);
                Ok(())
            }
            Phase::Body | Phase::Trailers => {
                let rs = self.streams.remove(&id).expect("present");
                let head = rs.head.expect("final response");
                // A HEAD or 304 content-length describes the representation,
                // not a body (RFC 9110 §8.6).
                if !rs.head_request
                    && head.status != 304
                    && head
                        .content_length
                        .is_some_and(|cl| cl != rs.body.len() as u64)
                {
                    self.ready.push_back(ClientEvent::Failed {
                        stream_id: id,
                        code: Code::MESSAGE_ERROR,
                    });
                    self.core.stats.malformed += 1;
                    return Ok(());
                }
                self.core.stats.responses += 1;
                self.ready.push_back(ClientEvent::Response(ClientResponse {
                    stream_id: id,
                    head,
                    body: rs.body,
                    trailers: rs.trailers,
                }));
                Ok(())
            }
        }
    }

    fn malformed<T: Transport>(&mut self, t: &mut T, id: u64, code: Code) {
        self.core.stats.malformed += 1;
        self.core.decoder.cancel_stream(id);
        if let Some(s) = self.streams.remove(&id)
            && s.connect
        {
            self.core
                .end_session(t, id, 0, Vec::new(), Some(code.0), true);
            self.core.forget_session(id);
            t.reset(id, code);
        }
        self.core.discard(id);
        t.stop_sending(id, code);
        self.ready.push_back(ClientEvent::Failed {
            stream_id: id,
            code,
        });
    }
}
