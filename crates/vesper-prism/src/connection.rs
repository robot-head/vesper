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
    collections::VecDeque,
    fmt, io,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    Decoder, Error, Limits, Request, RequestHead,
    response::{
        ConnectionPolicy, EncodedResponse, ResponseBuilder, ResponseContext, ResponseError,
        ResponseLimits,
    },
};

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// Opaque association between one issued request and its single response.
/// Ids from another connection are rejected as foreign.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId {
    connection: u64,
    sequence: u64,
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

enum SlotState {
    Awaiting,
    Ready(EncodedResponse),
    /// Parse failure marker, optionally with a generated error response.
    Failure(Option<EncodedResponse>),
}
struct Slot {
    id: Option<RequestId>,
    head: bool,
    request_close: bool,
    state: SlotState,
}
struct Chunk {
    id: Option<RequestId>,
    bytes: Vec<u8>,
}

pub struct Connection {
    connection: u64,
    limits: ConnectionLimits,
    decoder: Decoder,
    /// Sequence of `slots[0]`; equal to `next_sequence` when empty.
    base_sequence: u64,
    next_sequence: u64,
    slots: VecDeque<Slot>,
    outbound: VecDeque<Chunk>,
    cursor: usize,
    /// Encoded bytes in Ready slots plus unwritten outbound bytes.
    retained: usize,
    /// The connection closes after this sequence's response.
    close_after: Option<u64>,
    stop_issuing: bool,
    input_ended: bool,
    close_promoted: bool,
    failure: Option<Error>,
    failure_reported: bool,
    abandoned: Vec<Abandoned>,
    discarded_input: usize,
}

impl Connection {
    pub fn new(limits: ConnectionLimits) -> Result<Self, ConnectionError> {
        if limits.max_in_flight == 0 || limits.max_outbound_bytes < limits.response.max_total_bytes
        {
            return Err(ConnectionError::InvalidLimits);
        }
        let decoder = Decoder::new(limits.request).map_err(ConnectionError::Decoder)?;
        let mut slots = VecDeque::new();
        slots
            .try_reserve_exact(limits.max_in_flight)
            .map_err(|_| ConnectionError::Decoder(Error::AllocationFailed))?;
        Ok(Self {
            connection: NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
            limits,
            decoder,
            base_sequence: 0,
            next_sequence: 0,
            slots,
            outbound: VecDeque::new(),
            cursor: 0,
            retained: 0,
            close_after: None,
            stop_issuing: false,
            input_ended: false,
            close_promoted: false,
            failure: None,
            failure_reported: false,
            abandoned: Vec::new(),
            discarded_input: 0,
        })
    }
    pub fn limits(&self) -> ConnectionLimits {
        self.limits
    }

    /// Accept a prefix of `bytes` that fits the bounded input queue and return
    /// its length; the caller retains the rest. `Ok(0)` with non-empty input
    /// is backpressure: poll requests (and submit/write responses) first.
    /// Once no further requests can be issued, input is counted and discarded.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<usize, ConnectionError> {
        if self.input_ended {
            return Err(ConnectionError::InputEnded);
        }
        if self.stop_issuing {
            self.discarded_input = self.discarded_input.saturating_add(bytes.len());
            return Ok(bytes.len());
        }
        let capacity = self.limits.request.max_buffered_bytes - self.decoder.buffered_bytes();
        let accepted = bytes.len().min(capacity);
        self.decoder
            .push(&bytes[..accepted])
            .map_err(ConnectionError::Decoder)?;
        Ok(accepted)
    }
    /// Transport EOF from the peer. Complete queued requests can still drain.
    pub fn end_input(&mut self) {
        self.input_ended = true;
        self.decoder.end_input();
    }

    /// Issue at most one request. Performs one bounded decoder step.
    pub fn poll_request(&mut self) -> RequestEvent {
        if let Some(error) = self.failure
            && !self.failure_reported
        {
            self.failure_reported = true;
            return RequestEvent::Failed(error);
        }
        if self.stop_issuing {
            return RequestEvent::NoMoreRequests;
        }
        if self.slots.len() >= self.limits.max_in_flight {
            return RequestEvent::Backpressure;
        }
        match self.decoder.next_request() {
            Ok(Some(decoded)) => {
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                let id = RequestId {
                    connection: self.connection,
                    sequence,
                };
                let head = &decoded.request.head;
                let request_close = wants_close(head);
                self.slots.push_back(Slot {
                    id: Some(id),
                    head: head.method == "HEAD",
                    request_close,
                    state: SlotState::Awaiting,
                });
                if request_close {
                    self.close_after = Some(sequence);
                    self.stop_issuing = true;
                }
                RequestEvent::Request(Inbound {
                    id,
                    request: decoded.request,
                })
            }
            Ok(None) if self.input_ended => {
                self.stop_issuing = true;
                if !self.slots.is_empty() {
                    self.close_after = Some(self.next_sequence - 1);
                }
                RequestEvent::NoMoreRequests
            }
            Ok(None) => RequestEvent::NeedInput,
            Err(error) => {
                self.stop_issuing = true;
                self.failure = Some(error);
                self.failure_reported = true;
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                let response = error_status(error).and_then(|status| {
                    ResponseBuilder::new(
                        status,
                        ResponseContext::Ordinary,
                        ConnectionPolicy::Close,
                        0,
                        self.limits.response,
                    )
                    .and_then(|builder| builder.finish(b""))
                    .ok()
                });
                if let Some(response) = &response {
                    self.retained += response.len();
                }
                self.slots.push_back(Slot {
                    id: None,
                    head: false,
                    request_close: true,
                    state: SlotState::Failure(response),
                });
                self.close_after = Some(sequence);
                self.promote();
                RequestEvent::Failed(error)
            }
        }
    }

    /// Associate a response with its request. On any rejection the response is
    /// returned and the wire queue is unchanged.
    pub fn submit(&mut self, id: RequestId, response: Response) -> Result<(), SubmitError> {
        let reject = |kind, response| Err(SubmitError { kind, response });
        if id.connection != self.connection {
            return reject(SubmitErrorKind::Foreign, response);
        }
        if id.sequence >= self.next_sequence {
            return reject(SubmitErrorKind::Unknown, response);
        }
        if self.close_after.is_some_and(|last| id.sequence > last) {
            return reject(SubmitErrorKind::Abandoned, response);
        }
        if id.sequence < self.base_sequence {
            return reject(SubmitErrorKind::Stale, response);
        }
        let index = (id.sequence - self.base_sequence) as usize;
        let slot = &self.slots[index];
        if !matches!(slot.state, SlotState::Awaiting) {
            return reject(SubmitErrorKind::Duplicate, response);
        }
        let close = response.close || slot.request_close || self.close_after == Some(id.sequence);
        let context = if slot.head {
            ResponseContext::Head
        } else {
            ResponseContext::Ordinary
        };
        let encoded = match encode(&response, context, close, self.limits.response) {
            Ok(encoded) => encoded,
            Err(error) => return reject(SubmitErrorKind::Encode(error), response),
        };
        if self.retained.saturating_add(encoded.len()) > self.limits.max_outbound_bytes {
            return reject(SubmitErrorKind::OutboundLimit, response);
        }
        self.retained += encoded.len();
        self.slots[index].state = SlotState::Ready(encoded);
        if close {
            self.close_at(id.sequence);
        }
        self.promote();
        Ok(())
    }

    /// Responses cut off by a closing response since the last call.
    pub fn take_abandoned(&mut self) -> Vec<Abandoned> {
        std::mem::take(&mut self.abandoned)
    }

    /// The unwritten remainder of the front wire response; empty if none.
    pub fn pending_output(&self) -> &[u8] {
        self.outbound
            .front()
            .map_or(&[][..], |chunk| &chunk.bytes[self.cursor..])
    }
    /// Total unwritten bytes already in wire order.
    pub fn pending_output_len(&self) -> usize {
        self.outbound
            .iter()
            .map(|chunk| chunk.bytes.len())
            .sum::<usize>()
            - self.cursor
    }
    /// Record that the transport accepted `written` bytes of `pending_output`.
    /// Overruns are rejected without changing state.
    pub fn advance_output(&mut self, written: usize) -> Result<(), ConnectionError> {
        let pending = self.pending_output().len();
        if written > pending {
            return Err(ConnectionError::OutputOverrun {
                pending,
                advanced: written,
            });
        }
        self.cursor += written;
        self.retained -= written;
        if pending > 0 && written == pending {
            self.outbound.pop_front();
            self.cursor = 0;
        }
        Ok(())
    }
    /// One `write` call with the pending slice. A short write advances only by
    /// the reported count; an error or zero-length write advances nothing.
    pub fn write_to(&mut self, writer: &mut impl io::Write) -> io::Result<usize> {
        let pending = self.pending_output();
        if pending.is_empty() {
            return Ok(0);
        }
        let written = writer.write(pending)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.advance_output(written)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(written)
    }

    pub fn state(&self) -> ConnectionState {
        if !self.stop_issuing {
            ConnectionState::Open
        } else if self.slots.is_empty() && self.outbound.is_empty() {
            ConnectionState::Closed
        } else {
            ConnectionState::Closing
        }
    }
    pub fn in_flight(&self) -> usize {
        self.slots.len()
    }
    pub fn retained_outbound_bytes(&self) -> usize {
        self.retained
    }
    pub fn buffered_input(&self) -> usize {
        self.decoder.buffered_bytes()
    }
    pub fn failure(&self) -> Option<Error> {
        self.failure
    }

    /// Consume the connection, returning every retained owner and byte.
    pub fn shutdown(mut self) -> Shutdown {
        let mut unsent = Vec::new();
        for (index, chunk) in self.outbound.drain(..).enumerate() {
            unsent.push(UnsentOutput {
                id: chunk.id,
                bytes: chunk.bytes,
                written: if index == 0 { self.cursor } else { 0 },
            });
        }
        let mut blocked = Vec::new();
        let mut awaiting = Vec::new();
        for slot in self.slots.drain(..) {
            match (slot.id, slot.state) {
                (Some(id), SlotState::Ready(response)) => blocked.push((id, response)),
                (Some(id), SlotState::Awaiting) => awaiting.push(id),
                (None, SlotState::Failure(Some(response))) => unsent.push(UnsentOutput {
                    id: None,
                    bytes: response.into_bytes(),
                    written: 0,
                }),
                _ => {}
            }
        }
        let (front, back) = self.decoder.buffered_slices();
        Shutdown {
            unsent,
            blocked,
            awaiting,
            abandoned: self.abandoned,
            unprocessed_input: [front, back].concat(),
            discarded_input: self.discarded_input,
            failure: self.failure,
        }
    }

    /// Truncate every slot after `sequence`, retaining their owners.
    fn close_at(&mut self, sequence: u64) {
        self.close_after = Some(sequence);
        self.stop_issuing = true;
        let keep = (sequence - self.base_sequence + 1) as usize;
        for slot in self.slots.drain(keep.min(self.slots.len())..) {
            let response = match slot.state {
                SlotState::Ready(response) | SlotState::Failure(Some(response)) => {
                    self.retained -= response.len();
                    Some(response)
                }
                SlotState::Awaiting | SlotState::Failure(None) => None,
            };
            // A generated error response for a later parse failure is simply
            // superseded: the connection closes before that position.
            if let Some(id) = slot.id {
                self.abandoned.push(Abandoned { id, response });
            }
        }
    }
    /// Move completed responses at the front, in order, to the wire queue.
    fn promote(&mut self) {
        while !self.close_promoted {
            let Some(front) = self.slots.front() else {
                return;
            };
            if matches!(front.state, SlotState::Awaiting) {
                return;
            }
            let slot = self.slots.pop_front().expect("front exists");
            let sequence = self.base_sequence;
            self.base_sequence += 1;
            let bytes = match slot.state {
                SlotState::Ready(response) => Some(response.into_bytes()),
                SlotState::Failure(response) => response.map(EncodedResponse::into_bytes),
                SlotState::Awaiting => unreachable!("checked above"),
            };
            if let Some(bytes) = bytes {
                self.outbound.push_back(Chunk { id: slot.id, bytes });
            }
            if self.close_after == Some(sequence) {
                self.close_promoted = true;
            }
        }
    }
}

fn wants_close(head: &RequestHead) -> bool {
    head.headers("connection").any(|value| {
        value
            .split(|byte| *byte == b',')
            .map(crate::trim_ows)
            .any(|option| option.eq_ignore_ascii_case(b"close"))
    })
}
fn encode(
    response: &Response,
    context: ResponseContext,
    close: bool,
    limits: ResponseLimits,
) -> Result<EncodedResponse, ResponseError> {
    let policy = if close {
        ConnectionPolicy::Close
    } else {
        ConnectionPolicy::KeepAlive
    };
    let mut builder = ResponseBuilder::new(
        response.status,
        context,
        policy,
        response.body.len(),
        limits,
    )?;
    for (name, value) in &response.headers {
        builder = builder.header(name, value)?;
    }
    builder.finish(&response.body)
}
/// Generated status for a decoder failure; `None` means close silently.
fn error_status(error: Error) -> Option<u16> {
    Some(match error {
        Error::UnexpectedEof | Error::InputEnded | Error::Poisoned => return None,
        Error::AllocationFailed | Error::InvalidLimits => 500,
        Error::BodyLimit | Error::BufferedLimit => 413,
        Error::HeadLimit | Error::HeaderCountLimit => 431,
        Error::UnsupportedVersion => 505,
        Error::UnsupportedExpectation => 417,
        Error::UnsupportedTransferCoding | Error::UnsupportedUpgrade | Error::UnsupportedMethod => {
            501
        }
        _ => 400,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_issued_sequence_is_unknown_and_returns_the_response() {
        let mut connection = Connection::new(ConnectionLimits::default()).unwrap();
        let id = RequestId {
            connection: connection.connection,
            sequence: 0,
        };
        let response = Response::new(200, b"x".to_vec());
        let error = connection.submit(id, response.clone()).unwrap_err();
        assert_eq!(error.kind, SubmitErrorKind::Unknown);
        assert_eq!(error.response, response);
        assert!(connection.pending_output().is_empty());
    }
}
