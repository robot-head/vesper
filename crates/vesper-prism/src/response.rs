//! Bounded deterministic fixed-length HTTP/1.1 response encoding.
//!
//! Builders and completed responses are consumed by their final transitions.
//! Rust can still drop either value; this API does not prove source-language
//! linear response exhaustion and performs no socket I/O.

use std::fmt;

pub use crate::forward::response::ResponseBuilder;

/// Independent limits on representation bytes and actual encoded wire bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseLimits {
    pub max_head_bytes: usize,
    /// Includes encoder-generated Content-Length and Connection fields.
    pub max_headers: usize,
    /// Applies to the representation even when HEAD suppresses its wire body.
    pub max_body_bytes: usize,
    pub max_total_bytes: usize,
}
impl Default for ResponseLimits {
    fn default() -> Self {
        Self {
            max_head_bytes: 16 * 1024,
            max_headers: 100,
            max_body_bytes: 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseContext {
    Ordinary,
    Head,
    /// Tunnels are explicitly unsupported, including CONNECT error responses.
    Connect,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPolicy {
    KeepAlive,
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseError {
    UnsupportedStatus,
    UnsupportedConnect,
    UnsupportedUpgrade,
    UnsupportedTransferCoding,
    ReservedHeader,
    InvalidHeaderName,
    InvalidHeaderValue,
    BodyForbidden,
    LengthMismatch { expected: usize, actual: usize },
    LengthOverflow,
    HeadLimit,
    HeaderCountLimit,
    BodyLimit,
    TotalLimit,
    AllocationFailed,
}
impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP/1.1 response error: {self:?}")
    }
}
impl std::error::Error for ResponseError {}

/// Completed bytes with an explicit connection policy. This value is not Clone;
/// `into_bytes` consumes it, but Rust Drop still permits discarding it unsent.
#[derive(Debug)]
pub struct EncodedResponse {
    pub(crate) bytes: Vec<u8>,
    pub(crate) connection: ConnectionPolicy,
}
impl EncodedResponse {
    /// Policy to be enacted by the transport owner; encoding closes no socket.
    pub fn connection_policy(&self) -> ConnectionPolicy {
        self.connection
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
