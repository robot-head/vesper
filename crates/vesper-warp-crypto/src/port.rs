//! The representation adapters between the crate's public types and the
//! export ABI of the Vesper package (`vesper/src/lib.ves`): the generated
//! binding, error and cipher conversions, and the lending of the shim keys
//! the crate holds inline.

use vesper_crypto_sys as sys;

use crate::{Cipher, CryptoError};

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
pub(crate) mod binding;

pub(crate) use binding as b;

/// Bytes of the scratch buffer every packet-path call lends Vesper.
pub(crate) const SCRATCH: usize = 64;

/// A zeroed scratch buffer on the caller's stack.
pub(crate) fn scratch() -> [u8; SCRATCH] {
    [0; SCRATCH]
}

impl From<Cipher> for b::Cipher {
    fn from(c: Cipher) -> Self {
        match c {
            Cipher::Aes128Gcm => b::Cipher::Aes128Gcm,
            Cipher::Aes256Gcm => b::Cipher::Aes256Gcm,
            Cipher::ChaCha20Poly1305 => b::Cipher::ChaCha20Poly1305,
        }
    }
}

/// The shim's AEAD code for `c`.
pub(crate) fn aead_alg(c: Cipher) -> u32 {
    b::cipher_aead(c.into())
}

fn size(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// The crate's error for the Vesper one.
pub(crate) fn err(e: b::WcErr) -> CryptoError {
    use b::WcErr as W;
    match e {
        W::SecretLength { expected, actual } => CryptoError::SecretLength {
            expected: size(expected),
            actual: size(actual),
        },
        W::CipherMismatch => CryptoError::CipherMismatch,
        W::ConnectionIdTooLong { len } => CryptoError::ConnectionIdTooLong(size(len)),
        W::BufferTooShort { needed, available } => CryptoError::BufferTooShort {
            needed: size(needed),
            available: size(available),
        },
        W::PayloadTooLarge => CryptoError::PayloadTooLarge,
        W::InvalidPacketNumberOffset => CryptoError::InvalidPacketNumberOffset,
        W::SampleOutOfBounds {
            sample_end,
            packet_len,
        } => CryptoError::SampleOutOfBounds {
            sample_end: size(sample_end),
            packet_len: size(packet_len),
        },
        W::InvalidHeaderForm => CryptoError::InvalidHeaderForm,
        W::PacketNumberOutOfRange { pn } => CryptoError::PacketNumberOutOfRange(pn),
        W::PacketNumberReused { pn } => CryptoError::PacketNumberReused(pn),
        W::PacketNumberRegression {
            requested,
            next_allowed,
        } => CryptoError::PacketNumberRegression {
            requested,
            next_allowed,
        },
        W::PacketNumberMismatch => CryptoError::PacketNumberMismatch,
        W::PacketNumbersExhausted => CryptoError::PacketNumbersExhausted,
        W::ConfidentialityLimitReached => CryptoError::ConfidentialityLimitReached,
        W::IntegrityLimitReached => CryptoError::IntegrityLimitReached,
        W::StaleTicket => CryptoError::StaleTicket,
        W::KeyPhaseMismatch => CryptoError::KeyPhaseMismatch,
        W::AuthenticationFailed => CryptoError::AuthenticationFailed,
        W::KeyUpdateNotPermitted => CryptoError::KeyUpdateNotPermitted,
        W::KeyRetired => CryptoError::KeyRetired,
        W::PacketNumberReconstruction => CryptoError::PacketNumberReconstruction,
    }
}

/// Lends an AEAD key to Vesper for one call.
#[allow(unsafe_code)]
pub(crate) fn lend_aead(a: &sys::Aead) -> b::Aead<'_> {
    // SAFETY: the `Crypto` imports read the `Aead` handle word as
    // `*const vesper_crypto_sys::Aead`, the type lent here.
    unsafe { b::Aead::lend(a) }
}

/// Lends an AES key to Vesper for one call.
#[allow(unsafe_code)]
pub(crate) fn lend_block(k: &sys::Block) -> b::Block<'_> {
    // SAFETY: as for `lend_aead`, with `vesper_crypto_sys::Block`.
    unsafe { b::Block::lend(k) }
}
