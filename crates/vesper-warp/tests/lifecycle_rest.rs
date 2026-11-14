//! Wave seven lane 7 (issue #330): the rest of the Warp connection
//! lifecycle, one test (or more) per RFC 9000 rule, over SoftNic with UDP
//! addresses as metadata:
//!
//! * §12.2: 1-RTT packets are coalesced after long-header packets on send
//!   (data, ACKs and CONNECTION_CLOSE), and a coalesced client Initial
//!   still reaches 1200 bytes (§14.1);
//! * §5.1.2: a retired connection ID, including sequence 0, leaves the
//!   shard's connection table;
//! * §10.1: the idle timer restarts on the first ack-eliciting packet sent
//!   after a receive, and only on the first;
//! * §9.6: a server's `preferred_address` (validated by the client, then by
//!   the server, which keeps sending from its original address until
//!   then), probe failure, opting out, and a client discarding packets from
//!   unknown server addresses (§9);
//! * §9.3: a NAT rebinding keeps congestion state, and only the highest
//!   non-probing packet moves the path.

mod common;

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use common::Rng;
use vesper_warp::endpoint::{Discard, EndpointConfig, RxOutcome, TxTap, WarpEndpoint};
use vesper_warp::handshake::tls::FixedTime;
use vesper_warp::handshake::tls::rustls::RootCertStore;
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use vesper_warp::handshake::{
    CidSource, ClientSetup, ConnectionEvent, HandshakePhase, Listener, ServerSetup,
};
use vesper_warp::lifecycle::{LifecycleConfig, PeerAddr, PreferredAddressConfig};
use vesper_warp::stream::{ReadOutcome, Role, StreamConfig};
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

struct CountingCids(u8, u8);

impl CidSource for CountingCids {
    fn next_cid(&mut self, out: &mut [u8]) {
        self.1 = self.1.wrapping_add(1);
        out.fill(self.0);
        out[0] = self.1;
    }
}

/// A sent datagram: bytes, local source (`None`: the original address)
/// and destination.
type Sent = (Vec<u8>, Option<PeerAddr>, Option<PeerAddr>);

#[derive(Default)]
struct Wire {
    out: Vec<Sent>,
}

impl TxTap for Wire {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, d: &[u8]) {
        self.out.push((d.to_vec(), None, None));
    }
    fn on_transmit_via(
        &mut self,
        _: SlotHandle,
        _: u64,
        _: u64,
        d: &[u8],
        from: Option<PeerAddr>,
        to: Option<PeerAddr>,
    ) {
        self.out.push((d.to_vec(), from, to));
    }
    fn on_stateless(&mut self, d: &[u8], to: Option<PeerAddr>) {
        self.out.push((d.to_vec(), None, to));
    }
}

type Ep = WarpEndpoint<SoftNic, Discard, Wire>;

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

const SERVER: &str = "198.51.100.7:443";
const PREFERRED: &str = "203.0.113.9:8443";
const CLIENT: &str = "192.0.2.1:4433";

fn config(role: Role, lifecycle: LifecycleConfig) -> EndpointConfig {
    EndpointConfig {
        role,
        initial_rtt_ns: 20 * MS,
        streams: StreamConfig {
            stream_window: 32 * 1024,
            send_buffer: 32 * 1024,
            conn_window: 96 * 1024,
            ..StreamConfig::default()
        },
        lifecycle,
        ..EndpointConfig::default()
    }
}

fn preferred() -> LifecycleConfig {
    let SocketAddr::V4(p) = addr(PREFERRED) else {
        unreachable!()
    };
    LifecycleConfig {
        preferred_address: Some(PreferredAddressConfig {
            ipv4: Some(p),
            ipv6: None,
        }),
        ..LifecycleConfig::default()
    }
}

fn endpoint(config: EndpointConfig) -> Ep {
    let umem = UmemConfig::with_frames(1024);
    let xsk = SoftNic::xsk(umem, SoftNicMode::Scripted).unwrap();
    WarpEndpoint::new(config, xsk, Discard::default(), Wire::default()).unwrap()
}

/// What the link saw.
#[derive(Default)]
struct Seen {
    /// Client datagrams by destination: server, preferred.
    to_server: u64,
    to_preferred: u64,
    /// Server datagrams by source: original, preferred.
    from_server: u64,
    from_preferred: u64,
    /// Client datagrams that start with an Initial and are shorter than
    /// 1200 bytes (must stay 0).
    short_initials: u64,
    /// Every datagram, by sender (0 client, 1 server).
    log: [Vec<Vec<u8>>; 2],
}

/// A client and a server that may own a second (preferred) address.
struct Net {
    setup: ClientSetup,
    client: Ep,
    server: Ep,
    c_conn: SlotHandle,
    s_conn: Option<SlotHandle>,
    client_addr: SocketAddr,
    now: u64,
    events: [Vec<ConnectionEvent>; 2],
    seen: Seen,
    /// Drop every client datagram to the preferred address.
    preferred_dead: bool,
}

impl Net {
    fn new(client: LifecycleConfig, server: LifecycleConfig) -> Self {
        Self::with_setups(client, server, client_setup(), server_setup())
    }

    fn with_setups(
        client: LifecycleConfig,
        server: LifecycleConfig,
        client_setup: ClientSetup,
        server_setup: ServerSetup,
    ) -> Self {
        let mut s = endpoint(config(Role::Server, server));
        s.listen(Listener::with_cids(
            server_setup,
            Box::new(CountingCids(0x5e, 0)),
        ))
        .unwrap();
        let mut c = endpoint(config(Role::Client, client));
        c.set_cid_source(Box::new(CountingCids(0xc1, 0x80)));
        c.set_rx_source(Some(addr(SERVER)));
        let first = c
            .connect(&client_setup, SERVER_NAME, &CID0, &ODCID0, 0)
            .unwrap();
        Self {
            setup: client_setup,
            client: c,
            server: s,
            c_conn: first,
            s_conn: None,
            client_addr: addr(CLIENT),
            now: 0,
            events: [Vec::new(), Vec::new()],
            seen: Seen::default(),
            preferred_dead: false,
        }
    }

    fn deliver_one(ep: &mut Ep, d: &[u8], from: SocketAddr, to: SocketAddr, now: u64) {
        ep.set_rx_source(Some(from));
        ep.set_rx_destination(Some(to));
        ep.xsk_mut().backend_mut().deliver(d);
        while ep.poll_and_step_quic_rx_one(now).unwrap() != RxOutcome::Idle {}
    }

    fn deliver(&mut self) {
        let now = self.now;
        for (d, _, to) in std::mem::take(&mut self.client.tap_mut().out) {
            if d[0] & 0xf0 == 0xc0 && d.len() < 1200 {
                self.seen.short_initials += 1;
            }
            self.seen.log[0].push(d.clone());
            let to = to.expect("the client always names a destination");
            if to == addr(PREFERRED) {
                self.seen.to_preferred += 1;
                if self.preferred_dead {
                    continue;
                }
            } else {
                assert_eq!(to, addr(SERVER));
                self.seen.to_server += 1;
            }
            Self::deliver_one(&mut self.server, &d, self.client_addr, to, now);
        }
        for (d, from, to) in std::mem::take(&mut self.server.tap_mut().out) {
            self.seen.log[1].push(d.clone());
            let from = from.unwrap_or(addr(SERVER));
            if from == addr(PREFERRED) {
                self.seen.from_preferred += 1;
            } else {
                self.seen.from_server += 1;
            }
            if to.is_some_and(|t| t != self.client_addr) {
                continue;
            }
            Self::deliver_one(&mut self.client, &d, from, self.client_addr, now);
        }
    }

    fn step(&mut self) {
        self.deliver();
        let now = self.now;
        for ep in [&mut self.client, &mut self.server] {
            ep.xsk_mut().backend_mut().step();
            ep.poll(now).unwrap();
        }
        self.collect_events();
        self.now += 5 * MS;
    }

    fn collect_events(&mut self) {
        while let Some(e) = self.client.poll_connection_event() {
            self.events[0].push(e);
        }
        while let Some(e) = self.server.poll_connection_event() {
            if let ConnectionEvent::Accepted(h) = e {
                self.s_conn = Some(h);
            }
            self.events[1].push(e);
        }
    }

    /// Opens a second client connection (resuming the first's session).
    fn reconnect(&mut self) {
        let cid = [2, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1];
        let odcid = [0x83, 2, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
        self.client.set_rx_source(Some(addr(SERVER)));
        self.c_conn = self
            .client
            .connect(&self.setup, SERVER_NAME, &cid, &odcid, self.now)
            .unwrap();
        self.s_conn = None;
    }

    fn s(&self) -> SlotHandle {
        self.s_conn.expect("accepted")
    }

    fn handshake(&mut self) {
        for _ in 0..200 {
            let confirmed = self.client.handshake_phase(self.c_conn)
                == Some(HandshakePhase::Confirmed)
                && self.s_conn.is_some_and(|s| {
                    self.server.handshake_phase(s) == Some(HandshakePhase::Confirmed)
                });
            if confirmed {
                return;
            }
            self.step();
        }
        panic!("no handshake");
    }

    fn steps(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    /// Client uploads `up` bytes, the server answers with `down`; both
    /// checked byte for byte.
    fn transfer(&mut self, seed: u64, up: usize, down: usize) {
        let mut rng = Rng(seed);
        let up = rng.bytes(up);
        let down = rng.bytes(down);
        let sid = self.client.open_stream(self.c_conn, false).unwrap();
        let (mut up_sent, mut down_sent) = (0, 0);
        let (mut up_got, mut down_got) = (Vec::new(), Vec::new());
        let (mut up_fin, mut down_fin) = (false, false);
        let mut buf = vec![0u8; 16 * 1024];
        for _ in 0..1000 {
            if up_sent < up.len() {
                up_sent += self
                    .client
                    .stream_write(self.c_conn, sid, &up[up_sent..])
                    .unwrap();
                if up_sent == up.len() {
                    self.client.stream_finish(self.c_conn, sid).unwrap();
                }
            }
            let s = self.s();
            if !up_fin && let Ok(r) = self.server.stream_read(s, sid, &mut buf) {
                up_got.extend_from_slice(&buf[..r.len]);
                up_fin = r.fin;
            }
            if up_fin && down_sent < down.len() {
                down_sent += self
                    .server
                    .stream_write(s, sid, &down[down_sent..])
                    .unwrap();
                if down_sent == down.len() {
                    self.server.stream_finish(s, sid).unwrap();
                }
            }
            if !down_fin
                && let Ok(ReadOutcome { len, fin }) =
                    self.client.stream_read(self.c_conn, sid, &mut buf)
            {
                down_got.extend_from_slice(&buf[..len]);
                down_fin = fin;
            }
            if up_fin && down_fin {
                assert!(up_got == up, "upload corrupted");
                assert!(down_got == down, "download corrupted");
                return;
            }
            self.step();
        }
        panic!(
            "transfer incomplete: client {:?} server {:?}",
            self.client.stats(),
            self.server.stats()
        );
    }
}

/// The packet types in a datagram, in order: 0 Initial, 1 0-RTT,
/// 2 Handshake, 3 Retry, 4 short header. Long-header lengths are walked
/// (RFC 9000 §17.2); a short header takes the rest.
fn packet_types(d: &[u8]) -> Vec<u8> {
    fn varint(b: &[u8]) -> (u64, usize) {
        let len = 1usize << (b[0] >> 6);
        let mut v = u64::from(b[0] & 0x3f);
        for &x in &b[1..len] {
            v = (v << 8) | u64::from(x);
        }
        (v, len)
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < d.len() {
        let first = d[at];
        if first == 0 {
            break; // trailing PADDING of a padded datagram
        }
        if first & 0x80 == 0 {
            out.push(4);
            break;
        }
        let ty = (first >> 4) & 0x03;
        out.push(ty);
        let mut p = at + 5;
        let dcid = usize::from(d[p]);
        p += 1 + dcid;
        let scid = usize::from(d[p]);
        p += 1 + scid;
        if ty == 0 {
            let (tl, n) = varint(&d[p..]);
            p += n + tl as usize;
        }
        let (len, n) = varint(&d[p..]);
        at = p + n + len as usize;
    }
    out
}

// ----- RFC 9000 §12.2: coalescing 1-RTT after long headers ---------------

/// A resumed connection whose request is written before the handshake
/// ends (remembered transport parameters allow it) but not sent in 0-RTT:
/// it waits for 1-RTT keys. Returns the packet types of the datagrams each
/// side sent during the handshake.
fn resumed_request() -> (Net, [Vec<Vec<u8>>; 2]) {
    let mut net = Net::with_setups(
        LifecycleConfig::default(),
        LifecycleConfig::default(),
        client_setup().with_zero_rtt(),
        server_setup().with_zero_rtt(),
    );
    net.handshake();
    net.transfer(1, 1_000, 1_000);
    net.steps(10);
    net.reconnect();
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    net.client
        .stream_write(net.c_conn, sid, b"request")
        .unwrap();
    net.client.stream_finish(net.c_conn, sid).unwrap();
    net.seen.log = [Vec::new(), Vec::new()];
    net.steps(20);
    let types = [0, 1].map(|i| net.seen.log[i].iter().map(|d| packet_types(d)).collect());
    assert_eq!(net.seen.short_initials, 0, "RFC 9000 §14.1 padding");
    let s = net.s();
    let mut buf = [0u8; 64];
    let r = net.server.stream_read(s, sid, &mut buf).unwrap();
    assert_eq!(&buf[..r.len], b"request");
    (net, types)
}

/// Datagrams that carry a short-header packet after long-header ones;
/// each must end in exactly one short header.
fn coalesced_short(types: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let found: Vec<Vec<u8>> = types
        .iter()
        .filter(|t| t.len() > 1 && t.contains(&4))
        .cloned()
        .collect();
    for t in &found {
        assert_eq!(t.last(), Some(&4), "a short header is last: {t:?}");
        assert_eq!(t.iter().filter(|&&x| x == 4).count(), 1, "{t:?}");
    }
    found
}

#[test]
fn client_data_rides_behind_its_handshake_finished() {
    let (mut net, types) = resumed_request();
    // The client's first datagram with 1-RTT keys: its Initial ACK and
    // Handshake Finished, then the request in 1-RTT, padded to 1200 bytes
    // inside the 1-RTT packet.
    let found = coalesced_short(&types[0]);
    assert!(
        found.iter().any(|t| t == &[0, 2, 4] || t == &[2, 4]),
        "client datagrams {:?}",
        types[0]
    );
    assert!(net.client.stats().short_coalesced_sent >= 1);
    assert!(net.server.stats().coalesced_received >= 1);
    net.handshake();
    net.transfer(2, 20_000, 20_000);
}

#[test]
fn a_close_before_confirmation_coalesces_both_closes() {
    let mut net = Net::new(LifecycleConfig::default(), LifecycleConfig::default());
    // Run until the client has 1-RTT keys but still its Handshake keys
    // (HANDSHAKE_DONE not yet received).
    for _ in 0..200 {
        if net.client.handshake_phase(net.c_conn) == Some(HandshakePhase::Complete) {
            break;
        }
        net.step();
    }
    assert_eq!(
        net.client.handshake_phase(net.c_conn),
        Some(HandshakePhase::Complete)
    );
    net.client.tap_mut().out.clear();
    let before = net.client.stats().short_coalesced_sent;
    net.client.close(net.c_conn, 0x42, net.now).unwrap();
    let out = &net.client.tap().out;
    assert_eq!(out.len(), 1, "one datagram carries both closes");
    assert_eq!(packet_types(&out[0].0), vec![2, 4], "Handshake then 1-RTT");
    assert_eq!(net.client.stats().short_coalesced_sent, before + 1);
    net.steps(5);
    let s = net.s();
    // The server reads the first close it can: the Handshake packet's
    // APPLICATION_ERROR (RFC 9000 §10.2.3), or the 1-RTT application close.
    let error = net.server.connection_error(s);
    assert!(
        matches!(
            error,
            Some(vesper_warp::endpoint::ConnectionError::Peer {
                error_code: 0x0c | 0x42,
                ..
            })
        ),
        "{error:?}"
    );
}

// ----- RFC 9000 §5.1.2: retired IDs leave the connection table ----------

#[test]
fn retired_connection_ids_including_sequence_zero_leave_the_table() {
    let mut net = Net::new(LifecycleConfig::default(), LifecycleConfig::default());
    net.handshake();
    net.steps(10);
    let s = net.s();
    let seq0 = net.server.table().local_cid(s).unwrap();
    assert_eq!(net.server.table().lookup(seq0.as_bytes()), Ok(Some(s)));
    for round in 0..2u64 {
        let keyed = net.server.table().local_cid(s).unwrap();
        // The client's new path uses a fresh server ID and retires the
        // one in use (sequence 0 the first time).
        net.client_addr = SocketAddr::V4(SocketAddrV4::new(
            [192, 0, 2, 50 + round as u8].into(),
            5000,
        ));
        net.client.migrate(net.c_conn, net.now).unwrap();
        net.transfer(10 + round, 8_000, 8_000);
        net.steps(10);
        let now_keyed = net.server.table().local_cid(s).unwrap();
        assert_ne!(now_keyed, keyed, "round {round}: the table was rekeyed");
        assert_eq!(
            net.server.table().lookup(keyed.as_bytes()),
            Ok(None),
            "round {round}: a retired ID routes nowhere"
        );
        assert_eq!(net.server.table().lookup(now_keyed.as_bytes()), Ok(Some(s)));
    }
    let st = net.server.stats();
    assert_eq!(st.table_cids_rekeyed, 2);
    assert!(st.cids_retired >= 2);
    // Replacements were issued: the client still has spare IDs to move to.
    net.client_addr = addr("192.0.2.99:7000");
    net.client.migrate(net.c_conn, net.now).unwrap();
    net.transfer(20, 4_000, 4_000);
}

// ----- RFC 9000 §10.1: idle restart on the first eliciting send ---------

#[test]
fn the_idle_timer_restarts_on_the_first_ack_eliciting_send_after_a_receive() {
    let mut net = Net::new(LifecycleConfig::default(), LifecycleConfig::default());
    net.handshake();
    net.steps(20);
    // Quiet: the last packet was received a while ago.
    for _ in 0..10 {
        net.now += 20 * MS;
        let now = net.now;
        net.client.poll(now).unwrap();
        net.client.tap_mut().out.clear();
    }
    let deadline = |net: &Net| {
        net.client
            .connection(net.c_conn)
            .unwrap()
            .tile
            .idle_deadline_ns
    };
    let d0 = deadline(&net);
    let restarts0 = net.client.stats().idle_restarts_on_send;
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    net.client.stream_write(net.c_conn, sid, b"one").unwrap();
    let t1 = net.now;
    net.client.flush(net.c_conn, t1).unwrap();
    let d1 = deadline(&net);
    assert!(d1 > d0, "restarted on send: {d0} -> {d1}");
    assert!(d1 >= t1 + (d0.saturating_sub(t1)), "from the send time");
    assert_eq!(net.client.stats().idle_restarts_on_send, restarts0 + 1);
    // A second ack-eliciting send before any receive does not restart it.
    net.now += 50 * MS;
    net.client.stream_write(net.c_conn, sid, b"two").unwrap();
    let t2 = net.now;
    net.client.flush(net.c_conn, t2).unwrap();
    assert_eq!(deadline(&net), d1, "only the first send after a receive");
    assert_eq!(net.client.stats().idle_restarts_on_send, restarts0 + 1);
    // A receive re-arms it.
    net.steps(5);
    net.client.stream_write(net.c_conn, sid, b"three").unwrap();
    let t3 = net.now;
    net.client.flush(net.c_conn, t3).unwrap();
    assert_eq!(net.client.stats().idle_restarts_on_send, restarts0 + 2);
}

// ----- RFC 9000 §9.6: the server's preferred address ---------------------

#[test]
fn the_client_moves_to_the_servers_preferred_address() {
    let mut net = Net::new(LifecycleConfig::default(), preferred());
    net.handshake();
    net.transfer(30, 10_000, 10_000);
    net.steps(20);
    assert!(
        net.events[0].contains(&ConnectionEvent::MovedToPreferredAddress(net.c_conn)),
        "client events {:?}",
        net.events[0]
    );
    assert!(
        net.events[1].contains(&ConnectionEvent::MovedToPreferredAddress(net.s())),
        "server events {:?}",
        net.events[1]
    );
    assert_eq!(net.client.stats().preferred_address_migrations, 1);
    assert_eq!(net.server.stats().preferred_address_migrations, 1);
    // The preferred address's ID (sequence 1) is in use; the handshake ID
    // was retired and left the server's table.
    assert_eq!(net.server.stats().table_cids_rekeyed, 1);
    // Limit 4: the preferred-address ID plus two NEW_CONNECTION_IDs, and a
    // replacement for the retired handshake ID; never past the client's
    // limit (no CONNECTION_ID_LIMIT_ERROR).
    assert_eq!(net.server.stats().cids_issued, 4);
    assert_eq!(net.client.connection_error(net.c_conn), None);
    // From now on: the client sends to the preferred address, the server
    // sends from it.
    net.seen = Seen::default();
    net.transfer(31, 30_000, 30_000);
    net.steps(10);
    assert_eq!(net.seen.to_server, 0, "nothing to the original address");
    assert!(net.seen.to_preferred > 10);
    assert_eq!(net.seen.from_server, 0, "nothing from the original address");
    assert!(net.seen.from_preferred > 10);
    assert_eq!(net.client.stats().unknown_server_address_drops, 0);
}

#[test]
fn the_server_sends_from_its_original_address_until_it_validated_the_preferred_path() {
    let mut net = Net::new(LifecycleConfig::default(), preferred());
    net.handshake();
    // Step until the client has moved but the server has not yet.
    let mut client_moved_first = false;
    for _ in 0..200 {
        net.step();
        let c = net.client.stats().preferred_address_migrations;
        let s = net.server.stats().preferred_address_migrations;
        if c == 1 && s == 0 {
            client_moved_first = true;
            // Only path frames (the PATH_RESPONSE to the client's probe
            // and the server's own PATH_CHALLENGE) left from the preferred
            // address; everything else from the original one, and the
            // client accepted it.
            let st = net.server.stats();
            assert!(net.seen.from_preferred <= st.path_responses_sent + st.path_challenges_sent);
        }
        if s == 1 {
            break;
        }
    }
    assert!(client_moved_first);
    assert_eq!(net.server.stats().preferred_address_migrations, 1);
    assert_eq!(net.client.stats().unknown_server_address_drops, 0);
    net.transfer(32, 5_000, 5_000);
}

#[test]
fn a_dead_preferred_address_leaves_the_connection_on_the_original_one() {
    let mut net = Net::new(LifecycleConfig::default(), preferred());
    net.preferred_dead = true;
    net.handshake();
    net.transfer(33, 10_000, 10_000);
    for _ in 0..200 {
        if net.events[0].contains(&ConnectionEvent::PathFailed(net.c_conn)) {
            break;
        }
        net.step();
    }
    assert!(net.events[0].contains(&ConnectionEvent::PathFailed(net.c_conn)));
    assert_eq!(net.client.stats().preferred_address_migrations, 0);
    assert!(net.seen.to_preferred >= 1, "it was probed");
    net.seen = Seen::default();
    net.transfer(34, 10_000, 10_000);
    assert_eq!(net.seen.to_preferred, 0);
    assert!(net.seen.to_server > 0);
}

#[test]
fn a_client_may_ignore_the_preferred_address() {
    let off = LifecycleConfig {
        follow_preferred_address: false,
        ..LifecycleConfig::default()
    };
    let mut net = Net::new(off, preferred());
    net.handshake();
    net.transfer(35, 10_000, 10_000);
    net.steps(20);
    assert_eq!(net.seen.to_preferred, 0);
    assert_eq!(net.client.stats().preferred_address_migrations, 0);
    assert_eq!(net.server.stats().preferred_address_migrations, 0);
}

#[test]
fn a_server_with_migration_disabled_still_moves_to_its_preferred_address() {
    // §18.2: disable_active_migration does not prohibit migration to the
    // preferred address.
    let server = LifecycleConfig {
        migration: false,
        ..preferred()
    };
    let mut net = Net::new(LifecycleConfig::default(), server);
    net.handshake();
    net.transfer(36, 10_000, 10_000);
    net.steps(20);
    assert_eq!(net.client.stats().preferred_address_migrations, 1);
    assert_eq!(net.server.stats().preferred_address_migrations, 1);
}

#[test]
fn a_client_discards_packets_from_unknown_server_addresses() {
    let mut net = Net::new(LifecycleConfig::default(), LifecycleConfig::default());
    net.handshake();
    net.steps(10);
    let s = net.s();
    let stream = net.server.open_stream(s, true).unwrap();
    net.server
        .stream_write(s, stream, b"from a stranger")
        .unwrap();
    net.server.flush(s, net.now).unwrap();
    let out = std::mem::take(&mut net.server.tap_mut().out);
    assert!(!out.is_empty());
    let now = net.now;
    for (d, _, _) in &out {
        Net::deliver_one(
            &mut net.client,
            d,
            addr("203.0.113.200:443"),
            net.client_addr,
            now,
        );
    }
    assert_eq!(
        net.client.stats().unknown_server_address_drops,
        out.len() as u64
    );
    let mut buf = [0u8; 64];
    assert!(
        net.client
            .stream_read(net.c_conn, stream, &mut buf)
            .is_err()
    );
    // From the real address the data arrives (the server retransmits).
    for _ in 0..100 {
        net.step();
        if let Ok(r) = net.client.stream_read(net.c_conn, stream, &mut buf) {
            assert_eq!(&buf[..r.len], b"from a stranger");
            return;
        }
    }
    panic!("never delivered from the server's address");
}

// ----- RFC 9000 §9.3/§9.4: NAT rebinding ---------------------------------

#[test]
fn a_nat_rebinding_keeps_congestion_state_and_only_the_newest_packet_moves_the_path() {
    let mut net = Net::new(LifecycleConfig::default(), LifecycleConfig::default());
    net.handshake();
    net.transfer(40, 60_000, 60_000);
    let s = net.s();
    let cwnd = |net: &Net| net.server.connection(s).unwrap().tile.cwnd_bytes;
    let grown = cwnd(&net);
    // Two client packets; the NAT rebinds between them, and the older one
    // arrives last, from the old port.
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    net.client.stream_write(net.c_conn, sid, b"first").unwrap();
    net.client.flush(net.c_conn, net.now).unwrap();
    let first = std::mem::take(&mut net.client.tap_mut().out);
    net.client.stream_write(net.c_conn, sid, b"second").unwrap();
    net.now += MS;
    net.client.flush(net.c_conn, net.now).unwrap();
    let second = std::mem::take(&mut net.client.tap_mut().out);
    assert!(!first.is_empty() && !second.is_empty());
    let old = net.client_addr;
    let rebound = addr("192.0.2.1:6001");
    net.now += MS;
    let now = net.now;
    for (d, _, to) in &second {
        Net::deliver_one(&mut net.server, d, rebound, to.unwrap(), now);
    }
    assert_eq!(
        net.server.stats().migrations,
        1,
        "the newest packet moved the path"
    );
    for (d, _, to) in &first {
        Net::deliver_one(&mut net.server, d, old, to.unwrap(), now);
    }
    assert_eq!(
        net.server.stats().migrations,
        1,
        "an older packet does not move it back"
    );
    // §9.4: a port-only change keeps the congestion controller.
    assert!(cwnd(&net) >= grown, "congestion state kept on a rebinding");
    net.client_addr = rebound;
    net.server.poll(now).unwrap();
    for (_, _, to) in &net.server.tap().out {
        assert_eq!(*to, Some(rebound), "the server answers the new port");
    }
    net.transfer(41, 10_000, 10_000);
    assert!(net.events[1].contains(&ConnectionEvent::PathValidated(s)));
}
