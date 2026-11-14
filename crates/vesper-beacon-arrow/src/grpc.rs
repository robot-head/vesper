//! OTAP and OTLP over HTTP/2 gRPC with `tonic` (feature `grpc`).
//!
//! The service stubs are written by hand on tonic's codegen API (no
//! `protoc`, no build script) with the upstream names:
//!
//! * OTAP, bidirectional streaming `BatchArrowRecords` → `BatchStatus`:
//!   `/opentelemetry.proto.experimental.arrow.v1.ArrowTracesService/ArrowTraces`,
//!   `.../ArrowMetricsService/ArrowMetrics`, `.../ArrowLogsService/ArrowLogs`.
//! * OTLP, unary: `/opentelemetry.proto.collector.trace.v1.TraceService/Export`,
//!   `.../metrics.v1.MetricsService/Export`, `.../logs.v1.LogsService/Export`.
//!
//! **Client.** [`OtapStream`] opens one OTAP stream and pipelines batches
//! with at most `max_in_flight` unacknowledged; [`OtapStream::send`] waits
//! for acknowledgements once the window is full, so a slow receiver slows
//! the producer (back-pressure), and HTTP/2 flow control bounds the bytes in
//! flight below that. [`GrpcTransport`] is a blocking [`BatchTransport`]
//! over one stream (one batch at a time) for the existing exporters; it
//! reopens the stream after a failure, and the exporters reset their
//! encoders on any failure, so a new stream always starts with fresh schema
//! ids. [`OtlpClient`] sends OTLP export requests.
//!
//! **Server.** [`OtapService`] decodes each stream with its own
//! [`OtapDecoder`] (schema ids are scoped to one stream) and hands decoded
//! batches to a bounded queue. A batch the decoder rejects is answered with
//! `INVALID_ARGUMENT`; a batch that cannot enter the queue within
//! `admission_timeout` is answered with `RESOURCE_EXHAUSTED` (retryable), as
//! the collector's `otelarrow` receiver does under admission control. The
//! batch is decoded before admission, so the stream's dictionaries stay in
//! step whatever the client does next. [`OtlpService`] queues export
//! requests the same way and answers `RESOURCE_EXHAUSTED` when full.
//!
//! No TLS, compression, authentication or HPACK `headers` support: the
//! transport is plaintext HTTP/2 (`http://` endpoints) and compressed
//! messages are rejected by tonic.

use crate::otap::proto::{BatchArrowRecords, BatchStatus, StatusCode};
use crate::otap::{BatchTransport, DecodeLimits, DecodedBatch, OtapDecoder, TransportError};
use crate::otlp::proto::{
    ExportLogsServiceRequest, ExportLogsServiceResponse, ExportMetricsServiceRequest,
    ExportMetricsServiceResponse, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use std::collections::BTreeSet;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::codegen::{http, tokio_stream, Body, BoxFuture, StdError};
use tonic::transport::{Channel, Endpoint};

pub const ARROW_TRACES_PATH: &str =
    "/opentelemetry.proto.experimental.arrow.v1.ArrowTracesService/ArrowTraces";
pub const ARROW_METRICS_PATH: &str =
    "/opentelemetry.proto.experimental.arrow.v1.ArrowMetricsService/ArrowMetrics";
pub const ARROW_LOGS_PATH: &str =
    "/opentelemetry.proto.experimental.arrow.v1.ArrowLogsService/ArrowLogs";
pub const OTLP_TRACES_PATH: &str = "/opentelemetry.proto.collector.trace.v1.TraceService/Export";
pub const OTLP_METRICS_PATH: &str =
    "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export";
pub const OTLP_LOGS_PATH: &str = "/opentelemetry.proto.collector.logs.v1.LogsService/Export";

/// Default ceiling for one gRPC message in either direction.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 << 20;

/// The three OTAP streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OtapSignal {
    Traces,
    Metrics,
    Logs,
}
impl OtapSignal {
    pub fn path(self) -> &'static str {
        match self {
            Self::Traces => ARROW_TRACES_PATH,
            Self::Metrics => ARROW_METRICS_PATH,
            Self::Logs => ARROW_LOGS_PATH,
        }
    }
}

#[derive(Debug)]
pub enum GrpcError {
    InvalidEndpoint(String),
    Connect(tonic::transport::Error),
    Status(tonic::Status),
    /// The peer ended the response stream.
    StreamClosed,
    /// No acknowledgement within `status_timeout`.
    Timeout,
    /// An acknowledgement for a batch id that is not in flight.
    UnexpectedStatus(i64),
    /// A batch id that is already in flight on this stream.
    DuplicateBatchId(i64),
    /// No Tokio runtime could be built for the blocking wrappers.
    Runtime(std::io::Error),
}
impl std::fmt::Display for GrpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GrpcError {}

impl BatchStatus {
    /// Whether a sender may retry the batch (on a fresh stream): the
    /// receiver was overloaded or unavailable, not the batch malformed.
    pub fn is_retryable(&self) -> bool {
        [
            StatusCode::ResourceExhausted,
            StatusCode::Unavailable,
            StatusCode::DeadlineExceeded,
        ]
        .iter()
        .any(|c| *c as i32 == self.status_code)
    }
}

async fn channel(endpoint: &str) -> Result<Channel, GrpcError> {
    Endpoint::from_shared(endpoint.to_owned())
        .map_err(|_| GrpcError::InvalidEndpoint(endpoint.to_owned()))?
        .connect_timeout(Duration::from_secs(10))
        .connect()
        .await
        .map_err(GrpcError::Connect)
}

// ------------------------------------------------------------------ OTAP client

#[derive(Clone, Copy, Debug)]
pub struct ClientConfig {
    /// Unacknowledged batches allowed on one stream.
    pub max_in_flight: usize,
    /// How long to wait for an acknowledgement before failing the stream.
    pub status_timeout: Duration,
    pub max_message_bytes: usize,
}
impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            max_in_flight: 8,
            status_timeout: Duration::from_secs(30),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }
}

/// One OTAP bidirectional stream (client side).
pub struct OtapStream {
    requests: Option<mpsc::Sender<BatchArrowRecords>>,
    statuses: mpsc::Receiver<Result<BatchStatus, tonic::Status>>,
    pending: BTreeSet<i64>,
    config: ClientConfig,
}
impl OtapStream {
    pub async fn connect(
        endpoint: &str,
        signal: OtapSignal,
        config: ClientConfig,
    ) -> Result<Self, GrpcError> {
        Ok(Self::open(channel(endpoint).await?, signal, config))
    }
    /// Starts the stream on `channel`. The call runs in a background task
    /// so batches flow before the server sends response headers (grpc-go
    /// servers send them with the first status).
    pub fn open(channel: Channel, signal: OtapSignal, config: ClientConfig) -> Self {
        let window = config.max_in_flight.max(1);
        let (req_tx, req_rx) = mpsc::channel::<BatchArrowRecords>(window);
        let (st_tx, st_rx) = mpsc::channel(window + 1);
        let max = config.max_message_bytes;
        tokio::spawn(async move {
            let mut grpc = tonic::client::Grpc::new(channel)
                .max_decoding_message_size(max)
                .max_encoding_message_size(max);
            if let Err(e) = grpc.ready().await {
                let _ = st_tx
                    .send(Err(tonic::Status::unavailable(e.to_string())))
                    .await;
                return;
            }
            let codec = tonic_prost::ProstCodec::<BatchArrowRecords, BatchStatus>::default();
            let request = tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(req_rx));
            let path = http::uri::PathAndQuery::from_static(signal.path());
            let mut inbound = match grpc.streaming(request, path, codec).await {
                Ok(response) => response.into_inner(),
                Err(status) => {
                    let _ = st_tx.send(Err(status)).await;
                    return;
                }
            };
            loop {
                let item = match inbound.message().await {
                    Ok(Some(status)) => Ok(status),
                    Ok(None) => return,
                    Err(status) => Err(status),
                };
                let stop = item.is_err();
                if st_tx.send(item).await.is_err() || stop {
                    return;
                }
            }
        });
        Self {
            requests: Some(req_tx),
            statuses: st_rx,
            pending: BTreeSet::new(),
            config,
        }
    }
    /// Batch ids sent and not yet acknowledged.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }
    /// Sends one batch, first waiting for acknowledgements while the window
    /// is full. Returns the statuses received while waiting.
    pub async fn send(&mut self, batch: BatchArrowRecords) -> Result<Vec<BatchStatus>, GrpcError> {
        let mut acks = Vec::new();
        while self.pending.len() >= self.config.max_in_flight.max(1) {
            acks.push(self.recv_status().await?);
        }
        if !self.pending.insert(batch.batch_id) {
            return Err(GrpcError::DuplicateBatchId(batch.batch_id));
        }
        let id = batch.batch_id;
        let sent = self
            .requests
            .as_ref()
            .ok_or(GrpcError::StreamClosed)?
            .send(batch)
            .await;
        if sent.is_err() {
            self.pending.remove(&id);
            // The call ended; surface its status if there is one.
            return Err(match self.statuses.try_recv() {
                Ok(Err(status)) => GrpcError::Status(status),
                _ => GrpcError::StreamClosed,
            });
        }
        Ok(acks)
    }
    /// Waits for the next acknowledgement.
    pub async fn recv_status(&mut self) -> Result<BatchStatus, GrpcError> {
        let next = tokio::time::timeout(self.config.status_timeout, self.statuses.recv())
            .await
            .map_err(|_| GrpcError::Timeout)?;
        match next {
            None => Err(GrpcError::StreamClosed),
            Some(Err(status)) => Err(GrpcError::Status(status)),
            Some(Ok(status)) => {
                if !self.pending.remove(&status.batch_id) {
                    return Err(GrpcError::UnexpectedStatus(status.batch_id));
                }
                Ok(status)
            }
        }
    }
    /// Closes the request side and waits for every outstanding status.
    pub async fn finish(mut self) -> Result<Vec<BatchStatus>, GrpcError> {
        self.requests = None;
        let mut acks = Vec::new();
        while !self.pending.is_empty() {
            acks.push(self.recv_status().await?);
        }
        Ok(acks)
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, GrpcError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(GrpcError::Runtime)
}

/// Blocking [`BatchTransport`] over an OTAP gRPC stream: each `exchange`
/// sends one batch and waits for its status. It owns a one-worker Tokio
/// runtime, so it must not be used from inside another runtime.
pub struct GrpcTransport {
    runtime: tokio::runtime::Runtime,
    channel: Channel,
    signal: OtapSignal,
    config: ClientConfig,
    stream: Option<OtapStream>,
    /// Streams opened so far (the first, plus one after each failure).
    pub streams_opened: u64,
}
impl GrpcTransport {
    pub fn connect(
        endpoint: &str,
        signal: OtapSignal,
        config: ClientConfig,
    ) -> Result<Self, GrpcError> {
        let runtime = runtime()?;
        let channel = runtime.block_on(channel(endpoint))?;
        Ok(Self {
            runtime,
            channel,
            signal,
            config,
            stream: None,
            streams_opened: 0,
        })
    }
}
impl BatchTransport for GrpcTransport {
    fn exchange(&mut self, batch: &BatchArrowRecords) -> Result<BatchStatus, TransportError> {
        if self.stream.is_none() {
            let _guard = self.runtime.enter();
            self.stream = Some(OtapStream::open(
                self.channel.clone(),
                self.signal,
                self.config,
            ));
            self.streams_opened += 1;
        }
        let stream = self.stream.as_mut().expect("opened above");
        let result = self.runtime.block_on(async {
            stream.send(batch.clone()).await?;
            stream.recv_status().await
        });
        match result {
            Ok(status) => {
                if !status.is_ok() {
                    // The exporter resets its encoder; start a fresh stream
                    // so the receiver's schema ids start fresh too.
                    self.stream = None;
                }
                Ok(status)
            }
            Err(e) => {
                self.stream = None;
                Err(TransportError::Io(std::io::Error::other(e)))
            }
        }
    }
}

// ------------------------------------------------------------------ OTAP server

#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    /// Decoded batches the queue holds before admission waits.
    pub queue_depth: usize,
    /// How long a decoded batch may wait for queue space before the
    /// receiver answers `RESOURCE_EXHAUSTED`.
    pub admission_timeout: Duration,
    pub decode_limits: DecodeLimits,
    pub max_message_bytes: usize,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            queue_depth: 64,
            admission_timeout: Duration::from_secs(1),
            decode_limits: DecodeLimits::default(),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }
}

/// One accepted OTAP batch.
#[derive(Debug)]
pub struct Received {
    pub signal: OtapSignal,
    pub batch: DecodedBatch,
}

struct ServerInner<T> {
    sink: mpsc::Sender<T>,
    config: ServerConfig,
}

/// The OTAP receiver: one value serves all three Arrow services.
pub struct OtapService {
    inner: Arc<ServerInner<Received>>,
}
impl Clone for OtapService {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl OtapService {
    /// The service and the queue of accepted batches.
    pub fn new(config: ServerConfig) -> (Self, mpsc::Receiver<Received>) {
        let (sink, rx) = mpsc::channel(config.queue_depth.max(1));
        (
            Self {
                inner: Arc::new(ServerInner { sink, config }),
            },
            rx,
        )
    }
    pub fn traces(&self) -> ArrowServer<TracesSvc> {
        ArrowServer::new(self.clone())
    }
    pub fn metrics(&self) -> ArrowServer<MetricsSvc> {
        ArrowServer::new(self.clone())
    }
    pub fn logs(&self) -> ArrowServer<LogsSvc> {
        ArrowServer::new(self.clone())
    }

    async fn run_stream(
        self,
        signal: OtapSignal,
        mut inbound: tonic::Streaming<BatchArrowRecords>,
        out: mpsc::Sender<Result<BatchStatus, tonic::Status>>,
    ) {
        let config = self.inner.config;
        let mut decoder = OtapDecoder::new(config.decode_limits);
        loop {
            let batch = match inbound.message().await {
                Ok(Some(batch)) => batch,
                Ok(None) => return,
                Err(status) => {
                    let _ = out.send(Err(status)).await;
                    return;
                }
            };
            let status = match decoder.decode(&batch) {
                Err(e) => BatchStatus {
                    batch_id: batch.batch_id,
                    status_code: StatusCode::InvalidArgument as i32,
                    status_message: e.to_string(),
                },
                Ok(decoded) => {
                    let admitted =
                        tokio::time::timeout(config.admission_timeout, self.inner.sink.reserve())
                            .await;
                    match admitted {
                        Ok(Ok(permit)) => {
                            permit.send(Received {
                                signal,
                                batch: decoded,
                            });
                            BatchStatus::ok(batch.batch_id)
                        }
                        Ok(Err(_)) => BatchStatus {
                            batch_id: batch.batch_id,
                            status_code: StatusCode::Unavailable as i32,
                            status_message: "receiver closed".into(),
                        },
                        Err(_) => BatchStatus {
                            batch_id: batch.batch_id,
                            status_code: StatusCode::ResourceExhausted as i32,
                            status_message: "receiver queue full".into(),
                        },
                    }
                }
            };
            if out.send(Ok(status)).await.is_err() {
                return;
            }
        }
    }
}

/// Names one Arrow service for [`ArrowServer`].
pub trait ArrowSvc: Send + Sync + 'static {
    const NAME: &'static str;
    const SIGNAL: OtapSignal;
}
pub struct TracesSvc;
pub struct MetricsSvc;
pub struct LogsSvc;
impl ArrowSvc for TracesSvc {
    const NAME: &'static str = "opentelemetry.proto.experimental.arrow.v1.ArrowTracesService";
    const SIGNAL: OtapSignal = OtapSignal::Traces;
}
impl ArrowSvc for MetricsSvc {
    const NAME: &'static str = "opentelemetry.proto.experimental.arrow.v1.ArrowMetricsService";
    const SIGNAL: OtapSignal = OtapSignal::Metrics;
}
impl ArrowSvc for LogsSvc {
    const NAME: &'static str = "opentelemetry.proto.experimental.arrow.v1.ArrowLogsService";
    const SIGNAL: OtapSignal = OtapSignal::Logs;
}

/// One Arrow gRPC service of an [`OtapService`], for tonic's router.
pub struct ArrowServer<S> {
    service: OtapService,
    _svc: PhantomData<fn() -> S>,
}
impl<S> ArrowServer<S> {
    fn new(service: OtapService) -> Self {
        Self {
            service,
            _svc: PhantomData,
        }
    }
}
impl<S> Clone for ArrowServer<S> {
    fn clone(&self) -> Self {
        Self::new(self.service.clone())
    }
}
impl<S: ArrowSvc> tonic::server::NamedService for ArrowServer<S> {
    const NAME: &'static str = S::NAME;
}

struct StreamingHandler {
    service: OtapService,
    signal: OtapSignal,
}
impl tonic::server::StreamingService<BatchArrowRecords> for StreamingHandler {
    type Response = BatchStatus;
    type ResponseStream = tonic::codegen::BoxStream<BatchStatus>;
    type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;
    fn call(
        &mut self,
        request: tonic::Request<tonic::Streaming<BatchArrowRecords>>,
    ) -> Self::Future {
        let service = self.service.clone();
        let signal = self.signal;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(service.inner.config.queue_depth.max(1));
            tokio::spawn(service.run_stream(signal, request.into_inner(), tx));
            let stream: Self::ResponseStream =
                Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
            Ok(tonic::Response::new(stream))
        })
    }
}

fn unimplemented_response() -> http::Response<tonic::body::Body> {
    let mut response = http::Response::new(tonic::body::Body::default());
    let headers = response.headers_mut();
    headers.insert(
        tonic::Status::GRPC_STATUS,
        (tonic::Code::Unimplemented as i32).into(),
    );
    headers.insert(
        http::header::CONTENT_TYPE,
        tonic::metadata::GRPC_CONTENT_TYPE,
    );
    response
}

impl<S, B> tonic::codegen::Service<http::Request<B>> for ArrowServer<S>
where
    S: ArrowSvc,
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        if req.uri().path() != S::SIGNAL.path() {
            return Box::pin(async { Ok(unimplemented_response()) });
        }
        let service = self.service.clone();
        Box::pin(async move {
            let max = service.inner.config.max_message_bytes;
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                .apply_max_message_size_config(Some(max), Some(max));
            let handler = StreamingHandler {
                service,
                signal: S::SIGNAL,
            };
            Ok(grpc.streaming(handler, req).await)
        })
    }
}

/// Serves the three Arrow services on `listener` until `shutdown` resolves;
/// shutdown is graceful, so it then waits for open streams to end.
pub async fn serve_otap(
    listener: tokio::net::TcpListener,
    service: OtapService,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), tonic::transport::Error> {
    tonic::transport::Server::builder()
        .add_service(service.traces())
        .add_service(service.metrics())
        .add_service(service.logs())
        .serve_with_incoming_shutdown(
            tonic::transport::server::TcpIncoming::from(listener),
            shutdown,
        )
        .await
}

// ------------------------------------------------------------------ OTLP client

/// OTLP/gRPC export client.
#[derive(Clone)]
pub struct OtlpClient {
    grpc: tonic::client::Grpc<Channel>,
}
impl OtlpClient {
    pub async fn connect(endpoint: &str) -> Result<Self, GrpcError> {
        Ok(Self::new(channel(endpoint).await?))
    }
    pub fn new(channel: Channel) -> Self {
        Self {
            grpc: tonic::client::Grpc::new(channel)
                .max_decoding_message_size(DEFAULT_MAX_MESSAGE_BYTES)
                .max_encoding_message_size(DEFAULT_MAX_MESSAGE_BYTES),
        }
    }
    async fn unary<Req, Resp>(
        &mut self,
        path: &'static str,
        request: Req,
    ) -> Result<Resp, GrpcError>
    where
        Req: prost::Message + Send + Sync + 'static,
        Resp: prost::Message + Default + Send + Sync + 'static,
    {
        self.grpc
            .ready()
            .await
            .map_err(|e| GrpcError::Status(tonic::Status::unavailable(e.to_string())))?;
        let codec = tonic_prost::ProstCodec::<Req, Resp>::default();
        self.grpc
            .unary(
                tonic::Request::new(request),
                http::uri::PathAndQuery::from_static(path),
                codec,
            )
            .await
            .map(tonic::Response::into_inner)
            .map_err(GrpcError::Status)
    }
    pub async fn export_traces(
        &mut self,
        request: ExportTraceServiceRequest,
    ) -> Result<ExportTraceServiceResponse, GrpcError> {
        self.unary(OTLP_TRACES_PATH, request).await
    }
    pub async fn export_metrics(
        &mut self,
        request: ExportMetricsServiceRequest,
    ) -> Result<ExportMetricsServiceResponse, GrpcError> {
        self.unary(OTLP_METRICS_PATH, request).await
    }
    pub async fn export_logs(
        &mut self,
        request: ExportLogsServiceRequest,
    ) -> Result<ExportLogsServiceResponse, GrpcError> {
        self.unary(OTLP_LOGS_PATH, request).await
    }
}

/// Blocking wrapper of [`OtlpClient`] with its own one-worker runtime.
pub struct BlockingOtlpClient {
    runtime: tokio::runtime::Runtime,
    client: OtlpClient,
}
impl BlockingOtlpClient {
    pub fn connect(endpoint: &str) -> Result<Self, GrpcError> {
        let runtime = runtime()?;
        let client = runtime.block_on(OtlpClient::connect(endpoint))?;
        Ok(Self { runtime, client })
    }
    pub fn export_traces(
        &mut self,
        request: ExportTraceServiceRequest,
    ) -> Result<ExportTraceServiceResponse, GrpcError> {
        self.runtime.block_on(self.client.export_traces(request))
    }
    pub fn export_metrics(
        &mut self,
        request: ExportMetricsServiceRequest,
    ) -> Result<ExportMetricsServiceResponse, GrpcError> {
        self.runtime.block_on(self.client.export_metrics(request))
    }
    pub fn export_logs(
        &mut self,
        request: ExportLogsServiceRequest,
    ) -> Result<ExportLogsServiceResponse, GrpcError> {
        self.runtime.block_on(self.client.export_logs(request))
    }
}

// ------------------------------------------------------------------ OTLP server

/// One accepted OTLP export request.
#[derive(Debug)]
pub enum OtlpReceived {
    Traces(ExportTraceServiceRequest),
    Metrics(ExportMetricsServiceRequest),
    Logs(ExportLogsServiceRequest),
}

/// The OTLP receiver: queues export requests, `RESOURCE_EXHAUSTED` when
/// the queue stays full for `admission_timeout`.
pub struct OtlpService {
    inner: Arc<ServerInner<OtlpReceived>>,
}
impl Clone for OtlpService {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl OtlpService {
    pub fn new(config: ServerConfig) -> (Self, mpsc::Receiver<OtlpReceived>) {
        let (sink, rx) = mpsc::channel(config.queue_depth.max(1));
        (
            Self {
                inner: Arc::new(ServerInner { sink, config }),
            },
            rx,
        )
    }
    pub fn traces(&self) -> OtlpServer<OtlpTraces> {
        OtlpServer::new(self.clone())
    }
    pub fn metrics(&self) -> OtlpServer<OtlpMetrics> {
        OtlpServer::new(self.clone())
    }
    pub fn logs(&self) -> OtlpServer<OtlpLogs> {
        OtlpServer::new(self.clone())
    }
}

/// Names one OTLP collector service for [`OtlpServer`].
pub trait OtlpSvc: Send + Sync + 'static {
    const NAME: &'static str;
    const PATH: &'static str;
    type Request: prost::Message + Default + Send + Sync + 'static;
    type Response: prost::Message + Default + Send + Sync + 'static;
    fn wrap(request: Self::Request) -> OtlpReceived;
}
pub struct OtlpTraces;
pub struct OtlpMetrics;
pub struct OtlpLogs;
impl OtlpSvc for OtlpTraces {
    const NAME: &'static str = "opentelemetry.proto.collector.trace.v1.TraceService";
    const PATH: &'static str = OTLP_TRACES_PATH;
    type Request = ExportTraceServiceRequest;
    type Response = ExportTraceServiceResponse;
    fn wrap(request: Self::Request) -> OtlpReceived {
        OtlpReceived::Traces(request)
    }
}
impl OtlpSvc for OtlpMetrics {
    const NAME: &'static str = "opentelemetry.proto.collector.metrics.v1.MetricsService";
    const PATH: &'static str = OTLP_METRICS_PATH;
    type Request = ExportMetricsServiceRequest;
    type Response = ExportMetricsServiceResponse;
    fn wrap(request: Self::Request) -> OtlpReceived {
        OtlpReceived::Metrics(request)
    }
}
impl OtlpSvc for OtlpLogs {
    const NAME: &'static str = "opentelemetry.proto.collector.logs.v1.LogsService";
    const PATH: &'static str = OTLP_LOGS_PATH;
    type Request = ExportLogsServiceRequest;
    type Response = ExportLogsServiceResponse;
    fn wrap(request: Self::Request) -> OtlpReceived {
        OtlpReceived::Logs(request)
    }
}

pub struct OtlpServer<S> {
    service: OtlpService,
    _svc: PhantomData<fn() -> S>,
}
impl<S> OtlpServer<S> {
    fn new(service: OtlpService) -> Self {
        Self {
            service,
            _svc: PhantomData,
        }
    }
}
impl<S> Clone for OtlpServer<S> {
    fn clone(&self) -> Self {
        Self::new(self.service.clone())
    }
}
impl<S: OtlpSvc> tonic::server::NamedService for OtlpServer<S> {
    const NAME: &'static str = S::NAME;
}

struct UnaryHandler<S> {
    service: OtlpService,
    _svc: PhantomData<fn() -> S>,
}
impl<S: OtlpSvc> tonic::server::UnaryService<S::Request> for UnaryHandler<S> {
    type Response = S::Response;
    type Future = BoxFuture<tonic::Response<S::Response>, tonic::Status>;
    fn call(&mut self, request: tonic::Request<S::Request>) -> Self::Future {
        let inner = self.service.inner.clone();
        Box::pin(async move {
            let permit = tokio::time::timeout(inner.config.admission_timeout, inner.sink.reserve())
                .await
                .map_err(|_| tonic::Status::resource_exhausted("receiver queue full"))?
                .map_err(|_| tonic::Status::unavailable("receiver closed"))?;
            permit.send(S::wrap(request.into_inner()));
            Ok(tonic::Response::new(S::Response::default()))
        })
    }
}

impl<S, B> tonic::codegen::Service<http::Request<B>> for OtlpServer<S>
where
    S: OtlpSvc,
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        if req.uri().path() != S::PATH {
            return Box::pin(async { Ok(unimplemented_response()) });
        }
        let service = self.service.clone();
        Box::pin(async move {
            let max = service.inner.config.max_message_bytes;
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                S::Response,
                S::Request,
            >::default())
            .apply_max_message_size_config(Some(max), Some(max));
            let handler = UnaryHandler::<S> {
                service,
                _svc: PhantomData,
            };
            Ok(grpc.unary(handler, req).await)
        })
    }
}

/// Serves the three OTLP collector services on `listener` until `shutdown`.
pub async fn serve_otlp(
    listener: tokio::net::TcpListener,
    service: OtlpService,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), tonic::transport::Error> {
    tonic::transport::Server::builder()
        .add_service(service.traces())
        .add_service(service.metrics())
        .add_service(service.logs())
        .serve_with_incoming_shutdown(
            tonic::transport::server::TcpIncoming::from(listener),
            shutdown,
        )
        .await
}
