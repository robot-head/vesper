//! Wave-five integration A (issue #135): connections opened through
//! `WarpEndpoint` with lane 12's TLS 1.3 handshake over lane 13's CRYPTO
//! buffers, with no caller-supplied secrets, then carrying reliable
//! streams over lossy, reordering links.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use common::Rng;
use vesper_warp::endpoint::{
    ConnectionError, Discard, DropReason, EndpointConfig, RxOutcome, TxTap, WarpEndpoint, WarpError,
};
use vesper_warp::handshake::tls::rustls::RootCertStore;
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use vesper_warp::handshake::tls::{FixedTime, KeyState, Space};
use vesper_warp::handshake::{
    CidSource, ClientSetup, ConnectionEvent, HandshakePhase, Listener, ServerSetup,
};
use vesper_warp::header::{LongHeader, LongPacketType, write_long};
use vesper_warp::packet_number::PnLen;
use vesper_warp::stream::{ReadOutcome, Role, StreamConfig};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

const MS: u64 = 1_000_000;
const ALPN: &[u8] = b"vesper-warp/1";
const SERVER_NAME: &str = "warp.test";
const CLIENT_CID: [u8; 8] = *b"client01";
const ODCID: [u8; 8] = [0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
const TEST_CA: &[u8] = include_bytes!("fixtures/test-ca.der");
const ROGUE_CA: &[u8] = include_bytes!("fixtures/rogue-ca.der");
const SERVER_CERT: &[u8] = include_bytes!("fixtures/server.der");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/server.key.der");
/// 2030-01-01, inside the fixtures' validity.
const NOW_SECS: u64 = 1_893_456_000;

fn client_setup_with(ca: &[u8], alpn: &[u8]) -> ClientSetup {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(ca.to_vec())).unwrap();
    let time = Arc::new(FixedTime(UnixTime::since_unix_epoch(Duration::from_secs(
        NOW_SECS,
    ))));
    ClientSetup::with_time(roots, vec![alpn.to_vec()], time).unwrap()
}

fn client_setup() -> ClientSetup {
    client_setup_with(TEST_CA, ALPN)
}

fn server_setup() -> ServerSetup {
    ServerSetup::new(
        vec![CertificateDer::from(SERVER_CERT.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec())),
        vec![ALPN.to_vec()],
    )
    .unwrap()
}

/// Deterministic server connection IDs, so runs are reproducible.
struct CountingCids(u8);

impl CidSource for CountingCids {
    fn next_cid(&mut self, out: &mut [u8]) {
        self.0 += 1;
        out.fill(0x5e);
        out[0] = self.0;
    }
}

/// Every transmitted datagram, in order.
#[derive(Default)]
struct Capture {
    out: Vec<Vec<u8>>,
    bytes: u64,
}

impl TxTap for Capture {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, datagram: &[u8]) {
        self.bytes += datagram.len() as u64;
        // The test's wire, not the endpoint: not counted as its allocation.
        uncounted(|| self.out.push(datagram.to_vec()));
    }
}

type Ep = WarpEndpoint<SoftNic, Discard, Capture>;

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
    WarpEndpoint::new(config, xsk, Discard::default(), Capture::default()).unwrap()
}

/// A client and a server joined by a link that can lose, reorder and
/// filter datagrams.
struct Net {
    client: Ep,
    server: Ep,
    c_conn: SlotHandle,
    s_conn: Option<SlotHandle>,
    now: u64,
    delay_ns: u64,
    rng: Rng,
    loss_ppm: u64,
    reorder_ppm: u64,
    held: [Vec<Vec<u8>>; 2],
    dropped: u64,
    /// Drop every datagram in this direction (0: to server, 1: to client).
    blackhole: [bool; 2],
    events: [Vec<ConnectionEvent>; 2],
}

impl Net {
    fn new(seed: u64, loss_ppm: u64, reorder_ppm: u64) -> Self {
        Self::with_client(seed, loss_ppm, reorder_ppm, client_setup())
    }

    fn with_client(seed: u64, loss_ppm: u64, reorder_ppm: u64, setup: ClientSetup) -> Self {
        let mut server = endpoint(config(Role::Server));
        server
            .listen(Listener::with_cids(
                server_setup(),
                Box::new(CountingCids(0)),
            ))
            .unwrap();
        let mut client = endpoint(config(Role::Client));
        let c_conn = client
            .connect(&setup, SERVER_NAME, &CLIENT_CID, &ODCID, 0)
            .unwrap();
        Self {
            client,
            server,
            c_conn,
            s_conn: None,
            now: 0,
            delay_ns: 5 * MS,
            rng: Rng(seed),
            loss_ppm,
            reorder_ppm,
            held: [Vec::new(), Vec::new()],
            dropped: 0,
            blackhole: [false; 2],
            events: [Vec::new(), Vec::new()],
        }
    }

    fn deliver(&mut self, dir: usize) {
        let batch = uncounted(|| {
            let out = match dir {
                0 => std::mem::take(&mut self.client.tap_mut().out),
                _ => std::mem::take(&mut self.server.tap_mut().out),
            };
            let mut batch = std::mem::take(&mut self.held[dir]);
            for d in out {
                if self.blackhole[dir] || self.rng.below(1_000_000) < self.loss_ppm {
                    self.dropped += 1;
                } else if self.rng.below(1_000_000) < self.reorder_ppm {
                    self.held[dir].push(d);
                } else {
                    batch.push(d);
                }
            }
            batch
        });
        let to = if dir == 0 {
            &mut self.server
        } else {
            &mut self.client
        };
        for d in &batch {
            to.xsk_mut().backend_mut().deliver(d);
        }
        uncounted(|| drop(batch));
    }

    fn step(&mut self) {
        let now = self.now;
        self.deliver(0);
        self.deliver(1);
        for _ in 0..8192 {
            let a = self.server.poll_and_step_quic_rx_one(now).unwrap();
            let b = self.client.poll_and_step_quic_rx_one(now).unwrap();
            if a == RxOutcome::Idle && b == RxOutcome::Idle {
                break;
            }
        }
        self.client.xsk_mut().backend_mut().step();
        self.server.xsk_mut().backend_mut().step();
        self.client.poll(now).unwrap();
        self.server.poll(now).unwrap();
        while let Some(e) = self.client.poll_connection_event() {
            uncounted(|| self.events[0].push(e));
        }
        while let Some(e) = self.server.poll_connection_event() {
            if let ConnectionEvent::Accepted(h) = e {
                assert!(self.s_conn.is_none(), "one connection");
                self.s_conn = Some(h);
            }
            uncounted(|| self.events[1].push(e));
        }
        self.now += self.delay_ns;
        // Nothing on the wire: skip ahead to the next timer, so long PTO
        // backoffs under heavy loss cost one step.
        let quiet = self.client.tap().out.is_empty()
            && self.server.tap().out.is_empty()
            && self.held.iter().all(Vec::is_empty);
        if quiet
            && let Some(wake) = [self.client.next_wake_ns(), self.server.next_wake_ns()]
                .into_iter()
                .flatten()
                .min()
        {
            self.now = self.now.max(wake);
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

    fn handshake(&mut self, max_steps: usize) -> usize {
        for i in 0..max_steps {
            if self.confirmed() {
                return i;
            }
            self.step();
        }
        panic!(
            "no handshake after {max_steps} steps: client {:?} server {:?}",
            self.client.stats(),
            self.server.stats()
        );
    }
}

fn assert_established(net: &Net) {
    let c = net.client.connection(net.c_conn).unwrap();
    let s = net.server.connection(net.s()).unwrap();
    for (v, peer_cert) in [(&c, true), (&s, false)] {
        assert_eq!(v.phase, HandshakePhase::Confirmed);
        assert!(v.keys.is_some(), "1-RTT keys installed by the handshake");
        assert!(v.error.is_none(), "{:?}", v.error);
        let tls = v.tls.unwrap();
        assert_eq!(tls.alpn_protocol(), Some(ALPN));
        assert_eq!(tls.key_state(Space::Initial), KeyState::Discarded);
        assert_eq!(tls.key_state(Space::Handshake), KeyState::Discarded);
        assert_eq!(tls.key_state(Space::Data), KeyState::Installed);
        assert_eq!(tls.peer_certificates().is_some(), peer_cert);
    }
    // Both sides derived the same 1-RTT keys from TLS: the cipher agrees
    // and packets open (no crypto drops).
    assert_eq!(c.keys.unwrap().cipher(), s.keys.unwrap().cipher());
    // The client switched to the server's connection ID.
    // CountingCids issued [1, 0x5e × 7] for the server's connection.
    assert_eq!(
        c.tile.peer_cid.as_bytes(),
        &[1, 0x5e, 0x5e, 0x5e, 0x5e, 0x5e, 0x5e, 0x5e]
    );
    assert_eq!(s.tile.peer_cid.as_bytes(), &CLIENT_CID);
    // Negotiated transport parameters replaced the zero defaults.
    let local = config(Role::Server).streams.local_params();
    assert_eq!(*c.streams.peer_params(), local);
    assert_eq!(*s.streams.peer_params(), local);
}

/// Client writes `upload` bytes on a bidirectional stream; the server
/// reads them all and sends `download` bytes back on the same stream.
/// Both directions are checked byte for byte.
fn transfer(net: &mut Net, seed: u64, upload: usize, download: usize, max_steps: usize) {
    let mut rng = Rng(seed ^ 0xfeed);
    let up = rng.bytes(upload);
    let down = rng.bytes(download);
    let sid = net.client.open_stream(net.c_conn, false).unwrap();
    let (mut up_sent, mut down_sent) = (0, 0);
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
        let s = net.s();
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
        if !down_fin {
            let ReadOutcome { len, fin } =
                net.client.stream_read(net.c_conn, sid, &mut buf).unwrap();
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
        upload,
        down_got.len(),
        download,
        net.client.stats(),
        net.server.stats()
    );
}

#[test]
fn handshake_opens_a_connection_without_caller_secrets() {
    let mut net = Net::new(1, 0, 0);
    // Before any server reply: TLS runs, there are no 1-RTT keys, and
    // streams wait for the peer's transport parameters.
    let v = net.client.connection(net.c_conn).unwrap();
    assert_eq!(v.phase, HandshakePhase::InProgress);
    assert!(v.keys.is_none());
    assert_eq!(
        net.client.open_stream(net.c_conn, false),
        Err(WarpError::Handshaking)
    );
    assert_eq!(
        net.client.initiate_key_update(net.c_conn),
        Err(WarpError::Handshaking)
    );
    // The ClientHello went out padded to 1200 bytes.
    let first = &net.client.tap().out[0];
    assert_eq!(first.len(), 1200);
    assert_eq!(first[0] & 0xf0, 0xc0, "long header, Initial");

    let steps = net.handshake(50);
    assert!(steps <= 6, "one round trip plus confirmation, took {steps}");
    assert_established(&net);
    assert!(net.events[0].contains(&ConnectionEvent::HandshakeComplete(net.c_conn)));
    assert!(net.events[0].contains(&ConnectionEvent::HandshakeConfirmed(net.c_conn)));
    assert_eq!(net.events[1][0], ConnectionEvent::Accepted(net.s()));
    let (c, s) = (net.client.stats(), net.server.stats());
    assert_eq!(c.rx_dropped_crypto + s.rx_dropped_crypto, 0);
    assert_eq!(c.handshake_failures + s.handshake_failures, 0);
    assert_eq!(s.accepted, 1);
    assert!(c.handshake_packets_sent >= 2 && s.handshake_packets_sent >= 2);

    transfer(&mut net, 1, 50_000, 70_000, 2_000);
    // Key updates are permitted once confirmed.
    net.client.initiate_key_update(net.c_conn).unwrap();
    transfer(&mut net, 2, 5_000, 5_000, 2_000);
    assert_eq!(
        net.server
            .connection(net.s())
            .unwrap()
            .keys
            .unwrap()
            .recv_generation(),
        1
    );
}

#[test]
fn handshake_and_streams_survive_a_lossy_reordering_link() {
    for (seed, loss) in [(11, 100_000), (12, 200_000), (13, 300_000), (14, 150_000)] {
        let mut net = Net::new(seed, loss, 100_000);
        net.handshake(3_000);
        assert_established(&net);
        transfer(&mut net, seed, 60_000, 60_000, 20_000);
        assert!(net.dropped > 0, "seed {seed}: the link lost nothing");
        let c = net.client.stats();
        let s = net.server.stats();
        assert_eq!(
            c.handshake_failures + s.handshake_failures,
            0,
            "seed {seed}"
        );
        assert!(c.packets_lost + s.packets_lost > 0, "seed {seed}");
    }
}

#[test]
fn handshake_completes_under_heavy_loss() {
    let mut lost_in_handshake = 0;
    for seed in 100..140 {
        let mut net = Net::new(seed, 400_000, 200_000);
        net.handshake(5_000);
        lost_in_handshake += net.dropped;
        assert_established(&net);
        transfer(&mut net, seed, 2_000, 2_000, 5_000);
        assert_eq!(net.client.stats().handshake_failures, 0, "seed {seed}");
        assert_eq!(net.server.stats().handshake_failures, 0, "seed {seed}");
    }
    assert!(
        lost_in_handshake > 40,
        "only {lost_in_handshake} handshake datagrams lost"
    );
}

#[test]
fn every_handshake_datagram_lost_once_is_recovered() {
    // Drop each of the first datagrams in turn (both directions), which
    // covers the ClientHello, the server's Initial and Handshake flights
    // and the client Finished.
    for victim in 0..6u64 {
        let mut net = Net::new(victim, 0, 0);
        let mut seen = 0u64;
        for _ in 0..400 {
            if net.confirmed() {
                break;
            }
            for dir in 0..2 {
                let tap = if dir == 0 {
                    &mut net.client.tap_mut().out
                } else {
                    &mut net.server.tap_mut().out
                };
                let mut keep = Vec::new();
                for d in tap.drain(..) {
                    if seen == victim {
                        net.dropped += 1;
                    } else {
                        keep.push(d);
                    }
                    seen += 1;
                }
                *tap = keep;
            }
            net.step();
        }
        assert!(net.confirmed(), "victim {victim}");
        assert_eq!(net.dropped, 1, "victim {victim}");
        assert_established(&net);
        transfer(&mut net, victim, 3_000, 3_000, 500);
    }
}

#[test]
fn untrusted_certificate_closes_with_a_crypto_error() {
    let mut net = Net::with_client(3, 0, 0, client_setup_with(ROGUE_CA, ALPN));
    for _ in 0..40 {
        net.step();
    }
    // bad_certificate (42) or unknown_ca (48) as CRYPTO_ERROR 0x100 + alert.
    let err = net.client.connection_error(net.c_conn).unwrap();
    let ConnectionError::Handshake { error_code } = err else {
        panic!("{err:?}");
    };
    assert!(
        error_code == 0x100 + 42 || error_code == 0x100 + 48,
        "{error_code:#x}"
    );
    assert!(net.client.connection(net.c_conn).unwrap().keys.is_none());
    // The server got the CONNECTION_CLOSE with the same code.
    assert_eq!(
        net.server.connection_error(net.s()),
        Some(ConnectionError::Peer {
            error_code,
            frame_type: Some(0x06)
        })
    );
    assert_eq!(
        net.client.open_stream(net.c_conn, false),
        Err(WarpError::ConnectionClosed(err))
    );
}

#[test]
fn alpn_mismatch_fails_the_handshake() {
    let mut net = Net::with_client(4, 0, 0, client_setup_with(TEST_CA, b"other/1"));
    for _ in 0..40 {
        net.step();
    }
    // The server refuses (no_application_protocol, 120) and closes.
    assert_eq!(
        net.server.connection_error(net.s()),
        Some(ConnectionError::Handshake {
            error_code: 0x100 + 120
        })
    );
    assert!(matches!(
        net.client.connection_error(net.c_conn),
        Some(ConnectionError::Peer {
            error_code: 0x178,
            ..
        })
    ));
}

#[test]
fn server_respects_the_anti_amplification_limit() {
    let mut net = Net::new(5, 0, 0);
    // Only the first client datagram (the ClientHello) reaches the server.
    net.step();
    net.blackhole[0] = true;
    let received = 1200;
    for _ in 0..400 {
        net.step();
    }
    let sent = net.server.stats();
    assert!(sent.handshake_packets_sent >= 1);
    let bytes: u64 = 3 * received;
    let server_tx = net.server.tap().bytes;
    assert!(
        server_tx <= bytes,
        "server sent {server_tx} bytes before validating a {received}-byte client"
    );
    assert!(sent.amplification_blocked > 0, "{sent:?}");
}

#[test]
fn short_or_unroutable_initials_are_dropped() {
    let mut server = endpoint(config(Role::Server));
    // Without a listener, an Initial for an unknown CID is dropped.
    let mut pkt = vec![0u8; 1200];
    let w = write_long(
        &mut pkt,
        &LongHeader {
            ty: LongPacketType::Initial,
            dcid: &ODCID,
            scid: &CLIENT_CID,
            token: &[],
            full_pn: 0,
            pn_len: PnLen::new(1).unwrap(),
            payload_len: 1173,
        },
    )
    .unwrap();
    let datagram = w.packet_len;
    assert_eq!(datagram, 1200);
    server.xsk_mut().backend_mut().deliver(&pkt[..datagram]);
    assert_eq!(
        server.poll_and_step_quic_rx_one(0).unwrap(),
        RxOutcome::Dropped(DropReason::UnknownCid)
    );
    server
        .listen(Listener::with_cids(
            server_setup(),
            Box::new(CountingCids(0)),
        ))
        .unwrap();
    // Garbage protection: authentication fails, and the tentative
    // connection is removed again (only an Initial that opens creates one).
    server.xsk_mut().backend_mut().deliver(&pkt[..datagram]);
    let out = server.poll_and_step_quic_rx_one(0).unwrap();
    assert!(
        matches!(out, RxOutcome::Dropped(DropReason::Crypto(_))),
        "{out:?}"
    );
    assert_eq!(server.table().len(), 0);
    assert_eq!(server.stats().accepted, 0);
    assert_eq!(server.poll_connection_event(), None);
    // A datagram under 1200 bytes cannot open a connection.
    let mut small = vec![0u8; 300];
    let w = write_long(
        &mut small,
        &LongHeader {
            ty: LongPacketType::Initial,
            dcid: &[9; 8],
            scid: &CLIENT_CID,
            token: &[],
            full_pn: 0,
            pn_len: PnLen::new(1).unwrap(),
            payload_len: 200,
        },
    )
    .unwrap();
    server
        .xsk_mut()
        .backend_mut()
        .deliver(&small[..w.packet_len]);
    assert_eq!(
        server.poll_and_step_quic_rx_one(0).unwrap(),
        RxOutcome::Dropped(DropReason::BadInitial)
    );
    // A Handshake packet for an unknown connection is dropped.
    let w = write_long(
        &mut small,
        &LongHeader {
            ty: LongPacketType::Handshake,
            dcid: &[9; 8],
            scid: &CLIENT_CID,
            token: &[],
            full_pn: 0,
            pn_len: PnLen::new(1).unwrap(),
            payload_len: 200,
        },
    )
    .unwrap();
    server
        .xsk_mut()
        .backend_mut()
        .deliver(&small[..w.packet_len]);
    assert_eq!(
        server.poll_and_step_quic_rx_one(0).unwrap(),
        RxOutcome::Dropped(DropReason::UnknownCid)
    );
}

#[test]
fn connect_and_listen_check_roles_and_datagram_size() {
    let mut server = endpoint(config(Role::Server));
    assert!(matches!(
        server.connect(&client_setup(), SERVER_NAME, &CLIENT_CID, &ODCID, 0),
        Err(WarpError::Config(_))
    ));
    let mut client = endpoint(config(Role::Client));
    assert!(matches!(
        client.listen(Listener::new(server_setup())),
        Err(WarpError::Config(_))
    ));
    assert!(matches!(
        client.connect(&client_setup(), SERVER_NAME, &CLIENT_CID, &[1; 4], 0),
        Err(WarpError::Config(_))
    ));
    let mut small = endpoint(EndpointConfig {
        max_datagram: 1000,
        ..config(Role::Client)
    });
    assert!(matches!(
        small.connect(&client_setup(), SERVER_NAME, &CLIENT_CID, &ODCID, 0),
        Err(WarpError::Config(_))
    ));
}

/// A wider sweep for local runs: `cargo test -p vesper-warp --test
/// handshake_endpoint -- --ignored`.
#[test]
#[ignore = "slow seed sweep"]
fn handshake_seed_sweep() {
    let seeds: u64 = std::env::var("SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    for seed in 0..seeds {
        for (loss, reorder) in [(300_000, 100_000), (200_000, 300_000)] {
            let mut net = Net::new(seed, loss, reorder);
            net.handshake(5_000);
            assert_established(&net);
            transfer(&mut net, seed, 4_000, 4_000, 5_000);
        }
        // 40% loss: the handshake alone (BBRv3's loss response can slow
        // the data phase far beyond the step budget at this rate).
        let mut net = Net::new(seed, 400_000, 200_000);
        net.handshake(5_000);
        assert_established(&net);
    }
}

/// Runs harness bookkeeping without counting its allocations.
fn uncounted<T>(f: impl FnOnce() -> T) -> T {
    let was = COUNTING.with(|c| c.replace(false));
    let out = f();
    COUNTING.with(|c| c.set(was));
    out
}

/// Counts this thread's heap allocations while `COUNTING` is set (other
/// tests in this binary run on other threads and are not counted).
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// "0 Alloc" after the handshake: once a handshake-opened connection is
/// confirmed and warm, the 1-RTT stream echo path (including the
/// per-packet handshake hooks: HANDSHAKE_DONE, 1-RTT CRYPTO and TLS
/// event polling) performs no heap allocation. The handshake itself
/// allocates (rustls, certificate verification) and is not counted.
#[test]
fn steady_state_after_the_handshake_allocates_nothing() {
    // The datagram copies of the test wire are not counted; everything the
    // endpoints and the software NIC do is.
    let mut net = Net::new(7, 0, 0);
    net.handshake(50);
    let s = net.s();
    let id = net.client.open_stream(net.c_conn, false).unwrap();
    let data = [0x5au8; 2048];
    let mut buf = vec![0u8; 8192];
    let mut returned = 0u64;
    let mut round = |net: &mut Net, returned: &mut u64| {
        let n = net.client.stream_write(net.c_conn, id, &data).unwrap();
        assert_eq!(n, data.len(), "the send ring keeps up");
        for _ in 0..4 {
            net.step();
            while net.server.poll_stream_event(s).is_some() {}
            while net.client.poll_stream_event(net.c_conn).is_some() {}
            if net
                .server
                .connection(s)
                .unwrap()
                .streams
                .recv_state(id)
                .is_some()
            {
                let space = net
                    .server
                    .connection(s)
                    .unwrap()
                    .streams
                    .write_space(id)
                    .unwrap();
                let r = net
                    .server
                    .stream_read(s, id, &mut buf[..space.min(8192)])
                    .unwrap();
                let w = net.server.stream_write(s, id, &buf[..r.len]).unwrap();
                assert_eq!(w, r.len);
            }
            let r = net.client.stream_read(net.c_conn, id, &mut buf).unwrap();
            *returned += r.len as u64;
        }
    };
    for _ in 0..100 {
        round(&mut net, &mut returned);
    }
    let before = returned;
    ALLOCS.with(|a| a.set(0));
    COUNTING.with(|c| c.set(true));
    for _ in 0..300 {
        round(&mut net, &mut returned);
    }
    COUNTING.with(|c| c.set(false));
    let allocs = ALLOCS.with(Cell::get);
    assert!(returned - before >= 290 * 2048, "the echo kept flowing");
    assert_established(&net);
    assert_eq!(allocs, 0, "heap allocations in the steady state");
    COUNTING.with(|c| c.set(true));
    drop(std::hint::black_box(Vec::<u8>::with_capacity(8)));
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 1, "the counter counts");
}
