//! gRPC loopback: OTAP streams (traces, metrics, logs) and OTLP exports over
//! real HTTP/2 on 127.0.0.1, through the tonic client and server in
//! `vesper_beacon_arrow::grpc`. Covers acknowledgements, the client's
//! in-flight window, receiver admission control (RESOURCE_EXHAUSTED),
//! malformed batches (INVALID_ARGUMENT) and the blocking transport used by
//! the existing exporters.
#![cfg(feature = "grpc")]

use core::num::NonZeroU64;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use vesper_beacon::*;
use vesper_beacon_arrow::grpc::*;
use vesper_beacon_arrow::otap::proto::{
    ArrowPayload, ArrowPayloadType, BatchArrowRecords, BatchStatus, StatusCode,
};
use vesper_beacon_arrow::otap::*;
use vesper_beacon_arrow::otlp;

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}
fn span(span_id: u64, parent: u64) -> OtapSpan {
    let pool =
        SpanPool::<2>::new(0xfeed_0000_0000_0000_0000_0000_0000_0001, nz(span_id), 1).unwrap();
    let mut ring = TelemetryRing::<2>::default();
    pool.start_span(None, SpanSchema::Request, 1_000 + span_id, 10)
        .unwrap()
        .finish_ok(&mut ring, 25, PrimaryAttribute::ItemCount(span_id))
        .unwrap();
    let mut r = ring.records().next().unwrap();
    r.parent_span_id = parent;
    let mut s = OtapSpan::from(r);
    s.kind = SpanKind::Server;
    s.attributes.insert("http.route", "/users").unwrap();
    s.events.push(SpanEvent::new("accepted", 1_001));
    s
}
fn groups(spans: Vec<OtapSpan>) -> Vec<ResourceGroup<OtapSpan>> {
    let mut resource = Resource::default();
    resource
        .attributes
        .insert("service.name", "vesper-test")
        .unwrap();
    vec![ResourceGroup {
        resource,
        scopes: vec![ScopeGroup {
            scope: Scope {
                name: Some("vesper.grpc.test"),
                version: Some("1"),
                ..Scope::default()
            },
            items: spans,
        }],
    }]
}
fn log(text: &'static str) -> LogRecord {
    let mut l = LogRecord {
        time_unix_ns: Some(5),
        severity: Severity::WARN,
        severity_text: Some("WARN"),
        ..LogRecord::default()
    };
    l.set_body(text);
    l.attributes.insert("component", "grpc").unwrap();
    l
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
async fn otap_server(config: ServerConfig) -> (Server, mpsc::Receiver<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (service, rx) = OtapService::new(config);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        serve_otap(listener, service, async {
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
async fn otlp_server(config: ServerConfig) -> (Server, mpsc::Receiver<OtlpReceived>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (service, rx) = OtlpService::new(config);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        serve_otlp(listener, service, async {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otap_traces_metrics_and_logs_round_trip_over_grpc() {
    let (server, mut rx) = otap_server(ServerConfig::default()).await;
    let config = ClientConfig {
        max_in_flight: 2,
        ..ClientConfig::default()
    };

    // Traces: three batches on one stream, so later batches carry
    // dictionary deltas that only decode with the stream's earlier state.
    let mut traces = OtapStream::connect(&server.endpoint, OtapSignal::Traces, config)
        .await
        .unwrap();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let mut acks = Vec::new();
    for i in 0..3u64 {
        let batch = encoder
            .encode(&groups(vec![
                span(10 * i + 1, 0),
                span(10 * i + 2, 10 * i + 1),
            ]))
            .unwrap();
        acks.extend(traces.send(batch).await.unwrap());
        assert!(traces.in_flight() <= 2, "window respected");
    }
    acks.extend(traces.finish().await.unwrap());
    assert_eq!(acks.len(), 3);
    assert!(acks.iter().all(BatchStatus::is_ok), "{acks:?}");
    let mut ids: Vec<i64> = acks.iter().map(|a| a.batch_id).collect();
    ids.sort();
    assert_eq!(ids, [0, 1, 2]);
    for i in 0..3u64 {
        let got = rx.recv().await.unwrap();
        assert_eq!(got.signal, OtapSignal::Traces);
        let b = got.batch;
        assert_eq!(b.spans.len(), 2);
        assert_eq!(b.spans[0].span_id, 10 * i + 1);
        assert_eq!(b.spans[1].parent_span_id, Some(10 * i + 1));
        assert_eq!(b.spans[0].kind, Some(SpanKind::Server as i32));
        assert_eq!(b.spans[0].events[0].name, "accepted");
        assert_eq!(b.resources.len(), 1);
        assert_eq!(b.scopes[0].name.as_deref(), Some("vesper.grpc.test"));
    }

    // Metrics.
    let mut h = FastLogHistogram::default();
    for v in [5, 50, 500] {
        h.record_with_exemplar(v, nz(1)).unwrap();
    }
    let mut point = HistogramPoint::new("rpc.duration", "ns", 1, 2, &h);
    point.attributes.insert("rpc.method", "Export").unwrap();
    let batch = MetricsEncoder::new(EncoderConfig::default())
        .encode_histograms(&[point])
        .unwrap();
    let mut metrics = OtapStream::connect(&server.endpoint, OtapSignal::Metrics, config)
        .await
        .unwrap();
    metrics.send(batch).await.unwrap();
    let acks = metrics.finish().await.unwrap();
    assert!(acks[0].is_ok(), "{acks:?}");
    let got = rx.recv().await.unwrap();
    assert_eq!(got.signal, OtapSignal::Metrics);
    let hist = &got.batch.histograms[0];
    assert_eq!(hist.name, "rpc.duration");
    assert_eq!(hist.count, Some(3));
    assert_eq!(hist.sum, Some(555.0));
    assert_eq!((hist.min, hist.max), (Some(5.0), Some(500.0)));

    // Logs.
    let batch = LogsEncoder::new(EncoderConfig::default())
        .encode_logs(&[log("first"), log("second")])
        .unwrap();
    let mut logs = OtapStream::connect(&server.endpoint, OtapSignal::Logs, config)
        .await
        .unwrap();
    logs.send(batch).await.unwrap();
    assert!(logs.finish().await.unwrap()[0].is_ok());
    let got = rx.recv().await.unwrap();
    assert_eq!(got.signal, OtapSignal::Logs);
    assert_eq!(got.batch.logs.len(), 2);
    assert_eq!(
        got.batch.logs[1].body,
        Some(DecodedValue::Str("second".into()))
    );
    assert_eq!(got.batch.logs[0].severity_number, Some(13));

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_batch_is_answered_invalid_argument_without_ending_the_stream() {
    let (server, mut rx) = otap_server(ServerConfig::default()).await;
    let mut stream = OtapStream::connect(
        &server.endpoint,
        OtapSignal::Traces,
        ClientConfig::default(),
    )
    .await
    .unwrap();
    let garbage = BatchArrowRecords {
        batch_id: 7,
        arrow_payloads: vec![ArrowPayload {
            schema_id: "0".into(),
            r#type: ArrowPayloadType::Spans as i32,
            record: vec![0xff; 32],
        }],
        headers: Vec::new(),
    };
    stream.send(garbage).await.unwrap();
    let status = stream.recv_status().await.unwrap();
    assert_eq!(status.batch_id, 7);
    assert_eq!(status.status_code, StatusCode::InvalidArgument as i32);
    assert!(!status.is_retryable());
    assert!(!status.status_message.is_empty());
    // The stream stays open and a valid batch is accepted afterwards.
    let mut enc = TraceEncoder::new(EncoderConfig::default());
    let mut good = enc.encode(&groups(vec![span(1, 0)])).unwrap();
    good.batch_id = 8;
    stream.send(good).await.unwrap();
    let ok = stream.recv_status().await.unwrap();
    assert!(ok.is_ok(), "{ok:?}");
    assert_eq!(rx.recv().await.unwrap().batch.spans[0].span_id, 1);
    // Graceful shutdown waits for open streams.
    assert!(stream.finish().await.unwrap().is_empty());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_receiver_queue_answers_resource_exhausted_then_recovers() {
    let (server, mut rx) = otap_server(ServerConfig {
        queue_depth: 1,
        admission_timeout: Duration::from_millis(100),
        ..ServerConfig::default()
    })
    .await;
    let mut stream = OtapStream::connect(
        &server.endpoint,
        OtapSignal::Traces,
        ClientConfig {
            max_in_flight: 1,
            ..ClientConfig::default()
        },
    )
    .await
    .unwrap();
    let mut enc = TraceEncoder::new(EncoderConfig::default());
    // First batch fills the one-slot queue (nobody drains it yet).
    stream
        .send(enc.encode(&groups(vec![span(1, 0)])).unwrap())
        .await
        .unwrap();
    assert_eq!(stream.in_flight(), 1);
    // The window is full: this send first waits for batch 0's status.
    let acks = stream
        .send(enc.encode(&groups(vec![span(2, 0)])).unwrap())
        .await
        .unwrap();
    assert_eq!(acks.len(), 1);
    assert!(acks[0].is_ok());
    assert_eq!(stream.in_flight(), 1);
    let exhausted = stream.recv_status().await.unwrap();
    assert_eq!(exhausted.batch_id, 1);
    assert_eq!(exhausted.status_code, StatusCode::ResourceExhausted as i32);
    assert!(exhausted.is_retryable());
    // Drain; the stream's dictionaries stayed in step, so the next batch on
    // the same stream is accepted.
    assert_eq!(rx.recv().await.unwrap().batch.spans[0].span_id, 1);
    stream
        .send(enc.encode(&groups(vec![span(3, 0)])).unwrap())
        .await
        .unwrap();
    let ok = stream.finish().await.unwrap();
    assert!(ok[0].is_ok(), "{ok:?}");
    assert_eq!(rx.recv().await.unwrap().batch.spans[0].span_id, 3);
    server.stop().await;
}

#[test]
fn blocking_grpc_transport_drives_the_trace_exporter() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (server, mut rx) = rt.block_on(otap_server(ServerConfig::default()));
    let transport = GrpcTransport::connect(
        &server.endpoint,
        OtapSignal::Traces,
        ClientConfig::default(),
    )
    .unwrap();
    let mut exporter = TraceExporter::new(TraceEncoder::new(EncoderConfig::default()), transport);
    assert_eq!(exporter.export_spans(&[span(1, 0), span(2, 1)]).unwrap(), 0);
    assert_eq!(exporter.export_spans(&[span(3, 0)]).unwrap(), 1);
    assert_eq!(exporter.transport_mut().streams_opened, 1);
    let first = rt.block_on(rx.recv()).unwrap().batch;
    let second = rt.block_on(rx.recv()).unwrap().batch;
    assert_eq!(first.spans.len(), 2);
    assert_eq!(second.spans[0].span_id, 3);
    assert_eq!(exporter.encoder().resets(), 0);

    // A ring drained only after the acknowledgement.
    let pool = SpanPool::<4>::new(9, nz(40), 1).unwrap();
    let mut ring = TelemetryRing::<4>::default();
    for _ in 0..3 {
        pool.start_span(None, SpanSchema::Background, 1, 2)
            .unwrap()
            .finish_ok(&mut ring, 3, PrimaryAttribute::None)
            .unwrap();
    }
    assert_eq!(exporter.export_ring(&mut ring, 8).unwrap(), 3);
    assert!(ring.is_empty());
    assert_eq!(rt.block_on(rx.recv()).unwrap().batch.spans.len(), 3);
    drop(exporter); // closes the stream; graceful shutdown waits for it
    rt.block_on(server.stop());
}

#[test]
fn blocking_transport_reports_unreachable_peer_as_transport_error() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (server, rx) = rt.block_on(otap_server(ServerConfig::default()));
    let endpoint = server.endpoint.clone();
    let mut transport = GrpcTransport::connect(
        &endpoint,
        OtapSignal::Traces,
        ClientConfig {
            status_timeout: Duration::from_secs(5),
            ..ClientConfig::default()
        },
    )
    .unwrap();
    rt.block_on(server.stop());
    drop(rx);
    let batch = TraceEncoder::new(EncoderConfig::default())
        .encode_spans(&[span(1, 0)])
        .unwrap();
    assert!(transport.exchange(&batch).is_err());
    assert!(matches!(
        GrpcTransport::connect("not a uri", OtapSignal::Traces, ClientConfig::default()),
        Err(GrpcError::InvalidEndpoint(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_export_round_trips_traces_metrics_and_logs() {
    let (server, mut rx) = otlp_server(ServerConfig::default()).await;
    let mut client = OtlpClient::connect(&server.endpoint).await.unwrap();

    let request = otlp::traces_request(&groups(vec![span(1, 0), span(2, 1)])).unwrap();
    client.export_traces(request.clone()).await.unwrap();
    let OtlpReceived::Traces(got) = rx.recv().await.unwrap() else {
        panic!("traces expected")
    };
    assert_eq!(got, request);
    let spans = &got.resource_spans[0].scope_spans[0].spans;
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].trace_id.len(), 16);
    assert_eq!(spans[0].span_id, 1u64.to_be_bytes());
    assert!(spans[0].parent_span_id.is_empty());
    assert_eq!(spans[1].parent_span_id, 1u64.to_be_bytes());
    assert_eq!(
        spans[0].end_time_unix_nano - spans[0].start_time_unix_nano,
        25 - 10
    );
    assert_eq!(spans[0].events[0].name, "accepted");
    assert_eq!(
        got.resource_spans[0].scope_spans[0]
            .scope
            .as_ref()
            .unwrap()
            .name,
        "vesper.grpc.test"
    );

    let mut h = FastLogHistogram::default();
    h.record_with_exemplar(42, nz(1)).unwrap();
    let points = [
        HistogramPoint::new("a", "ns", 1, 2, &h),
        HistogramPoint::new("a", "ns", 1, 2, &h),
        HistogramPoint::new("b", "ns", 1, 2, &h),
    ];
    let request = otlp::metrics_request(&[ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        points.to_vec(),
    )]);
    client.export_metrics(request.clone()).await.unwrap();
    let OtlpReceived::Metrics(got) = rx.recv().await.unwrap() else {
        panic!("metrics expected")
    };
    assert_eq!(got, request);
    let metrics = &got.resource_metrics[0].scope_metrics[0].metrics;
    assert_eq!(
        metrics.len(),
        2,
        "consecutive same-name points share a Metric"
    );

    let request = otlp::logs_request(&[ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        vec![log("hello")],
    )]);
    client.export_logs(request.clone()).await.unwrap();
    let OtlpReceived::Logs(got) = rx.recv().await.unwrap() else {
        panic!("logs expected")
    };
    assert_eq!(got, request);
    assert_eq!(
        got.resource_logs[0].scope_logs[0].log_records[0].severity_number,
        13
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_receiver_sheds_load_with_resource_exhausted() {
    let (server, mut rx) = otlp_server(ServerConfig {
        queue_depth: 1,
        admission_timeout: Duration::from_millis(100),
        ..ServerConfig::default()
    })
    .await;
    let mut client = OtlpClient::connect(&server.endpoint).await.unwrap();
    let request = otlp::logs_request(&[ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        vec![log("x")],
    )]);
    client.export_logs(request.clone()).await.unwrap();
    match client.export_logs(request.clone()).await {
        Err(GrpcError::Status(s)) => assert_eq!(s.code(), tonic::Code::ResourceExhausted),
        other => panic!("expected RESOURCE_EXHAUSTED, got {other:?}"),
    }
    rx.recv().await.unwrap();
    client.export_logs(request).await.unwrap();
    server.stop().await;
}

#[test]
fn blocking_otlp_client_exports() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (server, mut rx) = rt.block_on(otlp_server(ServerConfig::default()));
    let mut client = BlockingOtlpClient::connect(&server.endpoint).unwrap();
    let request = otlp::traces_request(&groups(vec![span(5, 0)])).unwrap();
    client.export_traces(request.clone()).unwrap();
    assert!(matches!(rt.block_on(rx.recv()), Some(OtlpReceived::Traces(r)) if r == request));
    rt.block_on(server.stop());
}
