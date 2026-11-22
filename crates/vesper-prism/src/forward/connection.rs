//! Forwards [`crate::connection::Connection`] to the `vesper/connection` package.
//! Vesper decides validity, budgets, ordering, close policy and the write
//! cursor; the adapter holds the bytes (decoder input, encoded responses by
//! request sequence, the wire queue) and applies Vesper's action plans.

use std::{
    collections::{BTreeMap, VecDeque},
    io,
};

use crate::{
    Decoder, Error,
    connection::{
        Abandoned, ConnectionError, ConnectionLimits, ConnectionState, Inbound, RequestEvent,
        RequestId, Response, Shutdown, SubmitError, SubmitErrorKind, UnsentOutput,
        next_connection_id,
    },
    forward::decoder::error_status,
    response::{
        ConnectionPolicy, EncodedResponse, ResponseBuilder, ResponseContext, ResponseError,
        ResponseLimits,
    },
};

/// The types at the library boundary (`--rust-types`).
pub(crate) mod abi {
    pub use crate::connection::ConnectionState;

    /// What `poll_request` does next.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Gate {
        Failed,
        NoMore,
        Backpressure,
        Decode,
    }

    /// What `receive` does with the offered bytes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Intake {
        Ended,
        Discard,
        Accept(u64),
    }

    /// Why a submission is refused before encoding.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Refusal {
        Foreign,
        Unknown,
        Duplicate,
        Stale,
        Abandoned,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Check {
        Refuse(Refusal),
        Accept { head: bool, close: bool },
    }

    /// A step the adapter applies to the bytes it holds.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Action {
        Promote { sequence: u64, id: bool },
        Abandon { sequence: u64 },
        Discard { sequence: u64 },
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Advance {
        Overrun { pending: u64, advanced: u64 },
        Advanced { popped: bool },
    }

    /// One issued request's slot (`ConnectionCore::slots`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Slot {
        pub id: bool,
        pub head: bool,
        pub request_close: bool,
        pub phase: u64,
        pub len: u64,
    }

    /// One wire-queue entry (`ConnectionCore::outbound`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Chunk {
        pub id: bool,
        pub sequence: u64,
        pub len: u64,
    }

    /// A slot still held at shutdown.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Owner {
        Blocked(u64),
        Awaiting(u64),
        Unsent(u64),
    }
}

#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../../port/connection.rs"]
mod binding;

use abi::{Action, Advance, Check, Gate, Intake, Owner, Refusal};

pub struct Connection {
    core: binding::ConnectionCore,
    connection: u64,
    limits: ConnectionLimits,
    decoder: Decoder,
    /// Encoded responses not yet on the wire, by request sequence.
    ready: BTreeMap<u64, EncodedResponse>,
    /// The wire queue, in order; Vesper tracks the written prefix.
    outbound: VecDeque<(Option<RequestId>, Vec<u8>)>,
    failure: Option<Error>,
    abandoned: Vec<Abandoned>,
}

impl Connection {
    pub fn new(limits: ConnectionLimits) -> Result<Self, ConnectionError> {
        let (in_flight, outbound, total) = (
            limits.max_in_flight as u64,
            limits.max_outbound_bytes as u64,
            limits.response.max_total_bytes as u64,
        );
        if !binding::connection_limits_valid(in_flight, outbound, total) {
            return Err(ConnectionError::InvalidLimits);
        }
        let decoder = Decoder::new(limits.request).map_err(ConnectionError::Decoder)?;
        // The replaced Rust build reserved every slot up front and reported a
        // failed reservation; Vesper aborts on allocation failure, so the same
        // fallible check runs here and Vesper grows its slots on demand.
        Vec::<abi::Slot>::new()
            .try_reserve_exact(limits.max_in_flight)
            .map_err(|_| ConnectionError::Decoder(Error::AllocationFailed))?;
        let connection = next_connection_id();
        Ok(Self {
            core: binding::ConnectionCore::new(connection, in_flight, outbound, total),
            connection,
            limits,
            decoder,
            ready: BTreeMap::new(),
            outbound: VecDeque::new(),
            failure: None,
            abandoned: Vec::new(),
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
        match self.core.intake(
            self.decoder.buffered_bytes() as u64,
            self.limits.request.max_buffered_bytes as u64,
            bytes.len() as u64,
        ) {
            Intake::Ended => Err(ConnectionError::InputEnded),
            Intake::Discard => Ok(bytes.len()),
            Intake::Accept(accepted) => {
                let accepted = accepted as usize;
                self.decoder
                    .push(&bytes[..accepted])
                    .map_err(ConnectionError::Decoder)?;
                Ok(accepted)
            }
        }
    }
    /// Transport EOF from the peer. Complete queued requests can still drain.
    pub fn end_input(&mut self) {
        self.core.end_input();
        self.decoder.end_input();
    }

    /// Issue at most one request. Performs one bounded decoder step.
    pub fn poll_request(&mut self) -> RequestEvent {
        match self.core.poll() {
            Gate::Failed => match self.failure {
                Some(error) => RequestEvent::Failed(error),
                None => RequestEvent::NoMoreRequests,
            },
            Gate::NoMore => RequestEvent::NoMoreRequests,
            Gate::Backpressure => RequestEvent::Backpressure,
            Gate::Decode => match self.decoder.next_request() {
                Ok(Some(decoded)) => {
                    let (head, close) = self.decoder.last_request_flags();
                    let sequence = self.core.issue(head, close);
                    RequestEvent::Request(Inbound {
                        id: self.id(sequence),
                        request: decoded.request,
                    })
                }
                Ok(None) if self.core.idle() => RequestEvent::NoMoreRequests,
                Ok(None) => RequestEvent::NeedInput,
                Err(error) => {
                    self.failure = Some(error);
                    let sequence = self.core.fail();
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
                    let len = response.as_ref().map_or(0, EncodedResponse::len) as u64;
                    let generated = response.is_some();
                    if let Some(response) = response {
                        self.ready.insert(sequence, response);
                    }
                    let plan = self.core.record_failure(sequence, generated, len);
                    self.apply(plan);
                    RequestEvent::Failed(error)
                }
            },
        }
    }

    /// Associate a response with its request. On any rejection the response is
    /// returned and the wire queue is unchanged.
    pub fn submit(&mut self, id: RequestId, response: Response) -> Result<(), SubmitError> {
        let reject = |kind, response| Err(SubmitError { kind, response });
        let (head, close) = match self.core.check(id.connection, id.sequence) {
            Check::Refuse(refusal) => {
                let kind = match refusal {
                    Refusal::Foreign => SubmitErrorKind::Foreign,
                    Refusal::Unknown => SubmitErrorKind::Unknown,
                    Refusal::Duplicate => SubmitErrorKind::Duplicate,
                    Refusal::Stale => SubmitErrorKind::Stale,
                    Refusal::Abandoned => SubmitErrorKind::Abandoned,
                };
                return reject(kind, response);
            }
            Check::Accept { head, close } => (head, close),
        };
        let close = response.close || close;
        let context = if head {
            ResponseContext::Head
        } else {
            ResponseContext::Ordinary
        };
        let encoded = match encode(&response, context, close, self.limits.response) {
            Ok(encoded) => encoded,
            Err(error) => return reject(SubmitErrorKind::Encode(error), response),
        };
        let len = encoded.len() as u64;
        if !self.core.fits(id.sequence, len) {
            return reject(SubmitErrorKind::OutboundLimit, response);
        }
        self.ready.insert(id.sequence, encoded);
        let plan = self.core.submit(id.sequence, len, close);
        self.apply(plan);
        Ok(())
    }

    /// Responses cut off by a closing response since the last call.
    pub fn take_abandoned(&mut self) -> Vec<Abandoned> {
        std::mem::take(&mut self.abandoned)
    }

    /// The unwritten remainder of the front wire response; empty if none.
    pub fn pending_output(&self) -> &[u8] {
        let cursor = self.core.cursor() as usize;
        self.outbound
            .front()
            .map_or(&[][..], |(_, bytes)| &bytes[cursor..])
    }
    /// Total unwritten bytes already in wire order.
    pub fn pending_output_len(&self) -> usize {
        self.core.pending_len() as usize
    }
    /// Record that the transport accepted `written` bytes of `pending_output`.
    /// Overruns are rejected without changing state.
    pub fn advance_output(&mut self, written: usize) -> Result<(), ConnectionError> {
        match self.core.advance(written as u64) {
            Advance::Overrun { pending, advanced } => Err(ConnectionError::OutputOverrun {
                pending: pending as usize,
                advanced: advanced as usize,
            }),
            Advance::Advanced { popped } => {
                if popped {
                    self.outbound.pop_front();
                }
                Ok(())
            }
        }
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
        self.core.state()
    }
    pub fn in_flight(&self) -> usize {
        self.core.in_flight() as usize
    }
    pub fn retained_outbound_bytes(&self) -> usize {
        self.core.as_ref().retained() as usize
    }
    pub fn buffered_input(&self) -> usize {
        self.decoder.buffered_bytes()
    }
    pub fn failure(&self) -> Option<Error> {
        self.failure
    }

    /// Consume the connection, returning every retained owner and byte.
    pub fn shutdown(mut self) -> Shutdown {
        let cursor = self.core.cursor() as usize;
        let mut unsent: Vec<UnsentOutput> = self
            .outbound
            .drain(..)
            .enumerate()
            .map(|(index, (id, bytes))| UnsentOutput {
                id,
                bytes,
                written: if index == 0 { cursor } else { 0 },
            })
            .collect();
        let mut blocked = Vec::new();
        let mut awaiting = Vec::new();
        for owner in self.core.owners() {
            match owner {
                Owner::Blocked(sequence) => {
                    if let Some(response) = self.ready.remove(&sequence) {
                        blocked.push((self.id(sequence), response));
                    }
                }
                Owner::Awaiting(sequence) => awaiting.push(self.id(sequence)),
                Owner::Unsent(sequence) => {
                    if let Some(response) = self.ready.remove(&sequence) {
                        unsent.push(UnsentOutput {
                            id: None,
                            bytes: response.into_bytes(),
                            written: 0,
                        });
                    }
                }
            }
        }
        let (front, back) = self.decoder.buffered_slices();
        Shutdown {
            unsent,
            blocked,
            awaiting,
            abandoned: self.abandoned,
            unprocessed_input: [front, back].concat(),
            discarded_input: self.core.as_ref().discarded_input() as usize,
            failure: self.failure,
        }
    }

    /// An id of this connection that may never have been issued.
    #[cfg(test)]
    pub(crate) fn request_id(&self, sequence: u64) -> RequestId {
        self.id(sequence)
    }

    fn id(&self, sequence: u64) -> RequestId {
        RequestId {
            connection: self.connection,
            sequence,
        }
    }
    /// Apply a Vesper plan to the held bytes, in order.
    fn apply(&mut self, plan: Vec<Action>) {
        for action in plan {
            match action {
                Action::Promote { sequence, id } => {
                    let bytes = self
                        .ready
                        .remove(&sequence)
                        .map(EncodedResponse::into_bytes)
                        .unwrap_or_default();
                    let id = id.then(|| self.id(sequence));
                    self.outbound.push_back((id, bytes));
                }
                Action::Abandon { sequence } => {
                    let response = self.ready.remove(&sequence);
                    self.abandoned.push(Abandoned {
                        id: self.id(sequence),
                        response,
                    });
                }
                Action::Discard { sequence } => {
                    self.ready.remove(&sequence);
                }
            }
        }
    }
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
