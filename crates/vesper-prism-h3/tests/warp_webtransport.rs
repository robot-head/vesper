//! Loopback over Warp (TLS 1.3, ALPN "h3", the software NIC): a
//! WebTransport session with streams under QUIC flow control, HTTP
//! Datagrams (as DATAGRAM capsules: Warp has no QUIC DATAGRAM frames, so
//! SETTINGS_H3_DATAGRAM is not sent), a streaming request body held back
//! by the server's window, and priorities on a lossy link.

use std::sync::Arc;
use std::time::Duration;

use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{Event, H3Config, Response, ServerConnection, Session};
use vesper_prism_h3::qpack::Field;
use vesper_prism_h3::session::{DatagramPath, SessionEvent};
use vesper_prism_h3::warp::{PeerStreams, WarpTransport};
use vesper_warp::endpoint::{Discard, EndpointConfig, RxOutcome, TxTap, WarpEndpoint};
use vesper_warp::handshake::tls::FixedTime;
use vesper_warp::handshake::tls::rustls::RootCertStore;
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use vesper_warp::handshake::{
    CidSource, ClientSetup, ConnectionEvent, HandshakePhase, Listener, ServerSetup,
};
use vesper_warp::stream::{Role, StreamConfig};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

const MS: u64 = 1_000_000;
const ALPN: &[u8] = b"h3";
const TEST_CA: &[u8] = include_bytes!("fixtures/test-ca.der");
const SERVER_CERT: &[u8] = include_bytes!("fixtures/server.der");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/server.key.der");
const NOW_SECS: u64 = 1_893_456_000;

#[derive(Default)]
struct Wire(Vec<Vec<u8>>);

impl TxTap for Wire {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, datagram: &[u8]) {
        self.0.push(datagram.to_vec());
    }
}

type Ep = WarpEndpoint<SoftNic, Discard, Wire>;

struct Cids(u8);
impl CidSource for Cids {
    fn next_cid(&mut self, out: &mut [u8]) {
        self.0 += 1;
        out.fill(0x44);
        out[0] = self.0;
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Small QUIC windows, so flow control is what holds senders back.
fn endpoint(role: Role) -> Ep {
    let cfg = EndpointConfig {
        role,
        initial_rtt_ns: 20 * MS,
        streams: StreamConfig {
            stream_window: 32 * 1024,
            send_buffer: 32 * 1024,
            conn_window: 256 * 1024,
            max_streams_bidi: 16,
            max_streams_uni: 8,
            ..StreamConfig::default()
        },
        ..EndpointConfig::default()
    };
    let xsk = SoftNic::xsk(UmemConfig::with_frames(2048), SoftNicMode::Scripted).unwrap();
    WarpEndpoint::new(cfg, xsk, Discard::default(), Wire::default()).unwrap()
}

struct Net {
    client: Ep,
    server: Ep,
    c_conn: SlotHandle,
    s_conn: Option<SlotHandle>,
    c_peers: PeerStreams,
    s_peers: PeerStreams,
    h3c: ClientConnection,
    h3s: ServerConnection,
    now: u64,
    rng: Rng,
    loss_ppm: u64,
}

macro_rules! ct {
    ($n:expr) => {
        &mut WarpTransport {
            endpoint: &mut $n.client,
            conn: $n.c_conn,
            peers: &mut $n.c_peers,
            role: Role::Client,
            now_ns: $n.now,
        }
    };
}

macro_rules! st {
    ($n:expr) => {
        &mut WarpTransport {
            endpoint: &mut $n.server,
            conn: $n.s_conn.expect("accepted"),
            peers: &mut $n.s_peers,
            role: Role::Server,
            now_ns: $n.now,
        }
    };
}

impl Net {
    fn new(seed: u64, loss_ppm: u64, ccfg: H3Config, scfg: H3Config) -> Self {
        let mut server = endpoint(Role::Server);
        let setup = ServerSetup::new(
            vec![CertificateDer::from(SERVER_CERT.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec())),
            vec![ALPN.to_vec()],
        )
        .unwrap();
        server
            .listen(Listener::with_cids(setup, Box::new(Cids(0))))
            .unwrap();
        let mut client = endpoint(Role::Client);
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(TEST_CA.to_vec())).unwrap();
        let time = Arc::new(FixedTime(UnixTime::since_unix_epoch(Duration::from_secs(
            NOW_SECS,
        ))));
        let cs = ClientSetup::with_time(roots, vec![ALPN.to_vec()], time).unwrap();
        let odcid = [0x84, 0x95, 0xc9, 0xf1, 0x3f, 0x52, 0x58, 0x09];
        let c_conn = client
            .connect(&cs, "warp.test", b"client02", &odcid, 0)
            .unwrap();
        let mut n = Self {
            client,
            server,
            c_conn,
            s_conn: None,
            c_peers: PeerStreams::default(),
            s_peers: PeerStreams::default(),
            h3c: ClientConnection::new(ccfg),
            h3s: ServerConnection::new(scfg),
            now: 0,
            rng: Rng(seed),
            loss_ppm,
        };
        for _ in 0..300 {
            n.step();
            if n.client.handshake_phase(n.c_conn) == Some(HandshakePhase::Confirmed)
                && n.s_conn.is_some()
                && n.h3c.peer_settings().is_some()
                && n.h3s.peer_settings().is_some()
            {
                return n;
            }
        }
        panic!("no HTTP/3 connection");
    }

    fn wire(&mut self) {
        let up = std::mem::take(&mut self.client.tap_mut().0);
        let down = std::mem::take(&mut self.server.tap_mut().0);
        for d in up {
            if self.rng.next() % 1_000_000 >= self.loss_ppm {
                self.server.xsk_mut().backend_mut().deliver(&d);
            }
        }
        for d in down {
            if self.rng.next() % 1_000_000 >= self.loss_ppm {
                self.client.xsk_mut().backend_mut().deliver(&d);
            }
        }
    }

    fn quic(&mut self) {
        self.wire();
        for _ in 0..8192 {
            let a = self.server.poll_and_step_quic_rx_one(self.now).unwrap();
            let b = self.client.poll_and_step_quic_rx_one(self.now).unwrap();
            if a == RxOutcome::Idle && b == RxOutcome::Idle {
                break;
            }
        }
        self.client.xsk_mut().backend_mut().step();
        self.server.xsk_mut().backend_mut().step();
        self.client.poll(self.now).unwrap();
        self.server.poll(self.now).unwrap();
        while self.client.poll_connection_event().is_some() {}
        while let Some(e) = self.server.poll_connection_event() {
            if let ConnectionEvent::Accepted(h) = e {
                self.s_conn = Some(h);
            }
        }
        self.now += 2 * MS;
        if self.client.tap().0.is_empty()
            && self.server.tap().0.is_empty()
            && let Some(w) = [self.client.next_wake_ns(), self.server.next_wake_ns()]
                .into_iter()
                .flatten()
                .min()
        {
            self.now = self.now.max(w.min(self.now + 100 * MS));
        }
    }

    fn step(&mut self) {
        if self.s_conn.is_some() {
            self.h3s.poll(st!(self));
        }
        self.h3c.poll(ct!(self));
        self.quic();
    }

    fn server_event(&mut self, max: usize) -> Event {
        for _ in 0..max {
            if let Some(e) = self.h3s.poll_event() {
                return e;
            }
            self.step();
        }
        panic!(
            "no server event; client {:?} server {:?}",
            self.h3c.error(),
            self.h3s.error()
        );
    }

    fn client_event(&mut self, max: usize) -> ClientEvent {
        for _ in 0..max {
            if let Some(e) = self.h3c.poll_event() {
                return e;
            }
            self.step();
        }
        panic!(
            "no client event; client {:?} server {:?}",
            self.h3c.error(),
            self.h3s.error()
        );
    }

    fn session(&mut self) -> (u64, Session) {
        let sid = self
            .h3c
            .open_session(
                ct!(self),
                &ClientRequest::connect("prism.test", "/chat", "webtransport"),
            )
            .unwrap();
        let Event::Connect(c) = self.server_event(200) else {
            panic!()
        };
        assert_eq!(c.stream_id, sid);
        let session = c
            .responder
            .accept_session(&mut self.h3s, st!(self), Vec::new())
            .unwrap();
        let ClientEvent::SessionEstablished { session_id, head } = self.client_event(200) else {
            panic!()
        };
        assert_eq!((session_id, head.status), (sid, 200));
        (sid, session)
    }
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn webtransport_session_over_warp() {
    let wt = || H3Config::default().with_webtransport(2);
    let mut n = Net::new(7, 0, wt(), wt());
    // Warp has no QUIC DATAGRAM frames: H3_DATAGRAM is not offered.
    assert!(!n.h3s.sent_settings().h3_datagram);
    assert_eq!(
        n.h3c.peer_settings().unwrap().webtransport_max_sessions,
        Some(2)
    );
    let (sid, session) = n.session();

    // 300 KB up a bidirectional stream, through 32 KB stream windows:
    // writes are partial while the server is not reading.
    let up = pattern(300_000, 1);
    let s = n.h3c.open_stream(ct!(n), sid, true).unwrap();
    let mut sent = 0;
    let mut partial = false;
    let mut got = Vec::new();
    let mut accepted = false;
    let mut buf = vec![0u8; 8192];
    for step in 0..5000 {
        if sent < up.len() {
            let w = n.h3c.write_stream(ct!(n), s, &up[sent..]).unwrap();
            partial |= w < up.len() - sent;
            sent += w;
            if sent == up.len() {
                n.h3c.finish_stream(ct!(n), s).unwrap();
            }
        }
        while let Some(e) = n.h3s.poll_event() {
            match e {
                Event::Session(SessionEvent::Stream {
                    stream_id, bidi, ..
                }) => {
                    assert!(bidi && stream_id == s);
                    accepted = true;
                }
                e => panic!("unexpected {e:?}"),
            }
        }
        // The server reads slowly at first.
        if accepted && step % 3 == 0 {
            loop {
                let r = n.h3s.read_stream(st!(n), s, &mut buf).unwrap();
                got.extend_from_slice(&buf[..r.len]);
                if r.len == 0 || r.fin {
                    break;
                }
            }
        }
        if got.len() == up.len() {
            break;
        }
        n.step();
    }
    assert!(partial, "flow control never pushed back");
    assert_eq!(got.len(), up.len());
    assert!(got == up);

    // Echo back on the same stream and a server uni stream.
    let down = pattern(50_000, 9);
    let mut off = 0;
    for _ in 0..2000 {
        if off < down.len() {
            off += n.h3s.write_stream(st!(n), s, &down[off..]).unwrap();
            if off == down.len() {
                n.h3s.finish_stream(st!(n), s).unwrap();
            }
        } else {
            break;
        }
        n.step();
    }
    let u = n.h3s.open_stream(st!(n), &session, false).unwrap();
    n.h3s.write_stream(st!(n), u, b"server uni").unwrap();
    n.h3s.finish_stream(st!(n), u).unwrap();
    // Datagrams both ways (capsules on the CONNECT stream).
    assert_eq!(
        n.h3c.send_datagram(ct!(n), sid, b"hello dgram").unwrap(),
        DatagramPath::Capsule
    );
    assert_eq!(
        n.h3s
            .send_datagram(st!(n), &session, b"dgram back")
            .unwrap(),
        DatagramPath::Capsule
    );
    let mut back = Vec::new();
    let mut uni_data = Vec::new();
    let mut client_dgram = false;
    let mut server_dgram = false;
    let mut uni_fin = false;
    let mut back_fin = false;
    for _ in 0..3000 {
        n.step();
        while let Some(e) = n.h3c.poll_event() {
            match e {
                ClientEvent::Session(SessionEvent::Datagram { payload, .. }) => {
                    assert_eq!(payload, b"dgram back");
                    client_dgram = true;
                }
                ClientEvent::Session(SessionEvent::Stream {
                    stream_id, bidi, ..
                }) => {
                    assert!(!bidi && stream_id == u);
                }
                e => panic!("unexpected {e:?}"),
            }
        }
        while let Some(e) = n.h3s.poll_event() {
            match e {
                Event::Session(SessionEvent::Datagram { payload, .. }) => {
                    assert_eq!(payload, b"hello dgram");
                    server_dgram = true;
                }
                e => panic!("unexpected {e:?}"),
            }
        }
        if !back_fin {
            let r = n.h3c.read_stream(ct!(n), s, &mut buf).unwrap();
            back.extend_from_slice(&buf[..r.len]);
            back_fin = r.fin;
        }
        if !uni_fin && let Ok(r) = n.h3c.read_stream(ct!(n), u, &mut buf) {
            uni_data.extend_from_slice(&buf[..r.len]);
            uni_fin = r.fin;
        }
        if back_fin && uni_fin && client_dgram && server_dgram {
            break;
        }
    }
    assert!(back_fin && uni_fin && client_dgram && server_dgram);
    assert!(back == down);
    assert_eq!(uni_data, b"server uni");

    // The server closes; the client learns the code and reason.
    let _ = session
        .close(&mut n.h3s, st!(n), 42, b"session over")
        .unwrap();
    let ClientEvent::Session(SessionEvent::Closed {
        session,
        code,
        reason,
        reset,
    }) = n.client_event(500)
    else {
        panic!()
    };
    assert_eq!(
        (session, code, reason.as_slice(), reset),
        (sid, 42, &b"session over"[..], None)
    );
    for _ in 0..50 {
        n.step();
    }
    assert!(n.h3c.error().is_none() && n.h3s.error().is_none());
    assert_eq!(n.h3s.open_requests(), 0);
}

#[test]
fn streaming_upload_is_held_back_by_the_server_window_over_warp() {
    let window = 16 * 1024;
    let scfg = H3Config {
        streaming_request_bodies: true,
        stream_body_window: window,
        ..H3Config::default()
    };
    // 5% loss in both directions.
    let mut n = Net::new(3, 50_000, H3Config::default(), scfg);
    let total = 400_000;
    let data = pattern(total, 5);
    let mut req = ClientRequest::get("prism.test", "/upload");
    req.method = "POST".into();
    req.fields
        .push(Field::new("content-length", total.to_string()));
    let id = n.h3c.send_request_streaming(ct!(n), &req).unwrap();
    let mut off = 0;
    let mut responder = None;
    let mut got = Vec::new();
    let mut max_held = 0;
    let mut max_client_queue = 0;
    let mut buf = vec![0u8; 4096];
    for step in 0..20_000 {
        // The client offers its body as fast as the send budget allows.
        while off < total {
            let end = (off + 8192).min(total);
            match n.h3c.send_body(ct!(n), id, &data[off..end]) {
                Ok(()) => off = end,
                Err(_) => break,
            }
            if off == total {
                n.h3c.finish_body(ct!(n), id).unwrap();
            }
        }
        max_client_queue = max_client_queue.max(n.h3c.queued(id));
        while let Some(e) = n.h3s.poll_event() {
            match e {
                Event::StreamingRequest(r) => responder = Some(r.responder),
                Event::BodyReadable { .. } => {}
                e => panic!("unexpected {e:?}"),
            }
        }
        max_held = max_held.max(n.h3s.buffered_request_bytes());
        // The application reads one 4 KB chunk every fourth step.
        if responder.is_some() && step % 4 == 0 {
            let r = n.h3s.read_body(st!(n), id, &mut buf).unwrap();
            got.extend_from_slice(&buf[..r.len]);
            if r.end {
                break;
            }
        }
        n.step();
    }
    assert_eq!(got.len(), total);
    assert!(got == data);
    // The server never held more than its window (plus one frame header);
    // the client's unsent bytes piled up instead.
    assert!(max_held <= window + 16, "{max_held}");
    assert!(max_client_queue > 100_000, "{max_client_queue}");
    let _ = responder
        .unwrap()
        .respond(
            &mut n.h3s,
            st!(n),
            Response {
                status: 204,
                fields: Vec::new(),
                body: Vec::new(),
            },
        )
        .unwrap();
    let ClientEvent::Response(r) = n.client_event(2000) else {
        panic!()
    };
    assert_eq!(r.head.status, 204);
}

#[test]
fn urgent_responses_finish_first_over_warp() {
    let mut n = Net::new(11, 0, H3Config::default(), H3Config::default());
    let mut ids = Vec::new();
    for (path, prio) in [("/bulk", "u=6"), ("/page", "u=0")] {
        let mut r = ClientRequest::get("prism.test", path);
        r.fields.push(Field::new("priority", prio));
        ids.push(n.h3c.send_request(ct!(n), &r).unwrap());
    }
    let mut reqs = Vec::new();
    while reqs.len() < 2 {
        match n.server_event(200) {
            Event::Request(r) => reqs.push(*r),
            e => panic!("unexpected {e:?}"),
        }
    }
    // Both answered at once with 150 KB: more than Warp takes at once.
    for r in reqs {
        let _ = r
            .responder
            .respond(
                &mut n.h3s,
                st!(n),
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: pattern(150_000, 2),
                },
            )
            .unwrap();
    }
    let mut order = Vec::new();
    while order.len() < 2 {
        match n.client_event(5000) {
            ClientEvent::Response(r) => {
                assert!(r.body == pattern(150_000, 2));
                order.push(r.stream_id);
            }
            e => panic!("unexpected {e:?}"),
        }
    }
    assert_eq!(order, [ids[1], ids[0]]);
}
