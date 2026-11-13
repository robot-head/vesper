//! Batch transport.
//!
//! [`BatchTransport`] sends one `BatchArrowRecords` and returns its
//! `BatchStatus`. [`FramedTransport`] implements it over any byte stream
//! using the gRPC length-prefixed message framing (1-byte compressed flag,
//! 4-byte big-endian length, protobuf body) for both directions. It is **not**
//! gRPC: there is no HTTP/2, no `:path` routing to
//! `/opentelemetry.proto.experimental.arrow.v1.ArrowTracesService/ArrowTraces`
//! and no trailers, so it does not interoperate with an OTel collector
//! directly. It carries the same messages in the same per-message framing.
//!
//! [`OtapReceiver`] is the matching server loop and [`TraceExporter`] /
//! [`MetricsExporter`] combine an encoder with a transport. A failed send or
//! a non-OK status resets the encoder's streams, because the receiver may not
//! have applied the dictionary deltas of the failed batch.

use super::decode::{DecodeError, DecodedBatch, OtapDecoder};
use super::encode::{EncodeError, HistogramPoint, MetricsEncoder, OtapSpan, TraceEncoder};
use super::proto::{BatchArrowRecords, BatchStatus, StatusCode};
use prost::Message;
use std::io::{self, Read, Write};
use vesper_beacon::TelemetryRing;

/// Default ceiling for one received frame body.
pub const DEFAULT_MAX_FRAME: usize = 16 << 20;

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    /// Peer closed the stream before a complete frame.
    UnexpectedEof,
    FrameTooLarge {
        len: usize,
        max: usize,
    },
    /// Compressed frames are not supported (no `grpc-encoding` negotiation).
    CompressedFrame,
    BadCompressionFlag(u8),
    Protobuf(prost::DecodeError),
    BatchIdMismatch {
        sent: i64,
        received: i64,
    },
}
impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TransportError {}
impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Self::UnexpectedEof
        } else {
            Self::Io(e)
        }
    }
}

/// Writes one gRPC-style length-prefixed, uncompressed message.
pub fn write_frame(out: &mut impl Write, message: &impl Message) -> Result<(), TransportError> {
    let len = message.encoded_len();
    let len32 = u32::try_from(len).map_err(|_| TransportError::FrameTooLarge {
        len,
        max: u32::MAX as usize,
    })?;
    let mut frame = Vec::with_capacity(5 + len);
    frame.push(0);
    frame.extend_from_slice(&len32.to_be_bytes());
    message
        .encode(&mut frame)
        .expect("Vec grows to the encoded length");
    out.write_all(&frame)?;
    out.flush()?;
    Ok(())
}

/// Reads one frame body. `Ok(None)` means a clean end of stream before any
/// byte of a new frame. The length is checked before any allocation.
pub fn read_frame(input: &mut impl Read, max: usize) -> Result<Option<Vec<u8>>, TransportError> {
    let mut header = [0u8; 5];
    let mut got = 0;
    while got < header.len() {
        match input.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(TransportError::UnexpectedEof),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    match header[0] {
        0 => {}
        1 => return Err(TransportError::CompressedFrame),
        flag => return Err(TransportError::BadCompressionFlag(flag)),
    }
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > max {
        return Err(TransportError::FrameTooLarge { len, max });
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

pub trait BatchTransport {
    /// Sends one batch and returns the peer's status for it.
    fn exchange(&mut self, batch: &BatchArrowRecords) -> Result<BatchStatus, TransportError>;
}

/// Request/response framing over a bidirectional byte stream.
pub struct FramedTransport<S> {
    stream: S,
    max_frame: usize,
}
impl<S: Read + Write> FramedTransport<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            max_frame: DEFAULT_MAX_FRAME,
        }
    }
    pub fn with_max_frame(mut self, max: usize) -> Self {
        self.max_frame = max;
        self
    }
    pub fn into_inner(self) -> S {
        self.stream
    }
}
impl<S: Read + Write> BatchTransport for FramedTransport<S> {
    fn exchange(&mut self, batch: &BatchArrowRecords) -> Result<BatchStatus, TransportError> {
        write_frame(&mut self.stream, batch)?;
        let body =
            read_frame(&mut self.stream, self.max_frame)?.ok_or(TransportError::UnexpectedEof)?;
        let status = BatchStatus::decode(body.as_slice()).map_err(TransportError::Protobuf)?;
        if status.batch_id != batch.batch_id {
            return Err(TransportError::BatchIdMismatch {
                sent: batch.batch_id,
                received: status.batch_id,
            });
        }
        Ok(status)
    }
}

/// Summary of one served connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServeSummary {
    pub accepted: u64,
    pub rejected: u64,
}

/// Server side of [`FramedTransport`]: decodes every batch and answers with
/// `OK`, or `INVALID_ARGUMENT` (and a reset decoder) for a batch it cannot
/// decode. Frame-level errors end the connection.
pub struct OtapReceiver {
    pub decoder: OtapDecoder,
    pub max_frame: usize,
}
impl Default for OtapReceiver {
    fn default() -> Self {
        Self {
            decoder: OtapDecoder::default(),
            max_frame: DEFAULT_MAX_FRAME,
        }
    }
}
impl OtapReceiver {
    pub fn serve<S: Read + Write>(
        &mut self,
        mut stream: S,
        mut on_batch: impl FnMut(DecodedBatch),
    ) -> Result<ServeSummary, TransportError> {
        let mut summary = ServeSummary::default();
        while let Some(body) = read_frame(&mut stream, self.max_frame)? {
            let status = match BatchArrowRecords::decode(body.as_slice()) {
                Err(e) => {
                    self.decoder.reset();
                    BatchStatus {
                        batch_id: 0,
                        status_code: StatusCode::InvalidArgument as i32,
                        status_message: format!("protobuf: {e}"),
                    }
                }
                Ok(batch) => match self.decoder.decode(&batch) {
                    Ok(decoded) => {
                        on_batch(decoded);
                        BatchStatus::ok(batch.batch_id)
                    }
                    Err(e) => BatchStatus {
                        batch_id: batch.batch_id,
                        status_code: StatusCode::InvalidArgument as i32,
                        status_message: e.to_string(),
                    },
                },
            };
            if status.is_ok() {
                summary.accepted += 1;
            } else {
                summary.rejected += 1;
            }
            write_frame(&mut stream, &status)?;
        }
        Ok(summary)
    }
}

#[derive(Debug)]
pub enum ExportFailure {
    Encode(EncodeError),
    Transport(TransportError),
    /// The receiver answered with a non-OK status.
    Rejected(BatchStatus),
}
impl std::fmt::Display for ExportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ExportFailure {}
impl From<DecodeError> for ExportFailure {
    fn from(e: DecodeError) -> Self {
        Self::Rejected(BatchStatus {
            batch_id: 0,
            status_code: StatusCode::InvalidArgument as i32,
            status_message: e.to_string(),
        })
    }
}

fn send<T: BatchTransport>(
    transport: &mut T,
    batch: &BatchArrowRecords,
) -> Result<(), ExportFailure> {
    let status = transport
        .exchange(batch)
        .map_err(ExportFailure::Transport)?;
    if status.is_ok() {
        Ok(())
    } else {
        Err(ExportFailure::Rejected(status))
    }
}

pub struct TraceExporter<T> {
    encoder: TraceEncoder,
    transport: T,
}
impl<T: BatchTransport> TraceExporter<T> {
    pub fn new(encoder: TraceEncoder, transport: T) -> Self {
        Self { encoder, transport }
    }
    pub fn encoder(&self) -> &TraceEncoder {
        &self.encoder
    }
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
    /// Encodes, sends and waits for an OK status; returns the batch id.
    pub fn export_spans(&mut self, spans: &[OtapSpan]) -> Result<i64, ExportFailure> {
        let batch = self
            .encoder
            .encode_spans(spans)
            .map_err(ExportFailure::Encode)?;
        match send(&mut self.transport, &batch) {
            Ok(()) => Ok(batch.batch_id),
            Err(e) => {
                self.encoder.reset();
                Err(e)
            }
        }
    }
    /// Exports up to `max_rows` queued records (primary attribute only) and
    /// removes them **only after** the receiver acknowledged the batch. Any
    /// encode, transport or rejection error leaves every record queued.
    pub fn export_ring<const N: usize>(
        &mut self,
        ring: &mut TelemetryRing<N>,
        max_rows: usize,
    ) -> Result<usize, ExportFailure> {
        ring.try_consume_prefix(max_rows, |records| {
            let spans: Vec<OtapSpan> = records.map(OtapSpan::from).collect();
            self.export_spans(&spans).map(|_| spans.len())
        })
    }
}

pub struct MetricsExporter<T> {
    encoder: MetricsEncoder,
    transport: T,
}
impl<T: BatchTransport> MetricsExporter<T> {
    pub fn new(encoder: MetricsEncoder, transport: T) -> Self {
        Self { encoder, transport }
    }
    pub fn export_histograms(
        &mut self,
        points: &[HistogramPoint<'_>],
    ) -> Result<i64, ExportFailure> {
        let batch = self
            .encoder
            .encode_histograms(points)
            .map_err(ExportFailure::Encode)?;
        match send(&mut self.transport, &batch) {
            Ok(()) => Ok(batch.batch_id),
            Err(e) => {
                self.encoder.reset();
                Err(e)
            }
        }
    }
}

/// In-process transport that hands each batch to a decoder, for tests and
/// for embedding a receiver without a byte stream.
pub struct LoopbackTransport<F> {
    pub decoder: OtapDecoder,
    pub sink: F,
}
impl<F: FnMut(DecodedBatch)> BatchTransport for LoopbackTransport<F> {
    fn exchange(&mut self, batch: &BatchArrowRecords) -> Result<BatchStatus, TransportError> {
        // Round-trip through protobuf bytes so the wire encoding is exercised.
        let bytes = batch.encode_to_vec();
        let batch =
            BatchArrowRecords::decode(bytes.as_slice()).map_err(TransportError::Protobuf)?;
        Ok(match self.decoder.decode(&batch) {
            Ok(decoded) => {
                (self.sink)(decoded);
                BatchStatus::ok(batch.batch_id)
            }
            Err(e) => BatchStatus {
                batch_id: batch.batch_id,
                status_code: StatusCode::InvalidArgument as i32,
                status_message: e.to_string(),
            },
        })
    }
}
