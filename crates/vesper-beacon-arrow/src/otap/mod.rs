//! OpenTelemetry Arrow Protocol (OTAP) export for Beacon (§22.3, §22.4).
//!
//! * [`proto`]: `BatchArrowRecords`, `ArrowPayload`, `BatchStatus`.
//! * [`TraceEncoder`] / [`MetricsEncoder`]: long-lived per-payload Arrow IPC
//!   streams with stream ids, schema reset and delta dictionaries.
//! * [`OtapDecoder`]: the bounded consumer.
//! * [`SpanAttributes`] and [`PiiPolicy`]: the PII redaction boundary.
//! * [`FramedTransport`], [`OtapReceiver`], exporters: gRPC-style message
//!   framing over a byte stream (not HTTP/2 gRPC).
//!
//! Allocating host adapter: no zero-allocation, zero-copy, `IORING_OP_SEND_ZC`
//! or fiber-propagation claim. Source `Trace` effect propagation is a
//! compiler step and is not part of this crate.

mod attributes;
mod decode;
mod encode;
mod ipc;
pub mod proto;
pub mod schema;
mod transport;

pub use attributes::*;
pub use decode::*;
pub use encode::*;
pub use transport::*;
