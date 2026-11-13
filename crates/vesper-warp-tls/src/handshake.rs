//! The sans-I/O QUIC TLS 1.3 handshake driver (RFC 9001 §4).

use std::collections::VecDeque;
use std::fmt;

use rustls::CipherSuite;
use rustls::pki_types::ServerName;
use rustls::quic::{self, KeyChange};
use vesper_warp_crypto::{
    Cipher, InitialSecrets, OneRttKeys, Opener, Sealer, Side, TAG_LEN, TrafficSecret,
};

use crate::Space;
use crate::config::{Captured, ClientSetup, ServerSetup, with_capture};
use crate::error::TlsError;
use crate::params::{CidExpectations, ConnectionId, TransportParameters};
use crate::reassembly::{CryptoReassembler, DEFAULT_CRYPTO_BUFFER, DEFAULT_MAX_RANGES};

/// Lifecycle of one packet-number space's keys (RFC 9001 §4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// Not derived yet.
    Pending,
    /// Handed to the caller in [`Event::KeysReady`].
    Installed,
    /// Discarded; packets and CRYPTO data for the space are rejected.
    Discarded,
}

/// Both directions' traffic secrets for one space, in the form
/// `vesper-warp-crypto` consumes. `local` seals, `remote` opens.
pub struct SpaceKeys {
    space: Space,
    local: TrafficSecret,
    remote: TrafficSecret,
}

impl fmt::Debug for SpaceKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpaceKeys")
            .field("space", &self.space)
            .field("cipher", &self.cipher())
            .finish_non_exhaustive()
    }
}

impl SpaceKeys {
    pub fn space(&self) -> Space {
        self.space
    }

    pub fn cipher(&self) -> Cipher {
        self.local.cipher()
    }

    pub fn local(&self) -> &TrafficSecret {
        &self.local
    }

    pub fn remote(&self) -> &TrafficSecret {
        &self.remote
    }

    /// `(local, remote)` secrets.
    pub fn into_secrets(self) -> (TrafficSecret, TrafficSecret) {
        (self.local, self.remote)
    }

    /// Long-header (Initial or Handshake) packet protection.
    pub fn into_sealer_opener(self) -> Result<(Sealer, Opener), TlsError> {
        if self.space == Space::Data {
            return Err(TlsError::KeyUpdate(
                "1-RTT secrets must go through into_one_rtt for key-phase handling",
            ));
        }
        Ok((
            Sealer::new(self.local.keys()),
            Opener::new(self.remote.keys()),
        ))
    }

    /// 1-RTT packet protection with RFC 9001 §6 key updates. Generation 1
    /// was checked against rustls's own next-phase keys at export.
    pub fn into_one_rtt(self, retire_window_ns: u64) -> Result<OneRttKeys, TlsError> {
        if self.space != Space::Data {
            return Err(TlsError::KeyUpdate("only 1-RTT secrets form OneRttKeys"));
        }
        // Both secrets were built with the negotiated cipher, so the only
        // possible failure (CipherMismatch) is an internal invariant break.
        OneRttKeys::new(self.local, self.remote, retire_window_ns)
            .map_err(|_| TlsError::KeyExportMismatch(Space::Data))
    }
}

/// The 0-RTT secret (RFC 9001 §4.6.1): the client seals early data with
/// it, the server opens it. There is no server-to-client 0-RTT direction.
pub struct ZeroRttKeys {
    side: Side,
    secret: TrafficSecret,
}

impl fmt::Debug for ZeroRttKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZeroRttKeys")
            .field("side", &self.side)
            .field("cipher", &self.cipher())
            .finish_non_exhaustive()
    }
}

impl ZeroRttKeys {
    pub fn cipher(&self) -> Cipher {
        self.secret.cipher()
    }

    pub fn side(&self) -> Side {
        self.side
    }

    pub fn secret(&self) -> &TrafficSecret {
        &self.secret
    }

    /// Client: protection for outgoing 0-RTT packets.
    pub fn into_sealer(self) -> Result<Sealer, TlsError> {
        if self.side != Side::Client {
            return Err(TlsError::ProtocolViolation("only a client seals 0-RTT"));
        }
        Ok(Sealer::new(self.secret.keys()))
    }

    /// Server: protection for incoming 0-RTT packets.
    pub fn into_opener(self) -> Result<Opener, TlsError> {
        if self.side != Side::Server {
            return Err(TlsError::ProtocolViolation("only a server opens 0-RTT"));
        }
        Ok(Opener::new(self.secret.keys()))
    }
}

/// What became of 0-RTT on this connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarlyData {
    /// Not offered (client) or not accepted (server; a server cannot tell
    /// a refused offer from no offer).
    None,
    /// Client: offered on a resumed session, outcome not known yet.
    Offered,
    /// The server accepted the client's 0-RTT data.
    Accepted,
    /// Client: the server refused it. Everything sent in 0-RTT is void
    /// (RFC 9000 §7.4.1): the client starts its streams afresh in 1-RTT.
    Rejected,
}

/// Output of the driver, drained with [`QuicTls::poll_event`] in order.
#[derive(Debug)]
pub enum Event {
    /// New keys for a space. Initial keys are produced at construction
    /// (and again after a Retry); Handshake and 1-RTT keys as TLS derives
    /// them.
    KeysReady(SpaceKeys),
    /// CRYPTO frame data to send in `space`, at stream `offset`.
    Crypto {
        space: Space,
        offset: u64,
        data: Vec<u8>,
    },
    /// The peer's transport parameters, decoded, value-checked and
    /// authenticated against the connection IDs (RFC 9000 §7.3).
    PeerTransportParameters(TransportParameters),
    /// TLS finished with an agreed ALPN protocol (RFC 9001 §4.1.1).
    HandshakeComplete,
    /// Server only: send a HANDSHAKE_DONE frame (RFC 9001 §4.1.2).
    SendHandshakeDone,
    /// The handshake is confirmed (RFC 9001 §4.1.2); key updates are
    /// permitted from now on.
    HandshakeConfirmed,
    /// Drop every key and pending packet for this space (RFC 9001 §4.9).
    DiscardKeys(Space),
    /// 0-RTT keys: a client offering early data gets them with the
    /// ClientHello; a server gets them once it accepted early data.
    ZeroRttKeys(ZeroRttKeys),
    /// Client offering 0-RTT: the transport parameters the server sent on
    /// the resumed connection (RFC 9000 §7.4.1), which bound what may be
    /// sent before the handshake completes. Decoded and value-checked, but
    /// not authenticated against this connection's connection IDs.
    RememberedTransportParameters(TransportParameters),
    /// Client: the server accepted or refused the 0-RTT offer (reported
    /// before [`Event::HandshakeComplete`]). Server: early data was
    /// accepted (reported after [`Event::ZeroRttKeys`]).
    EarlyData(EarlyData),
}

/// Connection IDs a server has seen before it starts TLS.
#[derive(Clone, Copy, Debug)]
pub struct ServerCids<'a> {
    /// Destination Connection ID of the client's first Initial packet
    /// (before any Retry).
    pub original_dcid: &'a [u8],
    /// Source Connection ID of the client's Initial packet.
    pub client_scid: &'a [u8],
    /// The Source Connection ID this server put in a Retry, if it sent
    /// one; the client then uses it as its Destination Connection ID.
    pub retry_scid: Option<&'a [u8]>,
}

/// A QUIC TLS 1.3 handshake for one connection, client or server.
///
/// Feed received CRYPTO frames with [`QuicTls::on_crypto_frame`], report
/// packet events with the `on_*` methods, and drain [`Event`]s with
/// [`QuicTls::poll_event`]. Errors from `on_crypto_frame` and
/// `on_handshake_done_received` are fatal: the driver then refuses every
/// further input with [`TlsError::Closed`]. Errors from `on_retry`,
/// `set_peer_initial_scid` and the packet-sent/received reports mean
/// "discard that packet" or caller misuse and leave the driver usable.
pub struct QuicTls {
    side: Side,
    conn: quic::Connection,
    key_state: [KeyState; 3],
    rx: [CryptoReassembler; 3],
    read_space: Space,
    write_space: Space,
    tx_offset: [u64; 3],
    events: VecDeque<Event>,
    captured: Captured,
    original_dcid: ConnectionId,
    retry_scid: Option<ConnectionId>,
    peer_initial_scid: Option<ConnectionId>,
    peer_params: Option<TransportParameters>,
    server_initial_seen: bool,
    complete: bool,
    confirmed: bool,
    failed: Option<u64>,
    scratch: Vec<u8>,
    early: EarlyData,
    zero_rtt_keys: KeyState,
    /// Client: Handshake-space data (EncryptedExtensions) reached rustls,
    /// so its transport parameters are the server's, not remembered ones.
    hs_read: bool,
}

fn reassemblers(capacity: usize, max_ranges: usize) -> [CryptoReassembler; 3] {
    [
        CryptoReassembler::new(capacity, max_ranges),
        CryptoReassembler::new(capacity, max_ranges),
        CryptoReassembler::new(capacity, max_ranges),
    ]
}

fn tls_error(e: rustls::Error, alert: Option<rustls::AlertDescription>) -> TlsError {
    TlsError::Tls {
        alert: alert.map(u8::from),
        message: e.to_string(),
    }
}

fn initial_keys(dcid: &[u8], side: Side) -> Result<SpaceKeys, TlsError> {
    let s = InitialSecrets::new_v1(dcid)
        .map_err(|_| TlsError::Config("Initial destination connection ID longer than 20".into()))?;
    let (local, remote) = match side {
        Side::Client => (s.client, s.server),
        Side::Server => (s.server, s.client),
    };
    Ok(SpaceKeys {
        space: Space::Initial,
        local,
        remote,
    })
}

impl QuicTls {
    /// Starts a client handshake. `params.initial_source_connection_id`
    /// must be the Source Connection ID of the client's Initial packets
    /// and server-only parameters must be absent. `original_dcid` is the
    /// Destination Connection ID of the first Initial packet; Initial keys
    /// are derived from it. Emits Initial keys and the ClientHello.
    pub fn client(
        setup: &ClientSetup,
        server_name: &str,
        params: &TransportParameters,
        original_dcid: &[u8],
    ) -> Result<Self, TlsError> {
        let original_dcid = ConnectionId::new(original_dcid)?;
        let encoded = params.encode(Side::Client)?;
        let name = ServerName::try_from(server_name.to_owned())
            .map_err(|e| TlsError::Config(format!("invalid server name: {e}")))?;
        let mut captured = Captured::default();
        let conn = with_capture(&mut captured, || {
            quic::ClientConnection::new(setup.config.clone(), quic::Version::V1, name, encoded)
        })
        .map_err(|e| TlsError::Config(e.to_string()))?;
        let mut me = Self::new(Side::Client, quic::Connection::Client(conn), original_dcid);
        me.captured = captured;
        me.install_initial(original_dcid.as_bytes())?;
        me.flush_writes()?;
        me.offer_zero_rtt()?;
        Ok(me)
    }

    /// Starts a server handshake for a client whose Initial packet carried
    /// `cids`. The driver fills in `original_destination_connection_id` and
    /// `retry_source_connection_id`; `params.initial_source_connection_id`
    /// must be the server's own Source Connection ID. Emits Initial keys.
    pub fn server(
        setup: &ServerSetup,
        params: &TransportParameters,
        cids: ServerCids<'_>,
    ) -> Result<Self, TlsError> {
        let original_dcid = ConnectionId::new(cids.original_dcid)?;
        let retry_scid = cids.retry_scid.map(ConnectionId::new).transpose()?;
        let mut params = params.clone();
        params.original_destination_connection_id = Some(original_dcid);
        params.retry_source_connection_id = retry_scid;
        let encoded = params.encode(Side::Server)?;
        let mut captured = Captured::default();
        let conn = with_capture(&mut captured, || {
            quic::ServerConnection::new(setup.config.clone(), quic::Version::V1, encoded)
        })
        .map_err(|e| TlsError::Config(e.to_string()))?;
        let mut me = Self::new(Side::Server, quic::Connection::Server(conn), original_dcid);
        me.captured = captured;
        me.retry_scid = retry_scid;
        me.peer_initial_scid = Some(ConnectionId::new(cids.client_scid)?);
        let initial_dcid = retry_scid.unwrap_or(original_dcid);
        me.install_initial(initial_dcid.as_bytes())?;
        Ok(me)
    }

    fn new(side: Side, conn: quic::Connection, original_dcid: ConnectionId) -> Self {
        Self {
            side,
            conn,
            key_state: [KeyState::Pending; 3],
            rx: reassemblers(DEFAULT_CRYPTO_BUFFER, DEFAULT_MAX_RANGES),
            read_space: Space::Initial,
            write_space: Space::Initial,
            tx_offset: [0; 3],
            events: VecDeque::new(),
            captured: Captured::default(),
            original_dcid,
            retry_scid: None,
            peer_initial_scid: None,
            peer_params: None,
            server_initial_seen: false,
            complete: false,
            confirmed: false,
            failed: None,
            scratch: Vec::new(),
            early: EarlyData::None,
            zero_rtt_keys: KeyState::Pending,
            hs_read: false,
        }
    }

    /// Replaces the per-space CRYPTO reassembly bounds. Only allowed before
    /// any CRYPTO data has been received.
    pub fn with_crypto_buffer(
        mut self,
        capacity: usize,
        max_ranges: usize,
    ) -> Result<Self, TlsError> {
        if capacity == 0 || max_ranges == 0 {
            return Err(TlsError::Config(
                "reassembly bounds must be non-zero".into(),
            ));
        }
        if self.rx.iter().any(|r| r.highest_end() > 0) {
            return Err(TlsError::Config("CRYPTO data already received".into()));
        }
        self.rx = reassemblers(capacity, max_ranges);
        Ok(self)
    }

    fn install_initial(&mut self, dcid: &[u8]) -> Result<(), TlsError> {
        let keys = initial_keys(dcid, self.side)?;
        self.key_state[Space::Initial.index()] = KeyState::Installed;
        self.events.push_back(Event::KeysReady(keys));
        Ok(())
    }

    // ----- inputs -----------------------------------------------------

    /// Client: a Retry was received and its integrity tag verified (with
    /// `vesper_warp_crypto::verify_retry_integrity`). Re-derives Initial
    /// keys for the new Destination Connection ID and records it for the
    /// RFC 9000 §7.3 check. An error means "discard the Retry".
    pub fn on_retry(&mut self, retry_scid: &[u8]) -> Result<(), TlsError> {
        self.check_open()?;
        if self.side != Side::Client {
            return Err(TlsError::ProtocolViolation("only a client processes Retry"));
        }
        if self.retry_scid.is_some() || self.server_initial_seen {
            return Err(TlsError::ProtocolViolation(
                "a client accepts at most one Retry, before any server Initial",
            ));
        }
        if retry_scid == self.original_dcid.as_bytes() {
            return Err(TlsError::ProtocolViolation(
                "Retry source connection ID equals the original destination connection ID",
            ));
        }
        let cid = ConnectionId::new(retry_scid)?;
        self.retry_scid = Some(cid);
        self.install_initial(cid.as_bytes())
    }

    /// Client: records the Source Connection ID of the first server Initial
    /// packet; call before feeding its CRYPTO data. A different value later
    /// is an error, and that packet must be discarded (RFC 9000 §7.2).
    pub fn set_peer_initial_scid(&mut self, scid: &[u8]) -> Result<(), TlsError> {
        self.check_open()?;
        if self.side != Side::Client {
            return Err(TlsError::ProtocolViolation(
                "a server takes the client SCID at construction",
            ));
        }
        let cid = ConnectionId::new(scid)?;
        match self.peer_initial_scid {
            None => {
                self.peer_initial_scid = Some(cid);
                self.server_initial_seen = true;
                Ok(())
            }
            Some(seen) if seen == cid => Ok(()),
            Some(_) => Err(TlsError::ProtocolViolation(
                "server Initial with a different source connection ID",
            )),
        }
    }

    /// One received CRYPTO frame in `space`. Fatal on error.
    pub fn on_crypto_frame(
        &mut self,
        space: Space,
        offset: u64,
        data: &[u8],
    ) -> Result<(), TlsError> {
        self.check_open()?;
        let r = self.crypto_inner(space, offset, data);
        if let Err(e) = &r {
            self.failed = Some(e.quic_error_code());
        }
        r
    }

    fn crypto_inner(&mut self, space: Space, offset: u64, data: &[u8]) -> Result<(), TlsError> {
        match self.key_state[space.index()] {
            KeyState::Pending => return Err(TlsError::SpaceUnavailable(space)),
            KeyState::Discarded => return Err(TlsError::SpaceDiscarded(space)),
            KeyState::Installed => {}
        }
        if self.side == Side::Client && space == Space::Initial && self.peer_initial_scid.is_none()
        {
            return Err(TlsError::ProtocolViolation(
                "set_peer_initial_scid must precede server Initial CRYPTO data",
            ));
        }
        let rx = &mut self.rx[space.index()];
        if space < self.read_space {
            // RFC 9001 §4.1.3: an earlier level may only repeat data.
            let end = offset.checked_add(data.len() as u64);
            if end.is_none_or(|e| e > rx.highest_end()) {
                return Err(TlsError::ProtocolViolation(
                    "new CRYPTO data at an encryption level TLS has moved past",
                ));
            }
            return Ok(());
        }
        rx.insert(offset, data)
            .map_err(|error| TlsError::CryptoBuffer { space, error })?;
        if space == self.read_space {
            self.pump()?;
        }
        Ok(())
    }

    /// Client: the first Handshake packet was sent; Initial keys are
    /// discarded (RFC 9001 §4.9.1).
    pub fn on_handshake_packet_sent(&mut self) -> Result<(), TlsError> {
        self.check_open()?;
        if self.side != Side::Client {
            return Err(TlsError::ProtocolViolation(
                "a server discards Initial keys on receipt, not on send",
            ));
        }
        self.discard_initial_after_handshake()
    }

    /// Server: a Handshake packet was successfully processed; Initial keys
    /// are discarded (RFC 9001 §4.9.1).
    pub fn on_handshake_packet_received(&mut self) -> Result<(), TlsError> {
        self.check_open()?;
        if self.side != Side::Server {
            return Err(TlsError::ProtocolViolation(
                "a client discards Initial keys on send, not on receipt",
            ));
        }
        self.discard_initial_after_handshake()
    }

    fn discard_initial_after_handshake(&mut self) -> Result<(), TlsError> {
        if self.key_state[Space::Handshake.index()] == KeyState::Pending {
            return Err(TlsError::SpaceUnavailable(Space::Handshake));
        }
        self.discard(Space::Initial);
        Ok(())
    }

    /// Client: a HANDSHAKE_DONE frame arrived. Fatal on error: a server
    /// receiving one, or a client before completion, is a
    /// `PROTOCOL_VIOLATION` (RFC 9000 §19.20).
    pub fn on_handshake_done_received(&mut self) -> Result<(), TlsError> {
        self.check_open()?;
        let r = if self.side == Side::Server {
            Err(TlsError::ProtocolViolation(
                "server received HANDSHAKE_DONE",
            ))
        } else if !self.complete {
            Err(TlsError::ProtocolViolation(
                "HANDSHAKE_DONE before the handshake completed",
            ))
        } else {
            self.confirm();
            Ok(())
        };
        if let Err(e) = &r {
            self.failed = Some(e.quic_error_code());
        }
        r
    }

    /// Client: a 1-RTT packet was acknowledged, which RFC 9001 §4.1.2 lets
    /// a client treat as confirmation. Ignored before completion.
    pub fn on_one_rtt_ack_received(&mut self) {
        if self.failed.is_none() && self.side == Side::Client && self.complete {
            self.confirm();
        }
    }

    /// The caller dropped its 0-RTT keys (RFC 9001 §4.9.3: a client once it
    /// has 1-RTT keys, a server some time after the first 1-RTT packet).
    pub fn discard_zero_rtt(&mut self) {
        if self.zero_rtt_keys == KeyState::Installed {
            self.zero_rtt_keys = KeyState::Discarded;
        }
    }

    /// RFC 9001 §6.1: initiates a 1-RTT key update, which is not permitted
    /// before the handshake is confirmed.
    pub fn initiate_key_update(&self, keys: &mut OneRttKeys) -> Result<(), TlsError> {
        self.check_open()?;
        if !self.confirmed {
            return Err(TlsError::KeyUpdate(
                "key update before the handshake is confirmed",
            ));
        }
        keys.initiate_key_update().map_err(|_| {
            TlsError::KeyUpdate("the current key phase has no acknowledged packet yet")
        })
    }

    // ----- outputs and state -------------------------------------------

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    pub fn side(&self) -> Side {
        self.side
    }

    pub fn key_state(&self, space: Space) -> KeyState {
        self.key_state[space.index()]
    }

    pub fn is_handshake_complete(&self) -> bool {
        self.complete
    }

    pub fn is_handshake_confirmed(&self) -> bool {
        self.confirmed
    }

    /// The state of this connection's 0-RTT keys.
    pub fn zero_rtt_key_state(&self) -> KeyState {
        self.zero_rtt_keys
    }

    /// What became of 0-RTT.
    pub fn early_data(&self) -> EarlyData {
        self.early
    }

    /// Whether this handshake resumed an earlier session.
    pub fn is_resumed(&self) -> bool {
        self.complete
            && matches!(
                self.conn.handshake_kind(),
                Some(rustls::HandshakeKind::Resumed)
            )
    }

    pub fn key_update_permitted(&self) -> bool {
        self.confirmed && self.failed.is_none()
    }

    /// The agreed ALPN protocol, once the handshake is complete.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        if self.complete {
            self.conn.alpn_protocol()
        } else {
            None
        }
    }

    pub fn peer_transport_parameters(&self) -> Option<&TransportParameters> {
        self.peer_params.as_ref()
    }

    pub fn negotiated_cipher(&self) -> Option<Cipher> {
        self.cipher().ok()
    }

    /// The QUIC error code the driver failed with, if it did.
    pub fn error_code(&self) -> Option<u64> {
        self.failed
    }

    /// The certificate chain the peer presented, once received.
    pub fn peer_certificates(&self) -> Option<&[rustls::pki_types::CertificateDer<'static>]> {
        self.conn.peer_certificates()
    }

    // ----- internals --------------------------------------------------

    fn check_open(&self) -> Result<(), TlsError> {
        match self.failed {
            Some(code) => Err(TlsError::Closed { code }),
            None => Ok(()),
        }
    }

    fn discard(&mut self, space: Space) {
        if self.key_state[space.index()] == KeyState::Installed {
            self.key_state[space.index()] = KeyState::Discarded;
            self.events.push_back(Event::DiscardKeys(space));
        }
    }

    fn confirm(&mut self) {
        if self.confirmed {
            return;
        }
        self.confirmed = true;
        self.events.push_back(Event::HandshakeConfirmed);
        // §4.9.1 normally discards Initial keys earlier; never keep them
        // past confirmation.
        self.discard(Space::Initial);
        self.discard(Space::Handshake);
    }

    fn cipher(&self) -> Result<Cipher, TlsError> {
        let suite = self
            .conn
            .negotiated_cipher_suite()
            .ok_or(TlsError::UnsupportedCipherSuite("none negotiated".into()))?
            .suite();
        match suite {
            CipherSuite::TLS13_AES_128_GCM_SHA256 => Ok(Cipher::Aes128Gcm),
            CipherSuite::TLS13_AES_256_GCM_SHA384 => Ok(Cipher::Aes256Gcm),
            CipherSuite::TLS13_CHACHA20_POLY1305_SHA256 => Ok(Cipher::ChaCha20Poly1305),
            other => Err(TlsError::UnsupportedCipherSuite(format!("{other:?}"))),
        }
    }

    /// Delivers contiguous CRYPTO bytes of the current read space to TLS
    /// until none are left (the read space may advance in between).
    fn pump(&mut self) -> Result<(), TlsError> {
        loop {
            let space = self.read_space;
            self.scratch.clear();
            if self.rx[space.index()].read_into(&mut self.scratch) == 0 {
                return Ok(());
            }
            let conn = &mut self.conn;
            let scratch = &self.scratch;
            let r = with_capture(&mut self.captured, || conn.read_hs(scratch));
            if let Err(e) = r {
                return Err(tls_error(e, self.conn.alert()));
            }
            if space >= Space::Handshake {
                self.hs_read = true;
            }
            self.after_tls()?;
        }
    }

    fn after_tls(&mut self) -> Result<(), TlsError> {
        self.check_peer_params()?;
        self.flush_writes()?;
        if self.side == Side::Server {
            self.accept_zero_rtt()?;
        }
        if !self.complete && !self.conn.is_handshaking() {
            if self.conn.alpn_protocol().is_none() {
                return Err(TlsError::NoApplicationProtocol);
            }
            if self.peer_params.is_none() {
                return Err(TlsError::ProtocolViolation(
                    "handshake completed without peer transport parameters",
                ));
            }
            if self.key_state[Space::Data.index()] != KeyState::Installed {
                return Err(TlsError::KeyExportMismatch(Space::Data));
            }
            if self.early == EarlyData::Offered {
                let accepted = match &self.conn {
                    quic::Connection::Client(c) => c.is_early_data_accepted(),
                    quic::Connection::Server(_) => false,
                };
                self.early = if accepted {
                    EarlyData::Accepted
                } else {
                    EarlyData::Rejected
                };
                self.events.push_back(Event::EarlyData(self.early));
            }
            self.complete = true;
            self.read_space = Space::Data;
            self.events.push_back(Event::HandshakeComplete);
            if self.side == Side::Server {
                // RFC 9001 §4.1.2: the server's handshake is confirmed at
                // completion; it then sends HANDSHAKE_DONE.
                self.events.push_back(Event::SendHandshakeDone);
                self.confirm();
            }
        }
        Ok(())
    }

    fn check_peer_params(&mut self) -> Result<(), TlsError> {
        if self.peer_params.is_some() {
            return Ok(());
        }
        // Before EncryptedExtensions a resuming client's rustls holds the
        // remembered parameters, not this server's.
        if self.side == Side::Client && !self.hs_read {
            return Ok(());
        }
        let Some(bytes) = self.conn.quic_transport_parameters() else {
            return Ok(());
        };
        let peer = match self.side {
            Side::Client => Side::Server,
            Side::Server => Side::Client,
        };
        let params = TransportParameters::decode(bytes, peer)?;
        let scid = self.peer_initial_scid.ok_or(TlsError::ProtocolViolation(
            "peer Initial source connection ID not recorded",
        ))?;
        let seen = CidExpectations {
            peer_initial_scid: scid.as_bytes(),
            original_dcid: Some(self.original_dcid.as_bytes()),
            retry_scid: self.retry_scid.as_ref().map(ConnectionId::as_bytes),
        };
        params.authenticate_cids(peer, &seen)?;
        self.events
            .push_back(Event::PeerTransportParameters(params.clone()));
        self.peer_params = Some(params);
        Ok(())
    }

    fn flush_writes(&mut self) -> Result<(), TlsError> {
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let conn = &mut self.conn;
            let change = with_capture(&mut self.captured, || conn.write_hs(&mut buf));
            if !buf.is_empty() {
                let i = self.write_space.index();
                let offset = self.tx_offset[i];
                self.tx_offset[i] = offset + buf.len() as u64;
                self.events.push_back(Event::Crypto {
                    space: self.write_space,
                    offset,
                    data: std::mem::take(&mut buf),
                });
            }
            match change {
                None if buf.is_empty() => return Ok(()),
                None => {}
                Some(KeyChange::Handshake { keys }) => {
                    self.install(Space::Handshake, &keys, None)?;
                    self.write_space = Space::Handshake;
                    self.read_space = self.read_space.max(Space::Handshake);
                }
                Some(KeyChange::OneRtt { keys, mut next }) => {
                    let next = next.next_packet_keys();
                    self.install(Space::Data, &keys, Some(&next))?;
                    self.write_space = Space::Data;
                }
            }
        }
    }

    /// Client: rustls offered early data on a resumed session. Exports the
    /// 0-RTT secret (checked against rustls's 0-RTT keys) and the
    /// remembered server transport parameters.
    fn offer_zero_rtt(&mut self) -> Result<(), TlsError> {
        let Some(keys) = self.conn.zero_rtt_keys() else {
            return Ok(());
        };
        let Some(remembered) = self.conn.quic_transport_parameters() else {
            return Ok(());
        };
        // A ticket from a server whose parameters no longer decode cannot
        // bound 0-RTT; send none (the handshake itself still resumes).
        let Ok(params) = TransportParameters::decode(remembered, Side::Server) else {
            return Ok(());
        };
        let secret = self.early_secret(&keys)?;
        self.early = EarlyData::Offered;
        self.zero_rtt_keys = KeyState::Installed;
        self.events
            .push_back(Event::RememberedTransportParameters(params));
        self.events.push_back(Event::ZeroRttKeys(ZeroRttKeys {
            side: Side::Client,
            secret,
        }));
        Ok(())
    }

    /// Server: rustls accepted early data while reading the ClientHello.
    fn accept_zero_rtt(&mut self) -> Result<(), TlsError> {
        if self.zero_rtt_keys != KeyState::Pending {
            return Ok(());
        }
        let Some(keys) = self.conn.zero_rtt_keys() else {
            return Ok(());
        };
        let secret = self.early_secret(&keys)?;
        self.early = EarlyData::Accepted;
        self.zero_rtt_keys = KeyState::Installed;
        self.events.push_back(Event::ZeroRttKeys(ZeroRttKeys {
            side: Side::Server,
            secret,
        }));
        self.events.push_back(Event::EarlyData(EarlyData::Accepted));
        Ok(())
    }

    fn early_secret(&mut self, keys: &quic::DirectionalKeys) -> Result<TrafficSecret, TlsError> {
        let cipher = self.cipher()?;
        let raw = self
            .captured
            .client_early
            .take()
            .ok_or(TlsError::KeyExportMismatch(Space::Data))?;
        let secret = TrafficSecret::new(cipher, &raw)
            .map_err(|_| TlsError::KeyExportMismatch(Space::Data))?;
        if !probe(&*keys.packet, Some(&*keys.header), &secret) {
            return Err(TlsError::KeyExportMismatch(Space::Data));
        }
        Ok(secret)
    }

    /// Turns captured secrets into [`SpaceKeys`] after checking that they
    /// reproduce rustls's own packet keys (and, for 1-RTT, rustls's
    /// generation-1 keys from `quic ku`).
    fn install(
        &mut self,
        space: Space,
        keys: &quic::Keys,
        next: Option<&quic::PacketKeySet>,
    ) -> Result<(), TlsError> {
        let cipher = self.cipher()?;
        let (client, server) = match space {
            Space::Handshake => (
                self.captured.client_hs.take(),
                self.captured.server_hs.take(),
            ),
            Space::Data => (
                self.captured.client_app.take(),
                self.captured.server_app.take(),
            ),
            Space::Initial => unreachable!("Initial keys come from the DCID"),
        };
        let (Some(client), Some(server)) = (client, server) else {
            return Err(TlsError::KeyExportMismatch(space));
        };
        let mismatch = |_| TlsError::KeyExportMismatch(space);
        let client = TrafficSecret::new(cipher, &client).map_err(mismatch)?;
        let server = TrafficSecret::new(cipher, &server).map_err(mismatch)?;
        let (local, remote) = match self.side {
            Side::Client => (client, server),
            Side::Server => (server, client),
        };
        let ok = probe(&*keys.local.packet, Some(&*keys.local.header), &local)
            && probe(&*keys.remote.packet, Some(&*keys.remote.header), &remote)
            && next.is_none_or(|n| {
                probe(&*n.local, None, &local.next_phase())
                    && probe(&*n.remote, None, &remote.next_phase())
            });
        if !ok {
            return Err(TlsError::KeyExportMismatch(space));
        }
        self.key_state[space.index()] = KeyState::Installed;
        self.events.push_back(Event::KeysReady(SpaceKeys {
            space,
            local,
            remote,
        }));
        Ok(())
    }
}

/// Seals a probe packet with rustls's key and checks that our key opens
/// it. Header protection is rustls's when given, else our own from the
/// same secret (QUIC key updates keep the original header key, so the
/// generation-1 probe checks only the packet key).
fn probe(
    packet: &dyn quic::PacketKey,
    header: Option<&dyn quic::HeaderProtectionKey>,
    ours: &TrafficSecret,
) -> bool {
    const PN: u64 = 0x1234;
    // Handshake long header: 0xe1 (2-byte PN), v1, empty CIDs, length 50.
    let mut buf = vec![
        0xe1,
        0,
        0,
        0,
        1,
        0,
        0,
        0x40,
        2 + 32 + TAG_LEN as u8,
        0x12,
        0x34,
    ];
    let pn_offset = 9;
    let hdr_len = buf.len();
    let plain: Vec<u8> = (0u8..32).collect();
    buf.extend_from_slice(&plain);
    let (head, payload) = buf.split_at_mut(hdr_len);
    let Ok(tag) = packet.encrypt_in_place(PN, head, payload) else {
        return false;
    };
    buf.extend_from_slice(tag.as_ref());
    let protected = match header {
        Some(hp) => {
            let mut sample = [0u8; 16];
            sample.copy_from_slice(&buf[pn_offset + 4..pn_offset + 20]);
            let (first, rest) = buf.split_at_mut(1);
            hp.encrypt_in_place(
                &sample,
                &mut first[0],
                &mut rest[pn_offset - 1..pn_offset + 1],
            )
            .is_ok()
        }
        None => ours.keys().header.apply(&mut buf, pn_offset).is_ok(),
    };
    if !protected {
        return false;
    }
    let mut opener = Opener::new(ours.keys());
    match opener.open_packet(&mut buf, pn_offset, |_| Some(PN)) {
        Ok(o) => buf[o.header_len..o.header_len + o.payload_len] == plain[..],
        Err(_) => false,
    }
}
