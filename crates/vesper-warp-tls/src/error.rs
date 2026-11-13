//! Typed handshake errors and their QUIC transport error codes
//! (RFC 9000 §20.1, RFC 9001 §4.8).

use std::fmt;

use crate::Space;
use crate::params::TransportParameterError;
use crate::reassembly::CryptoBufferError;

/// QUIC transport error codes used by this crate.
pub mod code {
    pub const INTERNAL_ERROR: u64 = 0x01;
    pub const FRAME_ENCODING_ERROR: u64 = 0x07;
    pub const TRANSPORT_PARAMETER_ERROR: u64 = 0x08;
    pub const PROTOCOL_VIOLATION: u64 = 0x0a;
    pub const CRYPTO_BUFFER_EXCEEDED: u64 = 0x0d;
    pub const KEY_UPDATE_ERROR: u64 = 0x0e;
    /// `CRYPTO_ERROR` range base: the TLS alert is added to it.
    pub const CRYPTO_ERROR: u64 = 0x0100;
}

/// TLS alert `no_application_protocol` (RFC 7301 §3.2).
pub const ALERT_NO_APPLICATION_PROTOCOL: u8 = 120;
/// TLS alert `internal_error`.
pub const ALERT_INTERNAL_ERROR: u8 = 80;

/// Why the handshake driver refused an operation or failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsError {
    /// rustls rejected the handshake. `alert` is the TLS alert it would
    /// send; the connection closes with `CRYPTO_ERROR + alert`.
    Tls { alert: Option<u8>, message: String },
    /// The handshake completed without an agreed ALPN protocol
    /// (RFC 9001 §8.1).
    NoApplicationProtocol,
    /// The peer's transport parameters are invalid or failed connection-ID
    /// authentication, or ours could not be encoded.
    TransportParameters(TransportParameterError),
    /// CRYPTO data could not be buffered.
    CryptoBuffer {
        space: Space,
        error: CryptoBufferError,
    },
    /// CRYPTO data arrived for a space whose keys are not installed yet.
    SpaceUnavailable(Space),
    /// CRYPTO data or a key operation targeted a space whose keys were
    /// discarded (RFC 9001 §4.9).
    SpaceDiscarded(Space),
    /// A QUIC protocol rule was broken (`PROTOCOL_VIOLATION`).
    ProtocolViolation(&'static str),
    /// The negotiated TLS cipher suite has no `vesper-warp-crypto` packet
    /// protection.
    UnsupportedCipherSuite(String),
    /// A key operation (e.g. a key update before confirmation) is not
    /// permitted.
    KeyUpdate(&'static str),
    /// The client or server configuration is unusable for QUIC.
    Config(String),
    /// Secrets exported by rustls did not reproduce rustls's own packet
    /// keys. This is an internal invariant failure, never peer-triggered.
    KeyExportMismatch(Space),
    /// The driver already failed; this is the original error's code.
    Closed { code: u64 },
}

impl TlsError {
    /// The QUIC transport error code to close the connection with.
    pub fn quic_error_code(&self) -> u64 {
        match self {
            TlsError::Tls { alert, .. } => {
                code::CRYPTO_ERROR + u64::from(alert.unwrap_or(ALERT_INTERNAL_ERROR))
            }
            TlsError::NoApplicationProtocol => {
                code::CRYPTO_ERROR + u64::from(ALERT_NO_APPLICATION_PROTOCOL)
            }
            TlsError::TransportParameters(e) => e.quic_error_code(),
            TlsError::CryptoBuffer { error, .. } => error.quic_error_code(),
            TlsError::SpaceUnavailable(_)
            | TlsError::SpaceDiscarded(_)
            | TlsError::ProtocolViolation(_) => code::PROTOCOL_VIOLATION,
            TlsError::KeyUpdate(_) => code::KEY_UPDATE_ERROR,
            TlsError::UnsupportedCipherSuite(_)
            | TlsError::Config(_)
            | TlsError::KeyExportMismatch(_) => code::INTERNAL_ERROR,
            TlsError::Closed { code } => *code,
        }
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::Tls { alert, message } => write!(f, "TLS error (alert {alert:?}): {message}"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for TlsError {}

impl From<TransportParameterError> for TlsError {
    fn from(e: TransportParameterError) -> Self {
        TlsError::TransportParameters(e)
    }
}
