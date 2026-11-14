//! Wave six lane 5 (issue #247): interop with quinn-proto, an independent
//! QUIC implementation, over an in-memory link. Each direction runs a full
//! TLS 1.3 handshake, a bidirectional stream echo, and an application
//! CONNECTION_CLOSE; the Retry tests exercise both sides' Retry integrity
//! tags and token handling.

mod common;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use common::Rng;
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{
    ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint, Event, ServerConfig,
    StreamId, TransportConfig, VarInt,
};
use vesper_warp::endpoint::{
    ConnectionError, Discard, EndpointConfig, RxOutcome, TxTap, WarpEndpoint,
};
use vesper_warp::handshake::tls::FixedTime;
use vesper_warp::handshake::tls::rustls;
use vesper_warp::handshake::tls::rustls::RootCertStore;
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use vesper_warp::handshake::{ClientSetup, ConnectionEvent, HandshakePhase, Listener, ServerSetup};
use vesper_warp::lifecycle::{AddressValidation, LifecycleConfig, PeerAddr};
use vesper_warp::stream::{Role, StreamConfig};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

const MS: u64 = 1_000_000;
const ALPN: &[u8] = b"vesper-warp/1";
const SERVER_NAME: &str = "warp.test";
const TEST_CA: &[u8] = include_bytes!("fixtures/test-ca.der");
const SERVER_CERT: &[u8] = include_bytes!("fixtures/server.der");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/server.key.der");
const NOW_SECS: u64 = 1_893_456_000;
const STEPS: usize = 2_000;

fn time() -> Arc<FixedTime> {
    Arc::new(FixedTime(UnixTime::since_unix_epoch(Duration::from_secs(
        NOW_SECS,
    ))))
}

fn roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(TEST_CA.to_vec())).unwrap();
    roots
}

fn cert() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        vec![CertificateDer::from(SERVER_CERT.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec())),
    )
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn quinn_transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    // The in-memory link has no path MTU to discover.
    t.mtu_discovery_config(None);
    Arc::new(t)
}

fn quinn_server_config() -> Arc<ServerConfig> {
    quinn_server_config_with(false)
}

fn quinn_server_config_with(early_data: bool) -> Arc<ServerConfig> {
    let (certs, key) = cert();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    if early_data {
        // quinn-proto requires 0 or u32::MAX.
        tls.max_early_data_size = u32::MAX;
    }
    let mut cfg = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
    cfg.transport_config(quinn_transport());
    Arc::new(cfg)
}

fn quinn_client_config() -> ClientConfig {
    quinn_client_config_from(quinn_client_tls(false))
}

fn quinn_client_tls(early_data: bool) -> Arc<rustls::ClientConfig> {
    let mut tls = rustls::ClientConfig::builder_with_details(provider(), time())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots())
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = early_data;
    Arc::new(tls)
}

/// Shares `tls`, so its session cache carries tickets across connections.
fn quinn_client_config_from(tls: Arc<rustls::ClientConfig>) -> ClientConfig {
    let mut cfg = ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls).unwrap()));
    cfg.transport_config(quinn_transport());
    cfg
}

/// Every Warp datagram, and its destination in `dest` (wave seven).
#[derive(Default)]
struct Wire {
    out: Vec<Vec<u8>>,
    dest: Vec<Option<PeerAddr>>,
}

impl TxTap for Wire {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, d: &[u8]) {
        self.out.push(d.to_vec());
        self.dest.push(None);
    }
    fn on_transmit_to(&mut self, _: SlotHandle, _: u64, _: u64, d: &[u8], to: Option<PeerAddr>) {
        self.out.push(d.to_vec());
        self.dest.push(to);
    }
    fn on_stateless(&mut self, d: &[u8], to: Option<PeerAddr>) {
        self.out.push(d.to_vec());
        self.dest.push(to);
    }
}

type Ep = WarpEndpoint<SoftNic, Discard, Wire>;

fn warp(role: Role, lifecycle: LifecycleConfig) -> Ep {
    let config = EndpointConfig {
        role,
        initial_rtt_ns: 20 * MS,
        lifecycle,
        streams: StreamConfig {
            stream_window: 64 * 1024,
            send_buffer: 64 * 1024,
            conn_window: 192 * 1024,
            ..StreamConfig::default()
        },
        ..EndpointConfig::default()
    };
    let xsk = SoftNic::xsk(UmemConfig::with_frames(1024), SoftNicMode::Scripted).unwrap();
    WarpEndpoint::new(config, xsk, Discard::default(), Wire::default()).unwrap()
}

/// quinn's stateless-reset key, fixed so that a restarted endpoint derives
/// the same tokens (HKDF-SHA-256 from the `hkdf` crate as the keyed PRF).
struct ResetKey;

impl quinn_proto::crypto::HmacKey for ResetKey {
    fn sign(&self, data: &[u8], out: &mut [u8]) {
        hkdf::Hkdf::<sha2::Sha256>::new(Some(b"quinn interop reset key"), data)
            .expand(b"reset", out)
            .unwrap();
    }
    fn signature_len(&self) -> usize {
        32
    }
    fn verify(
        &self,
        data: &[u8],
        signature: &[u8],
    ) -> Result<(), quinn_proto::crypto::CryptoError> {
        let mut want = [0u8; 32];
        self.sign(data, &mut want);
        if signature == want {
            Ok(())
        } else {
            Err(quinn_proto::crypto::CryptoError)
        }
    }
}

fn quinn_endpoint(server: Option<Arc<ServerConfig>>) -> Endpoint {
    let mut config = quinn_proto::EndpointConfig::new(Arc::new(ResetKey));
    // A fixed key too, so a restarted endpoint still recognises its CIDs.
    config.cid_generator(|| Box::new(quinn_proto::HashedConnectionIdGenerator::from_key(7)));
    Endpoint::new(Arc::new(config), server, false, Some([7; 32]))
}

/// A quinn-proto endpoint with at most one connection, driven by hand.
struct Quinn {
    ep: Endpoint,
    conn: Option<(ConnectionHandle, Connection)>,
    server: Option<Arc<ServerConfig>>,
    retry: bool,
    retries_sent: u32,
    out: Vec<Vec<u8>>,
    events: Vec<Event>,
    buf: Vec<u8>,
}

impl Quinn {
    fn new(server: Option<Arc<ServerConfig>>, retry: bool) -> Self {
        Self {
            ep: quinn_endpoint(server.clone()),
            conn: None,
            server,
            retry,
            retries_sent: 0,
            out: Vec::new(),
            events: Vec::new(),
            buf: Vec::new(),
        }
    }

    fn conn(&mut self) -> &mut Connection {
        &mut self.conn.as_mut().expect("connection").1
    }

    fn recv(&mut self, at: Instant, from: SocketAddr, local: Option<IpAddr>, d: &[u8]) {
        self.buf.clear();
        let event = self
            .ep
            .handle(at, from, local, None, BytesMut::from(d), &mut self.buf);
        match event {
            Some(DatagramEvent::NewConnection(incoming)) => {
                if self.retry && !incoming.remote_address_validated() {
                    let t = self.ep.retry(incoming, &mut self.buf).unwrap();
                    self.retries_sent += 1;
                    self.out.push(self.buf[..t.size].to_vec());
                    return;
                }
                let accepted = self
                    .ep
                    .accept(incoming, at, &mut self.buf, self.server.clone());
                self.conn = Some(accepted.unwrap());
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
        while let Some(e) = c.poll() {
            self.events.push(e);
        }
        loop {
            self.buf.clear();
            let Some(t) = c.poll_transmit(at, 1, &mut self.buf) else {
                break;
            };
            self.out.push(self.buf[..t.size].to_vec());
        }
    }

    fn next_wake(&mut self) -> Option<Instant> {
        self.conn.as_mut().and_then(|(_, c)| c.poll_timeout())
    }

    fn lost(&self) -> Option<&quinn_proto::ConnectionError> {
        self.events.iter().find_map(|e| match e {
            Event::ConnectionLost { reason } => Some(reason),
            _ => None,
        })
    }
}

/// A Warp endpoint and a quinn-proto endpoint joined by a link that can drop
/// datagrams.
struct Link {
    warp: Ep,
    quinn: Quinn,
    warp_addr: SocketAddr,
    quinn_addr: SocketAddr,
    base: Instant,
    now: u64,
    rng: Rng,
    loss_ppm: u64,
    warp_events: Vec<ConnectionEvent>,
    warp_conn: Option<SlotHandle>,
    /// quinn's preferred address (wave seven); Warp datagrams to it are
    /// handed to quinn as received on that address.
    quinn_preferred: Option<SocketAddr>,
    /// Warp datagrams to the original and the preferred quinn address.
    to_original: u64,
    to_preferred: u64,
}

impl Link {
    fn new(warp: Ep, quinn: Quinn, loss_ppm: u64) -> Self {
        Self {
            warp,
            quinn,
            warp_addr: "192.0.2.1:4433".parse().unwrap(),
            quinn_addr: "198.51.100.7:443".parse().unwrap(),
            base: Instant::now(),
            now: 0,
            rng: Rng(0x9e37),
            loss_ppm,
            warp_events: Vec::new(),
            warp_conn: None,
            quinn_preferred: None,
            to_original: 0,
            to_preferred: 0,
        }
    }

    fn at(&self) -> Instant {
        self.base + Duration::from_nanos(self.now)
    }

    fn lose(&mut self) -> bool {
        self.loss_ppm != 0 && self.rng.below(1_000_000) < self.loss_ppm
    }

    fn step(&mut self) {
        let at = self.at();
        let now = self.now;
        let dest = std::mem::take(&mut self.warp.tap_mut().dest);
        for (d, to) in std::mem::take(&mut self.warp.tap_mut().out)
            .into_iter()
            .zip(dest)
        {
            let local = match to {
                Some(to) if Some(to) == self.quinn_preferred => {
                    self.to_preferred += 1;
                    Some(to.ip())
                }
                _ => {
                    self.to_original += 1;
                    None
                }
            };
            if !self.lose() {
                self.quinn.recv(at, self.warp_addr, local, &d);
            }
        }
        self.quinn.drive(at);
        for d in std::mem::take(&mut self.quinn.out) {
            if self.lose() {
                continue;
            }
            self.warp.set_rx_source(Some(self.quinn_addr));
            self.warp.xsk_mut().backend_mut().deliver(&d);
            while self.warp.poll_and_step_quic_rx_one(now).unwrap() != RxOutcome::Idle {}
        }
        self.warp.xsk_mut().backend_mut().step();
        self.warp.poll(now).unwrap();
        while let Some(e) = self.warp.poll_connection_event() {
            if let ConnectionEvent::Accepted(h) = e {
                self.warp_conn = Some(h);
            }
            self.warp_events.push(e);
        }
        self.now += MS;
        let quiet = self.warp.tap().out.is_empty() && self.quinn.out.is_empty();
        if quiet {
            let quinn_wake = self
                .quinn
                .next_wake()
                .map(|t| t.saturating_duration_since(self.base).as_nanos() as u64);
            if let Some(wake) = [self.warp.next_wake_ns(), quinn_wake]
                .into_iter()
                .flatten()
                .min()
            {
                self.now = self.now.max(wake.min(self.now + 100 * MS));
            }
        }
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        for _ in 0..STEPS {
            if done(self) {
                return;
            }
            self.step();
        }
        panic!(
            "{what} did not happen: warp {:?}; quinn events {:?}",
            self.warp.stats(),
            self.quinn.events
        );
    }

    fn handshake(&mut self) {
        self.until("handshake", |l| {
            let warp = l
                .warp_conn
                .is_some_and(|c| l.warp.handshake_phase(c) == Some(HandshakePhase::Confirmed));
            let quinn = l.quinn.events.iter().any(|e| matches!(e, Event::Connected));
            warp && quinn
        });
    }
}

fn read_quinn(q: &mut Quinn, id: StreamId, into: &mut Vec<u8>) -> bool {
    let mut stream = q.conn().recv_stream(id);
    let Ok(mut chunks) = stream.read(true) else {
        return false;
    };
    let mut fin = false;
    loop {
        match chunks.next(usize::MAX) {
            Ok(Some(chunk)) => into.extend_from_slice(&chunk.bytes),
            Ok(None) => {
                fin = true;
                break;
            }
            Err(_) => break,
        }
    }
    let _ = chunks.finalize();
    fin
}

/// Writes as much of `data[*sent..]` as quinn takes, finishing at the end.
fn write_quinn(q: &mut Quinn, id: StreamId, data: &[u8], sent: &mut usize) {
    if *sent >= data.len() {
        return;
    }
    let mut s = q.conn().send_stream(id);
    if let Ok(n) = s.write(&data[*sent..]) {
        *sent += n;
        if *sent == data.len() {
            s.finish().unwrap();
        }
    }
}

/// Warp is the client: it uploads on its stream, quinn echoes a download.
fn warp_client_exchange(link: &mut Link, conn: SlotHandle, up: &[u8], down: &[u8]) {
    let sid = link.warp.open_stream(conn, false).unwrap();
    let (mut up_sent, mut down_sent) = (0, 0);
    let (mut up_got, mut down_got) = (Vec::new(), Vec::new());
    let (mut up_fin, mut down_fin) = (false, false);
    let mut buf = vec![0u8; 16 * 1024];
    let id = StreamId::new(quinn_proto::Side::Client, Dir::Bi, sid >> 2);
    link.until("echo", |l| {
        if up_sent < up.len() {
            up_sent += l.warp.stream_write(conn, sid, &up[up_sent..]).unwrap();
            if up_sent == up.len() {
                l.warp.stream_finish(conn, sid).unwrap();
            }
        }
        if l.quinn.conn.is_some() {
            while l.quinn.conn().streams().accept(Dir::Bi).is_some() {}
            if !up_fin {
                up_fin = read_quinn(&mut l.quinn, id, &mut up_got);
            }
            if up_fin {
                write_quinn(&mut l.quinn, id, down, &mut down_sent);
            }
        }
        if !down_fin && let Ok(r) = l.warp.stream_read(conn, sid, &mut buf) {
            down_got.extend_from_slice(&buf[..r.len]);
            down_fin = r.fin;
        }
        up_fin && down_fin
    });
    assert!(up_got == up, "upload corrupted");
    assert!(down_got == down, "download corrupted");
}

/// quinn is the client: it uploads on a new stream, Warp echoes a download.
fn quinn_client_exchange(link: &mut Link, up: &[u8], down: &[u8]) {
    let id = link.quinn.conn().streams().open(Dir::Bi).unwrap();
    let sid = id.index() << 2;
    let (mut up_sent, mut down_sent) = (0, 0);
    let (mut up_got, mut down_got) = (Vec::new(), Vec::new());
    let (mut up_fin, mut down_fin) = (false, false);
    let mut buf = vec![0u8; 16 * 1024];
    link.until("echo", |l| {
        write_quinn(&mut l.quinn, id, up, &mut up_sent);
        let conn = l.warp_conn.expect("accepted");
        if !up_fin && let Ok(r) = l.warp.stream_read(conn, sid, &mut buf) {
            up_got.extend_from_slice(&buf[..r.len]);
            up_fin = r.fin;
        }
        if up_fin && down_sent < down.len() {
            down_sent += l.warp.stream_write(conn, sid, &down[down_sent..]).unwrap();
            if down_sent == down.len() {
                l.warp.stream_finish(conn, sid).unwrap();
            }
        }
        if !down_fin {
            down_fin = read_quinn(&mut l.quinn, id, &mut down_got);
        }
        up_fin && down_fin
    });
    assert!(up_got == up, "upload corrupted");
    assert!(down_got == down, "download corrupted");
}

fn warp_client(lifecycle: LifecycleConfig, retry: bool, loss_ppm: u64) -> (Link, SlotHandle) {
    let mut warp = warp(Role::Client, lifecycle);
    warp.set_rx_source(Some("198.51.100.7:443".parse().unwrap()));
    let setup = ClientSetup::with_time(roots(), vec![ALPN.to_vec()], time()).unwrap();
    let cid = [0xc1; 8];
    let odcid = [0x83, 1, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
    let conn = warp.connect(&setup, SERVER_NAME, &cid, &odcid, 0).unwrap();
    let quinn = Quinn::new(Some(quinn_server_config()), retry);
    let mut link = Link::new(warp, quinn, loss_ppm);
    link.warp_conn = Some(conn);
    (link, conn)
}

fn warp_server(lifecycle: LifecycleConfig, loss_ppm: u64) -> Link {
    let mut warp = warp(Role::Server, lifecycle);
    let (certs, key) = cert();
    warp.listen(Listener::new(
        ServerSetup::new(certs, key, vec![ALPN.to_vec()]).unwrap(),
    ))
    .unwrap();
    let mut quinn = Quinn::new(None, false);
    let base = Instant::now();
    let (ch, c) = quinn
        .ep
        .connect(
            base,
            quinn_client_config(),
            "192.0.2.1:4433".parse().unwrap(),
            SERVER_NAME,
        )
        .unwrap();
    quinn.conn = Some((ch, c));
    let mut link = Link::new(warp, quinn, loss_ppm);
    link.base = base;
    link
}

#[test]
fn warp_client_to_quinn_server() {
    let (mut link, conn) = warp_client(LifecycleConfig::default(), false, 0);
    link.handshake();
    let mut rng = Rng(1);
    warp_client_exchange(&mut link, conn, &rng.bytes(100_000), &rng.bytes(100_000));

    // Migration onto a connection ID quinn issued: quinn validates the new
    // path, Warp answers its PATH_CHALLENGE and validates it too.
    let before = link.warp.connection(conn).unwrap().tile.peer_cid;
    link.warp_addr = "192.0.2.200:5000".parse().unwrap();
    link.warp.migrate(conn, link.now).unwrap();
    assert_ne!(before, link.warp.connection(conn).unwrap().tile.peer_cid);
    warp_client_exchange(&mut link, conn, &rng.bytes(30_000), &rng.bytes(30_000));
    link.until("path validated", |l| {
        l.warp_events
            .contains(&ConnectionEvent::PathValidated(conn))
    });
    assert!(
        link.warp.stats().path_responses_sent >= 1,
        "{:?}",
        link.warp.stats()
    );
    // Wave seven: quinn retires Warp's handshake connection ID (sequence
    // 0); Warp's connection table is rekeyed so that ID no longer routes.
    assert!(
        link.warp.stats().table_cids_rekeyed >= 1,
        "{:?}",
        link.warp.stats()
    );
    let keyed = link.warp.table().local_cid(conn).unwrap();
    assert_ne!(keyed.as_bytes(), &[0xc1; 8]);
    assert_eq!(link.warp.table().lookup(&[0xc1; 8]), Ok(None));

    link.warp.close(conn, 0x42, link.now).unwrap();
    link.until("quinn sees the close", |l| l.quinn.lost().is_some());
    match link.quinn.lost().unwrap() {
        quinn_proto::ConnectionError::ApplicationClosed(c) => {
            assert_eq!(c.error_code, VarInt::from_u32(0x42));
        }
        other => panic!("unexpected close {other:?}"),
    }
}

#[test]
fn quinn_client_to_warp_server() {
    let mut link = warp_server(LifecycleConfig::default(), 0);
    link.handshake();
    let mut rng = Rng(2);
    quinn_client_exchange(&mut link, &rng.bytes(100_000), &rng.bytes(100_000));
    // Warp issued connection IDs and quinn accepted them.
    assert_eq!(link.warp.stats().cids_issued, 3);

    let at = link.at();
    link.quinn.conn().close(
        at,
        VarInt::from_u32(0x17),
        bytes::Bytes::from_static(b"bye"),
    );
    let conn = link.warp_conn.unwrap();
    link.until("warp sees the close", |l| {
        l.warp.connection_error(conn).is_some()
    });
    assert_eq!(
        link.warp.connection_error(conn),
        Some(ConnectionError::Peer {
            error_code: 0x17,
            frame_type: None
        })
    );
}

#[test]
fn warp_client_follows_a_quinn_retry() {
    let (mut link, conn) = warp_client(LifecycleConfig::default(), true, 0);
    link.handshake();
    assert!(
        (1..=2).contains(&link.quinn.retries_sent),
        "one per Initial datagram"
    );
    assert_eq!(link.warp.stats().retries_received, 1);
    assert!(link.warp_events.contains(&ConnectionEvent::Retried(conn)));
    let mut rng = Rng(3);
    warp_client_exchange(&mut link, conn, &rng.bytes(10_000), &rng.bytes(10_000));
}

#[test]
fn quinn_client_follows_a_warp_retry() {
    let retry = LifecycleConfig {
        address_validation: AddressValidation::Retry,
        ..LifecycleConfig::default()
    };
    let mut link = warp_server(retry, 0);
    link.handshake();
    assert!(
        (1..=2).contains(&link.warp.stats().retries_sent),
        "one per Initial datagram"
    );
    assert_eq!(link.warp.stats().tokens_accepted, 1);
    let mut rng = Rng(4);
    quinn_client_exchange(&mut link, &rng.bytes(10_000), &rng.bytes(10_000));
}

#[test]
fn interop_under_loss() {
    for seed in 0..4u64 {
        let (mut link, conn) = warp_client(LifecycleConfig::default(), false, 100_000);
        link.rng = Rng(seed + 10);
        link.handshake();
        let mut rng = Rng(seed);
        warp_client_exchange(&mut link, conn, &rng.bytes(30_000), &rng.bytes(30_000));

        let mut link = warp_server(LifecycleConfig::default(), 100_000);
        link.rng = Rng(seed + 20);
        link.handshake();
        quinn_client_exchange(&mut link, &rng.bytes(30_000), &rng.bytes(30_000));
    }
}

#[test]
fn warp_client_sends_zero_rtt_to_quinn() {
    let mut warp = warp(Role::Client, LifecycleConfig::default());
    let quinn_addr: SocketAddr = "198.51.100.7:443".parse().unwrap();
    warp.set_rx_source(Some(quinn_addr));
    let setup = ClientSetup::with_time(roots(), vec![ALPN.to_vec()], time())
        .unwrap()
        .with_zero_rtt();
    let odcid = [0x83, 1, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
    let first = warp
        .connect(&setup, SERVER_NAME, &[0xc1; 8], &odcid, 0)
        .unwrap();
    let quinn = Quinn::new(Some(quinn_server_config_with(true)), false);
    let mut link = Link::new(warp, quinn, 0);
    link.warp_conn = Some(first);
    link.handshake();
    let mut rng = Rng(5);
    warp_client_exchange(&mut link, first, &rng.bytes(1_000), &rng.bytes(1_000));
    for _ in 0..50 {
        link.step();
    }

    // Second connection resumes and sends its stream in 0-RTT.
    let odcid = [0x83, 2, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
    link.warp.set_rx_source(Some(quinn_addr));
    let now = link.now;
    let second = link
        .warp
        .connect(&setup, SERVER_NAME, &[0xc2; 8], &odcid, now)
        .unwrap();
    link.warp_conn = Some(second);
    link.quinn.events.clear();
    let up = rng.bytes(2_000);
    let down = rng.bytes(2_000);
    let sid = link.warp.open_stream(second, false).unwrap();
    assert_eq!(link.warp.stream_write(second, sid, &up).unwrap(), up.len());
    link.warp.stream_finish(second, sid).unwrap();
    link.warp.poll(now).unwrap();
    assert!(link.warp.stats().zero_rtt_packets_sent >= 1);
    let id = StreamId::new(quinn_proto::Side::Client, Dir::Bi, sid >> 2);
    let (mut got, mut down_got, mut sent) = (Vec::new(), Vec::new(), 0);
    let (mut up_fin, mut down_fin) = (false, false);
    let mut buf = vec![0u8; 4096];
    link.until("0-RTT echo", |l| {
        if l.quinn.conn.is_some() {
            while l.quinn.conn().streams().accept(Dir::Bi).is_some() {}
            if !up_fin {
                up_fin = read_quinn(&mut l.quinn, id, &mut got);
            }
            if up_fin {
                write_quinn(&mut l.quinn, id, &down, &mut sent);
            }
        }
        if !down_fin && let Ok(r) = l.warp.stream_read(second, sid, &mut buf) {
            down_got.extend_from_slice(&buf[..r.len]);
            down_fin = r.fin;
        }
        up_fin && down_fin
    });
    assert!(got == up && down_got == down);
    // quinn's server reports no 0-RTT state of its own; its TLS stack's
    // acceptance reaches Warp through EncryptedExtensions.
    assert!(
        link.warp_events
            .contains(&ConnectionEvent::ZeroRttAccepted(second))
    );
    assert!(
        link.warp
            .connection(second)
            .unwrap()
            .tls
            .unwrap()
            .is_resumed()
    );
}

#[test]
fn quinn_client_sends_zero_rtt_to_warp() {
    let mut warp = warp(Role::Server, LifecycleConfig::default());
    let (certs, key) = cert();
    let setup = ServerSetup::new(certs, key, vec![ALPN.to_vec()])
        .unwrap()
        .with_zero_rtt();
    warp.listen(Listener::new(setup)).unwrap();
    let tls = quinn_client_tls(true);
    let warp_addr: SocketAddr = "192.0.2.1:4433".parse().unwrap();
    let mut quinn = Quinn::new(None, false);
    let base = Instant::now();
    quinn.conn = Some(
        quinn
            .ep
            .connect(
                base,
                quinn_client_config_from(tls.clone()),
                warp_addr,
                SERVER_NAME,
            )
            .unwrap(),
    );
    let mut link = Link::new(warp, quinn, 0);
    link.base = base;
    link.handshake();
    let mut rng = Rng(6);
    quinn_client_exchange(&mut link, &rng.bytes(1_000), &rng.bytes(1_000));
    for _ in 0..50 {
        link.step();
    }

    // Second connection: quinn has 0-RTT keys from the ticket Warp sent.
    let at = link.at();
    let (ch, c) = link
        .quinn
        .ep
        .connect(at, quinn_client_config_from(tls), warp_addr, SERVER_NAME)
        .unwrap();
    link.quinn.conn = Some((ch, c));
    link.quinn.events.clear();
    link.warp_conn = None;
    assert!(
        link.quinn.conn().has_0rtt(),
        "quinn resumed with early data"
    );
    let up = rng.bytes(2_000);
    let down = rng.bytes(2_000);
    let id = link.quinn.conn().streams().open(Dir::Bi).unwrap();
    let mut sent = 0;
    write_quinn(&mut link.quinn, id, &up, &mut sent);
    assert_eq!(sent, up.len());
    let sid = id.index() << 2;
    let (mut got, mut down_got, mut down_sent) = (Vec::new(), Vec::new(), 0);
    let (mut up_fin, mut down_fin) = (false, false);
    let mut buf = vec![0u8; 4096];
    link.until("0-RTT echo", |l| {
        if let Some(conn) = l.warp_conn {
            if !up_fin && let Ok(r) = l.warp.stream_read(conn, sid, &mut buf) {
                got.extend_from_slice(&buf[..r.len]);
                up_fin = r.fin;
            }
            if up_fin && down_sent < down.len() {
                down_sent += l.warp.stream_write(conn, sid, &down[down_sent..]).unwrap();
                if down_sent == down.len() {
                    l.warp.stream_finish(conn, sid).unwrap();
                }
            }
        }
        if !down_fin {
            down_fin = read_quinn(&mut l.quinn, id, &mut down_got);
        }
        up_fin && down_fin
    });
    assert!(got == up && down_got == down);
    assert!(link.warp.stats().zero_rtt_packets_received >= 1);
    assert!(
        link.quinn.conn().accepted_0rtt(),
        "Warp accepted the early data"
    );
}

#[test]
fn quinn_accepts_a_warp_stateless_reset() {
    let mut link = warp_server(LifecycleConfig::default(), 0);
    link.handshake();
    let mut rng = Rng(7);
    quinn_client_exchange(&mut link, &rng.bytes(2_000), &rng.bytes(2_000));
    // Warp forgets the connection; quinn's next packet gets a reset carrying
    // the token Warp advertised in its transport parameters.
    let conn = link.warp_conn.unwrap();
    let now = link.now;
    link.warp.close_connection(conn, now).unwrap();
    let id = link.quinn.conn().streams().open(Dir::Bi).unwrap();
    let mut sent = 0;
    write_quinn(&mut link.quinn, id, b"anyone there?", &mut sent);
    link.until("quinn sees the reset", |l| l.quinn.lost().is_some());
    assert!(matches!(
        link.quinn.lost(),
        Some(quinn_proto::ConnectionError::Reset)
    ));
    assert_eq!(link.warp.stats().stateless_resets_sent, 1);
}

#[test]
fn warp_accepts_a_quinn_stateless_reset() {
    let (mut link, conn) = warp_client(LifecycleConfig::default(), false, 0);
    link.handshake();
    let mut rng = Rng(8);
    warp_client_exchange(&mut link, conn, &rng.bytes(2_000), &rng.bytes(2_000));
    // quinn restarts with the same reset key and no connections; it
    // answers Warp's next packet with a stateless reset.
    link.quinn.ep = quinn_endpoint(Some(quinn_server_config()));
    link.quinn.conn = None;
    let sid = link.warp.open_stream(conn, false).unwrap();
    link.warp.stream_write(conn, sid, b"anyone there?").unwrap();
    link.until("warp sees the reset", |l| {
        l.warp.connection_error(conn) == Some(ConnectionError::StatelessReset)
    });
    assert_eq!(link.warp.stats().stateless_resets_received, 1);
}

/// A quinn token log that refuses reuse exactly (quinn's own Bloom-filter
/// log needs its `bloom` feature).
#[derive(Default)]
struct ExactTokenLog(std::sync::Mutex<std::collections::HashSet<u128>>);

impl quinn_proto::TokenLog for ExactTokenLog {
    fn check_and_insert(
        &self,
        nonce: u128,
        _issued: std::time::SystemTime,
        _lifetime: Duration,
    ) -> Result<(), quinn_proto::TokenReuseError> {
        if self.0.lock().unwrap().insert(nonce) {
            Ok(())
        } else {
            Err(quinn_proto::TokenReuseError)
        }
    }
}

#[test]
fn warp_client_reuses_a_quinn_new_token() {
    let mut server = (*quinn_server_config()).clone();
    let mut tokens = quinn_proto::ValidationTokenConfig::default();
    tokens.log(Arc::new(ExactTokenLog::default())).sent(1);
    server.validation_token_config(tokens);
    let mut warp = warp(Role::Client, LifecycleConfig::default());
    let quinn_addr: SocketAddr = "198.51.100.7:443".parse().unwrap();
    warp.set_rx_source(Some(quinn_addr));
    let setup = ClientSetup::with_time(roots(), vec![ALPN.to_vec()], time()).unwrap();
    let odcid = [0x83, 1, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
    let first = warp
        .connect(&setup, SERVER_NAME, &[0xc1; 8], &odcid, 0)
        .unwrap();
    let mut link = Link::new(warp, Quinn::new(Some(Arc::new(server)), true), 0);
    link.warp_conn = Some(first);
    link.handshake();
    let retries = link.quinn.retries_sent;
    assert!((1..=2).contains(&retries), "one per Initial datagram");
    link.until("NEW_TOKEN from quinn", |l| {
        l.warp_events.contains(&ConnectionEvent::NewToken(first))
    });

    // The token validates the address: quinn sends no second Retry.
    let odcid = [0x83, 2, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
    link.warp.set_rx_source(Some(quinn_addr));
    let now = link.now;
    let second = link
        .warp
        .connect(&setup, SERVER_NAME, &[0xc2; 8], &odcid, now)
        .unwrap();
    link.warp_conn = Some(second);
    link.quinn.events.clear();
    link.handshake();
    assert_eq!(link.quinn.retries_sent, retries, "no second Retry");
    let mut rng = Rng(9);
    warp_client_exchange(&mut link, second, &rng.bytes(5_000), &rng.bytes(5_000));
}

#[test]
fn quinn_client_reuses_a_warp_new_token() {
    let retry = LifecycleConfig {
        address_validation: AddressValidation::Retry,
        ..LifecycleConfig::default()
    };
    let mut warp = warp(Role::Server, retry);
    let (certs, key) = cert();
    warp.listen(Listener::new(
        ServerSetup::new(certs, key, vec![ALPN.to_vec()]).unwrap(),
    ))
    .unwrap();
    // One ClientConfig, so both connections share quinn's token store.
    let client = quinn_client_config();
    let warp_addr: SocketAddr = "192.0.2.1:4433".parse().unwrap();
    let mut quinn = Quinn::new(None, false);
    let base = Instant::now();
    quinn.conn = Some(
        quinn
            .ep
            .connect(base, client.clone(), warp_addr, SERVER_NAME)
            .unwrap(),
    );
    let mut link = Link::new(warp, quinn, 0);
    link.base = base;
    link.handshake();
    let retries = link.warp.stats().retries_sent;
    assert!((1..=2).contains(&retries), "one per Initial datagram");
    link.until("NEW_TOKEN sent", |l| l.warp.stats().tokens_issued == 1);
    for _ in 0..20 {
        link.step();
    }

    let at = link.at();
    let (ch, c) = link
        .quinn
        .ep
        .connect(at, client, warp_addr, SERVER_NAME)
        .unwrap();
    link.quinn.conn = Some((ch, c));
    link.quinn.events.clear();
    link.warp_conn = None;
    link.handshake();
    assert_eq!(link.warp.stats().retries_sent, retries, "no second Retry");
    assert_eq!(link.warp.stats().tokens_accepted, 2);
    let mut rng = Rng(10);
    quinn_client_exchange(&mut link, &rng.bytes(5_000), &rng.bytes(5_000));
}

#[test]
fn quinn_client_gets_version_negotiation_from_warp() {
    let mut warp = warp(Role::Server, LifecycleConfig::default());
    let (certs, key) = cert();
    warp.listen(Listener::new(
        ServerSetup::new(certs, key, vec![ALPN.to_vec()]).unwrap(),
    ))
    .unwrap();
    // quinn offers draft-29, which Warp does not speak.
    let mut config = quinn_proto::EndpointConfig::new(Arc::new(ResetKey));
    config.supported_versions(vec![0xff00_001d, 1]);
    let mut quinn = Quinn::new(None, false);
    quinn.ep = Endpoint::new(Arc::new(config), None, false, Some([7; 32]));
    let mut client = quinn_client_config();
    client.version(0xff00_001d);
    let base = Instant::now();
    quinn.conn = Some(
        quinn
            .ep
            .connect(base, client, "192.0.2.1:4433".parse().unwrap(), SERVER_NAME)
            .unwrap(),
    );
    let mut link = Link::new(warp, quinn, 0);
    link.base = base;
    link.until("quinn sees the version mismatch", |l| {
        l.quinn.lost().is_some()
    });
    assert!(matches!(
        link.quinn.lost(),
        Some(quinn_proto::ConnectionError::VersionMismatch)
    ));
    assert!((1..=2).contains(&link.warp.stats().version_negotiations_sent));
    assert_eq!(link.warp.stats().accepted, 0);
}

// ----- Wave seven (issue #330): the new lifecycle paths against quinn -----

const QUINN_PREFERRED: &str = "203.0.113.9:8443";

fn quinn_server_with_preferred() -> Arc<ServerConfig> {
    let mut cfg = (*quinn_server_config()).clone();
    let SocketAddr::V4(p) = QUINN_PREFERRED.parse().unwrap() else {
        unreachable!()
    };
    cfg.preferred_address_v4(Some(p));
    Arc::new(cfg)
}

#[test]
fn warp_client_moves_to_a_quinn_servers_preferred_address() {
    let preferred: SocketAddr = QUINN_PREFERRED.parse().unwrap();
    let mut warp = warp(Role::Client, LifecycleConfig::default());
    warp.set_rx_source(Some("198.51.100.7:443".parse().unwrap()));
    let setup = ClientSetup::with_time(roots(), vec![ALPN.to_vec()], time()).unwrap();
    let odcid = [0x83, 1, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
    let conn = warp
        .connect(&setup, SERVER_NAME, &[0xc1; 8], &odcid, 0)
        .unwrap();
    let mut link = Link::new(
        warp,
        Quinn::new(Some(quinn_server_with_preferred()), false),
        0,
    );
    link.warp_conn = Some(conn);
    link.quinn_preferred = Some(preferred);
    link.handshake();
    // Warp probes the preferred address with quinn's sequence-1 ID; quinn
    // answers the PATH_CHALLENGE, and Warp moves there.
    link.until("moved to the preferred address", |l| {
        l.warp_events
            .contains(&ConnectionEvent::MovedToPreferredAddress(conn))
    });
    assert_eq!(link.warp.stats().preferred_address_migrations, 1);
    let before = link.to_original;
    let mut rng = Rng(20);
    warp_client_exchange(&mut link, conn, &rng.bytes(60_000), &rng.bytes(60_000));
    assert_eq!(link.to_original, before, "nothing to the original address");
    assert!(link.to_preferred > 10, "{}", link.to_preferred);
    // quinn keeps sending from the address its connection started on
    // (quinn-proto does not change a connection's source address); Warp
    // accepts the server's original address throughout (RFC 9000 §9.6.2).
    assert_eq!(link.warp.stats().unknown_server_address_drops, 0);
    assert!(link.quinn.lost().is_none(), "{:?}", link.quinn.lost());
}

#[test]
fn quinn_client_accepts_a_warp_preferred_address() {
    let SocketAddr::V4(p) = "203.0.113.10:8443".parse().unwrap() else {
        unreachable!()
    };
    let lifecycle = LifecycleConfig {
        preferred_address: Some(vesper_warp::lifecycle::PreferredAddressConfig {
            ipv4: Some(p),
            ipv6: None,
        }),
        ..LifecycleConfig::default()
    };
    let mut link = warp_server(lifecycle, 0);
    link.handshake();
    let mut rng = Rng(21);
    quinn_client_exchange(&mut link, &rng.bytes(50_000), &rng.bytes(50_000));
    // quinn took the preferred address's ID (sequence 1) and the two
    // NEW_CONNECTION_IDs within its limit; quinn-proto does not migrate to
    // a preferred address itself, so the connection stays where it is.
    assert_eq!(link.warp.stats().cids_issued, 3);
    assert!(link.quinn.lost().is_none(), "{:?}", link.quinn.lost());
    assert_eq!(link.warp.stats().preferred_address_migrations, 0);
}

#[test]
fn quinn_reads_warp_closes_coalesced_before_confirmation() {
    // Warp closes while it still has Handshake keys: the Handshake
    // APPLICATION_ERROR and the 1-RTT application close leave in one
    // datagram (RFC 9000 §10.2.3, §12.2), and quinn reads one of them.
    let (mut link, conn) = warp_client(LifecycleConfig::default(), false, 0);
    link.until("1-RTT keys", |l| {
        l.warp.handshake_phase(conn) == Some(HandshakePhase::Complete)
    });
    let before = link.warp.stats().short_coalesced_sent;
    link.warp.close(conn, 0x42, link.now).unwrap();
    assert_eq!(link.warp.stats().short_coalesced_sent, before + 1);
    link.until("quinn sees the close", |l| l.quinn.lost().is_some());
    match link.quinn.lost().unwrap() {
        quinn_proto::ConnectionError::ApplicationClosed(c) => {
            assert_eq!(c.error_code, VarInt::from_u32(0x42));
        }
        quinn_proto::ConnectionError::ConnectionClosed(c) => {
            // APPLICATION_ERROR (0x0c) from the Handshake packet.
            assert_eq!(u64::from(c.error_code), 0x0c);
        }
        other => panic!("unexpected close {other:?}"),
    }
}
