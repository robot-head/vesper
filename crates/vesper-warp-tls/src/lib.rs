//! The QUIC TLS 1.3 handshake for Vesper Warp (design §35; RFC 9001 §4,
//! RFC 9000 §7 and §18).
//!
//! What it implements:
//!
//! * [`QuicTls`], a sans-I/O client/server handshake driver over
//!   `rustls::quic`. It takes received CRYPTO bytes per packet-number
//!   [`Space`] and yields [`Event`]s: outgoing CRYPTO bytes per space,
//!   traffic secrets per space in the form `vesper-warp-crypto` consumes
//!   ([`SpaceKeys`] → `Sealer`/`Opener` or `OneRttKeys`), the peer's
//!   authenticated transport parameters, completion, confirmation and key
//!   discard.
//! * Bounded CRYPTO-frame reassembly per space ([`CryptoReassembler`])
//!   with out-of-order and duplicate data and typed overflow errors.
//! * RFC 9000 §18 transport parameters ([`TransportParameters`]): encoding,
//!   decoding of untrusted bytes, value ranges, duplicates, server-only
//!   parameters and the §7.3 connection-ID authentication.
//! * The RFC 9001 §4.9 key lifecycle ([`KeyState`]) and §4.1.2
//!   confirmation, with key updates refused before confirmation and then
//!   handed to `vesper-warp-crypto`'s existing key-update path.
//!
//! Cryptography is rustls 0.23 with the aws-lc-rs provider; nothing is
//! hand-rolled. rustls does not expose QUIC traffic secrets directly, so
//! they are exported through its `KeyLog` hook (see [`config`]) and each
//! exported secret is checked at export time against rustls's own packet
//! and header keys, and for 1-RTT against rustls's generation-1 key-update
//! keys, by sealing a probe packet with rustls and opening it with
//! `vesper-warp-crypto`.
//!
//! Not implemented: 0-RTT (disabled in every configuration built here),
//! client certificates, QUIC v2, and endpoint wiring (integration A,
//! issue #135). No heap-allocation claim is made.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod handshake;
pub mod params;
pub mod reassembly;
pub mod varint;

pub use config::{ClientSetup, FixedTime, ServerSetup};
pub use error::TlsError;
pub use handshake::{Event, KeyState, QuicTls, ServerCids, SpaceKeys};
pub use params::{
    CidExpectations, ConnectionId, PreferredAddress, TransportParameterError, TransportParameters,
};
pub use reassembly::{CryptoBufferError, CryptoReassembler};
pub use rustls;
pub use vesper_warp_crypto::Side;

/// A QUIC packet-number space, which is also a TLS encryption level
/// (RFC 9001 §4.1.4). 0-RTT has no CRYPTO stream and is not supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Space {
    Initial,
    Handshake,
    /// The application data space, protected with 1-RTT keys.
    Data,
}

impl Space {
    pub const ALL: [Space; 3] = [Space::Initial, Space::Handshake, Space::Data];

    pub const fn index(self) -> usize {
        self as usize
    }
}
