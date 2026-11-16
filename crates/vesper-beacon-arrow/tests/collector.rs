//! Interoperability with the OpenTelemetry Collector (contrib distribution,
//! which bundles the `otelarrow` receiver and exporter).
//!
//! Gated on `VESPER_OTELCOL`: the path of an `otelcol-contrib` binary. When
//! it is unset the tests print a note and pass without running anything, so
//! CI does not need the collector. When it is set they start the collector
//! with a generated config on loopback ports and check, through its `file`
//! exporter's OTLP-JSON output:
//!
//! 1. Beacon OTAP traces, metrics and logs → collector `otelarrow` receiver
//!    (the upstream Go decoder) → file.
//! 2. Beacon OTLP/gRPC traces, metrics and logs → collector `otlp` receiver
//!    → file.
//! 3. OTLP into the collector → collector `otelarrow` exporter (the upstream
//!    Go encoder) → Beacon's OTAP gRPC server and decoder.
//! 4. Both directions again over TLS with `bearertokenauth` on every hop.
//!
//! Every hop runs with default compression: Beacon's clients send zstd, the
//! collector's `otelarrow` exporter sends `zstdarrow` gRPC compression, and
//! one Beacon stream also compresses its Arrow IPC buffers. The signals
//! cover gauges, sums, histograms with exemplars, exponential histograms,
//! and map, slice and bytes attribute values.
//!
//! Record the collector version (`otelcol-contrib --version`) with any
//! result; the report names the version this was run against.
#![cfg(feature = "grpc")]

use core::num::NonZeroU64;
use std::net::{TcpListener as StdListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use vesper_beacon::*;
use vesper_beacon_arrow::grpc::*;
use vesper_beacon_arrow::otap::*;
use vesper_beacon_arrow::otlp;

fn collector() -> Option<PathBuf> {
    match std::env::var_os("VESPER_OTELCOL") {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => {
            eprintln!("VESPER_OTELCOL unset: collector interop skipped");
            None
        }
    }
}
fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn wait_listening(port: u16, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("collector exited ({status})");
        }
        assert!(
            Instant::now() < deadline,
            "collector did not listen on {port}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
/// Polls the file exporter's output until every needle appears.
fn wait_for(path: &Path, needles: &[&str]) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if needles.iter().all(|n| text.contains(n)) {
            return text;
        }
        if Instant::now() > deadline {
            let missing: Vec<_> = needles.iter().filter(|n| !text.contains(**n)).collect();
            panic!("collector output lacks {missing:?}; got:\n{text}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Collector {
    child: Child,
    dir: PathBuf,
}
impl Drop for Collector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            // Leave the evidence: print the collector's log on failure.
            let log = std::fs::read_to_string(self.dir.join("collector.log")).unwrap_or_default();
            eprintln!("--- collector log ({}) ---\n{log}", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

struct Ports {
    arrow_in: u16,
    otlp_in: u16,
    otlp_back: u16,
    beacon_server: u16,
}
fn ports() -> Ports {
    Ports {
        arrow_in: free_port(),
        otlp_in: free_port(),
        otlp_back: free_port(),
        beacon_server: free_port(),
    }
}

/// Certificates for the TLS run: a CA and a leaf for `localhost` and
/// `127.0.0.1`, shared by the collector and Beacon's server.
struct Pki {
    ca_pem: Vec<u8>,
    tls: ServerTls,
}
fn pki() -> Pki {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    Pki {
        ca_pem: ca.pem().into_bytes(),
        tls: ServerTls {
            cert_pem: cert.pem().into_bytes(),
            key_pem: key.serialize_pem().into_bytes(),
            client_ca_pem: None,
        },
    }
}
const TOKEN: &str = "vesper-interop-token";

fn start(bin: &Path, name: &str, ports: &Ports, pki: Option<&Pki>) -> (Collector, PathBuf) {
    let dir = std::env::temp_dir().join(format!("vesper-otelcol-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.json");
    // Receivers Beacon sends to, and the exporter towards Beacon's server:
    // plaintext, or TLS plus bearer tokens.
    let (server_tls, client_tls, extensions, ext_list) = match pki {
        None => (
            String::new(),
            "\n    tls:\n      insecure: true".to_owned(),
            String::new(),
            String::new(),
        ),
        Some(pki) => {
            let cert = dir.join("server.pem");
            let key = dir.join("server.key");
            let ca = dir.join("ca.pem");
            std::fs::write(&cert, &pki.tls.cert_pem).unwrap();
            std::fs::write(&key, &pki.tls.key_pem).unwrap();
            std::fs::write(&ca, &pki.ca_pem).unwrap();
            (
                format!(
                    "\n        tls:\n          cert_file: {}\n          key_file: {}\n        auth:\n          authenticator: bearertokenauth/server",
                    cert.display(),
                    key.display()
                ),
                format!(
                    "\n    tls:\n      ca_file: {}\n    auth:\n      authenticator: bearertokenauth/client",
                    ca.display()
                ),
                format!(
                    "extensions:\n  bearertokenauth/server:\n    token: {TOKEN}\n  bearertokenauth/client:\n    token: {TOKEN}\n"
                ),
                "\n  extensions: [bearertokenauth/server, bearertokenauth/client]".to_owned(),
            )
        }
    };
    let host = if pki.is_some() {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let config = format!(
        r#"{extensions}receivers:
  otelarrow:
    protocols:
      grpc:
        endpoint: 127.0.0.1:{arrow_in}{server_tls}
  otlp:
    protocols:
      grpc:
        endpoint: 127.0.0.1:{otlp_in}{server_tls}
  otlp/back:
    protocols:
      grpc:
        endpoint: 127.0.0.1:{otlp_back}
exporters:
  file:
    path: {out}
    flush_interval: 100ms
  otelarrow:
    # Default compression (zstdarrow gRPC compression).
    endpoint: {host}:{beacon_server}{client_tls}
    arrow:
      num_streams: 1
      disable_downgrade: true
service:{ext_list}
  telemetry:
    metrics:
      level: none
    logs:
      level: info
  pipelines:
    traces:
      receivers: [otelarrow, otlp]
      exporters: [file]
    metrics:
      receivers: [otelarrow, otlp]
      exporters: [file]
    logs:
      receivers: [otelarrow, otlp]
      exporters: [file]
    traces/back:
      receivers: [otlp/back]
      exporters: [otelarrow]
    metrics/back:
      receivers: [otlp/back]
      exporters: [otelarrow]
    logs/back:
      receivers: [otlp/back]
      exporters: [otelarrow]
"#,
        arrow_in = ports.arrow_in,
        otlp_in = ports.otlp_in,
        otlp_back = ports.otlp_back,
        beacon_server = ports.beacon_server,
        out = out.display(),
    );
    let config_path = dir.join("config.yaml");
    std::fs::write(&config_path, config).unwrap();
    let version = Command::new(bin).arg("--version").output().unwrap();
    eprintln!(
        "collector: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("collector.log")).unwrap())
        .spawn()
        .unwrap();
    for port in [ports.arrow_in, ports.otlp_in, ports.otlp_back] {
        wait_listening(port, &mut child);
    }
    (Collector { child, dir }, out)
}

fn attr<'a>(attrs: &'a DecodedAttributes, key: &str) -> Option<&'a DecodedValue> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}
const TRACE: u128 = 0x5ee5_0000_0000_0000_0000_0000_0000_00ab;
fn span(span_id: u64, parent: u64, route: &'static str) -> OtapSpan {
    let pool = SpanPool::<2>::new(TRACE, nz(span_id), 1).unwrap();
    let mut ring = TelemetryRing::<2>::default();
    pool.start_span(None, SpanSchema::Request, 1_700_000_000_000_000_000, 1_000)
        .unwrap()
        .finish_ok(&mut ring, 1_000_500, PrimaryAttribute::ItemCount(3))
        .unwrap();
    let mut r = ring.records().next().unwrap();
    r.parent_span_id = parent;
    let mut s = OtapSpan::from(r);
    s.kind = SpanKind::Server;
    s.trace_state = Some(TraceState::parse("vesper=1").unwrap());
    s.attributes.insert("http.route", route).unwrap();
    s.attributes
        .insert_value("app.placement", placement())
        .unwrap();
    s.attributes
        .insert_value("app.digest", AttributeValue::bytes(DIGEST))
        .unwrap();
    let mut e = SpanEvent::new("vesper.accepted", 1_700_000_000_000_000_100);
    e.attributes.insert("queue", "fast").unwrap();
    s.events.push(e);
    s.links.push(SpanLink::new(0x1111, 0x22));
    s
}
const DIGEST: &[u8] = b"\x00\x01beacon\xff";
/// A map holding a scalar and a slice: OTLP `kvlistValue` / `arrayValue`.
fn placement() -> AttributeValue {
    AttributeValue::map([
        ("region", AttributeValue::Static("eu-west")),
        (
            "zones",
            AttributeValue::slice([AttributeValue::Int(1), AttributeValue::Int(2)]).unwrap(),
        ),
    ])
    .unwrap()
}
fn decoded_placement() -> DecodedValue {
    DecodedValue::Map(vec![
        ("region".into(), DecodedValue::Str("eu-west".into())),
        (
            "zones".into(),
            DecodedValue::Slice(vec![DecodedValue::Int(1), DecodedValue::Int(2)]),
        ),
    ])
}
fn resource(service: &'static str) -> Resource {
    let mut r = Resource::default();
    r.attributes.insert("service.name", service).unwrap();
    r
}
fn scope() -> Scope {
    let mut s = Scope {
        name: Some("vesper.interop"),
        version: Some("7"),
        ..Scope::default()
    };
    s.attributes.insert("scope.kind", "test").unwrap();
    s
}
fn log(body: &'static str, policy: &PiiPolicy) -> LogRecord {
    let mut l = LogRecord {
        time_unix_ns: Some(1_700_000_000_000_000_000),
        severity: Severity::ERROR,
        severity_text: Some("ERROR"),
        trace_id: Some(TRACE),
        span_id: Some(1),
        event_name: Some("vesper.login_failed"),
        ..LogRecord::default()
    };
    l.set_body(body);
    l.attributes
        .insert_pii(
            "user.email",
            Pii::new("alice@example.com".to_owned()),
            policy,
        )
        .unwrap();
    l.attributes
        .insert_value("app.placement", placement())
        .unwrap();
    l
}

fn exemplar(value: NumberValue) -> Exemplar {
    let mut e = Exemplar::new(1_700_000_000_000_000_050, value);
    e.span_id = Some(1);
    e.trace_id = Some(TRACE);
    e
}
/// One point of every kind: gauge, sum, histogram with bin exemplars and
/// exponential histogram, all named under `prefix`.
fn points<'a>(
    names: [&'static str; 4],
    h: &'a FastLogHistogram,
    exp: &ExponentialHistogram,
) -> Vec<MetricPoint<'a>> {
    const T: u64 = 1_700_000_000_000_000_000;
    let mut gauge = NumberPoint::gauge(names[0], "1", T, NumberValue::Int(7));
    gauge.attributes.insert("queue", "fast").unwrap();
    gauge.exemplars.push(exemplar(NumberValue::Int(7)));
    let mut sum = NumberPoint::sum(
        names[1],
        "By",
        true,
        Temporality::Cumulative,
        1,
        T,
        NumberValue::Double(1.5),
    );
    sum.exemplars.push(exemplar(NumberValue::Double(1.5)));
    let mut hist = HistogramPoint::new(names[2], "ns", 1, T, h).with_bin_exemplars(T, Some(TRACE));
    hist.attributes.insert("rpc.method", "Export").unwrap();
    let mut eh = ExpHistogramPoint::from_histogram(names[3], "By", 1, T, exp);
    eh.attributes
        .insert_value("app.placement", placement())
        .unwrap();
    vec![gauge.into(), sum.into(), hist.into(), eh.into()]
}
fn histograms() -> (FastLogHistogram, ExponentialHistogram) {
    let mut h = FastLogHistogram::default();
    for v in [5, 50, 500] {
        h.record_with_exemplar(v, nz(1)).unwrap();
    }
    let mut exp = ExponentialHistogram::default();
    for v in [0, 3, 1000, 4096, 1 << 30] {
        exp.record(v).unwrap();
    }
    (h, exp)
}
fn client_options(pki: Option<&Pki>, token: &str) -> ConnectOptions {
    ConnectOptions {
        tls: pki.map(|p| ClientTls {
            ca_pem: p.ca_pem.clone(),
            ..ClientTls::default()
        }),
        bearer_token: pki.map(|_| BearerToken::new(token).unwrap()),
        allow_insecure_auth: false,
    }
}
fn url(pki: Option<&Pki>, port: u16) -> String {
    match pki {
        Some(_) => format!("https://localhost:{port}"),
        None => format!("http://127.0.0.1:{port}"),
    }
}

/// Beacon → collector over OTAP and OTLP.
fn inbound(bin: &Path, pki: Option<&Pki>) {
    let ports = ports();
    let (_collector, out) = start(
        bin,
        if pki.is_some() { "in-tls" } else { "in" },
        &ports,
        pki,
    );
    let policy = PiiPolicy::Redact;
    let options = client_options(pki, TOKEN);

    // OTAP traces: two batches on one stream (the second has dictionary
    // deltas), then metrics (with zstd Arrow IPC buffers) and logs.
    let endpoint = url(pki, ports.arrow_in);
    let mut transport = GrpcTransport::connect_with(
        &endpoint,
        &options,
        OtapSignal::Traces,
        ClientConfig::default(),
    )
    .unwrap();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    for (first, routes) in [(1, ["/arrow/a", "/arrow/b"]), (3, ["/arrow/c", "/arrow/d"])] {
        let b = encoder
            .encode(&[ResourceGroup::single(
                resource("beacon-otap"),
                scope(),
                vec![span(first, 0, routes[0]), span(first + 1, first, routes[1])],
            )])
            .unwrap();
        let status = transport.exchange(&b).unwrap();
        assert!(status.is_ok(), "{status:?}");
    }
    assert_eq!(transport.streams_opened, 1);
    let (h, exp) = histograms();
    let otap_points = points(
        [
            "otap.queue.depth",
            "otap.bytes.sent",
            "otap.rpc.duration",
            "otap.rpc.size",
        ],
        &h,
        &exp,
    );
    let metrics_batch = MetricsEncoder::new(EncoderConfig {
        ipc_compression: IpcCompression::Zstd,
        ..EncoderConfig::default()
    })
    .encode_metrics(&[ResourceGroup::single(
        resource("beacon-otap"),
        scope(),
        otap_points,
    )])
    .unwrap();
    let mut mt = GrpcTransport::connect_with(
        &endpoint,
        &options,
        OtapSignal::Metrics,
        ClientConfig::default(),
    )
    .unwrap();
    let status = mt.exchange(&metrics_batch).unwrap();
    assert!(status.is_ok(), "{status:?}");
    let logs_batch = LogsEncoder::new(EncoderConfig::default())
        .encode(&[ResourceGroup::single(
            resource("beacon-otap"),
            scope(),
            vec![log("otap log", &policy)],
        )])
        .unwrap();
    let mut lt = GrpcTransport::connect_with(
        &endpoint,
        &options,
        OtapSignal::Logs,
        ClientConfig::default(),
    )
    .unwrap();
    let status = lt.exchange(&logs_batch).unwrap();
    assert!(status.is_ok(), "{status:?}");

    // OTLP/gRPC (zstd, the client default).
    let otlp_endpoint = url(pki, ports.otlp_in);
    let mut client =
        BlockingOtlpClient::connect_with(&otlp_endpoint, &options, GrpcCompression::default())
            .unwrap();
    client
        .export_traces(
            otlp::traces_request(&[ResourceGroup::single(
                resource("beacon-otlp"),
                scope(),
                vec![span(20, 0, "/otlp/a")],
            )])
            .unwrap(),
        )
        .unwrap();
    let otlp_points = points(
        [
            "otlp.queue.depth",
            "otlp.bytes.sent",
            "otlp.rpc.duration",
            "otlp.rpc.size",
        ],
        &h,
        &exp,
    );
    client
        .export_metrics(otlp::metric_points_request(&[ResourceGroup::single(
            resource("beacon-otlp"),
            scope(),
            otlp_points,
        )]))
        .unwrap();
    client
        .export_logs(otlp::logs_request(&[ResourceGroup::single(
            resource("beacon-otlp"),
            scope(),
            vec![log("otlp log", &policy)],
        )]))
        .unwrap();
    if pki.is_some() {
        // The collector's bearertokenauth refuses a wrong token.
        let mut wrong = BlockingOtlpClient::connect_with(
            &otlp_endpoint,
            &client_options(pki, "not-the-token"),
            GrpcCompression::default(),
        )
        .unwrap();
        let e = wrong
            .export_logs(otlp::logs_request(&[ResourceGroup::single(
                resource("beacon-intruder"),
                scope(),
                vec![log("intruder log", &policy)],
            )]))
            .unwrap_err();
        assert!(
            matches!(&e, GrpcError::Status(s) if s.code() == tonic::Code::Unauthenticated),
            "{e:?}"
        );
    }

    let mut needles = vec![
        // Traces from OTAP: ids in OTLP-JSON hex, parent, kind, state,
        // attributes, events, links, resource and scope.
        "5ee500000000000000000000000000ab",
        "\"spanId\":\"0000000000000001\"",
        "\"parentSpanId\":\"0000000000000001\"",
        "/arrow/a",
        "/arrow/b",
        "/arrow/c",
        "/arrow/d",
        "vesper=1",
        "vesper.accepted",
        "\"kind\":2",
        "beacon-otap",
        "vesper.interop",
        "scope.kind",
        // Structured values (bytes in base64).
        "\"kvlistValue\"",
        "\"arrayValue\"",
        "\"bytesValue\":\"AAFiZWFjb27/\"",
        "eu-west",
        // Logs.
        "otap log",
        "vesper.login_failed",
        // OTLP.
        "beacon-otlp",
        "/otlp/a",
        "otlp log",
        // Metric kinds and exemplars.
        "\"gauge\"",
        "\"asInt\":\"7\"",
        "\"asDouble\":1.5",
        "\"isMonotonic\":true",
        "\"exponentialHistogram\"",
        "\"exemplars\"",
        "\"sum\":555",
        "\"min\":5",
        "\"max\":500",
        "rpc.method",
    ];
    let names = [
        "otap.queue.depth",
        "otap.bytes.sent",
        "otap.rpc.duration",
        "otap.rpc.size",
        "otlp.queue.depth",
        "otlp.bytes.sent",
        "otlp.rpc.duration",
        "otlp.rpc.size",
    ];
    needles.extend(names);
    let text = wait_for(&out, &needles);
    assert!(
        !text.contains("alice@example.com"),
        "PII crossed the boundary"
    );
    assert!(
        !text.contains("intruder"),
        "unauthenticated export accepted"
    );
    assert!(text.contains("[REDACTED]"));
    // The exponential histograms arrive with Beacon's buckets: scale and
    // offset in OTLP-JSON.
    assert!(
        text.contains(&format!("\"scale\":{}", exp.scale())),
        "{text}"
    );
}

/// Collector `otelarrow` exporter (Go encoder) → Beacon's OTAP server.
fn outbound(bin: &Path, pki: Option<&Pki>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ports = ports();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind((
            "127.0.0.1",
            ports.beacon_server,
        )))
        .unwrap();
    let (service, mut rx) = OtapService::new(ServerConfig {
        bearer_tokens: pki
            .map(|_| vec![BearerToken::new(TOKEN).unwrap()])
            .unwrap_or_default(),
        ..ServerConfig::default()
    });
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let tls = pki.map(|p| p.tls.clone());
    let server = rt.spawn(async move {
        serve_otap_tls(listener, service, tls.as_ref(), async {
            let _ = stopped.await;
        })
        .await
    });
    let (collector, _out) = start(
        bin,
        if pki.is_some() { "back-tls" } else { "back" },
        &ports,
        pki,
    );

    let mut client =
        BlockingOtlpClient::connect(&format!("http://127.0.0.1:{}", ports.otlp_back)).unwrap();
    client
        .export_traces(
            otlp::traces_request(&[ResourceGroup::single(
                resource("via-go-encoder"),
                scope(),
                vec![span(1, 0, "/go/a"), span(2, 1, "/go/b")],
            )])
            .unwrap(),
        )
        .unwrap();
    let (h, exp) = histograms();
    client
        .export_metrics(otlp::metric_points_request(&[ResourceGroup::single(
            resource("via-go-encoder"),
            scope(),
            points(["go.gauge", "go.sum", "go.hist", "go.exp"], &h, &exp),
        )]))
        .unwrap();
    client
        .export_logs(otlp::logs_request(&[ResourceGroup::single(
            resource("via-go-encoder"),
            scope(),
            vec![log("go log", &PiiPolicy::Redact)],
        )]))
        .unwrap();

    let (mut spans, mut hists, mut numbers, mut exps, mut logs) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let deadline = Instant::now() + Duration::from_secs(30);
    while spans.len() < 2
        || hists.is_empty()
        || numbers.len() < 2
        || exps.is_empty()
        || logs.is_empty()
    {
        let left = deadline.saturating_duration_since(Instant::now());
        let got = rt
            .block_on(async { tokio::time::timeout(left, rx.recv()).await })
            .expect("collector did not deliver to the Beacon server")
            .unwrap();
        let b = got.batch;
        for s in &b.spans {
            assert_eq!(
                attr(&b.resources[s.resource].attributes, "service.name"),
                Some(&DecodedValue::Str("via-go-encoder".into()))
            );
        }
        spans.extend(b.spans);
        hists.extend(b.histograms);
        numbers.extend(b.numbers);
        exps.extend(b.exp_histograms);
        logs.extend(b.logs);
    }
    spans.sort_by_key(|s| s.span_id);
    numbers.sort_by(|a, b| a.name.cmp(&b.name));
    eprintln!(
        "decoded {} spans, {} histograms, {} number points, {} exponential histograms, {} logs from the Go encoder",
        spans.len(),
        hists.len(),
        numbers.len(),
        exps.len(),
        logs.len()
    );
    assert_eq!(spans[0].trace_id, TRACE);
    assert_eq!(spans[1].parent_span_id, Some(1));
    assert_eq!(spans[0].trace_state.as_deref(), Some("vesper=1"));
    assert_eq!(spans[0].events[0].name, "vesper.accepted");
    assert_eq!(spans[0].links[0].span_id, 0x22);
    assert_eq!(
        attr(&spans[0].attributes, "http.route"),
        Some(&DecodedValue::Str("/go/a".into()))
    );
    // Structured values through upstream's CBOR `ser` column.
    assert_eq!(
        attr(&spans[0].attributes, "app.placement"),
        Some(&decoded_placement())
    );
    assert_eq!(
        attr(&spans[0].attributes, "app.digest"),
        Some(&DecodedValue::Bytes(DIGEST.to_vec()))
    );
    assert_eq!(
        attr(&logs[0].attributes, "app.placement"),
        Some(&decoded_placement())
    );
    assert_eq!(hists[0].name, "go.hist");
    assert_eq!(hists[0].count, Some(3));
    assert_eq!(hists[0].exemplars.len(), 3, "{:?}", hists[0].exemplars);
    assert_eq!(hists[0].exemplars[0].trace_id, Some(TRACE));
    let (g, s) = (&numbers[0], &numbers[1]);
    assert_eq!(g.name, "go.gauge");
    assert_eq!(g.metric_type, schema::METRIC_TYPE_GAUGE);
    assert_eq!(g.value, Some(NumberValue::Int(7)));
    assert_eq!(g.exemplars[0].value, NumberValue::Int(7));
    assert_eq!(g.exemplars[0].span_id, Some(1));
    assert_eq!(g.exemplars[0].trace_id, Some(TRACE));
    assert_eq!(s.name, "go.sum");
    assert_eq!(s.metric_type, schema::METRIC_TYPE_SUM);
    assert_eq!(s.value, Some(NumberValue::Double(1.5)));
    assert_eq!(s.is_monotonic, Some(true));
    assert_eq!(
        s.aggregation_temporality,
        Some(schema::TEMPORALITY_CUMULATIVE)
    );
    assert_eq!(s.exemplars[0].value, NumberValue::Double(1.5));
    let e = &exps[0];
    assert_eq!(e.name, "go.exp");
    assert_eq!((e.count, e.zero_count, e.scale), (5, 1, exp.scale()));
    assert_eq!(e.positive.offset, exp.offset());
    assert_eq!(e.positive.counts, exp.positive_counts());
    // The Go encoder writes a zero min as null (observed with 0.165.0), so
    // a min of 0 does not survive this hop; the max does.
    assert_eq!(e.min, None);
    assert_eq!(e.max, Some((1u64 << 30) as f64));
    assert_eq!(
        attr(&e.attributes, "app.placement"),
        Some(&decoded_placement())
    );
    assert_eq!(logs[0].body, Some(DecodedValue::Str("go log".into())));
    drop(collector);
    let _ = stop.send(());
    rt.block_on(server).unwrap().unwrap();
}

#[test]
fn beacon_otap_and_otlp_reach_the_collector() {
    let Some(bin) = collector() else { return };
    inbound(&bin, None);
}

#[test]
fn collector_otelarrow_exporter_reaches_the_beacon_server() {
    let Some(bin) = collector() else { return };
    outbound(&bin, None);
}

#[test]
fn both_directions_over_tls_with_bearer_tokens() {
    let Some(bin) = collector() else { return };
    let pki = pki();
    inbound(&bin, Some(&pki));
    outbound(&bin, Some(&pki));
}
