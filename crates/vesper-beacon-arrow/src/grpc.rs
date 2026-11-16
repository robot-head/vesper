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
//! **Compression.** Clients compress requests with zstd by default
//! ([`GrpcCompression`]) and accept zstd and gzip responses. Servers accept
//! zstd and gzip, plus upstream's `zstdarrow1` to `zstdarrow10` encodings
//! (the collector's `otelarrow` exporter's default: ordinary zstd frames at
//! a named level, decoded as `zstd`). Decompressed messages are bounded by
//! `max_message_bytes`. Arrow IPC buffer compression is handled by the
//! decoder (see [`EncoderConfig::ipc_compression`]).
//!
//! **TLS and auth.** [`ClientTls`] and [`ServerTls`] configure rustls 0.23
//! (ring provider) through tonic; `https://` endpoints need a client TLS
//! configuration. A [`BearerToken`] is sent as `authorization: Bearer ...`
//! on every call, and only over TLS unless `allow_insecure_auth` is set; a
//! server with `bearer_tokens` rejects other calls with `UNAUTHENTICATED`
//! before reading any message, comparing tokens in constant time.
//!
//! **Headers.** The server HPACK-decodes each batch's `headers` in stream
//! order ([`HeaderPolicy::Decode`], [`DEFAULT_MAX_HEADER_BYTES`]) into
//! [`DecodedBatch::header_fields`].
//!
//! [`EncoderConfig::ipc_compression`]: crate::otap::EncoderConfig::ipc_compression

use crate::otap::proto::{BatchArrowRecords, BatchStatus, StatusCode};
use crate::otap::{
    BatchTransport, DecodeLimits, DecodedBatch, HeaderPolicy, OtapDecoder, TransportError,
};
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
use tonic::codec::CompressionEncoding;
use tonic::codegen::{http, tokio_stream, Body, BoxFuture, StdError};
use tonic::transport::{
    Certificate, Channel, ClientTlsConfig, Endpoint, Identity, ServerTlsConfig,
};

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

/// Default ceiling for one batch's raw HPACK `headers` at the server.
pub const DEFAULT_MAX_HEADER_BYTES: usize = 64 << 10;

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
    /// A bearer token would be sent without TLS (see
    /// [`ConnectOptions::allow_insecure_auth`]).
    InsecureAuth,
    /// A bearer token with characters outside the RFC 6750 `b64token` set.
    InvalidToken,
    /// An `https://` endpoint without a [`ClientTls`] configuration, or the
    /// reverse.
    TlsMismatch,
    /// The TLS configuration was rejected.
    Tls(tonic::transport::Error),
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

/// gRPC message compression a client applies to its requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GrpcCompression {
    None,
    #[default]
    Zstd,
    Gzip,
}
impl GrpcCompression {
    fn encoding(self) -> Option<CompressionEncoding> {
        match self {
            Self::None => None,
            Self::Zstd => Some(CompressionEncoding::Zstd),
            Self::Gzip => Some(CompressionEncoding::Gzip),
        }
    }
}

/// An OAuth 2.0 bearer token (RFC 6750). Its `Debug` form hides it.
#[derive(Clone)]
pub struct BearerToken(String);
impl BearerToken {
    /// Accepts a nonempty RFC 6750 `b64token`: ASCII letters, digits and
    /// `-._~+/`, followed by any number of `=`.
    pub fn new(token: impl Into<String>) -> Result<Self, GrpcError> {
        let token = token.into();
        let body = token.trim_end_matches('=');
        let ok = !body.is_empty()
            && body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b));
        if ok {
            Ok(Self(token))
        } else {
            Err(GrpcError::InvalidToken)
        }
    }
    fn header(&self) -> tonic::metadata::MetadataValue<tonic::metadata::Ascii> {
        format!("Bearer {}", self.0)
            .parse()
            .expect("b64token characters are valid metadata")
    }
    /// Constant-time comparison with a received token.
    fn matches(&self, received: &[u8]) -> bool {
        use subtle::ConstantTimeEq;
        self.0.as_bytes().ct_eq(received).into()
    }
}
impl std::fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerToken(<hidden>)")
    }
}

/// Client-side TLS: trust anchors (PEM), the expected server name when it
/// differs from the endpoint host, and an optional client identity (mTLS).
#[derive(Clone, Debug, Default)]
pub struct ClientTls {
    pub ca_pem: Vec<u8>,
    pub domain: Option<String>,
    /// Certificate chain and private key, both PEM.
    pub identity: Option<(Vec<u8>, Vec<u8>)>,
}
/// Server-side TLS: the certificate chain and key (PEM), and the client CA
/// (PEM) when clients must present certificates.
#[derive(Clone, Debug)]
pub struct ServerTls {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    pub client_ca_pem: Option<Vec<u8>>,
}

/// How a client reaches its peer.
#[derive(Clone, Debug, Default)]
pub struct ConnectOptions {
    pub tls: Option<ClientTls>,
    pub bearer_token: Option<BearerToken>,
    /// Allow a bearer token over plaintext HTTP/2 (tests, loopback).
    pub allow_insecure_auth: bool,
}

/// A connected channel with the caller's auth header.
#[derive(Clone)]
pub struct Connection {
    channel: Channel,
    auth: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
}
impl Connection {
    /// A plaintext connection (no TLS, no token).
    pub async fn plaintext(endpoint: &str) -> Result<Self, GrpcError> {
        Self::connect(endpoint, &ConnectOptions::default()).await
    }
    pub async fn connect(endpoint: &str, options: &ConnectOptions) -> Result<Self, GrpcError> {
        let https = endpoint.starts_with("https://");
        if https != options.tls.is_some() {
            return Err(GrpcError::TlsMismatch);
        }
        if options.bearer_token.is_some() && !https && !options.allow_insecure_auth {
            return Err(GrpcError::InsecureAuth);
        }
        let mut ep = Endpoint::from_shared(endpoint.to_owned())
            .map_err(|_| GrpcError::InvalidEndpoint(endpoint.to_owned()))?
            .connect_timeout(Duration::from_secs(10));
        if let Some(tls) = &options.tls {
            let mut config =
                ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&tls.ca_pem));
            if let Some(domain) = &tls.domain {
                config = config.domain_name(domain.clone());
            }
            if let Some((cert, key)) = &tls.identity {
                config = config.identity(Identity::from_pem(cert, key));
            }
            ep = ep.tls_config(config).map_err(GrpcError::Tls)?;
        }
        let channel = ep.connect().await.map_err(GrpcError::Connect)?;
        Ok(Self {
            channel,
            auth: options.bearer_token.as_ref().map(BearerToken::header),
        })
    }
    /// An existing channel, with an optional token.
    pub fn from_channel(channel: Channel, bearer_token: Option<&BearerToken>) -> Self {
        Self {
            channel,
            auth: bearer_token.map(BearerToken::header),
        }
    }
    fn authorize<T>(&self, request: &mut tonic::Request<T>) {
        if let Some(auth) = &self.auth {
            request.metadata_mut().insert("authorization", auth.clone());
        }
    }
}

// ------------------------------------------------------------------ OTAP client

#[derive(Clone, Copy, Debug)]
pub struct ClientConfig {
    /// Unacknowledged batches allowed on one stream.
    pub max_in_flight: usize,
    /// How long to wait for an acknowledgement before failing the stream.
    pub status_timeout: Duration,
    pub max_message_bytes: usize,
    pub compression: GrpcCompression,
}
impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            max_in_flight: 8,
            status_timeout: Duration::from_secs(30),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            compression: GrpcCompression::Zstd,
        }
    }
}
fn client_grpc(
    channel: Channel,
    max: usize,
    compression: GrpcCompression,
) -> tonic::client::Grpc<Channel> {
    let mut grpc = tonic::client::Grpc::new(channel)
        .max_decoding_message_size(max)
        .max_encoding_message_size(max)
        .accept_compressed(CompressionEncoding::Zstd)
        .accept_compressed(CompressionEncoding::Gzip);
    if let Some(encoding) = compression.encoding() {
        grpc = grpc.send_compressed(encoding);
    }
    grpc
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
        Ok(Self::open(
            Connection::plaintext(endpoint).await?,
            signal,
            config,
        ))
    }
    /// Starts the stream on `connection`. The call runs in a background
    /// task so batches flow before the server sends response headers
    /// (grpc-go servers send them with the first status).
    pub fn open(connection: Connection, signal: OtapSignal, config: ClientConfig) -> Self {
        let window = config.max_in_flight.max(1);
        let (req_tx, req_rx) = mpsc::channel::<BatchArrowRecords>(window);
        let (st_tx, st_rx) = mpsc::channel(window + 1);
        let max = config.max_message_bytes;
        tokio::spawn(async move {
            let mut grpc = client_grpc(connection.channel.clone(), max, config.compression);
            if let Err(e) = grpc.ready().await {
                let _ = st_tx
                    .send(Err(tonic::Status::unavailable(e.to_string())))
                    .await;
                return;
            }
            let codec = tonic_prost::ProstCodec::<BatchArrowRecords, BatchStatus>::default();
            let mut request =
                tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(req_rx));
            connection.authorize(&mut request);
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
    connection: Connection,
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
        Self::connect_with(endpoint, &ConnectOptions::default(), signal, config)
    }
    pub fn connect_with(
        endpoint: &str,
        options: &ConnectOptions,
        signal: OtapSignal,
        config: ClientConfig,
    ) -> Result<Self, GrpcError> {
        let runtime = runtime()?;
        let connection = runtime.block_on(Connection::connect(endpoint, options))?;
        Ok(Self {
            runtime,
            connection,
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
                self.connection.clone(),
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

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Decoded batches the queue holds before admission waits.
    pub queue_depth: usize,
    /// How long a decoded batch may wait for queue space before the
    /// receiver answers `RESOURCE_EXHAUSTED`.
    pub admission_timeout: Duration,
    pub decode_limits: DecodeLimits,
    pub max_message_bytes: usize,
    /// Compression of responses, when the client accepts it.
    pub response_compression: GrpcCompression,
    /// When nonempty, every call must carry one of these tokens.
    pub bearer_tokens: Vec<BearerToken>,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            queue_depth: 64,
            admission_timeout: Duration::from_secs(1),
            decode_limits: DecodeLimits {
                headers: HeaderPolicy::Decode {
                    max_bytes: DEFAULT_MAX_HEADER_BYTES,
                    max_table_size: crate::otap::DEFAULT_HPACK_TABLE_BYTES,
                },
                ..DecodeLimits::default()
            },
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            response_compression: GrpcCompression::None,
            bearer_tokens: Vec::new(),
        }
    }
}
impl ServerConfig {
    fn grpc<C: tonic::codec::Codec>(&self, codec: C) -> tonic::server::Grpc<C> {
        let max = self.max_message_bytes;
        let mut grpc = tonic::server::Grpc::new(codec)
            .apply_max_message_size_config(Some(max), Some(max))
            .accept_compressed(CompressionEncoding::Zstd)
            .accept_compressed(CompressionEncoding::Gzip);
        if let Some(encoding) = self.response_compression.encoding() {
            grpc = grpc.send_compressed(encoding);
        }
        grpc
    }
    /// Whether the request carries an accepted token (or none is needed).
    fn authorized<B>(&self, req: &http::Request<B>) -> bool {
        if self.bearer_tokens.is_empty() {
            return true;
        }
        let Some(value) = req.headers().get(http::header::AUTHORIZATION) else {
            return false;
        };
        let value = value.as_bytes();
        let Some(token) = value
            .get(..7)
            .filter(|scheme| scheme.eq_ignore_ascii_case(b"bearer "))
            .map(|_| &value[7..])
        else {
            return false;
        };
        // Check every token so the time does not depend on which matched.
        self.bearer_tokens
            .iter()
            .fold(false, |ok, t| t.matches(token) | ok)
    }
}

/// Upstream's `zstdarrowN` encodings are zstd frames at level N; tonic
/// knows them as `zstd`.
fn normalize_encoding<B>(req: &mut http::Request<B>) {
    let headers = req.headers_mut();
    let is_zstdarrow = headers
        .get("grpc-encoding")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("zstdarrow"))
        .and_then(|level| level.parse::<u8>().ok())
        .is_some_and(|level| (1..=10).contains(&level));
    if is_zstdarrow {
        headers.insert("grpc-encoding", http::HeaderValue::from_static("zstd"));
    }
}

fn status_response(code: tonic::Code) -> http::Response<tonic::body::Body> {
    let mut response = http::Response::new(tonic::body::Body::default());
    let headers = response.headers_mut();
    headers.insert(tonic::Status::GRPC_STATUS, (code as i32).into());
    headers.insert(
        http::header::CONTENT_TYPE,
        tonic::metadata::GRPC_CONTENT_TYPE,
    );
    response
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
        let config = &self.inner.config;
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
    status_response(tonic::Code::Unimplemented)
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
    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        if req.uri().path() != S::SIGNAL.path() {
            return Box::pin(async { Ok(unimplemented_response()) });
        }
        if !self.service.inner.config.authorized(&req) {
            return Box::pin(async { Ok(status_response(tonic::Code::Unauthenticated)) });
        }
        normalize_encoding(&mut req);
        let service = self.service.clone();
        Box::pin(async move {
            let mut grpc = service
                .inner
                .config
                .grpc(tonic_prost::ProstCodec::default());
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
    serve_otap_tls(listener, service, None, shutdown).await
}
/// [`serve_otap`] over TLS when `tls` is set.
pub async fn serve_otap_tls(
    listener: tokio::net::TcpListener,
    service: OtapService,
    tls: Option<&ServerTls>,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), tonic::transport::Error> {
    server_builder(tls)?
        .add_service(service.traces())
        .add_service(service.metrics())
        .add_service(service.logs())
        .serve_with_incoming_shutdown(
            tonic::transport::server::TcpIncoming::from(listener),
            shutdown,
        )
        .await
}

fn server_builder(
    tls: Option<&ServerTls>,
) -> Result<tonic::transport::Server, tonic::transport::Error> {
    let mut builder = tonic::transport::Server::builder();
    if let Some(tls) = tls {
        let mut config =
            ServerTlsConfig::new().identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem));
        if let Some(ca) = &tls.client_ca_pem {
            config = config.client_ca_root(Certificate::from_pem(ca));
        }
        builder = builder.tls_config(config)?;
    }
    Ok(builder)
}

// ------------------------------------------------------------------ OTLP client

/// OTLP/gRPC export client.
#[derive(Clone)]
pub struct OtlpClient {
    grpc: tonic::client::Grpc<Channel>,
    connection: Connection,
}
impl OtlpClient {
    pub async fn connect(endpoint: &str) -> Result<Self, GrpcError> {
        Ok(Self::new(Connection::plaintext(endpoint).await?))
    }
    pub async fn connect_with(
        endpoint: &str,
        options: &ConnectOptions,
        compression: GrpcCompression,
    ) -> Result<Self, GrpcError> {
        Ok(Self::with_compression(
            Connection::connect(endpoint, options).await?,
            compression,
        ))
    }
    /// zstd-compressed requests ([`GrpcCompression::default`]).
    pub fn new(connection: Connection) -> Self {
        Self::with_compression(connection, GrpcCompression::default())
    }
    pub fn with_compression(connection: Connection, compression: GrpcCompression) -> Self {
        Self {
            grpc: client_grpc(
                connection.channel.clone(),
                DEFAULT_MAX_MESSAGE_BYTES,
                compression,
            ),
            connection,
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
        let mut request = tonic::Request::new(request);
        self.connection.authorize(&mut request);
        self.grpc
            .unary(request, http::uri::PathAndQuery::from_static(path), codec)
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
        Self::connect_with(
            endpoint,
            &ConnectOptions::default(),
            GrpcCompression::default(),
        )
    }
    pub fn connect_with(
        endpoint: &str,
        options: &ConnectOptions,
        compression: GrpcCompression,
    ) -> Result<Self, GrpcError> {
        let runtime = runtime()?;
        let client = runtime.block_on(OtlpClient::connect_with(endpoint, options, compression))?;
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
    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        if req.uri().path() != S::PATH {
            return Box::pin(async { Ok(unimplemented_response()) });
        }
        if !self.service.inner.config.authorized(&req) {
            return Box::pin(async { Ok(status_response(tonic::Code::Unauthenticated)) });
        }
        normalize_encoding(&mut req);
        let service = self.service.clone();
        Box::pin(async move {
            let mut grpc = service
                .inner
                .config
                .grpc(tonic_prost::ProstCodec::<S::Response, S::Request>::default());
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
    serve_otlp_tls(listener, service, None, shutdown).await
}
/// [`serve_otlp`] over TLS when `tls` is set.
pub async fn serve_otlp_tls(
    listener: tokio::net::TcpListener,
    service: OtlpService,
    tls: Option<&ServerTls>,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), tonic::transport::Error> {
    server_builder(tls)?
        .add_service(service.traces())
        .add_service(service.metrics())
        .add_service(service.logs())
        .serve_with_incoming_shutdown(
            tonic::transport::server::TcpIncoming::from(listener),
            shutdown,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::{http, normalize_encoding};

    fn normalized(encoding: &str) -> String {
        let mut req = http::Request::builder()
            .header("grpc-encoding", encoding)
            .body(())
            .unwrap();
        normalize_encoding(&mut req);
        req.headers()["grpc-encoding"].to_str().unwrap().to_owned()
    }

    #[test]
    fn upstream_zstdarrow_levels_read_as_zstd() {
        for level in 1..=10 {
            assert_eq!(normalized(&format!("zstdarrow{level}")), "zstd");
        }
        for other in [
            "zstd",
            "gzip",
            "identity",
            "zstdarrow0",
            "zstdarrow11",
            "zstdarrow",
        ] {
            assert_eq!(normalized(other), other);
        }
    }
}
