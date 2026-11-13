//! Interop probe: the `h3` crate client over `quinn` on loopback UDP,
//! against a Prism router served by vesper-prism-h3 on a Warp endpoint whose
//! software NIC is bridged to a real UDP socket.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Buf;
use vesper_prism::connection::Response as PrismResponse;
use vesper_prism::router::{RouteMatch, Router, RouterLimits};
use vesper_prism::service::{ParsedRequest, Service};
use vesper_prism::target::TargetLimits;
use vesper_prism_h3::connection::{H3Config, ServerConnection};
use vesper_prism_h3::prism::serve;
use vesper_prism_h3::warp::{PeerStreams, WarpTransport};
use vesper_warp::endpoint::{Discard, EndpointConfig, RxOutcome, TxTap, WarpEndpoint};
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer,
};
use vesper_warp::handshake::{ConnectionEvent, Listener, ServerSetup};
use vesper_warp::stream::{Role, StreamConfig};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures");

#[derive(Default)]
struct Wire(Vec<Vec<u8>>);
impl TxTap for Wire {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, d: &[u8]) {
        self.0.push(d.to_vec());
    }
}

fn handler(_req: &ParsedRequest, route: RouteMatch<'_, &'static str>) -> PrismResponse {
    match *route.value {
        "user" => {
            let id = route.param("id").and_then(|s| s.as_str()).unwrap_or("?");
            PrismResponse::new(200, format!("{{\"user\":\"{id}\"}}").into_bytes())
                .header("Content-Type", b"application/json")
        }
        _ => PrismResponse::new(200, b"prism index".to_vec()),
    }
}

fn server(sock: UdpSocket, deadline: Instant) {
    let mut router = Router::new(RouterLimits::default()).unwrap();
    router.insert("GET", "/", "index").unwrap();
    router.insert("GET", "/users/:id", "user").unwrap();
    let service = Service::new(router, 0);
    let cfg = EndpointConfig {
        role: Role::Server,
        streams: StreamConfig {
            max_streams_bidi: 100,
            max_streams_uni: 4,
            ..StreamConfig::default()
        },
        ..EndpointConfig::default()
    };
    let xsk = SoftNic::xsk(UmemConfig::with_frames(2048), SoftNicMode::Scripted).unwrap();
    let mut ep: WarpEndpoint<SoftNic, Discard, Wire> =
        WarpEndpoint::new(cfg, xsk, Discard::default(), Wire::default()).unwrap();
    let setup = ServerSetup::new(
        vec![CertificateDer::from(
            std::fs::read(format!("{FIX}/server.der")).unwrap(),
        )],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            std::fs::read(format!("{FIX}/server.key.der")).unwrap(),
        )),
        vec![b"h3".to_vec()],
    )
    .unwrap();
    ep.listen(Listener::new(setup)).unwrap();
    sock.set_nonblocking(true).unwrap();
    let start = Instant::now();
    let mut peer: Option<SocketAddr> = None;
    let mut conn: Option<SlotHandle> = None;
    let mut peers = PeerStreams::default();
    let mut h3 = ServerConnection::new(H3Config::default());
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        let now = start.elapsed().as_nanos() as u64;
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            peer = Some(from);
            eprintln!("server rx {n} bytes, first {:02x}", buf[0]);
            ep.xsk_mut().backend_mut().deliver(&buf[..n]);
        }
        for _ in 0..1024 {
            match ep.poll_and_step_quic_rx_one(now) {
                Ok(RxOutcome::Idle) => break,
                Ok(o) => eprintln!("server rx outcome {o:?}"),
                Err(e) => {
                    eprintln!("server rx error {e:?}");
                    break;
                }
            }
        }
        ep.xsk_mut().backend_mut().step();
        if let Err(e) = ep.poll(now) {
            eprintln!("server poll error {e:?}");
        }
        while let Some(e) = ep.poll_connection_event() {
            eprintln!("server event {e:?}");
            if let ConnectionEvent::Accepted(h) = e {
                conn = Some(h);
            }
        }
        if let Some(c) = conn {
            if let Some(err) = ep.connection_error(c) {
                eprintln!("connection error {err:?}");
            }
            let mut t = WarpTransport {
                endpoint: &mut ep,
                conn: c,
                peers: &mut peers,
                role: Role::Server,
                now_ns: now,
            };
            let r = serve(&mut h3, &mut t, &service, &handler, TargetLimits::default());
            if r.answered > 0 || r.failed_requests > 0 {
                eprintln!("server report {r:?}");
            }
            if let Some(e) = h3.error() {
                eprintln!("h3 error {e:?}");
            }
        }
        for d in std::mem::take(&mut ep.tap_mut().0) {
            if let Some(p) = peer {
                let _ = sock.send_to(&d, p);
            }
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    eprintln!("server stats {:?}", ep.stats());
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let th = std::thread::spawn(move || server(sock, deadline));

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            std::fs::read(format!("{FIX}/test-ca.der")).unwrap(),
        ))
        .unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let qc = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut cc = quinn::ClientConfig::new(Arc::new(qc));
    let mut tc = quinn::TransportConfig::default();
    tc.mtu_discovery_config(None);
    cc.transport_config(Arc::new(tc));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(cc);
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        let conn = endpoint
            .connect(addr, "warp.test")
            .unwrap()
            .await
            .map_err(|e| format!("connect: {e}"))?;
        println!("QUIC established with Warp: {:?}", conn.remote_address());
        let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(conn))
            .await
            .map_err(|e| format!("h3 client: {e}"))?;
        tokio::spawn(async move {
            let e = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
            eprintln!("client driver closed: {e}");
        });
        let mut out = Vec::new();
        for path in ["/users/7", "/", "/users/ada%20lovelace"] {
            let req = http::Request::get(format!("https://warp.test{path}"))
                .body(())
                .unwrap();
            let mut stream = send
                .send_request(req)
                .await
                .map_err(|e| format!("send: {e}"))?;
            stream.finish().await.map_err(|e| format!("finish: {e}"))?;
            let resp = stream
                .recv_response()
                .await
                .map_err(|e| format!("recv: {e}"))?;
            let mut body = Vec::new();
            while let Some(mut chunk) =
                stream.recv_data().await.map_err(|e| format!("data: {e}"))?
            {
                while chunk.has_remaining() {
                    let c = chunk.chunk().to_vec();
                    chunk.advance(c.len());
                    body.extend(c);
                }
            }
            out.push((
                path,
                resp.status(),
                resp.headers().clone(),
                String::from_utf8_lossy(&body).to_string(),
            ));
        }
        Ok::<_, String>(out)
    })
    .await;
    match result {
        Ok(Ok(out)) => {
            for o in out {
                println!("INTEROP OK {o:?}");
            }
        }
        Ok(Err(e)) => println!("INTEROP FAILED: {e}"),
        Err(_) => println!("INTEROP FAILED: timeout"),
    }
    th.join().unwrap();
}
