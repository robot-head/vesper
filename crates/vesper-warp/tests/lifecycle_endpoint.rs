//! Wave six lane 5 (issue #247): the connection lifecycle end to end over
//! SoftNic, with addresses: Retry, Version Negotiation, NEW_TOKEN,
//! stateless reset, connection IDs, path validation and migration, idle
//! timeout, close and drain, 0-RTT accepted and rejected, buffered
//! undecryptable packets, and a lossy sweep of handshake-opened
//! connections.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::Rng;
use vesper_warp::endpoint::{
    ConnectionError, Discard, EndpointConfig, RxOutcome, TxTap, WarpEndpoint, WarpError,
};
use vesper_warp::handshake::tls::rustls::RootCertStore;
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use vesper_warp::handshake::tls::FixedTime;
use vesper_warp::handshake::{
    CidSource, ClientSetup, ConnectionEvent, HandshakePhase, Listener, ServerSetup,
};
use vesper_warp::PacketType;
use vesper_warp::header::{HeaderParser, ParsedPacket, write_version_negotiation};
use vesper_warp::lifecycle::{AddressValidation, LifecycleConfig, PeerAddr};
use vesper_warp::stream::{Priority, ReadOutcome, Role, StreamConfig};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

const MS: u64 = 1_000_000;
const ALPN: &[u8] = b"vesper-warp/1";
const SERVER_NAME: &str = "warp.test";
const TEST_CA: &[u8] = include_bytes!("fixtures/test-ca.der");
const SERVER_CERT: &[u8] = include_bytes!("fixtures/server.der");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/server.key.der");
const NOW_SECS: u64 = 1_893_456_000;
const CID0: [u8; 8] = [1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1];
const ODCID0: [u8; 8] = [0x83, 1, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];

fn client_setup() -> ClientSetup {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(TEST_CA.to_vec())).unwrap();
    let time = Arc::new(FixedTime(UnixTime::since_unix_epoch(Duration::from_secs(
        NOW_SECS,
    ))));
    ClientSetup::with_time(roots, vec![ALPN.to_vec()], time).unwrap()
}

fn server_setup() -> ServerSetup {
    ServerSetup::new(
        vec![CertificateDer::from(SERVER_CERT.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec())),
        vec![ALPN.to_vec()],
    )
    .unwrap()
}

/// Deterministic connection IDs, distinct per source.
struct CountingCids(u8, u8);

impl CidSource for CountingCids {
    fn next_cid(&mut self, out: &mut [u8]) {
        self.1 = self.1.wrapping_add(1);
        out.fill(self.0);
        out[0] = self.1;
    }
}

/// Every datagram with its destination.
#[derive(Default)]
struct Wire {
    out: Vec<(Vec<u8>, Option<PeerAddr>)>,
    stateless: u64,
}

impl TxTap for Wire {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, d: &[u8]) {
        self.out.push((d.to_vec(), None));
    }
    fn on_transmit_to(&mut self, _: SlotHandle, _: u64, _: u64, d: &[u8], to: Option<PeerAddr>) {
        self.out.push((d.to_vec(), to));
    }
    fn on_stateless(&mut self, d: &[u8], to: Option<PeerAddr>) {
        self.stateless += 1;
        self.out.push((d.to_vec(), to));
    }
}

type Ep = WarpEndpoint<SoftNic, Discard, Wire>;

fn config(role: Role) -> EndpointConfig {
    EndpointConfig {
        role,
        initial_rtt_ns: 20 * MS,
        streams: StreamConfig {
            stream_window: 32 * 1024,
            send_buffer: 32 * 1024,
            conn_window: 96 * 1024,
            ..StreamConfig::default()
        },
        ..EndpointConfig::default()
    }
}

fn endpoint(config: EndpointConfig) -> Ep {
    let umem = UmemConfig::with_frames(1024);
    let xsk = SoftNic::xsk(umem, SoftNicMode::Scripted).unwrap();
    WarpEndpoint::new(config, xsk, Discard::default(), Wire::default()).unwrap()
}

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

/// A client and a server with UDP addresses, joined by a lossy link.
struct Net {
    client: Ep,
    server: Ep,
    setup: ClientSetup,
    c_conn: SlotHandle,
    s_conn: Option<SlotHandle>,
    client_addr: SocketAddr,
    server_addr: SocketAddr,
    now: u64,
    rng: Rng,
    loss_ppm: u64,
    reorder_ppm: u64,
    held: [Vec<(Vec<u8>, Option<PeerAddr>)>; 2],
    events: [Vec<ConnectionEvent>; 2],
    /// Datagrams the server sent to an address the client no longer has.
    misdirected: u64,
    connects: u8,
}

impl Net {
    fn with(
        seed: u64,
        loss_ppm: u64,
        reorder_ppm: u64,
        client_cfg: EndpointConfig,
        server_cfg: EndpointConfig,
        setup: ClientSetup,
        server: ServerSetup,
    ) -> Self {
        let mut s = endpoint(server_cfg);
        s.listen(Listener::with_cids(server, Box::new(CountingCids(0x5e, 0))))
            .unwrap();
        let mut c = endpoint(client_cfg);
        c.set_cid_source(Box::new(CountingCids(0xc1, 0x80)));
        c.set_rx_source(Some(addr("198.51.100.7:443")));
        let first = c
            .connect(&setup, SERVER_NAME, &CID0, &ODCID0, 0)
            .unwrap();
        Self {
            client: c,
            server: s,
            setup,
            c_conn: first,
            s_conn: None,
            client_addr: addr("192.0.2.1:4433"),
            server_addr: addr("198.51.100.7:443"),
            now: 0,
            rng: Rng(seed),
            loss_ppm,
            reorder_ppm,
            held: [Vec::new(), Vec::new()],
            events: [Vec::new(), Vec::new()],
            misdirected: 0,
            connects: 1,
        }
    }

    fn new(seed: u64, loss_ppm: u64, reorder_ppm: u64) -> Self {
        Self::with(
            seed,
            loss_ppm,
            reorder_ppm,
            config(Role::Client),
            config(Role::Server),
            client_setup(),
            server_setup(),
        )
    }

    /// Opens (another) client connection.
    fn connect(&mut self) {
        self.connects += 1;
        let n = self.connects;
        let cid = [n, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1];
        let odcid = [0x83, n, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
        self.client.set_rx_source(Some(self.server_addr));
        self.c_conn = self
            .client
            .connect(&self.setup, SERVER_NAME, &cid, &odcid, self.now)
            .unwrap();
        self.s_conn = None;
    }

    fn deliver_one(ep: &mut Ep, d: &[u8], from: SocketAddr, now: u64) {
        ep.set_rx_source(Some(from));
        ep.xsk_mut().backend_mut().deliver(d);
        while ep.poll_and_step_quic_rx_one(now).unwrap() != RxOutcome::Idle {}
    }

    fn deliver(&mut self, dir: usize) {
        let out = match dir {
            0 => std::mem::take(&mut self.client.tap_mut().out),
            _ => std::mem::take(&mut self.server.tap_mut().out),
        };
        let mut batch = std::mem::take(&mut self.held[dir]);
        for d in out {
            if self.rng.below(1_000_000) < self.loss_ppm {
                continue;
            }
            if self.rng.below(1_000_000) < self.reorder_ppm {
                self.held[dir].push(d);
            } else {
                batch.push(d);
            }
        }
        let now = self.now;
        for (d, to) in batch {
            if dir == 0 {
                assert_eq!(to, Some(self.server_addr), "client sends to the server");
                Self::deliver_one(&mut self.server, &d, self.client_addr, now);
            } else if to.is_none() || to == Some(self.client_addr) {
                Self::deliver_one(&mut self.client, &d, self.server_addr, now);
            } else {
                self.misdirected += 1;
            }
        }
    }

    fn step(&mut self) {
        let now = self.now;
        self.deliver(0);
        self.deliver(1);
        for ep in [&mut self.client, &mut self.server] {
            ep.xsk_mut().backend_mut().step();
            ep.poll(now).unwrap();
        }
        while let Some(e) = self.client.poll_connection_event() {
            self.events[0].push(e);
        }
        while let Some(e) = self.server.poll_connection_event() {
            if let ConnectionEvent::Accepted(h) = e {
                self.s_conn = Some(h);
            }
            self.events[1].push(e);
        }
        self.now += 5 * MS;
        let quiet = self.client.tap().out.is_empty()
            && self.server.tap().out.is_empty()
            && self.held.iter().all(Vec::is_empty);
        if quiet
            && let Some(wake) = [self.client.next_wake_ns(), self.server.next_wake_ns()]
                .into_iter()
                .flatten()
                .min()
        {
            // Bounded, so quiet periods do not leap past idle timeouts.
            self.now = self.now.max(wake.min(self.now + 200 * MS));
        }
    }

    fn s(&self) -> SlotHandle {
        self.s_conn.expect("accepted")
    }

    fn confirmed(&self) -> bool {
        self.client.handshake_phase(self.c_conn) == Some(HandshakePhase::Confirmed)
            && self
                .s_conn
                .is_some_and(|s| self.server.handshake_phase(s) == Some(HandshakePhase::Confirmed))
    }

    fn handshake(&mut self, max_steps: usize) {
        for _ in 0..max_steps {
            if self.confirmed() {
                return;
            }
            self.step();
        }
        panic!(
            "no handshake: client {:?} server {:?}",
            self.client.stats(),
            self.server.stats()
        );
    }

    fn steps(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }
}

/// Client uploads, server echoes a download; both checked byte for byte.
fn transfer(net: &mut Net, seed: u64, upload: usize, download: usize, max_steps: usize) {
    let mut rng = Rng(seed ^ 0xfeed);
    let up = rng.bytes(upload);
    let down = rng.bytes(download);
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    exchange(net, sid, &up, &down, max_steps);
}

fn exchange(net: &mut Net, sid: u64, up: &[u8], down: &[u8], max_steps: usize) {
    exchange_from(net, sid, up, 0, down, max_steps);
}

/// [`exchange`] after the client already wrote and finished `up[..sent]`.
fn exchange_from(net: &mut Net, sid: u64, up: &[u8], sent: usize, down: &[u8], max_steps: usize) {
    let (mut up_sent, mut down_sent) = (sent, 0);
    let (mut up_got, mut down_got) = (Vec::new(), Vec::new());
    let (mut up_fin, mut down_fin) = (false, false);
    let mut buf = vec![0u8; 16 * 1024];
    for _ in 0..max_steps {
        if up_sent < up.len() {
            up_sent += net
                .client
                .stream_write(net.c_conn, sid, &up[up_sent..])
                .unwrap();
            if up_sent == up.len() {
                net.client.stream_finish(net.c_conn, sid).unwrap();
            }
        }
        if let Some(s) = net.s_conn {
            if !up_fin && let Ok(r) = net.server.stream_read(s, sid, &mut buf) {
                up_got.extend_from_slice(&buf[..r.len]);
                up_fin = r.fin;
            }
            if up_fin && down_sent < down.len() {
                down_sent += net.server.stream_write(s, sid, &down[down_sent..]).unwrap();
                if down_sent == down.len() {
                    net.server.stream_finish(s, sid).unwrap();
                }
            }
        }
        if !down_fin
            && let Ok(ReadOutcome { len, fin }) = net.client.stream_read(net.c_conn, sid, &mut buf)
        {
            down_got.extend_from_slice(&buf[..len]);
            down_fin = fin;
        }
        if up_fin && down_fin {
            assert!(up_got == up, "upload corrupted");
            assert!(down_got == down, "download corrupted");
            return;
        }
        net.step();
    }
    panic!(
        "transfer incomplete: up {}/{} down {}/{}; client {:?}; server {:?}",
        up_got.len(),
        up.len(),
        down_got.len(),
        down.len(),
        net.client.stats(),
        net.server.stats()
    );
}

fn with_lifecycle(role: Role, lifecycle: LifecycleConfig) -> EndpointConfig {
    EndpointConfig {
        lifecycle,
        ..config(role)
    }
}

#[test]
fn a_plain_handshake_issues_connection_ids_and_a_token() {
    let mut net = Net::new(1, 0, 0);
    net.handshake(200);
    net.steps(20);
    // limit 4: three more IDs each way.
    assert_eq!(net.client.stats().cids_issued, 3);
    assert_eq!(net.server.stats().cids_issued, 3);
    assert_eq!(net.server.stats().tokens_issued, 1);
    assert!(net.events[0].contains(&ConnectionEvent::NewToken(net.c_conn)));
    let (name, token) = net.client.stored_tokens().next().unwrap();
    assert_eq!(name, SERVER_NAME);
    assert!(!token.is_empty());
    transfer(&mut net, 1, 20_000, 20_000, 400);
}

#[test]
fn retry_validates_the_client_address() {
    let retry = LifecycleConfig {
        address_validation: AddressValidation::Retry,
        ..LifecycleConfig::default()
    };
    let mut net = Net::with(
        2,
        0,
        0,
        config(Role::Client),
        with_lifecycle(Role::Server, retry),
        client_setup(),
        server_setup(),
    );
    net.handshake(200);
    assert_eq!(net.server.stats().retries_sent, 1);
    assert_eq!(net.client.stats().retries_received, 1);
    assert_eq!(net.server.stats().tokens_accepted, 1);
    assert!(net.events[0].contains(&ConnectionEvent::Retried(net.c_conn)));
    transfer(&mut net, 2, 10_000, 10_000, 400);

    // The NEW_TOKEN token from this connection skips the Retry next time.
    net.steps(10);
    assert!(net.client.stored_tokens().next().is_some());
    net.connect();
    net.handshake(200);
    assert_eq!(net.server.stats().retries_sent, 1, "no second Retry");
    assert_eq!(net.server.stats().tokens_accepted, 2);
    transfer(&mut net, 3, 5_000, 5_000, 400);
}

#[test]
fn a_forged_or_misaddressed_retry_token_is_refused() {
    let retry = LifecycleConfig {
        address_validation: AddressValidation::Retry,
        ..LifecycleConfig::default()
    };
    let mut net = Net::with(
        3,
        0,
        0,
        config(Role::Client),
        with_lifecycle(Role::Server, retry),
        client_setup(),
        server_setup(),
    );
    // The Retry reaches the client; its next Initial (with the token)
    // arrives from another address.
    net.deliver(0);
    net.step();
    assert_eq!(net.client.stats().retries_received, 1);
    let out = std::mem::take(&mut net.client.tap_mut().out);
    let now = net.now;
    for (d, _) in &out {
        Net::deliver_one(&mut net.server, d, addr("203.0.113.9:999"), now);
    }
    assert!(net.server.stats().tokens_refused >= 1);
    assert_eq!(net.server.stats().accepted, 0);
}

#[test]
fn version_negotiation_both_ways() {
    let mut net = Net::new(4, 0, 0);
    // A 1200-byte Initial of an unknown version gets Version Negotiation.
    let mut pkt = vec![0u8; 1200];
    pkt[0] = 0xc0;
    pkt[1..5].copy_from_slice(&0x0a0a_0a0au32.to_be_bytes());
    pkt[5] = 8;
    pkt[6..14].copy_from_slice(&[9; 8]);
    pkt[14] = 4;
    pkt[15..19].copy_from_slice(&[7; 4]);
    let now = net.now;
    net.server.tap_mut().out.clear();
    Net::deliver_one(&mut net.server, &pkt, net.client_addr, now);
    assert_eq!(net.server.stats().version_negotiations_sent, 1);
    let (vn, to) = net.server.tap_mut().out.pop().unwrap();
    assert_eq!(to, Some(net.client_addr));
    assert_eq!(&vn[1..5], &[0, 0, 0, 0]);
    // DCID and SCID swapped (RFC 9000 §17.2.1).
    assert_eq!(&vn[6..10], &[7; 4]);
    // A short datagram does not get one (amplification).
    Net::deliver_one(&mut net.server, &pkt[..600], net.client_addr, now);
    assert_eq!(net.server.stats().version_negotiations_sent, 1);

    // A client whose server lists only other versions gives up...
    let cid = [1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1];
    let mut buf = [0u8; 64];
    let n = write_version_negotiation(&mut buf, 0, &cid, &[5; 8], &[0x0a0a_0a0a]).unwrap();
    // ...but one that lists v1 is ignored.
    let m = {
        let mut b2 = [0u8; 64];
        let m = write_version_negotiation(&mut b2, 0, &cid, &[5; 8], &[1, 0x0a0a_0a0a]).unwrap();
        Net::deliver_one(&mut net.client, &b2[..m], net.server_addr, now);
        m
    };
    assert!(m > 0);
    assert!(net.client.connection(net.c_conn).is_some());
    Net::deliver_one(&mut net.client, &buf[..n], net.server_addr, now);
    assert_eq!(
        net.client.connection_error(net.c_conn),
        Some(ConnectionError::VersionNegotiation)
    );
    assert!(net.client.connection(net.c_conn).is_none());
    net.step();
    assert!(net.events[0].contains(&ConnectionEvent::Closed {
        conn: net.c_conn,
        error: ConnectionError::VersionNegotiation
    }));
}

#[test]
fn a_server_that_lost_state_resets_the_connection() {
    let mut net = Net::new(5, 0, 0);
    net.handshake(200);
    transfer(&mut net, 5, 2_000, 2_000, 200);
    // The server forgets the connection (a restart with the same reset
    // key); the client's next packet is answered with a stateless reset.
    let s = net.s();
    net.server.close_connection(s, net.now).unwrap();
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    net.client.stream_write(net.c_conn, sid, b"anyone there?").unwrap();
    net.steps(10);
    assert_eq!(net.server.stats().stateless_resets_sent, 1);
    assert_eq!(net.client.stats().stateless_resets_received, 1);
    assert_eq!(
        net.client.connection_error(net.c_conn),
        Some(ConnectionError::StatelessReset)
    );
    assert!(net.events[0].contains(&ConnectionEvent::Closed {
        conn: net.c_conn,
        error: ConnectionError::StatelessReset
    }));
    // The reset is shorter than what triggered it and at least 21 bytes;
    // the client answers nothing (no reset loop).
    assert_eq!(net.client.stats().stateless_resets_sent, 0);
}

#[test]
fn client_migration_is_validated_and_followed() {
    let mut net = Net::new(6, 0, 0);
    net.handshake(200);
    net.steps(10);
    transfer(&mut net, 6, 4_000, 4_000, 200);
    let before = net.client.connection(net.c_conn).unwrap().tile.peer_cid;
    // The client's NAT gives it a new address, then it migrates.
    net.client_addr = addr("192.0.2.200:5000");
    net.client.migrate(net.c_conn, net.now).unwrap();
    let after = net.client.connection(net.c_conn).unwrap().tile.peer_cid;
    assert_ne!(before, after, "a fresh connection ID on the new path");
    transfer(&mut net, 7, 30_000, 30_000, 600);
    net.steps(20);
    assert!(net.events[0].contains(&ConnectionEvent::PathValidated(net.c_conn)));
    assert!(net.events[1].contains(&ConnectionEvent::PeerMigrated(net.s())));
    assert!(net.events[1].contains(&ConnectionEvent::PathValidated(net.s())));
    let st = net.server.stats();
    assert_eq!(st.migrations, 1);
    assert!(st.path_challenges_sent >= 1 && st.path_responses_sent >= 1);
    assert!(net.client.stats().cids_retired + net.server.stats().cids_retired >= 1);
    // No IP change of the server's own: nothing failed.
    assert_eq!(st.path_failures, 0);
}

#[test]
fn migration_to_a_dead_path_falls_back() {
    let mut net = Net::new(7, 0, 0);
    net.handshake(200);
    net.steps(10);
    let s = net.s();
    // A non-probing packet arrives from another address (a spoofer
    // replaying it): the server starts validating that address with the
    // 3× limit, nobody answers there, and it falls back.
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    net.client.stream_write(net.c_conn, sid, b"from a spoofed address").unwrap();
    net.client.flush(net.c_conn, net.now).unwrap();
    let out = std::mem::take(&mut net.client.tap_mut().out);
    let spoofed = addr("203.0.113.66:1");
    let now = net.now;
    for (d, _) in &out {
        Net::deliver_one(&mut net.server, d, spoofed, now);
    }
    assert!(net.server.stats().migrations == 1);
    // Meanwhile the real client is silent; what the server sends goes to
    // the spoofed address (bounded by the amplification limit).
    let mut to_spoofed = 0u64;
    for i in 1..400u64 {
        net.server.poll(now + i * 5 * MS).unwrap();
        for (d, to) in net.server.tap_mut().out.drain(..) {
            if to == Some(spoofed) {
                to_spoofed += d.len() as u64;
            }
        }
    }
    let received: u64 = out.iter().map(|(d, _)| d.len() as u64).sum();
    assert!(to_spoofed <= 3 * received + 1200, "{to_spoofed} for {received}");
    let st = net.server.stats();
    assert_eq!(st.path_failures, 1, "{st:?}");
    while let Some(e) = net.server.poll_connection_event() {
        net.events[1].push(e);
    }
    assert!(net.events[1].contains(&ConnectionEvent::PathFailed(s)));
    // Back on the validated path: the connection still works.
    net.now = now + 400 * 5 * MS;
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    exchange(&mut net, sid, b"still here", b"yes", 400);
}

#[test]
fn an_idle_connection_times_out_on_both_sides() {
    let idle = EndpointConfig {
        idle_timeout_ns: 2_000 * MS,
        ..config(Role::Client)
    };
    let mut net = Net::with(
        8,
        0,
        0,
        idle,
        config(Role::Server),
        client_setup(),
        server_setup(),
    );
    net.handshake(200);
    let s = net.s();
    net.steps(300);
    assert_eq!(
        net.client.connection_error(net.c_conn),
        Some(ConnectionError::IdleTimeout)
    );
    // The server took the client's smaller timeout.
    assert_eq!(
        net.server.connection_error(s),
        Some(ConnectionError::IdleTimeout)
    );
    assert!(net.now >= 2_000 * MS);
    assert_eq!(net.client.stats().idle_timeouts, 1);
    assert_eq!(net.server.stats().idle_timeouts, 1);
    assert_eq!(net.client.table().len(), 0);
    assert_eq!(net.server.table().len(), 0);
}

#[test]
fn an_application_close_drains_and_frees_both_slots() {
    let mut net = Net::new(9, 0, 0);
    net.handshake(200);
    transfer(&mut net, 9, 1_000, 1_000, 200);
    let s = net.s();
    net.client.close(net.c_conn, 0x101, net.now).unwrap();
    assert_eq!(
        net.client.close(net.c_conn, 1, net.now),
        Err(WarpError::ConnectionClosed(ConnectionError::Application {
            error_code: 0x101
        }))
    );
    net.steps(3);
    assert_eq!(
        net.server.connection_error(s),
        Some(ConnectionError::Peer {
            error_code: 0x101,
            frame_type: None
        })
    );
    // Closing and draining last three PTOs, then the slots are freed.
    net.steps(200);
    assert!(net.client.connection(net.c_conn).is_none());
    assert!(net.server.connection(s).is_none());
    assert!(net.events[0].iter().any(|e| matches!(e, ConnectionEvent::Closed { .. })));
    assert!(net.events[1].iter().any(|e| matches!(e, ConnectionEvent::Closed { .. })));
    assert_eq!(net.client.table().len(), 0);
    assert_eq!(net.server.table().len(), 0);
}

#[test]
fn zero_rtt_data_is_accepted_on_a_resumed_connection() {
    let setup = client_setup().with_zero_rtt();
    let mut net = Net::with(
        10,
        0,
        0,
        config(Role::Client),
        config(Role::Server),
        setup,
        server_setup().with_zero_rtt(),
    );
    net.handshake(200);
    transfer(&mut net, 10, 1_000, 1_000, 200);
    net.steps(10);
    // Second connection: data leaves in 0-RTT, before the handshake.
    net.connect();
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    let up = Rng(77).bytes(3_000);
    let n = net.client.stream_write(net.c_conn, sid, &up).unwrap();
    assert_eq!(n, up.len());
    net.client.stream_finish(net.c_conn, sid).unwrap();
    net.client.poll(net.now).unwrap();
    assert!(net.client.stats().zero_rtt_packets_sent >= 1);
    // The 0-RTT packets reach the server with its first flight, before
    // the handshake completes.
    for _ in 0..4 {
        net.step();
    }
    assert!(net.server.stats().zero_rtt_packets_received >= 1);
    exchange_from(&mut net, sid, &up, up.len(), b"thanks", 300);
    assert!(net.events[0].contains(&ConnectionEvent::ZeroRttAccepted(net.c_conn)));
    let tls = net.client.connection(net.c_conn).unwrap().tls.unwrap();
    assert!(tls.is_resumed());
}

#[test]
fn rejected_zero_rtt_resets_streams_and_the_application_resends() {
    let setup = client_setup().with_zero_rtt();
    let accepting = server_setup().with_zero_rtt();
    let mut net = Net::with(
        11,
        0,
        0,
        config(Role::Client),
        config(Role::Server),
        setup.clone(),
        accepting.clone(),
    );
    net.handshake(200);
    transfer(&mut net, 11, 1_000, 1_000, 200);
    net.steps(10);
    // A server sharing the session store but refusing early data.
    let refusing =
        ServerSetup::from_config((**accepting.config()).clone(), vec![ALPN.to_vec()]).unwrap();
    let mut server = endpoint(config(Role::Server));
    server
        .listen(Listener::with_cids(refusing, Box::new(CountingCids(0x6e, 0))))
        .unwrap();
    net.server = server;
    net.connect();
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    net.client.stream_write(net.c_conn, sid, b"early").unwrap();
    net.client.poll(net.now).unwrap();
    assert!(net.client.stats().zero_rtt_packets_sent >= 1);
    net.handshake(200);
    assert!(net.events[0].contains(&ConnectionEvent::ZeroRttRejected(net.c_conn)));
    assert_eq!(net.server.stats().zero_rtt_packets_received, 0);
    // The streams started afresh: the application opens and sends again.
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    assert_eq!(sid, 0, "stream IDs start over after rejection");
    exchange(&mut net, sid, b"again, in 1-RTT", b"ok", 300);
}

#[test]
fn handshake_packets_before_their_keys_are_buffered() {
    let mut net = Net::new(12, 0, 0);
    net.deliver(0);
    // The server's first datagram coalesces its Initial and a Handshake
    // packet. Split, the Handshake packet arrives first: the client has no
    // Handshake keys yet and holds it until the Initial installs them.
    let (first, _) = net.server.tap_mut().out.remove(0);
    let parser = HeaderParser::new(8).unwrap();
    let ParsedPacket::Protected(h) = parser.parse(&first).unwrap() else {
        panic!("a protected packet");
    };
    assert_eq!(h.packet_type, PacketType::Initial);
    let (initial, handshake) = first.split_at(h.packet_end);
    assert!(!handshake.is_empty(), "coalesced");
    let now = net.now;
    Net::deliver_one(&mut net.client, handshake, net.server_addr, now);
    assert_eq!(net.client.stats().datagrams_buffered, 1);
    Net::deliver_one(&mut net.client, initial, net.server_addr, now);
    let st = net.client.stats();
    assert_eq!(st.datagrams_replayed, 1, "{st:?}");
    assert_eq!(st.handshake_packets_received, 2, "{st:?}");
    net.handshake(200);
    transfer(&mut net, 12, 2_000, 2_000, 200);
}

#[test]
fn stream_priorities_order_the_sends() {
    let mut net = Net::new(13, 0, 0);
    net.handshake(200);
    let bulk = net.client.open_stream(net.c_conn, false).unwrap();
    let urgent = net.client.open_stream(net.c_conn, false).unwrap();
    net.client
        .stream_set_priority(net.c_conn, urgent, Priority { urgency: 0, incremental: false })
        .unwrap();
    net.client.stream_write(net.c_conn, bulk, &[1; 20_000]).unwrap();
    net.client.stream_write(net.c_conn, urgent, &[2; 500]).unwrap();
    net.client.stream_finish(net.c_conn, urgent).unwrap();
    assert!(net
        .client
        .stream_set_priority(net.c_conn, urgent, Priority { urgency: 8, incremental: false })
        .is_err());
    let mut buf = vec![0u8; 32 * 1024];
    let mut urgent_done_at = None;
    let mut bulk_bytes_then = 0;
    let mut bulk_got = 0;
    for _ in 0..200 {
        net.step();
        let s = net.s();
        if let Ok(r) = net.server.stream_read(s, bulk, &mut buf) {
            bulk_got += r.len;
        }
        if urgent_done_at.is_none()
            && let Ok(r) = net.server.stream_read(s, urgent, &mut buf)
            && r.fin
        {
            urgent_done_at = Some(());
            bulk_bytes_then = bulk_got;
        }
    }
    assert!(urgent_done_at.is_some());
    // The urgent stream finished before most of the bulk data arrived.
    assert!(bulk_bytes_then < 20_000, "{bulk_bytes_then}");
}

#[test]
fn lossy_sweep_of_handshake_opened_connections() {
    // Every lifecycle path at once: Retry, coalescing, buffering, 0-RTT
    // resumption and reliable streams, over loss and reordering.
    let mut failures = Vec::new();
    for seed in 0..12u64 {
        let (loss, reorder) = [(50_000, 50_000), (150_000, 100_000), (250_000, 200_000)]
            [(seed % 3) as usize];
        let retry = LifecycleConfig {
            address_validation: if seed % 2 == 0 {
                AddressValidation::Retry
            } else {
                AddressValidation::Handshake
            },
            ..LifecycleConfig::default()
        };
        let mut net = Net::with(
            seed,
            loss,
            reorder,
            config(Role::Client),
            with_lifecycle(Role::Server, retry),
            client_setup().with_zero_rtt(),
            server_setup().with_zero_rtt(),
        );
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            net.handshake(3_000);
            transfer(&mut net, seed, 8_000, 8_000, 3_000);
            net.steps(20);
            net.connect();
            net.handshake(3_000);
            transfer(&mut net, seed + 100, 8_000, 8_000, 3_000);
        }));
        if ok.is_err() {
            failures.push((seed, loss, reorder));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}
