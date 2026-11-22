//! Bounded HTTP/1.1 request framing for the Prism host prototype.
//!
//! Only origin-form requests (and `OPTIONS *`) with no body or a single
//! `Content-Length` are supported. Transfer codings, upgrades, CONNECT,
//! expectations, HTTP/1.0 and absolute-form targets fail closed. This is not a
//! server, source-language linearity implementation, TLS stack or SIMD parser.
//! [`connection`] adds a socket-independent connection state machine,
//! [`target`] the single URI decoding policy, and [`router`]/[`service`]
//! bounded routing and middleware over that decoded target.
//! All parse errors poison the decoder: the caller must close the connection,
//! not search the remaining bytes for another request boundary.

// The Rust build has no unsafe code. The Vesper build's generated bindings
// (port/*.rs) call the port through `extern "C"`; only they may use unsafe.
#![deny(unsafe_code)]

pub mod connection;
pub mod response;
pub mod router;
pub mod service;
pub mod target;

use std::fmt;

// The implementation: the Vesper port (the packages in vesper/) behind thin
// forwarding adapters. It replaced the Rust implementation in SH6 lane 5
// (#1400); that implementation is in git history before this change.
mod forward;
pub use forward::decoder::Decoder;

/// Logical byte/count limits, checked before retaining input or copying bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_head_bytes: usize,
    pub max_headers: usize,
    pub max_body_bytes: usize,
    /// Includes the current request and all queued pipelined bytes.
    pub max_buffered_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_head_bytes: 16 * 1024,
            max_headers: 100,
            max_body_bytes: 1024 * 1024,
            max_buffered_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    AllocationFailed,
    BufferedLimit,
    HeadLimit,
    HeaderCountLimit,
    BodyLimit,
    InvalidLineEnding,
    InvalidRequestLine,
    UnsupportedVersion,
    InvalidTarget,
    UnsupportedMethod,
    InvalidHeader,
    InvalidHost,
    MissingHost,
    DuplicateHost,
    DuplicateContentLength,
    InvalidContentLength,
    AmbiguousFraming,
    UnsupportedTransferCoding,
    UnsupportedUpgrade,
    UnsupportedExpectation,
    InvalidConnection,
    UnexpectedEof,
    InputEnded,
    Poisoned,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP/1.1 framing error: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Header names are ASCII lowercase; values retain bytes after edge OWS trim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHead {
    /// Case is preserved: HTTP methods are case-sensitive.
    pub method: String,
    /// Validated ASCII origin-form or `OPTIONS *`; no URI decoding is done.
    pub target: String,
    pub headers: Vec<Header>,
    pub content_length: usize,
}
impl RequestHead {
    /// ASCII case-insensitive lookup. Repeated non-framing fields remain separate.
    pub fn headers<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.headers
            .iter()
            .filter(move |header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_slice())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub head: RequestHead,
    pub body: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedRequest {
    pub request: Request,
    /// Wire bytes removed for this request; queued pipeline bytes are untouched.
    pub consumed: usize,
}
