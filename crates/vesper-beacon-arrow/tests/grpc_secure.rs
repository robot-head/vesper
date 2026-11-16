//! gRPC compression, TLS (rustls) and bearer-token auth over real HTTP/2 on
//! 127.0.0.1, with certificates generated per run by `rcgen`.
#![cfg(feature = "grpc")]

use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use vesper_beacon_arrow::grpc::*;
use vesper_beacon_arrow::otap::proto::BatchStatus;
use vesper_beacon_arrow::otap::*;
use vesper_beacon_arrow::otlp;

struct Pki {
    ca_pem: Vec<u8>,
    server: ServerTls,
    client_identity: (Vec<u8>, Vec<u8>),
}
fn pki() -> Pki {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf = |name: &str| {
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec![name.to_owned()])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
    };
    let (cert_pem, key_pem) = leaf("localhost");
    let ca_pem = ca.pem().into_bytes();
    Pki {
        server: ServerTls {
            cert_pem,
            key_pem,
            client_ca_pem: None,
        },
        client_identity: leaf("vesper-client"),
        ca_pem,
    }
}

struct Server {
    endpoint: String,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.await.unwrap();
    }
}
async fn otlp_server(
    config: ServerConfig,
    tls: Option<ServerTls>,
) -> (Server, mpsc::Receiver<OtlpReceived>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = match tls {
        Some(_) => format!("https://localhost:{port}"),
        None => format!("http://127.0.0.1:{port}"),
    };
    let (service, rx) = OtlpService::new(config);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        serve_otlp_tls(listener, service, tls.as_ref(), async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    (
        Server {
            endpoint,
            stop: Some(stop),
            task,
        },
        rx,
    )
}
async fn otap_server(
    config: ServerConfig,
    tls: Option<ServerTls>,
) -> (Server, mpsc::Receiver<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = match tls {
        Some(_) => format!("https://localhost:{port}"),
        None => format!("http://127.0.0.1:{port}"),
    };
    let (service, rx) = OtapService::new(config);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        serve_otap_tls(listener, service, tls.as_ref(), async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    (
        Server {
            endpoint,
            stop: Some(stop),
            task,
        },
        rx,
    )
}

fn logs(text: &'static str) -> Vec<ResourceGroup<LogRecord>> {
    let mut l = LogRecord {
        time_unix_ns: Some(5),
        severity: Severity::INFO,
        ..LogRecord::default()
    };
    l.set_body(text);
    vec![ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        vec![l],
    )]
}
fn token(t: &str) -> BearerToken {
    BearerToken::new(t).unwrap()
}
fn code(e: GrpcError) -> tonic::Code {
    match e {
        GrpcError::Status(s) => s.code(),
        other => panic!("status expected, got {other:?}"),
    }
}

#[test]
fn bearer_tokens_are_validated_and_hidden() {
    for ok in ["abc", "a-b._~+/c==", "Zm9vYmFy"] {
        assert!(BearerToken::new(ok).is_ok(), "{ok}");
    }
    for bad in ["", "==", "a b", "a\r\nX-Evil: 1", "tök", "a=b"] {
        assert!(
            matches!(BearerToken::new(bad), Err(GrpcError::InvalidToken)),
            "{bad:?}"
        );
    }
    assert_eq!(format!("{:?}", token("s3cret")), "BearerToken(<hidden>)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_compression_round_trips_otlp_and_otap() {
    let (server, mut rx) = otlp_server(ServerConfig::default(), None).await;
    for compression in [
        GrpcCompression::None,
        GrpcCompression::Gzip,
        GrpcCompression::Zstd,
    ] {
        let mut client =
            OtlpClient::connect_with(&server.endpoint, &ConnectOptions::default(), compression)
                .await
                .unwrap();
        let request = otlp::logs_request(&logs("compressed"));
        client.export_logs(request.clone()).await.unwrap();
        let OtlpReceived::Logs(got) = rx.recv().await.unwrap() else {
            panic!("logs expected")
        };
        assert_eq!(got, request, "{compression:?}");
    }
    server.stop().await;

    for response_compression in [GrpcCompression::None, GrpcCompression::Zstd] {
        let (server, mut rx) = otap_server(
            ServerConfig {
                response_compression,
                ..ServerConfig::default()
            },
            None,
        )
        .await;
        for compression in [
            GrpcCompression::None,
            GrpcCompression::Gzip,
            GrpcCompression::Zstd,
        ] {
            let config = ClientConfig {
                compression,
                ..ClientConfig::default()
            };
            let mut stream = OtapStream::connect(&server.endpoint, OtapSignal::Logs, config)
                .await
                .unwrap();
            let batch = LogsEncoder::new(EncoderConfig {
                ipc_compression: IpcCompression::Zstd,
                ..EncoderConfig::default()
            })
            .encode(&logs("both layers"))
            .unwrap();
            stream.send(batch).await.unwrap();
            let acks = stream.finish().await.unwrap();
            assert!(acks.iter().all(BatchStatus::is_ok), "{acks:?}");
            let got = rx.recv().await.unwrap();
            assert_eq!(
                got.batch.logs[0].body,
                Some(DecodedValue::Str("both layers".into()))
            );
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_with_bearer_tokens_accepts_the_right_token_only() {
    let pki = pki();
    let config = ServerConfig {
        bearer_tokens: vec![token("old-token"), token("current-token")],
        ..ServerConfig::default()
    };
    let (server, mut rx) = otlp_server(config.clone(), Some(pki.server.clone())).await;
    let tls = ClientTls {
        ca_pem: pki.ca_pem.clone(),
        ..ClientTls::default()
    };
    let with = |t: Option<&str>| ConnectOptions {
        tls: Some(tls.clone()),
        bearer_token: t.map(token),
        allow_insecure_auth: false,
    };
    let request = otlp::logs_request(&logs("secret"));
    for good in ["old-token", "current-token"] {
        let mut client =
            OtlpClient::connect_with(&server.endpoint, &with(Some(good)), GrpcCompression::Zstd)
                .await
                .unwrap();
        client.export_logs(request.clone()).await.unwrap();
        assert!(matches!(rx.recv().await, Some(OtlpReceived::Logs(_))));
    }
    for bad in [Some("current-tokem"), Some("current-token2"), None] {
        let mut client =
            OtlpClient::connect_with(&server.endpoint, &with(bad), GrpcCompression::Zstd)
                .await
                .unwrap();
        let e = client.export_logs(request.clone()).await.unwrap_err();
        assert_eq!(code(e), tonic::Code::Unauthenticated, "{bad:?}");
    }
    assert!(rx.try_recv().is_err(), "rejected exports are not delivered");

    // A client that does not trust the CA cannot connect at all.
    let other = pki_untrusted_options();
    let result = async {
        let mut c =
            OtlpClient::connect_with(&server.endpoint, &other, GrpcCompression::Zstd).await?;
        c.export_logs(request.clone()).await
    }
    .await;
    assert!(result.is_err());
    server.stop().await;

    // The OTAP stream service applies the same check.
    let (server, mut rx) = otap_server(config, Some(pki.server.clone())).await;
    let batch = || {
        LogsEncoder::new(EncoderConfig::default())
            .encode(&logs("otap over tls"))
            .unwrap()
    };
    let connection = Connection::connect(&server.endpoint, &with(Some("current-token")))
        .await
        .unwrap();
    let mut stream = OtapStream::open(connection, OtapSignal::Logs, ClientConfig::default());
    stream.send(batch()).await.unwrap();
    assert!(stream.finish().await.unwrap()[0].is_ok());
    assert_eq!(rx.recv().await.unwrap().batch.logs.len(), 1);
    let connection = Connection::connect(&server.endpoint, &with(Some("nope")))
        .await
        .unwrap();
    let mut stream = OtapStream::open(connection, OtapSignal::Logs, ClientConfig::default());
    let refused = match stream.send(batch()).await {
        Err(e) => Some(e),
        Ok(_) => stream.finish().await.err(),
    };
    assert_eq!(
        code(refused.expect("refused")),
        tonic::Code::Unauthenticated
    );
    assert!(rx.try_recv().is_err());
    server.stop().await;
}

fn pki_untrusted_options() -> ConnectOptions {
    ConnectOptions {
        tls: Some(ClientTls {
            ca_pem: pki().ca_pem,
            ..ClientTls::default()
        }),
        bearer_token: Some(token("current-token")),
        allow_insecure_auth: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutual_tls_requires_a_client_certificate() {
    let pki = pki();
    let mut server_tls = pki.server.clone();
    server_tls.client_ca_pem = Some(pki.ca_pem.clone());
    let (server, mut rx) = otlp_server(ServerConfig::default(), Some(server_tls)).await;
    let request = otlp::logs_request(&logs("mtls"));
    let options = |identity| ConnectOptions {
        tls: Some(ClientTls {
            ca_pem: pki.ca_pem.clone(),
            domain: None,
            identity,
        }),
        ..ConnectOptions::default()
    };
    let mut client = OtlpClient::connect_with(
        &server.endpoint,
        &options(Some(pki.client_identity.clone())),
        GrpcCompression::Zstd,
    )
    .await
    .unwrap();
    client.export_logs(request.clone()).await.unwrap();
    assert!(matches!(rx.recv().await, Some(OtlpReceived::Logs(_))));
    let anonymous = async {
        let mut c =
            OtlpClient::connect_with(&server.endpoint, &options(None), GrpcCompression::Zstd)
                .await?;
        c.export_logs(request.clone()).await
    };
    let anonymous = tokio::time::timeout(Duration::from_secs(20), anonymous)
        .await
        .expect("handshake failure is prompt");
    assert!(anonymous.is_err());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insecure_configurations_are_typed_errors() {
    let plain = ConnectOptions {
        bearer_token: Some(token("t")),
        ..ConnectOptions::default()
    };
    assert!(matches!(
        Connection::connect("http://127.0.0.1:1", &plain).await,
        Err(GrpcError::InsecureAuth)
    ));
    assert!(matches!(
        Connection::connect("https://127.0.0.1:1", &ConnectOptions::default()).await,
        Err(GrpcError::TlsMismatch)
    ));
    let tls = ConnectOptions {
        tls: Some(ClientTls::default()),
        ..ConnectOptions::default()
    };
    assert!(matches!(
        Connection::connect("http://127.0.0.1:1", &tls).await,
        Err(GrpcError::TlsMismatch)
    ));

    // Loopback tests may opt in to tokens over plaintext.
    let (server, mut rx) = otlp_server(
        ServerConfig {
            bearer_tokens: vec![token("t")],
            ..ServerConfig::default()
        },
        None,
    )
    .await;
    let opt_in = ConnectOptions {
        allow_insecure_auth: true,
        ..plain
    };
    let mut client = OtlpClient::connect_with(&server.endpoint, &opt_in, GrpcCompression::Gzip)
        .await
        .unwrap();
    client
        .export_logs(otlp::logs_request(&logs("loopback")))
        .await
        .unwrap();
    assert!(rx.recv().await.is_some());
    server.stop().await;
}
