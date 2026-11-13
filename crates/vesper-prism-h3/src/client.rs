//! A small HTTP/3 client connection: request streams with whole bodies.
//!
//! It shares the control and QPACK machinery with the server
//! ([`crate::connection::Core`]) and exists so the server can be tested end
//! to end over Warp with the same wire behaviour in both directions.
//! Interim (1xx) responses are skipped; server push is never enabled (no
//! MAX_PUSH_ID is sent, so a push stream is a connection error).

use std::collections::{BTreeMap, VecDeque};

use crate::connection::{ConnectionError, ConnectionStats, Core, H3Config};
use crate::error::Code;
use crate::frame::{self, Frame, FrameError, FrameReader, Settings};
use crate::message::{self, ResponseHead};
use crate::qpack::{Decoded, Field, FieldSection, QpackError};
use crate::transport::{Transport, TransportError, is_uni};

/// A request to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientRequest {
    pub method: String,
    pub scheme: String,
    pub authority: String,
    pub path: String,
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
            fields: Vec::new(),
            body: Vec::new(),
        }
    }

    /// The field list as sent (pseudo-headers first).
    pub fn field_list(&self) -> Vec<Field> {
        let mut list = vec![
            Field::new(":method", self.method.as_bytes()),
            Field::new(":scheme", self.scheme.as_bytes()),
            Field::new(":authority", self.authority.as_bytes()),
            Field::new(":path", self.path.as_bytes()),
        ];
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
    /// The transport cannot open a stream yet (handshake or stream limit).
    NotReady,
    /// The server sent GOAWAY; new requests would not be processed.
    GoingAway,
    Failed(ConnectionError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Headers,
    Blocked { trailers: bool },
    Body,
    Trailers,
}

struct ResponseStream {
    reader: FrameReader,
    phase: Phase,
    head: Option<ResponseHead>,
    body: Vec<u8>,
    trailers: Vec<Field>,
    head_request: bool,
    fin: bool,
}

pub struct ClientConnection {
    core: Core,
    streams: BTreeMap<u64, ResponseStream>,
    ready: VecDeque<ClientEvent>,
    goaway_seen: bool,
}

impl ClientConnection {
    pub fn new(cfg: H3Config) -> Self {
        Self {
            core: Core::new(cfg),
            streams: BTreeMap::new(),
            ready: VecDeque::new(),
            goaway_seen: false,
        }
    }

    pub fn stats(&self) -> ConnectionStats {
        self.core.stats
    }
    pub fn error(&self) -> Option<ConnectionError> {
        self.core.error
    }
    pub fn peer_settings(&self) -> Option<Settings> {
        self.core.peer_settings
    }
    pub fn in_flight(&self) -> usize {
        self.streams.len()
    }

    /// Opens a request stream and queues the whole request.
    pub fn send_request<T: Transport>(
        &mut self,
        t: &mut T,
        req: &ClientRequest,
    ) -> Result<u64, ClientError> {
        if let Some(e) = self.core.error {
            return Err(ClientError::Failed(e));
        }
        if self.core.goaway_received.is_some() {
            return Err(ClientError::GoingAway);
        }
        match self.core.open_local(t) {
            Ok(true) => {}
            Ok(false) => return Err(ClientError::NotReady),
            Err(e) => {
                self.core.fail(t, e);
                return Err(ClientError::Failed(e));
            }
        }
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
        self.core.queue_fin(id);
        self.streams.insert(
            id,
            ResponseStream {
                reader: FrameReader::new(self.core.cfg.max_frame_payload),
                phase: Phase::Headers,
                head: None,
                body: Vec::new(),
                trailers: Vec::new(),
                head_request: req.method == "HEAD",
                fin: false,
            },
        );
        self.core.flush(t);
        Ok(id)
    }

    /// Cancels a request: STOP_SENDING and RESET_STREAM with
    /// H3_REQUEST_CANCELLED.
    pub fn cancel<T: Transport>(&mut self, t: &mut T, id: u64) {
        if self.streams.remove(&id).is_some() {
            self.core.decoder.cancel_stream(id);
            self.core.discard(id);
            t.stop_sending(id, Code::REQUEST_CANCELLED);
            t.reset(id, Code::REQUEST_CANCELLED);
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
    }

    fn poll_inner<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        if !self.core.open_local(t)? {
            return Ok(());
        }
        while let Some(id) = t.accept() {
            if is_uni(id) {
                self.core.accept_uni(t, id);
            } else {
                // Servers cannot open bidirectional streams in HTTP/3.
                return Err(ConnectionError::StreamCreation { stream_id: id });
            }
        }
        self.core.read_uni(t, false)?;
        if let (Some(g), false) = (self.core.goaway_received, self.goaway_seen) {
            self.goaway_seen = true;
            self.ready.push_back(ClientEvent::GoAway(g));
            // Requests at or above the ID were not processed (§5.2).
            let gone: Vec<u64> = self.streams.range(g..).map(|(id, _)| *id).collect();
            for id in gone {
                self.streams.remove(&id);
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
            self.read_stream(t, id)?;
        }
        Ok(())
    }

    pub fn poll_event(&mut self) -> Option<ClientEvent> {
        self.ready.pop_front()
    }

    fn read_stream<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ConnectionError> {
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
                        self.streams.remove(&id);
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
                    if rs.head_request && !d.is_empty() {
                        self.malformed(t, id, Code::MESSAGE_ERROR);
                        return Ok(());
                    }
                    if d.len() > self.core.cfg.max_body_bytes - rs.body.len() {
                        self.malformed(t, id, Code::EXCESSIVE_LOAD);
                        return Ok(());
                    }
                    rs.body.extend_from_slice(&d);
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
            // Interim responses: wait for the final one.
            Ok(h) if h.status < 200 => Ok(()),
            Ok(h) => {
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
            Phase::Body | Phase::Trailers => {
                let rs = self.streams.remove(&id).expect("present");
                let head = rs.head.expect("final response");
                if !rs.head_request
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
        self.streams.remove(&id);
        self.core.discard(id);
        t.stop_sending(id, code);
        self.ready.push_back(ClientEvent::Failed {
            stream_id: id,
            code,
        });
    }
}
