//! Vesper Warp QUIC packet protection (design §35.1 item 2, RFC 9001).
//!
//! This crate is the Warp-specific slice of packet protection. It is **not**
//! an implementation of Vesper Vault (§23); it only provides the
//! `NonceTicket` discipline that Warp needs on its send path.
//!
//! What it implements:
//!
//! * The RFC 9001 §5.2 Initial key schedule and the same HKDF-Expand-Label
//!   schedule for traffic secrets supplied by a TLS stack ([`schedule`]).
//!   No TLS handshake is implemented.
//! * AEAD seal/open in place over a `header || payload || tag` buffer with
//!   the header as associated data, for AES-128-GCM, AES-256-GCM and
//!   ChaCha20-Poly1305, with the RFC 9001 §5.3 nonce ([`protect`]).
//! * Header protection (RFC 9001 §5.4) for AES and ChaCha20, applied and
//!   removed in place with sample bounds checks ([`protect`]).
//! * Linear [`NonceTicket`]s issued by a per-key, per-direction [`Sealer`]
//!   for strictly increasing packet numbers; sealing consumes the ticket
//!   ([`ticket`]).
//! * The RFC 9001 §6 key update state machine with key-phase tracking and
//!   time-windowed retirement of old receive keys ([`update`]).
//! * Retry integrity tags (RFC 9001 §5.8, QUIC v1) ([`retry`]).
//!
//! # Boundary with the packet core (lane 1)
//!
//! The contract boundary is byte offsets, not types. On receive the order is:
//!
//! 1. the packet core parses the header and reports `pn_offset`;
//! 2. [`HeaderKey::remove`] (or the opener helpers) removes header
//!    protection in place and reports the packet-number length (1–4) and the
//!    truncated value;
//! 3. the packet core reconstructs the full packet number;
//! 4. [`Opener::open`] opens the payload, taking that **full** packet number,
//!    because the RFC 9001 §5.3 nonce is built from it.
//!
//! On send the packet core writes the unprotected header (including the
//! truncated packet number at `pn_offset`) and the plaintext payload into one
//! buffer with 16 spare bytes for the tag, and calls [`Sealer::seal`] with a
//! ticket for that packet number.
//!
//! # Linearity
//!
//! [`NonceTicket`] is a non-`Clone`, non-`Copy`, `#[must_use]` owner. Its
//! only exits are [`Sealer::seal`] (consumes it on success),
//! [`Sealer::abandon`] (burns the packet number) and the error values that
//! hand it back. Dropping a live ticket is counted, per sealer through
//! [`Sealer::audit`] and process-wide through [`leaked_nonce_tickets`];
//! its packet number is never re-issued. This is host-library discipline
//! enforced by Rust ownership; it is not an implementation of the Vesper
//! source typing rules for `linear` values.
//!
//! # Secrets and constant time
//!
//! Secrets, derived keys, IVs and header-protection keys are zeroized on
//! drop: our own buffers through `zeroize`, the AES key schedules through
//! the `aes` crate's `zeroize` feature, the GHASH/POLYVAL state through the
//! `polyval` `zeroize` feature, and the ChaCha20 keys through
//! `chacha20`/`chacha20poly1305`'s own `ZeroizeOnDrop`. The HMAC state held
//! transiently inside `hkdf` during expansion is not zeroized by that crate.
//!
//! AEAD tag comparison is done by `aes-gcm` and `chacha20poly1305` with
//! `subtle` before any plaintext is written, so a failed open leaves the
//! payload bytes untouched (tested). Retry tag verification goes through the
//! same AEAD path. No constant-time claim is made beyond those dependency
//! guarantees; see `docs/parallel-results/warp-packet-protection.md`.
//!
//! Hardware AEAD offload (kTLS, NIC inline crypto) is not implemented and is
//! unavailable.

#![forbid(unsafe_code)]

pub mod protect;
pub mod retry;
pub mod schedule;
pub mod ticket;
pub mod update;

pub use protect::{HeaderKey, PacketKey, Unprotected};
pub use retry::{retry_integrity_tag, verify_retry_integrity};
pub use schedule::{DirectionalKeys, InitialSecrets, KeyMaterial, Side, TrafficSecret};
pub use ticket::{NonceTicket, Opened, Opener, Sealer, TicketAudit, leaked_nonce_tickets};
pub use update::{OneRttKeys, OneRttOpened};

use std::fmt;

/// Largest valid QUIC packet number plus one (RFC 9000 §12.3).
pub const PACKET_NUMBER_LIMIT: u64 = 1 << 62;
/// AEAD tag length for every supported cipher suite.
pub const TAG_LEN: usize = 16;
/// AEAD nonce / QUIC IV length for every supported cipher suite.
pub const IV_LEN: usize = 12;
/// Header-protection sample length (RFC 9001 §5.4.2).
pub const SAMPLE_LEN: usize = 16;
/// Maximum QUIC v1 connection ID length (RFC 9000 §17.2).
pub const MAX_CID_LEN: usize = 20;

/// Packet protection cipher suites supported by this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cipher {
    /// `TLS_AES_128_GCM_SHA256`.
    Aes128Gcm,
    /// `TLS_AES_256_GCM_SHA384`.
    Aes256Gcm,
    /// `TLS_CHACHA20_POLY1305_SHA256`.
    ChaCha20Poly1305,
}

impl Cipher {
    /// AEAD key length (also the header-protection key length).
    pub const fn key_len(self) -> usize {
        match self {
            Cipher::Aes128Gcm => 16,
            Cipher::Aes256Gcm | Cipher::ChaCha20Poly1305 => 32,
        }
    }

    /// Length of a traffic secret, i.e. the TLS hash output length.
    pub const fn secret_len(self) -> usize {
        match self {
            Cipher::Aes128Gcm | Cipher::ChaCha20Poly1305 => 32,
            Cipher::Aes256Gcm => 48,
        }
    }

    /// RFC 9001 §6.6 confidentiality limit: packets that may be sealed with
    /// one key. ChaCha20-Poly1305's limit exceeds the packet-number space.
    pub const fn confidentiality_limit(self) -> u64 {
        match self {
            Cipher::Aes128Gcm | Cipher::Aes256Gcm => 1 << 23,
            Cipher::ChaCha20Poly1305 => PACKET_NUMBER_LIMIT,
        }
    }

    /// RFC 9001 §6.6 integrity limit: failed openings tolerated across all
    /// keys of a connection.
    pub const fn integrity_limit(self) -> u64 {
        match self {
            Cipher::Aes128Gcm | Cipher::Aes256Gcm => 1 << 52,
            Cipher::ChaCha20Poly1305 => 1 << 36,
        }
    }
}

/// Typed packet-protection errors. None of them are produced by panics on
/// wire bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// A supplied secret does not match the cipher suite's hash length.
    SecretLength { expected: usize, actual: usize },
    /// Local and remote secrets belong to different cipher suites.
    CipherMismatch,
    /// A connection ID is longer than [`MAX_CID_LEN`].
    ConnectionIdTooLong(usize),
    /// The buffer is shorter than the requested operation needs.
    BufferTooShort { needed: usize, available: usize },
    /// The payload exceeds the AEAD's maximum message length.
    PayloadTooLarge,
    /// `pn_offset` is zero, so there is no first byte to protect.
    InvalidPacketNumberOffset,
    /// The header-protection sample would run past the packet end.
    SampleOutOfBounds {
        sample_end: usize,
        packet_len: usize,
    },
    /// The header form does not fit the operation (e.g. a long header given
    /// to 1-RTT keys, or a fixed-bit-free first byte).
    InvalidHeaderForm,
    /// A packet number is `>= 2^62`.
    PacketNumberOutOfRange(u64),
    /// A ticket was requested for an already-issued packet number.
    PacketNumberReused(u64),
    /// A ticket was requested below the next allowed packet number.
    PacketNumberRegression { requested: u64, next_allowed: u64 },
    /// The truncated packet number in the header does not encode the
    /// ticket's (or the reconstructed) full packet number.
    PacketNumberMismatch,
    /// The packet-number space has no packet numbers left.
    PacketNumbersExhausted,
    /// The AEAD confidentiality limit for the current key is reached; a key
    /// update (or connection close) is required.
    ConfidentialityLimitReached,
    /// The connection's AEAD integrity limit is reached.
    IntegrityLimitReached,
    /// The ticket was issued by a different sealer or an older key phase.
    StaleTicket,
    /// The header's key-phase bit disagrees with the sealing key's phase.
    KeyPhaseMismatch,
    /// AEAD authentication failed.
    AuthenticationFailed,
    /// A key update was requested before it is permitted (RFC 9001 §6.1).
    KeyUpdateNotPermitted,
    /// The packet belongs to a key phase whose keys were retired.
    KeyRetired,
    /// The caller's packet-number reconstruction failed.
    PacketNumberReconstruction,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CryptoError {}

/// Checks `pn < 2^62`.
pub(crate) fn check_pn(pn: u64) -> Result<(), CryptoError> {
    if pn < PACKET_NUMBER_LIMIT {
        Ok(())
    } else {
        Err(CryptoError::PacketNumberOutOfRange(pn))
    }
}
