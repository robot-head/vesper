//! Extended CONNECT sessions (RFC 9220), HTTP Datagrams (RFC 9297) and
//! WebTransport over HTTP/3 (draft-ietf-webtrans-http3), shared by both
//! roles through [`Core`].
//!
//! A session is identified by the stream ID of its extended CONNECT
//! request. Once the response is 2xx the session is *established*:
//!
//! * The CONNECT stream's DATA payload is a capsule stream
//!   ([`crate::capsule`]): DATAGRAM capsules are HTTP Datagrams, and
//!   CLOSE_WEBTRANSPORT_SESSION / DRAIN_WEBTRANSPORT_SESSION end or drain a
//!   WebTransport session. A FIN on the CONNECT stream closes the session
//!   (as a CLOSE with code 0 would).
//! * HTTP Datagrams travel in QUIC DATAGRAM frames, prefixed by the quarter
//!   stream ID, when the transport negotiated them and both sides sent
//!   SETTINGS_H3_DATAGRAM=1; otherwise as DATAGRAM capsules on the CONNECT
//!   stream (reliable, ordered: RFC 9297 §3.5).
//! * For WebTransport, either side opens bidirectional streams (signal
//!   0x41 + session ID) and unidirectional streams (type 0x54 + session
//!   ID). Stream data is never framed and is read by the application
//!   straight from the transport, so QUIC flow control is the back-pressure.
//!   Streams for a session that is not established yet are held (bounded,
//!   unread) and released when it is; past the bound they are refused with
//!   WT_BUFFERED_STREAM_REJECTED. When a session ends, its streams are reset
//!   with WT_SESSION_GONE.

use std::collections::{BTreeMap, VecDeque};

use crate::capsule::{self, Capsule, CapsuleReader};
use crate::connection::{ConnectionError, Core};
use crate::error::Code;
use crate::frame::{self, put_varint};
use crate::transport::{ReadOutcome, Transport, TransportError};

/// What kind of protocol runs in an extended CONNECT session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// `:protocol` = `webtransport`: streams, datagrams, close capsules.
    WebTransport,
    /// Any other `:protocol`: a capsule stream and HTTP Datagrams only.
    Tunnel,
}

/// Something the application should know about a session.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The peer opened a WebTransport stream in `session`.
    Stream {
        session: u64,
        stream_id: u64,
        bidi: bool,
    },
    /// An HTTP Datagram for `session` (WebTransport: the application
    /// payload; other protocols: the payload after the quarter stream ID).
    Datagram { session: u64, payload: Vec<u8> },
    /// The peer asked to wind the session down (DRAIN_WEBTRANSPORT_SESSION).
    Draining { session: u64 },
    /// The session ended: by a CLOSE_WEBTRANSPORT_SESSION capsule (`code`,
    /// `reason`), by a FIN on the CONNECT stream (code 0), or by a reset of
    /// it (`reset` holds the stream error code).
    Closed {
        session: u64,
        code: u32,
        reason: Vec<u8>,
        reset: Option<u64>,
    },
}

/// Why a session operation did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// No such session, or it is not established yet.
    NotEstablished,
    /// The session was closed (by either side).
    Closed,
    /// The operation needs a WebTransport session.
    NotWebTransport,
    /// No such WebTransport stream, or it belongs to another session.
    UnknownStream,
    /// The peer reset the stream (receiving) or sent STOP_SENDING
    /// (sending), with this code.
    StreamReset(u64),
    /// The datagram is larger than the peer accepts.
    TooLarge,
    /// A close reason over 1024 bytes or not UTF-8.
    BadReason,
    /// The transport cannot open a stream yet (stream credit).
    NotReady,
    /// The connection failed.
    Connection,
    /// The session belongs to another connection.
    Foreign,
}

/// How a datagram was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatagramPath {
    /// A QUIC DATAGRAM frame (RFC 9221): unreliable.
    Quic,
    /// A DATAGRAM capsule on the CONNECT stream: reliable and ordered.
    Capsule,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub sessions: u64,
    pub datagrams_received: u64,
    pub datagrams_sent: u64,
    /// Datagrams for no live session, or over the pending bound.
    pub datagrams_dropped: u64,
    pub streams_accepted: u64,
    pub streams_rejected: u64,
}

pub(crate) struct SessionState {
    pub(crate) kind: SessionKind,
    pub(crate) established: bool,
    pub(crate) capsules: CapsuleReader,
    /// Either side closed it; no more streams or datagrams.
    pub(crate) closed: bool,
}

struct WtStream {
    session: u64,
    /// Bytes read from the transport after the stream header.
    held: Vec<u8>,
    /// The receiving part ended (FIN or reset).
    recv_done: bool,
    /// The sending part was finished or reset (or does not exist).
    send_done: bool,
}

struct Held {
    id: u64,
    session: u64,
    bidi: bool,
    rest: Vec<u8>,
}

/// Session bookkeeping for one connection.
#[derive(Default)]
pub(crate) struct Sessions {
    pub(crate) map: BTreeMap<u64, SessionState>,
    streams: BTreeMap<u64, WtStream>,
    held: Vec<Held>,
    pending_datagrams: VecDeque<(u64, Vec<u8>)>,
    pub(crate) events: VecDeque<SessionEvent>,
    pub(crate) stats: SessionStats,
}

/// Streams held for sessions not established yet, per connection.
const MAX_HELD_STREAMS: usize = 16;
/// Datagrams held for sessions not established yet, per connection.
const MAX_PENDING_DATAGRAMS: usize = 16;

fn session_header(bidi: bool, session: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(16);
    put_varint(
        &mut h,
        if bidi {
            frame::WEBTRANSPORT_STREAM
        } else {
            frame::STREAM_WEBTRANSPORT
        },
    );
    put_varint(&mut h, session);
    h
}

impl Core {
    /// WebTransport is offered by us and by the peer.
    pub(crate) fn webtransport_negotiated(&self) -> bool {
        self.sent_settings.webtransport_max_sessions.is_some()
            && self
                .peer_settings
                .is_some_and(|s| s.webtransport_max_sessions.is_some())
    }

    /// QUIC DATAGRAM frames carry HTTP Datagrams on this connection.
    pub(crate) fn quic_datagrams(&self, t: &mut impl Transport) -> bool {
        self.sent_settings.h3_datagram
            && self.peer_settings.is_some_and(|s| s.h3_datagram)
            && t.max_datagram_size().is_some()
    }

    pub(crate) fn live_webtransport_sessions(&self) -> usize {
        self.sessions
            .map
            .values()
            .filter(|s| s.kind == SessionKind::WebTransport && !s.closed)
            .count()
    }

    /// Registers a session for an extended CONNECT stream (not yet
    /// established).
    pub(crate) fn add_session(&mut self, id: u64, kind: SessionKind) {
        self.sessions.map.insert(
            id,
            SessionState {
                kind,
                established: false,
                capsules: CapsuleReader::new(self.cfg.max_frame_payload),
                closed: false,
            },
        );
        self.sessions.stats.sessions += 1;
    }

    /// The 2xx response was sent or received: streams and datagrams held
    /// for the session are released.
    pub(crate) fn establish_session(&mut self, id: u64) {
        let Some(s) = self.sessions.map.get_mut(&id) else {
            return;
        };
        s.established = true;
        let held = std::mem::take(&mut self.sessions.held);
        for h in held {
            if h.session == id {
                self.adopt_stream(h);
            } else {
                self.sessions.held.push(h);
            }
        }
        let pending = std::mem::take(&mut self.sessions.pending_datagrams);
        for (session, payload) in pending {
            if session == id {
                self.sessions.stats.datagrams_received += 1;
                self.sessions
                    .events
                    .push_back(SessionEvent::Datagram { session, payload });
            } else {
                self.sessions
                    .pending_datagrams
                    .push_back((session, payload));
            }
        }
    }

    fn adopt_stream(&mut self, h: Held) {
        self.sessions.stats.streams_accepted += 1;
        self.sessions.streams.insert(
            h.id,
            WtStream {
                session: h.session,
                held: h.rest,
                recv_done: false,
                send_done: !h.bidi,
            },
        );
        self.sessions.events.push_back(SessionEvent::Stream {
            session: h.session,
            stream_id: h.id,
            bidi: h.bidi,
        });
    }

    /// A peer stream announced itself as a WebTransport stream of
    /// `session`. `known_gone` says the session ID names a stream that is
    /// not (or no longer) a session.
    pub(crate) fn incoming_wt_stream(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        session: u64,
        bidi: bool,
        rest: Vec<u8>,
        known_gone: bool,
    ) -> Result<(), ConnectionError> {
        // Session IDs are client-initiated bidirectional stream IDs.
        if !session.is_multiple_of(4) {
            return Err(ConnectionError::BadStreamId(session));
        }
        let h = Held {
            id,
            session,
            bidi,
            rest,
        };
        match self.sessions.map.get(&session) {
            Some(s) if s.kind == SessionKind::WebTransport && s.established && !s.closed => {
                self.adopt_stream(h);
            }
            Some(s) if s.kind == SessionKind::WebTransport && !s.closed => {
                self.hold_stream(t, h);
            }
            None if !known_gone => self.hold_stream(t, h),
            _ => {
                self.sessions.stats.streams_rejected += 1;
                refuse(t, id, bidi, Code::WT_SESSION_GONE);
            }
        }
        Ok(())
    }

    fn hold_stream(&mut self, t: &mut impl Transport, h: Held) {
        if self.sessions.held.len() >= MAX_HELD_STREAMS {
            self.sessions.stats.streams_rejected += 1;
            refuse(t, h.id, h.bidi, Code::WT_BUFFERED_STREAM_REJECTED);
        } else {
            self.sessions.held.push(h);
        }
    }

    /// Streams held for session IDs that turned out not to be sessions.
    pub(crate) fn drop_held_for(&mut self, t: &mut impl Transport, session: u64) {
        let held = std::mem::take(&mut self.sessions.held);
        for h in held {
            if h.session == session {
                self.sessions.stats.streams_rejected += 1;
                refuse(t, h.id, h.bidi, Code::WT_SESSION_GONE);
            } else {
                self.sessions.held.push(h);
            }
        }
    }

    /// Ends a session (either side): its streams are reset, held streams
    /// refused, and the application told once.
    pub(crate) fn end_session(
        &mut self,
        t: &mut impl Transport,
        session: u64,
        code: u32,
        reason: Vec<u8>,
        reset: Option<u64>,
        notify: bool,
    ) {
        let Some(s) = self.sessions.map.get_mut(&session) else {
            return;
        };
        if s.closed {
            return;
        }
        s.closed = true;
        let ids: Vec<u64> = self
            .sessions
            .streams
            .iter()
            .filter(|(_, w)| w.session == session)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let w = self.sessions.streams.remove(&id).expect("listed");
            if !w.send_done {
                t.reset(id, Code::WT_SESSION_GONE);
            }
            if !w.recv_done {
                t.stop_sending(id, Code::WT_SESSION_GONE);
            }
            self.discard(id);
        }
        self.drop_held_for(t, session);
        self.sessions
            .pending_datagrams
            .retain(|(s, _)| *s != session);
        if notify {
            self.sessions.events.push_back(SessionEvent::Closed {
                session,
                code,
                reason,
                reset,
            });
        }
    }

    /// Forgets a session whose CONNECT stream is gone.
    pub(crate) fn forget_session(&mut self, session: u64) {
        self.sessions.map.remove(&session);
    }

    /// Handles DATA payload received on an established session's CONNECT
    /// stream. Returns the stream error code if the capsule stream is
    /// malformed.
    pub(crate) fn session_data(
        &mut self,
        t: &mut impl Transport,
        session: u64,
        data: &[u8],
    ) -> Result<(), Code> {
        let Some(s) = self.sessions.map.get_mut(&session) else {
            return Ok(());
        };
        s.capsules.push(data);
        loop {
            let Some(s) = self.sessions.map.get_mut(&session) else {
                return Ok(());
            };
            let c = match s.capsules.next_capsule() {
                Ok(Some(c)) => c,
                Ok(None) => return Ok(()),
                Err(capsule::CapsuleError::TooLarge {
                    capsule_type: capsule::DATAGRAM,
                    ..
                })
                | Err(capsule::CapsuleError::Malformed {
                    capsule_type: capsule::DATAGRAM,
                }) => return Err(Code::DATAGRAM_ERROR),
                Err(_) => return Err(Code::MESSAGE_ERROR),
            };
            let (kind, closed) = (s.kind, s.closed);
            match c {
                Capsule::Datagram(payload) => {
                    if closed {
                        self.sessions.stats.datagrams_dropped += 1;
                    } else {
                        self.sessions.stats.datagrams_received += 1;
                        self.sessions
                            .events
                            .push_back(SessionEvent::Datagram { session, payload });
                    }
                }
                Capsule::CloseSession { code, reason } if kind == SessionKind::WebTransport => {
                    self.end_session(t, session, code, reason, None, true);
                }
                Capsule::DrainSession if kind == SessionKind::WebTransport && !closed => {
                    self.sessions
                        .events
                        .push_back(SessionEvent::Draining { session });
                }
                // WebTransport capsules on another protocol, and unknown
                // capsules, are skipped (RFC 9297 §3.2).
                _ => {}
            }
        }
    }

    /// Reads QUIC DATAGRAM frames and routes them to sessions.
    pub(crate) fn poll_datagrams(&mut self, t: &mut impl Transport) -> Result<(), ConnectionError> {
        while let Some(d) = t.recv_datagram() {
            if !self.sent_settings.h3_datagram {
                // We never offered HTTP Datagrams (RFC 9297 §2.1.1).
                return Err(ConnectionError::Datagram);
            }
            let Ok((q, n)) = vesper_warp::varint::decode(&d) else {
                return Err(ConnectionError::Datagram);
            };
            // Quarter stream IDs name client bidirectional streams; one
            // past the stream ID space is malformed.
            if q >= 1 << 60 {
                return Err(ConnectionError::Datagram);
            }
            let session = q * 4;
            let payload = d[n..].to_vec();
            match self.sessions.map.get(&session) {
                Some(s) if s.established && !s.closed => {
                    self.sessions.stats.datagrams_received += 1;
                    self.sessions
                        .events
                        .push_back(SessionEvent::Datagram { session, payload });
                }
                Some(s) if !s.closed => {
                    if self.sessions.pending_datagrams.len() < MAX_PENDING_DATAGRAMS {
                        self.sessions
                            .pending_datagrams
                            .push_back((session, payload));
                    } else {
                        self.sessions.stats.datagrams_dropped += 1;
                    }
                }
                _ => self.sessions.stats.datagrams_dropped += 1,
            }
        }
        Ok(())
    }

    fn live_session(&self, session: u64) -> Result<&SessionState, SessionError> {
        if self.error.is_some() {
            return Err(SessionError::Connection);
        }
        match self.sessions.map.get(&session) {
            Some(s) if s.closed => Err(SessionError::Closed),
            Some(s) if s.established => Ok(s),
            _ => Err(SessionError::NotEstablished),
        }
    }

    /// Sends an HTTP Datagram: a QUIC DATAGRAM frame when negotiated, else
    /// a DATAGRAM capsule on the CONNECT stream.
    pub(crate) fn send_session_datagram(
        &mut self,
        t: &mut impl Transport,
        session: u64,
        payload: &[u8],
    ) -> Result<DatagramPath, SessionError> {
        self.live_session(session)?;
        if self.quic_datagrams(t) {
            let mut d = Vec::with_capacity(payload.len() + 8);
            put_varint(&mut d, session / 4);
            d.extend_from_slice(payload);
            if t.max_datagram_size().is_some_and(|m| d.len() > m) {
                return Err(SessionError::TooLarge);
            }
            return match t.send_datagram(&d) {
                Ok(()) => {
                    self.sessions.stats.datagrams_sent += 1;
                    Ok(DatagramPath::Quic)
                }
                Err(TransportError::TooLarge) => Err(SessionError::TooLarge),
                Err(_) => Err(SessionError::Connection),
            };
        }
        if payload.len() > self.cfg.max_frame_payload {
            return Err(SessionError::TooLarge);
        }
        let mut c = Vec::with_capacity(payload.len() + 8);
        capsule::write_capsule(&mut c, capsule::DATAGRAM, payload);
        if self.queued(session) + c.len() + 9 > self.cfg.max_stream_buffer {
            return Err(SessionError::TooLarge);
        }
        let mut f = Vec::with_capacity(c.len() + 9);
        frame::write_frame(&mut f, frame::DATA, &c);
        self.queue(session, &f);
        self.flush(t);
        self.sessions.stats.datagrams_sent += 1;
        Ok(DatagramPath::Capsule)
    }

    /// Queues the CLOSE_WEBTRANSPORT_SESSION capsule (WebTransport only)
    /// and the FIN that end our side of a session.
    pub(crate) fn queue_session_close(
        &mut self,
        session: u64,
        code: u32,
        reason: &[u8],
    ) -> Result<(), SessionError> {
        if reason.len() > capsule::MAX_CLOSE_REASON || std::str::from_utf8(reason).is_err() {
            return Err(SessionError::BadReason);
        }
        let kind = self.sessions.map.get(&session).map(|s| s.kind);
        let already = self.sessions.map.get(&session).is_none_or(|s| s.closed);
        if kind == Some(SessionKind::WebTransport) && !already {
            let mut c = Vec::new();
            capsule::write_close_session(&mut c, code, reason);
            let mut f = Vec::new();
            frame::write_frame(&mut f, frame::DATA, &c);
            self.queue(session, &f);
        }
        self.queue_fin(session);
        Ok(())
    }

    /// Opens a WebTransport stream in `session`.
    pub(crate) fn open_wt_stream(
        &mut self,
        t: &mut impl Transport,
        session: u64,
        bidi: bool,
    ) -> Result<u64, SessionError> {
        if self.live_session(session)?.kind != SessionKind::WebTransport {
            return Err(SessionError::NotWebTransport);
        }
        let id = if bidi { t.open_bidi() } else { t.open_uni() }.map_err(|e| match e {
            TransportError::NotReady => SessionError::NotReady,
            _ => SessionError::Connection,
        })?;
        self.queue(id, &session_header(bidi, session));
        self.sessions.streams.insert(
            id,
            WtStream {
                session,
                held: Vec::new(),
                recv_done: !bidi,
                send_done: false,
            },
        );
        self.flush(t);
        Ok(id)
    }

    fn wt_stream(&mut self, id: u64) -> Result<&mut WtStream, SessionError> {
        self.sessions
            .streams
            .get_mut(&id)
            .ok_or(SessionError::UnknownStream)
    }

    /// The session a WebTransport stream belongs to.
    pub(crate) fn wt_session_of(&self, id: u64) -> Option<u64> {
        self.sessions.streams.get(&id).map(|w| w.session)
    }

    /// Writes stream data straight to the transport (after the stream
    /// header has left). Returns how many bytes were accepted.
    pub(crate) fn wt_write(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        data: &[u8],
    ) -> Result<usize, SessionError> {
        let w = self.wt_stream(id)?;
        if w.send_done {
            return Err(SessionError::UnknownStream);
        }
        if self.queued(id) > 0 {
            self.flush(t);
            if self.queued(id) > 0 {
                return Ok(0);
            }
        }
        match t.write(id, data) {
            Ok(n) => Ok(n),
            Err(TransportError::Reset(code)) => {
                self.wt_stream(id)?.send_done = true;
                self.retire_wt(id);
                Err(SessionError::StreamReset(code))
            }
            Err(TransportError::NotReady) => Ok(0),
            Err(_) => Err(SessionError::Connection),
        }
    }

    pub(crate) fn wt_finish(
        &mut self,
        t: &mut impl Transport,
        id: u64,
    ) -> Result<(), SessionError> {
        let w = self.wt_stream(id)?;
        if w.send_done {
            return Err(SessionError::UnknownStream);
        }
        w.send_done = true;
        if self.queued(id) > 0 {
            self.queue_fin(id);
            self.flush(t);
        } else {
            let _ = t.finish(id);
        }
        self.retire_wt(id);
        Ok(())
    }

    /// Reads stream data. `fin` marks the end of the peer's data.
    pub(crate) fn wt_read(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        out: &mut [u8],
    ) -> Result<ReadOutcome, SessionError> {
        let w = self.wt_stream(id)?;
        if w.recv_done && w.held.is_empty() {
            return Ok(ReadOutcome { len: 0, fin: true });
        }
        if !w.held.is_empty() {
            let n = out.len().min(w.held.len());
            out[..n].copy_from_slice(&w.held[..n]);
            w.held.drain(..n);
            return Ok(ReadOutcome { len: n, fin: false });
        }
        match t.read(id, out) {
            Ok(r) => {
                if r.fin {
                    self.wt_stream(id)?.recv_done = true;
                    self.retire_wt(id);
                }
                Ok(r)
            }
            Err(TransportError::Reset(code)) => {
                self.wt_stream(id)?.recv_done = true;
                self.retire_wt(id);
                Err(SessionError::StreamReset(code))
            }
            Err(TransportError::NotReady) => Ok(ReadOutcome { len: 0, fin: false }),
            Err(TransportError::UnknownStream) => {
                self.wt_stream(id)?.recv_done = true;
                self.retire_wt(id);
                Ok(ReadOutcome { len: 0, fin: true })
            }
            Err(_) => Err(SessionError::Connection),
        }
    }

    /// Abandons sending with a WebTransport application error code.
    pub(crate) fn wt_reset(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        let w = self.wt_stream(id)?;
        if !w.send_done {
            w.send_done = true;
            t.reset(id, Code::webtransport(code));
        }
        self.discard(id);
        self.retire_wt(id);
        Ok(())
    }

    /// Asks the peer to stop sending, with a WebTransport application code.
    pub(crate) fn wt_stop_sending(
        &mut self,
        t: &mut impl Transport,
        id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        let w = self.wt_stream(id)?;
        if !w.recv_done {
            w.recv_done = true;
            w.held.clear();
            t.stop_sending(id, Code::webtransport(code));
        }
        self.retire_wt(id);
        Ok(())
    }

    fn retire_wt(&mut self, id: u64) {
        if self
            .sessions
            .streams
            .get(&id)
            .is_some_and(|w| w.recv_done && w.send_done && w.held.is_empty())
        {
            self.sessions.streams.remove(&id);
        }
    }

    /// WebTransport streams open (either direction not finished).
    pub(crate) fn wt_streams(&self) -> usize {
        self.sessions.streams.len()
    }
}

fn refuse(t: &mut impl Transport, id: u64, bidi: bool, code: Code) {
    t.stop_sending(id, code);
    if bidi {
        t.reset(id, code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_headers() {
        assert_eq!(session_header(true, 0), [0x40, 0x41, 0x00]);
        assert_eq!(session_header(false, 4), [0x40, 0x54, 0x04]);
    }
}
