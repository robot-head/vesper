#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use vesper_warp_tls::rustls::RootCertStore;
use vesper_warp_tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use vesper_warp_tls::{
    ClientSetup, ConnectionId, Event, FixedTime, QuicTls, ServerCids, ServerSetup, Space,
    SpaceKeys, TlsError, TransportParameters,
};

pub const ALPN: &[u8] = b"vesper-warp/1";
pub const SERVER_NAME: &str = "warp.test";
pub const CLIENT_SCID: &[u8] = &[0xc1, 0xc2, 0xc3, 0xc4];
pub const SERVER_SCID: &[u8] = &[0x5e, 0x5e, 0x5e, 0x5e, 0x5e];
pub const ODCID: &[u8] = &[0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];

pub const TEST_CA: &[u8] = include_bytes!("../fixtures/test-ca.der");
pub const SERVER_CERT: &[u8] = include_bytes!("../fixtures/server.der");
pub const SERVER_KEY: &[u8] = include_bytes!("../fixtures/server.key.der");
pub const ROGUE_CERT: &[u8] = include_bytes!("../fixtures/rogue-server.der");
pub const ROGUE_KEY: &[u8] = include_bytes!("../fixtures/rogue-server.key.der");

/// 2030-01-01T00:00:00Z, inside the fixtures' validity (2026–2126).
pub const NOW_SECS: u64 = 1_893_456_000;

pub fn time(secs: u64) -> Arc<FixedTime> {
    Arc::new(FixedTime(UnixTime::since_unix_epoch(Duration::from_secs(
        secs,
    ))))
}

pub fn roots() -> RootCertStore {
    let mut r = RootCertStore::empty();
    r.add(CertificateDer::from(TEST_CA.to_vec())).unwrap();
    r
}

pub fn client_setup() -> ClientSetup {
    ClientSetup::with_time(roots(), vec![ALPN.to_vec()], time(NOW_SECS)).unwrap()
}

pub fn server_setup_with(cert: &[u8], key: &[u8], alpn: &[u8]) -> ServerSetup {
    ServerSetup::new(
        vec![CertificateDer::from(cert.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
        vec![alpn.to_vec()],
    )
    .unwrap()
}

pub fn server_setup() -> ServerSetup {
    server_setup_with(SERVER_CERT, SERVER_KEY, ALPN)
}

pub fn client_params() -> TransportParameters {
    TransportParameters {
        initial_source_connection_id: Some(ConnectionId::new(CLIENT_SCID).unwrap()),
        initial_max_data: 1 << 20,
        initial_max_stream_data_bidi_local: 1 << 18,
        initial_max_streams_bidi: 16,
        max_idle_timeout_ms: 30_000,
        ..TransportParameters::default()
    }
}

pub fn server_params() -> TransportParameters {
    TransportParameters {
        initial_source_connection_id: Some(ConnectionId::new(SERVER_SCID).unwrap()),
        stateless_reset_token: Some([0xab; 16]),
        initial_max_data: 1 << 21,
        initial_max_stream_data_bidi_remote: 1 << 18,
        initial_max_streams_uni: 3,
        disable_active_migration: true,
        ..TransportParameters::default()
    }
}

pub fn new_client() -> QuicTls {
    QuicTls::client(&client_setup(), SERVER_NAME, &client_params(), ODCID).unwrap()
}

pub fn new_server() -> QuicTls {
    QuicTls::server(
        &server_setup(),
        &server_params(),
        ServerCids {
            original_dcid: ODCID,
            client_scid: CLIENT_SCID,
            retry_scid: None,
        },
    )
    .unwrap()
}

pub type Frame = (Space, u64, Vec<u8>);

/// One endpoint and everything its driver has emitted.
pub struct Peer {
    pub tls: QuicTls,
    pub keys: [Option<SpaceKeys>; 3],
    pub outgoing: Vec<Frame>,
    pub log: Vec<String>,
    pub peer_params: Option<TransportParameters>,
    pub send_handshake_done: bool,
    sent_handshake: bool,
    recv_handshake: bool,
    peer_scid: Vec<u8>,
}

impl Peer {
    pub fn new(tls: QuicTls, peer_scid: &[u8]) -> Self {
        let mut p = Self {
            tls,
            keys: [None, None, None],
            outgoing: Vec::new(),
            log: Vec::new(),
            peer_params: None,
            send_handshake_done: false,
            sent_handshake: false,
            recv_handshake: false,
            peer_scid: peer_scid.to_vec(),
        };
        p.drain();
        p
    }

    pub fn drain(&mut self) {
        while let Some(e) = self.tls.poll_event() {
            match e {
                Event::KeysReady(k) => {
                    self.log.push(format!("keys {:?}", k.space()));
                    let i = k.space().index();
                    self.keys[i] = Some(k);
                }
                Event::Crypto {
                    space,
                    offset,
                    data,
                } => self.outgoing.push((space, offset, data)),
                Event::PeerTransportParameters(p) => {
                    self.log.push("params".into());
                    self.peer_params = Some(p);
                }
                Event::HandshakeComplete => self.log.push("complete".into()),
                Event::SendHandshakeDone => {
                    self.log.push("send done".into());
                    self.send_handshake_done = true;
                }
                Event::HandshakeConfirmed => self.log.push("confirmed".into()),
                Event::DiscardKeys(s) => self.log.push(format!("discard {s:?}")),
            }
        }
    }

    /// Delivers frames as if each arrived in a packet of its space.
    pub fn receive(&mut self, frames: Vec<Frame>) -> Result<(), TlsError> {
        for (space, offset, data) in frames {
            if self.tls.key_state(space) == vesper_warp_tls::KeyState::Discarded {
                // A packet for a discarded space cannot be decrypted, so
                // its frames never reach TLS.
                continue;
            }
            if space == Space::Initial && self.tls.side() == vesper_warp_tls::Side::Client {
                self.tls.set_peer_initial_scid(&self.peer_scid.clone())?;
            }
            self.tls.on_crypto_frame(space, offset, &data)?;
            if space == Space::Handshake
                && self.tls.side() == vesper_warp_tls::Side::Server
                && !self.recv_handshake
            {
                self.recv_handshake = true;
                self.tls.on_handshake_packet_received()?;
            }
            self.drain();
        }
        Ok(())
    }

    /// Takes the frames to send, reporting the first Handshake packet.
    pub fn send(&mut self) -> Vec<Frame> {
        let out = std::mem::take(&mut self.outgoing);
        if self.tls.side() == vesper_warp_tls::Side::Client
            && !self.sent_handshake
            && out.iter().any(|f| f.0 == Space::Handshake)
        {
            self.sent_handshake = true;
            self.tls.on_handshake_packet_sent().unwrap();
            self.drain();
        }
        out
    }
}

/// Which side failed, and how.
#[derive(Debug)]
pub struct Failure {
    pub client: bool,
    pub error: TlsError,
}

/// Runs the handshake to completion, passing every flight through
/// `mangle`. Delivers HANDSHAKE_DONE to the client.
pub fn run(
    client: &mut Peer,
    server: &mut Peer,
    mut mangle: impl FnMut(Vec<Frame>) -> Vec<Frame>,
) -> Result<(), Failure> {
    for _ in 0..16 {
        let to_server = mangle(client.send());
        server.receive(to_server).map_err(|error| Failure {
            client: false,
            error,
        })?;
        let to_client = mangle(server.send());
        client.receive(to_client).map_err(|error| Failure {
            client: true,
            error,
        })?;
        if server.send_handshake_done && client.tls.is_handshake_complete() {
            server.send_handshake_done = false;
            client
                .tls
                .on_handshake_done_received()
                .map_err(|error| Failure {
                    client: true,
                    error,
                })?;
            client.drain();
        }
        if client.tls.is_handshake_confirmed()
            && server.tls.is_handshake_confirmed()
            && client.outgoing.is_empty()
            && server.outgoing.is_empty()
        {
            return Ok(());
        }
    }
    panic!("handshake did not converge");
}

pub fn pair() -> (Peer, Peer) {
    (
        Peer::new(new_client(), SERVER_SCID),
        Peer::new(new_server(), CLIENT_SCID),
    )
}
