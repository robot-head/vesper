//! A small HTTP/3 client connection.
//!
//! All connection/client/session state lives in Vesper
//! (`vesper/src/{core,client,session}.ves`). [`ClientConnection`] holds the
//! Vesper handle alongside [`Encoder`]/[`Decoder`] and drives [`Transport`] I/O.

use std::collections::VecDeque;

use crate::connection::{
    ConnectionError, ConnectionStats, H3Config, RoleCore, cfg_to_abi, decode_fields,
    decode_session_events, decoder_config_of, drive_poll_datagrams, execute_cmds,
};
use crate::error::Code;
use crate::frame::Settings;
use crate::message::ResponseHead;
use crate::priority::Priority;
use crate::qpack::port::{binding, huff};
use crate::qpack::{Decoder, Encoder, Field};
use crate::session::{DatagramPath, SessionError, SessionEvent, SessionStats};
use crate::transport::{ReadOutcome, Transport, TransportError, is_uni};

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

pub struct ClientConnection {
    core: binding::ClientCore,
    encoder: Encoder,
    decoder: Decoder,
    ready: VecDeque<ClientEvent>,
    rx_scratch: Vec<u8>,
    tmp_ids: Vec<u64>,
    tmp_bytes: Vec<u8>,
    tmp_meta: Vec<u64>,
}

impl ClientConnection {
    pub fn new(cfg: H3Config) -> Self {
        Self {
            core: binding::ClientCore::new(cfg_to_abi(cfg)),
            encoder: Encoder::new(cfg.encoder),
            decoder: Decoder::new(decoder_config_of(&cfg)),
            ready: VecDeque::new(),
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
    pub fn sent_settings(&self) -> Settings {
        self.core.as_ref().conn().sent_settings()
    }
    pub fn in_flight(&self) -> usize {
        self.core.as_ref().c_id().as_slice().len()
    }
    /// Bytes queued on a stream and not yet taken by the transport.
    pub fn queued(&self, stream_id: u64) -> usize {
        self.core.queued(stream_id) as usize
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
                    10 => {
                        let head = decode_resp_head(hbytes, hmeta, w);
                        let body = hbytes[w[7] as usize..(w[7] + w[8]) as usize].to_vec();
                        let trailers = decode_fields(
                            hbytes,
                            hmeta,
                            w[9] as usize,
                            w[10] as usize,
                            w[11] as usize,
                        );
                        self.ready.push_back(ClientEvent::Response(ClientResponse {
                            stream_id: w[0],
                            head,
                            body,
                            trailers,
                        }));
                    }
                    11 => {
                        let head = decode_resp_head(hbytes, hmeta, w);
                        self.ready.push_back(ClientEvent::Interim {
                            stream_id: w[0],
                            head,
                        });
                    }
                    12 => {
                        let head = decode_resp_head(hbytes, hmeta, w);
                        self.ready.push_back(ClientEvent::SessionEstablished {
                            session_id: w[0],
                            head,
                        });
                    }
                    13 => self.ready.push_back(ClientEvent::Failed {
                        stream_id: w[0],
                        code: Code(w[1]),
                    }),
                    14 => self.ready.push_back(ClientEvent::GoAway(w[0])),
                    _ => {}
                }
            }
            decode_session_events(&sev, |ev| self.ready.push_back(ClientEvent::Session(ev)));
        }
        self.core.clear_events();
    }

    fn flush<T: Transport>(&mut self, t: &mut T) {
        RoleCore::Client(&mut self.core).flush(t, &mut self.encoder, &mut self.decoder);
    }

    fn open_local<T: Transport>(&mut self, t: &mut T) -> Result<bool, ConnectionError> {
        RoleCore::Client(&mut self.core).open_local(t)
    }

    fn ready_to_send<T: Transport>(&mut self, t: &mut T) -> Result<(), ClientError> {
        if let Some(e) = self.core.ready_check() {
            return Err(e);
        }
        match self.open_local(t) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ClientError::NotReady),
            Err(e) => {
                self.core.fail(e);
                self.run_cmds(t);
                Err(ClientError::Failed(e))
            }
        }
    }

    fn flatten_request(&mut self, req: &ClientRequest, include_body_cl: bool) {
        self.tmp_bytes.clear();
        self.tmp_meta.clear();
        let mut push = |name: &[u8], val: &[u8], sens: bool| {
            self.tmp_bytes.extend_from_slice(name);
            self.tmp_bytes.extend_from_slice(val);
            self.tmp_meta.extend_from_slice(&[
                name.len() as u64,
                val.len() as u64,
                u64::from(sens),
            ]);
        };
        push(b":method", req.method.as_bytes(), false);
        if let Some(p) = &req.protocol {
            push(b":protocol", p.as_bytes(), false);
        }
        push(b":scheme", req.scheme.as_bytes(), false);
        push(b":authority", req.authority.as_bytes(), false);
        push(b":path", req.path.as_bytes(), false);
        for f in &req.fields {
            push(&f.name, &f.value, f.sensitive);
        }
        if include_body_cl && !req.body.is_empty() {
            let s = req.body.len().to_string();
            push(b"content-length", s.as_bytes(), false);
        }
    }

    fn open_request<T: Transport>(
        &mut self,
        t: &mut T,
        req: &ClientRequest,
        fin: bool,
        include_body: bool,
        is_session: bool,
        wt_session: bool,
    ) -> Result<u64, ClientError> {
        let id = match t.open_bidi() {
            Ok(id) => id,
            Err(TransportError::NotReady) => return Err(ClientError::NotReady),
            Err(e) => return Err(ClientError::Failed(ConnectionError::Transport(e))),
        };
        self.flatten_request(req, include_body);
        let body: &[u8] = if include_body { &req.body } else { &[] };
        self.core.register_request(
            &mut self.encoder.core,
            huff(),
            id,
            fin,
            req.method == "HEAD",
            req.protocol.is_some(),
            is_session,
            wt_session,
            &self.tmp_bytes,
            &self.tmp_meta,
            body,
        );
        self.flush(t);
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
        self.open_request(t, req, true, true, false, false)
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
        let max_buf = self.core.as_ref().conn().cfg().max_stream_buffer as usize;
        if !req.body.is_empty() && req.body.len() + 9 > max_buf {
            return Err(ClientError::BufferFull);
        }
        let id = self.open_request(t, req, false, false, false, false)?;
        if !req.body.is_empty()
            && let Err(e) = self.send_body(t, id, &req.body)
        {
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
        self.core.send_body(id, data)?;
        self.flush(t);
        Ok(())
    }

    /// Ends a streaming request body (FIN).
    pub fn finish_body<T: Transport>(&mut self, t: &mut T, id: u64) -> Result<(), ClientError> {
        self.core.finish_body(id)?;
        self.flush(t);
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
        let has_proto = req.protocol.is_some();
        let is_wt = req.protocol.as_deref() == Some("webtransport");
        self.core
            .check_open_session(has_proto, req.method == "CONNECT", is_wt)?;
        self.open_request(t, req, false, false, true, is_wt)
    }

    /// Closes a session: CLOSE_WEBTRANSPORT_SESSION (WebTransport) and FIN.
    pub fn close_session<T: Transport>(
        &mut self,
        t: &mut T,
        session: u64,
        code: u32,
        reason: &[u8],
    ) -> Result<(), SessionError> {
        self.core.close_session(session, code, reason)?;
        self.run_cmds(t);
        self.flush(t);
        Ok(())
    }

    pub fn open_stream<T: Transport>(
        &mut self,
        t: &mut T,
        session: u64,
        bidi: bool,
    ) -> Result<u64, SessionError> {
        RoleCore::Client(&mut self.core).wt_open(
            t,
            &mut self.encoder,
            &mut self.decoder,
            session,
            bidi,
        )
    }

    pub fn send_datagram<T: Transport>(
        &mut self,
        t: &mut T,
        session: u64,
        payload: &[u8],
    ) -> Result<DatagramPath, SessionError> {
        RoleCore::Client(&mut self.core).wt_send_datagram(
            t,
            &mut self.encoder,
            &mut self.decoder,
            session,
            payload,
        )
    }

    pub fn write_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        data: &[u8],
    ) -> Result<usize, SessionError> {
        RoleCore::Client(&mut self.core).wt_write(
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
        RoleCore::Client(&mut self.core).wt_finish(
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
        RoleCore::Client(&mut self.core).wt_read(t, stream_id, out)
    }

    pub fn reset_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        RoleCore::Client(&mut self.core).wt_reset(t, stream_id, code)
    }

    pub fn stop_stream<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        code: u32,
    ) -> Result<(), SessionError> {
        RoleCore::Client(&mut self.core).wt_stop_sending(t, stream_id, code)
    }

    /// Changes a request's priority with PRIORITY_UPDATE (RFC 9218 §7.2).
    pub fn update_priority<T: Transport>(
        &mut self,
        t: &mut T,
        stream_id: u64,
        priority: Priority,
    ) -> Result<(), ClientError> {
        self.ready_to_send(t)?;
        self.core.update_priority(stream_id, priority)?;
        self.flush(t);
        Ok(())
    }

    /// Cancels a request: STOP_SENDING and RESET_STREAM with
    /// H3_REQUEST_CANCELLED.
    pub fn cancel<T: Transport>(&mut self, t: &mut T, id: u64) {
        self.core.cancel(&mut self.decoder.core, id);
        self.run_cmds(t);
        self.flush(t);
    }

    pub fn poll<T: Transport>(&mut self, t: &mut T) {
        if self.error().is_some() {
            return;
        }
        if let Err(e) = self.poll_inner(t) {
            self.core.fail(e);
            self.run_cmds(t);
        }
        self.flush(t);
        self.drain_events();
    }

    fn poll_inner<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        if !self.open_local(t)? {
            return Ok(());
        }
        while let Some(id) = t.accept() {
            if is_uni(id) {
                self.core
                    .accept_uni(&mut self.encoder.core, &mut self.decoder.core, huff(), id);
                self.run_cmds(t);
            } else {
                if self.core.accept_bidi_need_sniff(id)? {
                    self.read_sniffed(t)?;
                }
                self.core.accept_bidi_finish(id);
                self.run_cmds(t);
            }
        }
        self.read_uni(t)?;
        self.read_sniffed(t)?;
        self.core.handle_goaway();
        self.run_cmds(t);
        let unb = self.core.drain_unblocked(&mut self.decoder.core, huff());
        self.run_cmds(t);
        unb?;
        self.tmp_ids.clear();
        self.tmp_ids
            .extend_from_slice(self.core.as_ref().c_id().as_slice());
        for i in 0..self.tmp_ids.len() {
            let id = self.tmp_ids[i];
            self.read_stream_frames(t, id)?;
        }
        drive_poll_datagrams(t, |d| self.core.on_quic_datagram(d))?;
        Ok(())
    }

    fn read_uni<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        RoleCore::Client(&mut self.core).read_uni(
            t,
            &mut self.encoder,
            &mut self.decoder,
            &mut self.rx_scratch,
            &mut self.tmp_ids,
        )
    }

    /// Reads server bidi streams up to their WEBTRANSPORT_STREAM signal.
    fn read_sniffed<T: Transport>(&mut self, t: &mut T) -> Result<(), ConnectionError> {
        self.tmp_ids.clear();
        self.tmp_ids
            .extend_from_slice(self.core.as_ref().sn_id().as_slice());
        let mut buf = [0u8; 64];
        for i in 0..self.tmp_ids.len() {
            let id = self.tmp_ids[i];
            let r = match t.read(id, &mut buf) {
                Ok(r) => r,
                Err(TransportError::NotReady) => continue,
                Err(_) => {
                    self.core.sniff_err(id);
                    continue;
                }
            };
            let res = self.core.sniff_data(id, &buf[..r.len], r.fin);
            self.run_cmds(t);
            res?;
        }
        Ok(())
    }

    pub fn poll_event(&mut self) -> Option<ClientEvent> {
        self.drain_events();
        self.ready.pop_front()
    }

    fn read_stream_frames<T: Transport>(
        &mut self,
        t: &mut T,
        id: u64,
    ) -> Result<(), ConnectionError> {
        loop {
            if !self.core.can_read_stream(id) {
                return Ok(());
            }
            let r = match t.read(id, &mut self.rx_scratch) {
                Ok(r) => r,
                Err(TransportError::Reset(code)) => {
                    self.core.stream_reset(&mut self.decoder.core, id, code);
                    self.run_cmds(t);
                    return Ok(());
                }
                Err(TransportError::NotReady | TransportError::UnknownStream) => return Ok(()),
                Err(e) => return Err(ConnectionError::Transport(e)),
            };
            if r.len > 0 || r.fin {
                let res = self.core.stream_data(
                    &mut self.decoder.core,
                    huff(),
                    id,
                    &self.rx_scratch[..r.len],
                    r.fin,
                );
                self.run_cmds(t);
                res?;
            }
            if r.fin || r.len == 0 {
                return Ok(());
            }
        }
    }
}

fn decode_resp_head(bytes: &[u8], meta: &[u64], w: &[u64]) -> ResponseHead {
    ResponseHead {
        status: w[1] as u16,
        fields: decode_fields(bytes, meta, w[4] as usize, w[5] as usize, w[6] as usize),
        content_length: (w[2] != 0).then_some(w[3]),
    }
}
