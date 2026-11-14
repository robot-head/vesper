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

fn start(bin: &Path, name: &str, ports: &Ports) -> (Collector, PathBuf) {
    let dir = std::env::temp_dir().join(format!("vesper-otelcol-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.json");
    let config = format!(
        r#"receivers:
  otelarrow:
    protocols:
      grpc:
        endpoint: 127.0.0.1:{arrow_in}
  otlp:
    protocols:
      grpc:
        endpoint: 127.0.0.1:{otlp_in}
  otlp/back:
    protocols:
      grpc:
        endpoint: 127.0.0.1:{otlp_back}
exporters:
  file:
    path: {out}
    flush_interval: 100ms
  otelarrow:
    endpoint: 127.0.0.1:{beacon_server}
    tls:
      insecure: true
    # Beacon's server has no gRPC compression; the exporter's default is
    # `zstdarrow5`.
    compression: none
    arrow:
      num_streams: 1
      disable_downgrade: true
      # Nor Arrow IPC buffer compression (on when gRPC compression is off).
      payload_compression: none
service:
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
    let mut e = SpanEvent::new("vesper.accepted", 1_700_000_000_000_000_100);
    e.attributes.insert("queue", "fast").unwrap();
    s.events.push(e);
    s.links.push(SpanLink::new(0x1111, 0x22));
    s
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
    l
}

#[test]
fn beacon_otap_and_otlp_reach_the_collector() {
    let Some(bin) = collector() else { return };
    let ports = Ports {
        arrow_in: free_port(),
        otlp_in: free_port(),
        otlp_back: free_port(),
        beacon_server: free_port(),
    };
    let (_collector, out) = start(&bin, "in", &ports);
    let policy = PiiPolicy::Redact;

    // OTAP traces: two batches on one stream (the second has dictionary
    // deltas), then metrics and logs on their own streams.
    let endpoint = format!("http://127.0.0.1:{}", ports.arrow_in);
    let mut transport =
        GrpcTransport::connect(&endpoint, OtapSignal::Traces, ClientConfig::default()).unwrap();
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
    let mut h = FastLogHistogram::default();
    for v in [5, 50, 500] {
        h.record_with_exemplar(v, nz(1)).unwrap();
    }
    let mut point = HistogramPoint::new(
        "vesper.rpc.duration",
        "ns",
        1,
        1_700_000_000_000_000_000,
        &h,
    );
    point.attributes.insert("rpc.method", "Export").unwrap();
    let metrics_batch = MetricsEncoder::new(EncoderConfig::default())
        .encode(&[ResourceGroup::single(
            resource("beacon-otap"),
            scope(),
            vec![point.clone()],
        )])
        .unwrap();
    let mut mt =
        GrpcTransport::connect(&endpoint, OtapSignal::Metrics, ClientConfig::default()).unwrap();
    let status = mt.exchange(&metrics_batch).unwrap();
    assert!(status.is_ok(), "{status:?}");
    let logs_batch = LogsEncoder::new(EncoderConfig::default())
        .encode(&[ResourceGroup::single(
            resource("beacon-otap"),
            scope(),
            vec![log("otap log", &policy)],
        )])
        .unwrap();
    let mut lt =
        GrpcTransport::connect(&endpoint, OtapSignal::Logs, ClientConfig::default()).unwrap();
    let status = lt.exchange(&logs_batch).unwrap();
    assert!(status.is_ok(), "{status:?}");

    // OTLP/gRPC.
    let mut client =
        BlockingOtlpClient::connect(&format!("http://127.0.0.1:{}", ports.otlp_in)).unwrap();
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
    client
        .export_metrics(otlp::metrics_request(&[ResourceGroup::single(
            resource("beacon-otlp"),
            scope(),
            vec![point],
        )]))
        .unwrap();
    client
        .export_logs(otlp::logs_request(&[ResourceGroup::single(
            resource("beacon-otlp"),
            scope(),
            vec![log("otlp log", &policy)],
        )]))
        .unwrap();

    let text = wait_for(
        &out,
        &[
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
            // Metrics and logs from OTAP.
            "vesper.rpc.duration",
            "\"sum\":555",
            "\"min\":5",
            "\"max\":500",
            "rpc.method",
            "otap log",
            "vesper.login_failed",
            // OTLP.
            "beacon-otlp",
            "/otlp/a",
            "otlp log",
        ],
    );
    assert!(
        !text.contains("alice@example.com"),
        "PII crossed the boundary"
    );
    assert!(text.contains("[REDACTED]"));
}

#[test]
fn collector_otelarrow_exporter_reaches_the_beacon_server() {
    let Some(bin) = collector() else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ports = Ports {
        arrow_in: free_port(),
        otlp_in: free_port(),
        otlp_back: free_port(),
        beacon_server: free_port(),
    };
    let listener = rt
        .block_on(tokio::net::TcpListener::bind((
            "127.0.0.1",
            ports.beacon_server,
        )))
        .unwrap();
    let (service, mut rx) = OtapService::new(ServerConfig::default());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = rt.spawn(serve_otap(listener, service, async {
        let _ = stopped.await;
    }));
    let (collector, _out) = start(&bin, "back", &ports);

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
    let mut h = FastLogHistogram::default();
    h.record_with_exemplar(42, nz(1)).unwrap();
    client
        .export_metrics(otlp::metrics_request(&[ResourceGroup::single(
            resource("via-go-encoder"),
            scope(),
            vec![HistogramPoint::new("go.hist", "ns", 1, 2, &h)],
        )]))
        .unwrap();
    client
        .export_logs(otlp::logs_request(&[ResourceGroup::single(
            resource("via-go-encoder"),
            scope(),
            vec![log("go log", &PiiPolicy::Redact)],
        )]))
        .unwrap();

    let (mut spans, mut hists, mut logs) = (Vec::new(), Vec::new(), Vec::new());
    let deadline = Instant::now() + Duration::from_secs(30);
    while spans.len() < 2 || hists.is_empty() || logs.is_empty() {
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
        logs.extend(b.logs);
    }
    spans.sort_by_key(|s| s.span_id);
    eprintln!(
        "decoded {} spans, {} histograms, {} logs from the Go encoder",
        spans.len(),
        hists.len(),
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
    assert_eq!(hists[0].name, "go.hist");
    assert_eq!(hists[0].count, Some(1));
    assert_eq!(logs[0].body, Some(DecodedValue::Str("go log".into())));
    drop(collector);
    let _ = stop.send(());
    rt.block_on(server).unwrap().unwrap();
}
