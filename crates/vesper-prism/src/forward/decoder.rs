//! Forwards [`crate::Decoder`] to the `vesper/decoder` package. The adapter keeps the
//! queued bytes (lent to Vesper as the ring buffer's two slices) and copies
//! out the head and body Vesper located.

use std::collections::VecDeque;

use crate::{DecodedRequest, Error, Header, Limits, Request, RequestHead};

/// The types at the library boundary (`--rust-types`).
pub(crate) mod abi {
    pub use crate::{Error, Limits};

    /// The outcome of one decoder step.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Step {
        Incomplete,
        Ready { head_bytes: u64, consumed: u64 },
        Failed(Error),
    }
}

#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../../port/decoder.rs"]
mod binding;

use abi::Step;

/// Incremental, connection-local decoder with an explicitly bounded byte queue.
///
/// `push` is all-or-nothing. `next_request` returns `Ok(None)` for incomplete
/// input and an owned request only when its entire fixed-length body is present.
/// Head bytes are scanned once and parsed once; body waits do not reparse heads.
/// A ring queue avoids repeatedly moving a large pipelined suffix.
///
/// ```
/// use vesper_prism::{Decoder, Limits};
/// let mut decoder = Decoder::new(Limits::default())?;
/// decoder.push(b"POST /events HTTP/1.1\r\nHost: example.test\r\nContent-Length: 3\r\n\r\na")?;
/// assert!(decoder.next_request()?.is_none());
/// decoder.push(b"bc")?;
/// let decoded = decoder.next_request()?.unwrap();
/// assert_eq!(decoded.request.body, b"abc");
/// assert_eq!(decoder.buffered_bytes(), 0);
/// # Ok::<(), vesper_prism::Error>(())
/// ```
pub struct Decoder {
    core: binding::DecoderCore,
    buffer: VecDeque<u8>,
}

impl Decoder {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        let core = binding::DecoderCore::new(limits)?;
        let mut buffer = VecDeque::new();
        buffer
            .try_reserve_exact(limits.max_buffered_bytes)
            .map_err(|_| Error::AllocationFailed)?;
        Ok(Self { core, buffer })
    }
    pub fn limits(&self) -> Limits {
        self.core.as_ref().limits()
    }
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
    /// Unconsumed bytes, possibly split at the ring's wrap point.
    pub fn buffered_slices(&self) -> (&[u8], &[u8]) {
        self.buffer.as_slices()
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if let Some(error) = self
            .core
            .admit(self.buffer.len() as u64, bytes.len() as u64)
        {
            return Err(error);
        }
        self.buffer.extend(bytes);
        Ok(())
    }
    /// Mark transport EOF. Already complete pipelined requests can still drain.
    /// A subsequent `next_request` reports `UnexpectedEof` for any partial frame.
    pub fn end_input(&mut self) {
        self.core.end_input();
    }
    pub fn next_request(&mut self) -> Result<Option<DecodedRequest>, Error> {
        let (front, back) = self.buffer.as_slices();
        match self.core.next(front, back) {
            Step::Incomplete => Ok(None),
            Step::Failed(error) => Err(error),
            Step::Ready {
                head_bytes,
                consumed,
            } => {
                let (head_bytes, consumed) = (head_bytes as usize, consumed as usize);
                let head = self.head();
                let body = self.buffer.range(head_bytes..consumed).copied().collect();
                self.buffer.drain(..consumed);
                Ok(Some(DecodedRequest {
                    request: Request { head, body },
                    consumed,
                }))
            }
        }
    }
    /// Whether the last decoded request was `HEAD` and asked for
    /// `Connection: close` (both decided by the head parser).
    pub(crate) fn last_request_flags(&self) -> (bool, bool) {
        let head = self.core.as_ref().head();
        (head.is_head(), head.close())
    }
    fn head(&self) -> RequestHead {
        let view = self.core.as_ref().head();
        let names = view.names().as_slice();
        let values = view.values().as_slice();
        let mut headers = Vec::with_capacity(view.name_ends().len());
        let (mut name_at, mut value_at) = (0, 0);
        for (name_end, value_end) in view.name_ends().iter().zip(view.value_ends().iter()) {
            let (name_end, value_end) = (name_end as usize, value_end as usize);
            headers.push(Header {
                name: ascii(&names[name_at..name_end]),
                value: values[value_at..value_end].to_vec(),
            });
            (name_at, value_at) = (name_end, value_end);
        }
        RequestHead {
            method: ascii(view.method().as_slice()),
            target: ascii(view.target().as_slice()),
            headers,
            content_length: view.content_length() as usize,
        }
    }
}

/// The status of the response a connection sends after `error`, if any.
pub(crate) fn error_status(error: Error) -> Option<u16> {
    let status = binding::decoder_error_status(error);
    (status != 0).then_some(status as u16)
}

/// Tokens and targets are ASCII by construction (the parser checked them).
/// ASCII is converted as one copy instead of a `char` push per byte; the
/// byte-to-char mapping of other bytes (never reached) is unchanged.
fn ascii(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if bytes.is_ascii() => text.to_owned(),
        _ => bytes.iter().copied().map(char::from).collect(),
    }
}
