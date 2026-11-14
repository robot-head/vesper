//! HTTP/3, WebTransport and HTTP Datagrams over quinn-proto 0.11.13, an
//! independent QUIC implementation that has QUIC DATAGRAM frames
//! (RFC 9221), joined by an in-memory link. Both quinn-proto endpoints run
//! this crate's HTTP/3 client and server through a [`Transport`] adapter,
//! so SETTINGS_H3_DATAGRAM is negotiated and datagrams travel as real QUIC
//! DATAGRAM frames prefixed by the quarter stream ID (RFC 9297 §2.1).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{
    ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint, ReadError,
    ReadableError, ServerConfig, StreamId, TransportConfig, VarInt, WriteError,
};
use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{Event, H3Config, Response, ServerConnection};
use vesper_prism_h3::error::Code;
use vesper_prism_h3::session::{DatagramPath, SessionEvent};
use vesper_prism_h3::transport::{ReadOutcome, Transport, TransportError};
use vesper_warp::handshake::tls::FixedTime;
use vesper_warp::handshake::tls::rustls;
use vesper_warp::handshake::tls::rustls::RootCertStore;
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};

const TEST_CA: &[u8] = include_bytes!("fixtures/test-ca.der");
const SERVER_CERT: &[u8] = include_bytes!("fixtures/server.der");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/server.key.der");
const NOW_SECS: u64 = 1_893_456_000;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn transport_config() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.mtu_discovery_config(None);
    Arc::new(t)
}

fn server_config() -> Arc<ServerConfig> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(SERVER_CERT.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec())),
        )
        .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut cfg = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
    cfg.transport_config(transport_config());
    Arc::new(cfg)
}

fn client_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(TEST_CA.to_vec())).unwrap();
    let time = Arc::new(FixedTime(UnixTime::since_unix_epoch(Duration::from_secs(
        NOW_SECS,
    ))));
    let mut tls = rustls::ClientConfig::builder_with_details(provider(), time)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut cfg = ClientConfig::new(Arc::new(QuicClientConfig::try_from(Arc::new(tls)).unwrap()));
    cfg.transport_config(transport_config());
    cfg
}

/// A fixed stateless-reset key (no cryptographic strength needed here).
struct ResetKey;
impl quinn_proto::crypto::HmacKey for ResetKey {
    fn sign(&self, data: &[u8], out: &mut [u8]) {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (i, o) in out.iter_mut().enumerate() {
            for b in data {
                h = (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
            }
            h ^= i as u64;
            *o = (h >> 24) as u8;
        }
    }
    fn signature_len(&self) -> usize {
        32
    }
    fn verify(&self, data: &[u8], sig: &[u8]) -> Result<(), quinn_proto::crypto::CryptoError> {
        let mut want = [0u8; 32];
        self.sign(data, &mut want);
        if sig == want {
            Ok(())
        } else {
            Err(quinn_proto::crypto::CryptoError)
        }
    }
}

fn endpoint(server: Option<Arc<ServerConfig>>) -> Endpoint {
    let config = quinn_proto::EndpointConfig::new(Arc::new(ResetKey));
    Endpoint::new(Arc::new(config), server, false, Some([9; 32]))
}

struct Peer {
    ep: Endpoint,
    conn: Option<(ConnectionHandle, Connection)>,
    out: Vec<Vec<u8>>,
    buf: Vec<u8>,
}

impl Peer {
    fn recv(&mut self, at: Instant, from: SocketAddr, d: &[u8]) {
        self.buf.clear();
        match self
            .ep
            .handle(at, from, None, None, BytesMut::from(d), &mut self.buf)
        {
            Some(DatagramEvent::NewConnection(incoming)) => {
                let c = self.ep.accept(incoming, at, &mut self.buf, None).unwrap();
                self.conn = Some(c);
            }
            Some(DatagramEvent::ConnectionEvent(ch, ev)) => {
                if let Some((h, c)) = self.conn.as_mut()
                    && *h == ch
                {
                    c.handle_event(ev);
                }
            }
            Some(DatagramEvent::Response(t)) => self.out.push(self.buf[..t.size].to_vec()),
            None => {}
        }
    }

    fn drive(&mut self, at: Instant) {
        let Some((ch, c)) = self.conn.as_mut() else {
            return;
        };
        if c.poll_timeout().is_some_and(|t| t <= at) {
            c.handle_timeout(at);
        }
        while let Some(ev) = c.poll_endpoint_events() {
            if let Some(back) = self.ep.handle_event(*ch, ev) {
                c.handle_event(back);
            }
        }
        while c.poll().is_some() {}
        loop {
            self.buf.clear();
            let Some(t) = c.poll_transmit(at, 1, &mut self.buf) else {
                break;
            };
            self.out.push(self.buf[..t.size].to_vec());
        }
    }
}

/// [`Transport`] over one quinn-proto connection.
struct QuinnTransport<'a> {
    conn: &'a mut Connection,
    now: Instant,
}

fn sid(id: u64) -> StreamId {
    StreamId::from(VarInt::from_u64(id).expect("stream id"))
}

impl Transport for QuinnTransport<'_> {
    fn open_uni(&mut self) -> Result<u64, TransportError> {
        self.conn
            .streams()
            .open(Dir::Uni)
            .map(u64::from)
            .ok_or(TransportError::NotReady)
    }
    fn open_bidi(&mut self) -> Result<u64, TransportError> {
        self.conn
            .streams()
            .open(Dir::Bi)
            .map(u64::from)
            .ok_or(TransportError::NotReady)
    }
    fn write(&mut self, id: u64, data: &[u8]) -> Result<usize, TransportError> {
        match self.conn.send_stream(sid(id)).write(data) {
            Ok(n) => Ok(n),
            Err(WriteError::Blocked) => Ok(0),
            Err(WriteError::Stopped(code)) => Err(TransportError::Reset(code.into_inner())),
            Err(WriteError::ClosedStream) => Err(TransportError::UnknownStream),
        }
    }
    fn finish(&mut self, id: u64) -> Result<(), TransportError> {
        let _ = self.conn.send_stream(sid(id)).finish();
        Ok(())
    }
    fn read(&mut self, id: u64, out: &mut [u8]) -> Result<ReadOutcome, TransportError> {
        if out.is_empty() {
            return Ok(ReadOutcome { len: 0, fin: false });
        }
        let mut stream = self.conn.recv_stream(sid(id));
        let mut chunks = match stream.read(true) {
            Ok(c) => c,
            Err(ReadableError::ClosedStream) => return Err(TransportError::UnknownStream),
            Err(_) => return Err(TransportError::UnknownStream),
        };
        let mut len = 0;
        let mut fin = false;
        let mut reset = None;
        while len < out.len() {
            match chunks.next(out.len() - len) {
                Ok(Some(chunk)) => {
                    out[len..len + chunk.bytes.len()].copy_from_slice(&chunk.bytes);
                    len += chunk.bytes.len();
                }
                Ok(None) => {
                    fin = true;
                    break;
                }
                Err(ReadError::Blocked) => break,
                Err(ReadError::Reset(code)) => {
                    reset = Some(code.into_inner());
                    break;
                }
            }
        }
        let _ = chunks.finalize();
        match reset {
            Some(code) if len == 0 => Err(TransportError::Reset(code)),
            _ => Ok(ReadOutcome { len, fin }),
        }
    }
    fn reset(&mut self, id: u64, code: Code) {
        let _ = self
            .conn
            .send_stream(sid(id))
            .reset(VarInt::from_u64(code.0).unwrap());
    }
    fn stop_sending(&mut self, id: u64, code: Code) {
        let _ = self
            .conn
            .recv_stream(sid(id))
            .stop(VarInt::from_u64(code.0).unwrap());
    }
    fn accept(&mut self) -> Option<u64> {
        let mut s = self.conn.streams();
        s.accept(Dir::Uni)
            .or_else(|| s.accept(Dir::Bi))
            .map(u64::from)
    }
    fn close(&mut self, code: Code) {
        self.conn
            .close(self.now, VarInt::from_u64(code.0).unwrap(), Bytes::new());
    }
    fn max_datagram_size(&mut self) -> Option<usize> {
        self.conn.datagrams().max_size()
    }
    fn send_datagram(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self.conn.datagrams().max_size() {
            None => return Err(TransportError::Unsupported),
            Some(m) if data.len() > m => return Err(TransportError::TooLarge),
            Some(_) => {}
        }
        self.conn
            .datagrams()
            .send(Bytes::copy_from_slice(data), true)
            .map_err(|_| TransportError::Unsupported)
    }
    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        self.conn.datagrams().recv().map(|b| b.to_vec())
    }
}

struct Net {
    client: Peer,
    server: Peer,
    h3c: ClientConnection,
    h3s: ServerConnection,
    base: Instant,
    now: Duration,
}

const C_ADDR: &str = "198.51.100.7:50000";
const S_ADDR: &str = "192.0.2.1:443";

macro_rules! ct {
    ($n:expr) => {
        &mut QuinnTransport {
            conn: &mut $n.client.conn.as_mut().unwrap().1,
            now: $n.base + $n.now,
        }
    };
}
macro_rules! st {
    ($n:expr) => {
        &mut QuinnTransport {
            conn: &mut $n.server.conn.as_mut().unwrap().1,
            now: $n.base + $n.now,
        }
    };
}

impl Net {
    fn new(cfg: H3Config) -> Self {
        let base = Instant::now();
        let mut client = Peer {
            ep: endpoint(None),
            conn: None,
            out: Vec::new(),
            buf: Vec::new(),
        };
        let c = client
            .ep
            .connect(base, client_config(), S_ADDR.parse().unwrap(), "warp.test")
            .unwrap();
        client.conn = Some(c);
        let mut n = Self {
            client,
            server: Peer {
                ep: endpoint(Some(server_config())),
                conn: None,
                out: Vec::new(),
                buf: Vec::new(),
            },
            h3c: ClientConnection::new(cfg),
            h3s: ServerConnection::new(cfg),
            base,
            now: Duration::ZERO,
        };
        for _ in 0..500 {
            n.step();
            if n.h3c.peer_settings().is_some() && n.h3s.peer_settings().is_some() {
                return n;
            }
        }
        panic!("no HTTP/3 over quinn-proto");
    }

    fn step(&mut self) {
        let at = self.base + self.now;
        let handshaking = |p: &Peer| p.conn.as_ref().is_none_or(|(_, c)| c.is_handshaking());
        if !handshaking(&self.server) {
            self.h3s.poll(st!(self));
        }
        if !handshaking(&self.client) {
            self.h3c.poll(ct!(self));
        }
        self.client.drive(at);
        for d in std::mem::take(&mut self.client.out) {
            self.server.recv(at, C_ADDR.parse().unwrap(), &d);
        }
        self.server.drive(at);
        for d in std::mem::take(&mut self.server.out) {
            self.client.recv(at, S_ADDR.parse().unwrap(), &d);
        }
        self.now += Duration::from_millis(1);
    }

    fn server_events(&mut self, steps: usize) -> Vec<Event> {
        let mut out = Vec::new();
        for _ in 0..steps {
            self.step();
            out.extend(std::iter::from_fn(|| self.h3s.poll_event()));
        }
        out
    }

    fn client_events(&mut self, steps: usize) -> Vec<ClientEvent> {
        let mut out = Vec::new();
        for _ in 0..steps {
            self.step();
            out.extend(std::iter::from_fn(|| self.h3c.poll_event()));
        }
        out
    }
}

#[test]
fn http3_requests_over_quinn_proto() {
    let mut n = Net::new(H3Config::default());
    let id = n
        .h3c
        .send_request(ct!(n), &ClientRequest::get("warp.test", "/q"))
        .unwrap();
    let ev = n.server_events(20);
    let Some(Event::Request(r)) = ev.into_iter().next() else {
        panic!()
    };
    assert_eq!(r.head.path, "/q");
    let _ = r
        .responder
        .respond(
            &mut n.h3s,
            st!(n),
            Response {
                status: 200,
                fields: Vec::new(),
                body: vec![7; 50_000],
            },
        )
        .unwrap();
    let ev = n.client_events(200);
    assert!(
        matches!(&ev[..], [ClientEvent::Response(r)] if r.stream_id == id && r.body == vec![7; 50_000])
    );
}

#[test]
fn webtransport_datagrams_travel_in_quic_datagram_frames() {
    let mut n = Net::new(H3Config::default().with_webtransport(2));
    assert!(n.h3s.sent_settings().h3_datagram);
    assert!(n.h3c.peer_settings().unwrap().h3_datagram);
    let sid = n
        .h3c
        .open_session(
            ct!(n),
            &ClientRequest::connect("warp.test", "/wt", "webtransport"),
        )
        .unwrap();
    let Some(Event::Connect(c)) = n.server_events(20).into_iter().next() else {
        panic!()
    };
    let session = c
        .responder
        .accept_session(&mut n.h3s, st!(n), Vec::new())
        .unwrap();
    let ev = n.client_events(20);
    assert!(matches!(&ev[..], [ClientEvent::SessionEstablished { .. }]));
    let before = n.client.conn.as_ref().unwrap().1.stats().frame_tx.datagram;
    for i in 0..20u8 {
        assert_eq!(
            n.h3c.send_datagram(ct!(n), sid, &[i; 100]).unwrap(),
            DatagramPath::Quic
        );
    }
    let ev = n.server_events(30);
    let got: Vec<Vec<u8>> = ev
        .into_iter()
        .filter_map(|e| match e {
            Event::Session(SessionEvent::Datagram { session, payload }) if session == sid => {
                Some(payload)
            }
            _ => None,
        })
        .collect();
    // No loss on this link: all twenty arrive, in QUIC DATAGRAM frames.
    assert_eq!(got.len(), 20);
    let sent = n.client.conn.as_ref().unwrap().1.stats().frame_tx.datagram - before;
    assert_eq!(sent, 20);
    // Too large for one datagram frame: refused, not truncated.
    assert_eq!(
        n.h3c.send_datagram(ct!(n), sid, &[0; 1500]),
        Err(vesper_prism_h3::session::SessionError::TooLarge)
    );
    // Streams work on the same connection.
    let s = n.h3c.open_stream(ct!(n), sid, true).unwrap();
    n.h3c.write_stream(ct!(n), s, b"stream bytes").unwrap();
    n.h3c.finish_stream(ct!(n), s).unwrap();
    let ev = n.server_events(20);
    assert!(ev.iter().any(|e| matches!(e,
        Event::Session(SessionEvent::Stream { stream_id, .. }) if *stream_id == s)));
    let mut buf = [0u8; 64];
    let mut got = Vec::new();
    for _ in 0..10 {
        let r = n.h3s.read_stream(st!(n), s, &mut buf).unwrap();
        got.extend_from_slice(&buf[..r.len]);
        if r.fin {
            break;
        }
        n.step();
    }
    assert_eq!(got, b"stream bytes");
    n.h3s
        .send_datagram(st!(n), &session, b"server dgram")
        .unwrap();
    let ev = n.client_events(20);
    assert!(ev.contains(&ClientEvent::Session(SessionEvent::Datagram {
        session: sid,
        payload: b"server dgram".to_vec()
    })));
    let _ = session.close(&mut n.h3s, st!(n), 3, b"bye").unwrap();
    let ev = n.client_events(20);
    assert!(ev.contains(&ClientEvent::Session(SessionEvent::Closed {
        session: sid,
        code: 3,
        reason: b"bye".to_vec(),
        reset: None
    })));
    assert!(n.h3s.error().is_none() && n.h3c.error().is_none());
}
