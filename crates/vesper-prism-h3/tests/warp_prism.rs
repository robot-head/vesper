//! End to end: a Prism router served over HTTP/3 on a Warp endpoint, with
//! a TLS 1.3 handshake (ALPN "h3") over the software NIC, answering the
//! same handler that answers HTTP/1.1 through `vesper_prism::connection`.

use std::sync::Arc;
use std::time::Duration;

use vesper_prism::connection::{
    Connection, ConnectionLimits, RequestEvent, Response as PrismResponse,
};
use vesper_prism::router::{RouteMatch, Router, RouterLimits};
use vesper_prism::service::{Flow, Middleware, ParsedRequest, Service};
use vesper_prism::target::TargetLimits;
use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest, ClientResponse};
use vesper_prism_h3::connection::{H3Config, ServerConnection};
use vesper_prism_h3::prism::{self, answer, serve};
use vesper_prism_h3::qpack::Field;
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
/// 2030-01-01, inside the fixtures' validity.
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
        out.fill(0x33);
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

fn endpoint(role: Role) -> Ep {
    let cfg = EndpointConfig {
        role,
        initial_rtt_ns: 20 * MS,
        streams: StreamConfig {
            stream_window: 64 * 1024,
            send_buffer: 64 * 1024,
            conn_window: 512 * 1024,
            max_streams_bidi: 16,
            max_streams_uni: 4,
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

impl Net {
    fn new(seed: u64, loss_ppm: u64) -> Self {
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
        let odcid = [0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
        let c_conn = client
            .connect(&cs, "warp.test", b"client01", &odcid, 0)
            .unwrap();
        Self {
            client,
            server,
            c_conn,
            s_conn: None,
            c_peers: PeerStreams::default(),
            s_peers: PeerStreams::default(),
            h3c: ClientConnection::new(H3Config::default()),
            h3s: ServerConnection::new(H3Config::default()),
            now: 0,
            rng: Rng(seed),
            loss_ppm,
        }
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

    /// One network step: deliver, receive, run TLS/streams, send.
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
            self.now = self.now.max(w);
        }
    }

    fn client_t(&mut self) -> WarpTransport<'_, SoftNic, Discard, Wire> {
        WarpTransport {
            endpoint: &mut self.client,
            conn: self.c_conn,
            peers: &mut self.c_peers,
            role: Role::Client,
            now_ns: self.now,
        }
    }

    fn established(&self) -> bool {
        self.client.handshake_phase(self.c_conn) == Some(HandshakePhase::Confirmed)
    }

    /// Runs HTTP/3 on both sides and the network, serving `service`.
    fn step(&mut self, service: &Service<&'static str>) -> prism::ServeReport {
        let mut report = prism::ServeReport::default();
        if let Some(s) = self.s_conn {
            let mut t = WarpTransport {
                endpoint: &mut self.server,
                conn: s,
                peers: &mut self.s_peers,
                role: Role::Server,
                now_ns: self.now,
            };
            report = serve(
                &mut self.h3s,
                &mut t,
                service,
                &handler,
                TargetLimits::default(),
            );
        }
        let mut h3c = std::mem::replace(&mut self.h3c, ClientConnection::new(H3Config::default()));
        h3c.poll(&mut self.client_t());
        self.h3c = h3c;
        self.quic();
        report
    }

    fn send(&mut self, req: &ClientRequest) -> u64 {
        let mut h3c = std::mem::replace(&mut self.h3c, ClientConnection::new(H3Config::default()));
        let id = h3c.send_request(&mut self.client_t(), req).unwrap();
        self.h3c = h3c;
        id
    }

    fn handshake(&mut self, service: &Service<&'static str>) {
        for _ in 0..200 {
            if self.established() && self.s_conn.is_some() {
                // Let SETTINGS cross.
                for _ in 0..10 {
                    self.step(service);
                }
                return;
            }
            self.step(service);
        }
        panic!("no handshake");
    }

    fn responses(
        &mut self,
        service: &Service<&'static str>,
        n: usize,
        max_steps: usize,
    ) -> Vec<ClientResponse> {
        let mut out = Vec::new();
        for _ in 0..max_steps {
            self.step(service);
            while let Some(e) = self.h3c.poll_event() {
                match e {
                    ClientEvent::Response(r) => out.push(r),
                    e => panic!("unexpected {e:?}"),
                }
            }
            if out.len() == n {
                return out;
            }
        }
        panic!(
            "{} of {n} responses; client {:?} server {:?}",
            out.len(),
            self.h3c.error(),
            self.h3s.error()
        );
    }
}

/// Requires `x-api-key: secret` under /admin.
struct AdminGuard;
impl Middleware for AdminGuard {
    fn inspect(&self, request: &ParsedRequest) -> Flow {
        let admin = request.target().origin().is_some_and(|o| {
            o.segments()
                .first()
                .is_some_and(|s| s.as_bytes() == b"admin")
        });
        let ok = request
            .request()
            .head
            .headers("x-api-key")
            .any(|v| v == b"secret");
        if admin && !ok {
            Flow::Respond(PrismResponse::new(401, b"unauthorized".to_vec()))
        } else {
            Flow::Continue
        }
    }
}

fn service() -> Service<&'static str> {
    let mut router = Router::new(RouterLimits::default()).unwrap();
    router.insert("GET", "/", "index").unwrap();
    router.insert("GET", "/users/:id", "user").unwrap();
    router.insert("POST", "/echo", "echo").unwrap();
    router.insert("GET", "/admin/stats", "stats").unwrap();
    let mut s = Service::new(router, 4);
    s.push(Box::new(AdminGuard)).unwrap();
    s
}

/// The one handler, for HTTP/1.1 and HTTP/3 alike.
fn handler(req: &ParsedRequest, route: RouteMatch<'_, &'static str>) -> PrismResponse {
    match *route.value {
        "index" => {
            PrismResponse::new(200, b"prism index".to_vec()).header("Content-Type", b"text/plain")
        }
        "user" => {
            let id = route.param("id").and_then(|s| s.as_str()).unwrap_or("?");
            PrismResponse::new(200, format!("{{\"user\":\"{id}\"}}").into_bytes())
                .header("Content-Type", b"application/json")
        }
        "echo" => PrismResponse::new(200, req.request().body.clone()),
        "stats" => PrismResponse::new(200, b"42".to_vec()),
        _ => PrismResponse::new(500, Vec::new()),
    }
}

/// The same request through Prism's HTTP/1.1 connection codec.
fn http1(
    service: &Service<&'static str>,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, Vec<u8>) {
    let mut conn = Connection::new(ConnectionLimits::default()).unwrap();
    let mut wire = format!("{method} {path} HTTP/1.1\r\nHost: prism.test\r\n");
    for (n, v) in headers {
        wire.push_str(&format!("{n}: {v}\r\n"));
    }
    if !body.is_empty() {
        wire.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    wire.push_str("\r\n");
    let mut bytes = wire.into_bytes();
    bytes.extend_from_slice(body);
    conn.receive(&bytes).unwrap();
    let RequestEvent::Request(inbound) = conn.poll_request() else {
        panic!("HTTP/1.1 request")
    };
    let response = answer(service, &handler, inbound.request, TargetLimits::default());
    conn.submit(inbound.id, response).unwrap();
    let out = conn.pending_output().to_vec();
    let text = String::from_utf8_lossy(&out);
    let status: u16 = text[9..12].parse().unwrap();
    let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let body = if method == "HEAD" {
        Vec::new()
    } else {
        out[split..].to_vec()
    };
    (status, body)
}

fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> ClientRequest {
    ClientRequest {
        method: method.into(),
        scheme: "https".into(),
        authority: "prism.test".into(),
        path: path.into(),
        fields: headers.iter().map(|(n, v)| Field::new(*n, *v)).collect(),
        body: body.to_vec(),
    }
}

/// (method, path, fields, body).
type Case = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [u8],
);

const CASES: &[Case] = &[
    ("GET", "/", &[], b""),
    ("GET", "/users/ada%20lovelace", &[], b""),
    (
        "POST",
        "/echo",
        &[("content-type", "application/octet-stream")],
        b"\x00\x01binary body\xff",
    ),
    ("GET", "/admin/stats", &[], b""),
    ("GET", "/admin/stats", &[("x-api-key", "secret")], b""),
    ("DELETE", "/users/7", &[], b""),
    ("GET", "/missing", &[], b""),
    ("GET", "/users/%2e%2e", &[], b""),
    ("HEAD", "/", &[], b""),
];

#[test]
fn same_handler_answers_http1_and_http3_over_warp() {
    let service = service();
    let mut net = Net::new(7, 0);
    net.handshake(&service);
    assert_eq!(
        net.client
            .connection(net.c_conn)
            .unwrap()
            .tls
            .unwrap()
            .alpn_protocol(),
        Some(ALPN)
    );
    assert!(net.h3c.peer_settings().is_some(), "server SETTINGS arrived");
    let mut ids = Vec::new();
    for (m, p, h, b) in CASES {
        ids.push(net.send(&request(m, p, h, b)));
    }
    let mut got = net.responses(&service, CASES.len(), 2_000);
    got.sort_by_key(|r| r.stream_id);
    for ((m, p, h, b), r) in CASES.iter().zip(&got) {
        let (status, body) = http1(&service, m, p, h, b);
        assert_eq!(r.head.status, status, "{m} {p}");
        assert_eq!(r.body, body, "{m} {p}");
    }
    let statuses: Vec<u16> = got.iter().map(|r| r.head.status).collect();
    assert_eq!(statuses, [200, 200, 200, 401, 200, 405, 404, 400, 200]);
    assert_eq!(got[1].body, b"{\"user\":\"ada lovelace\"}");
    let allow = got[5]
        .head
        .fields
        .iter()
        .find(|f| f.name == b"allow")
        .expect("Allow");
    assert_eq!(allow.value, b"GET, HEAD");
    assert!(
        got[8]
            .head
            .fields
            .contains(&Field::new("content-length", "11"))
    );
    assert_eq!(net.h3s.stats().requests, CASES.len() as u64);
    assert!(net.h3s.error().is_none() && net.h3c.error().is_none());
    // The server's QPACK encoder used the dynamic table across responses.
    assert!(net.h3s.encoder().stats().inserts > 0);
}

#[test]
fn http3_over_a_lossy_link() {
    let service = service();
    let mut net = Net::new(11, 50_000);
    net.handshake(&service);
    let body: Vec<u8> = (0..60_000u32).map(|i| (i * 7) as u8).collect();
    for i in 0..6 {
        net.send(&request(
            "POST",
            "/echo",
            &[("x-seq", &i.to_string())],
            &body,
        ));
    }
    let got = net.responses(&service, 6, 20_000);
    for r in got {
        assert_eq!(r.head.status, 200);
        assert_eq!(r.body, body);
    }
    assert!(net.h3s.error().is_none() && net.h3c.error().is_none());
}
