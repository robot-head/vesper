//! A bounded, socket-independent HTTP/1.1 connection state machine.
//!
//! [`Connection`] composes the strict [`Decoder`] and the fixed-length
//! [`ResponseBuilder`]. The transport owner feeds received bytes with
//! [`Connection::receive`], drains requests with [`Connection::poll_request`],
//! submits one [`Response`] per [`RequestId`] in any order, and copies
//! [`Connection::pending_output`] to its socket, reporting how many bytes the
//! socket accepted with [`Connection::advance_output`]. Responses reach the wire
//! strictly in request order; out-of-order completions wait in their slot.
//!
//! Close policy, all deterministic in event order:
//! * A request carrying `Connection: close` is the last one issued. Its
//!   response always says `Connection: close`.
//! * A response with [`Response::close`] ends the connection after it. Later
//!   requests that were already issued are abandoned: their submissions are
//!   rejected (returning the response) and any already-submitted later
//!   responses are returned through [`Connection::take_abandoned`].
//! * After [`Connection::end_input`], complete pipelined requests still drain.
//!   The connection closes after the last outstanding response. A partial frame
//!   at EOF closes without an error response.
//! * A parse error poisons the decoder; it is never asked to resynchronize.
//!   Responses for earlier requests are still written in order, followed by a
//!   minimal `Connection: close` error response (400, 413, 417, 431, 501 or
//!   505), then the connection is closed. Later pipelined bytes are never
//!   interpreted.
//!
//! The connection never sends or closes a socket and has no timers. HEAD is
//! applied from the associated request automatically. Rust can still drop a
//! [`Connection`], [`Response`] or [`EncodedResponse`]; this is not Vesper's
//! source-level linear responder.
//!
//! ```
//! use vesper_prism::connection::{Connection, ConnectionLimits, RequestEvent, Response};
//! let mut connection = Connection::new(ConnectionLimits::default())?;
//! connection.receive(b"HEAD /a HTTP/1.1\r\nHost: x\r\n\r\n")?;
//! let RequestEvent::Request(inbound) = connection.poll_request() else { unreachable!() };
//! connection.submit(inbound.id, Response::new(200, b"body".to_vec())).unwrap();
//! assert_eq!(connection.pending_output(),
//!     b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\n");
//! # Ok::<(), vesper_prism::connection::ConnectionError>(())
//! ```

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    Error, Limits, Request,
    response::{EncodedResponse, ResponseError, ResponseLimits},
};

pub use crate::forward::connection::Connection;

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// A process-unique id, so a `RequestId` from another connection is foreign.
pub(crate) fn next_connection_id() -> u64 {
    NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed)
}

/// Opaque association between one issued request and its single response.
/// Ids from another connection are rejected as foreign.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId {
    pub(crate) connection: u64,
    pub(crate) sequence: u64,
}
impl RequestId {
    /// Zero-based position of this request on its connection.
    pub fn sequence(self) -> u64 {
        self.sequence
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
    pub request: Limits,
    pub response: ResponseLimits,
    /// Issued requests whose responses have not yet moved to the wire queue.
    pub max_in_flight: usize,
    /// Encoded bytes retained for this connection: unsent wire output plus
    /// completed out-of-order responses. Must admit one maximal response.
    /// Out-of-order completions are admitted only while one maximal response
    /// (`response.max_total_bytes`) still fits, so the earliest outstanding
    /// response can always be submitted once unsent output has been written.
    /// A generated parse-error response (under 128 bytes) may exceed it once.
    pub max_outbound_bytes: usize,
}
impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            request: Limits::default(),
            response: ResponseLimits::default(),
            max_in_flight: 16,
            max_outbound_bytes: 4 * 1024 * 1024,
        }
    }
}

/// An application-owned response before framing. Connection and Content-Length
/// fields are produced by the encoder; HEAD suppression is automatic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    /// The representation; for a HEAD request its length is sent, not its bytes.
    pub body: Vec<u8>,
    /// Close the connection after this response.
    pub close: bool,
}
impl Response {
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
            close: false,
        }
    }
    pub fn header(mut self, name: &str, value: &[u8]) -> Self {
        self.headers.push((name.to_owned(), value.to_vec()));
        self
    }
    pub fn with_close(mut self) -> Self {
        self.close = true;
        self
    }
}

#[derive(Debug)]
pub struct Inbound {
    pub id: RequestId,
    pub request: Request,
}

#[derive(Debug)]
pub enum RequestEvent {
    Request(Inbound),
    /// Incomplete input; receive more bytes.
    NeedInput,
    /// `max_in_flight` responses are outstanding; submit or write first.
    Backpressure,
    /// The decoder failed. This is reported once; then `NoMoreRequests`.
    Failed(Error),
    NoMoreRequests,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitErrorKind {
    /// The id belongs to a different connection.
    Foreign,
    /// No request with this id was ever issued here.
    Unknown,
    /// A response for this id is already queued.
    Duplicate,
    /// This id's response was already moved to the wire.
    Stale,
    /// An earlier response closed the connection before this one.
    Abandoned,
    /// Retained encoded bytes would exceed `max_outbound_bytes`; retry later.
    OutboundLimit,
    Encode(ResponseError),
}

/// A rejected submission. The response is returned to its owner unchanged.
#[derive(Debug)]
pub struct SubmitError {
    pub kind: SubmitErrorKind,
    pub response: Response,
}
impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "response submission rejected: {:?}", self.kind)
    }
}
impl std::error::Error for SubmitError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    InvalidLimits,
    Decoder(Error),
    InputEnded,
    /// `advance_output` was told more bytes than were pending.
    OutputOverrun {
        pending: usize,
        advanced: usize,
    },
}
impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connection error: {self:?}")
    }
}
impl std::error::Error for ConnectionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Open,
    /// No further requests will be issued; responses/output remain.
    Closing,
    /// Everything owed has been written; the transport should close.
    Closed,
}

/// A request issued but cut off by an earlier closing response.
#[derive(Debug)]
pub struct Abandoned {
    pub id: RequestId,
    /// Present when its response had already been submitted.
    pub response: Option<EncodedResponse>,
}

/// Encoded bytes moved to the wire queue but not acknowledged as written.
#[derive(Debug, PartialEq, Eq)]
pub struct UnsentOutput {
    /// `None` for a connection-generated error response.
    pub id: Option<RequestId>,
    pub bytes: Vec<u8>,
    /// Prefix of `bytes` already reported written.
    pub written: usize,
}

/// Every owner retained at shutdown. Nothing is silently discarded.
#[derive(Debug)]
pub struct Shutdown {
    pub unsent: Vec<UnsentOutput>,
    /// Completed responses still waiting behind an earlier outstanding request.
    pub blocked: Vec<(RequestId, EncodedResponse)>,
    /// Issued requests with no submitted response.
    pub awaiting: Vec<RequestId>,
    pub abandoned: Vec<Abandoned>,
    /// Received bytes never issued as requests (pipelined after a close or error).
    pub unprocessed_input: Vec<u8>,
    /// Bytes received after the connection stopped issuing requests.
    pub discarded_input: usize,
    pub failure: Option<Error>,
}

// It needs an id that was never issued, which the public API cannot make.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_issued_sequence_is_unknown_and_returns_the_response() {
        let mut connection = Connection::new(ConnectionLimits::default()).unwrap();
        let id = connection.request_id(0);
        let response = Response::new(200, b"x".to_vec());
        let error = connection.submit(id, response.clone()).unwrap_err();
        assert_eq!(error.kind, SubmitErrorKind::Unknown);
        assert_eq!(error.response, response);
        assert!(connection.pending_output().is_empty());
    }
}
