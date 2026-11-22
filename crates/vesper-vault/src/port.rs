//! Representation adapters between `vesper-vault`'s public types and the
//! export ABI of `@vesper/vault` (`vesper/src/lib.ves`): the generated
//! binding, error/enum conversions, stack scratch buckets for allocation-free
//! AEAD framing, and lending of pre-expanded `vesper_crypto_sys::Aead` keys.

use vesper_crypto_sys as sys;
use zeroize::Zeroizing;

use crate::aead::{AuthError, CryptoError, FrameHeader, TAG_LEN};
use crate::kem::{KemError, KemIsa};

// Generated and committed, kept outside `src/` so workspace formatters never
// reflow it (see `docs/maintenance/porting-a-crate.md`).
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
pub(crate) mod binding;

pub(crate) use binding as b;

/// Lends a pre-expanded `vesper-crypto-sys` AEAD key handle to `@vesper/vault`
/// for one export-ABI call without heap allocation.
#[allow(unsafe_code)]
pub(crate) fn lend_aead(a: &sys::Aead) -> b::Aead<'_> {
    // SAFETY: `@vesper/crypto`'s `Aead` handle word is read by
    // `vesper-crypto-sys` as `*const vesper_crypto_sys::Aead`, matching `a`.
    unsafe { b::Aead::lend(a) }
}

pub(crate) fn aead_kind(alg_code: u32) -> b::AeadKind {
    match alg_code {
        2 => b::AeadKind::Aes256Gcm,
        3 => b::AeadKind::ChaCha20Poly1305,
        _ => unreachable!("invalid AEAD algorithm code {alg_code}"),
    }
}

pub(crate) fn isa_kind(isa: KemIsa) -> b::KemIsaKind {
    match isa {
        KemIsa::Portable => b::KemIsaKind::Portable,
        KemIsa::Avx2 => b::KemIsaKind::Avx2,
        KemIsa::Neon => b::KemIsaKind::Neon,
    }
}

pub(crate) fn isa_from_kind(k: b::KemIsaKind) -> KemIsa {
    match k {
        b::KemIsaKind::Portable => KemIsa::Portable,
        b::KemIsaKind::Avx2 => KemIsa::Avx2,
        b::KemIsaKind::Neon => KemIsa::Neon,
    }
}

pub(crate) fn frame_header_from_hdr(h: b::repr::FrameHdr) -> FrameHeader {
    let mut tag = [0u8; TAG_LEN];
    tag[0..8].copy_from_slice(&h.tag_lo.to_le_bytes());
    tag[8..16].copy_from_slice(&h.tag_hi.to_le_bytes());
    FrameHeader {
        key_epoch: h.key_epoch.get(),
        nonce_seq: h.nonce_seq,
        tag,
    }
}

pub(crate) fn frame_header_parse(frame: &[u8]) -> Option<FrameHeader> {
    b::frame_header_parse(frame)
        .get()
        .map(frame_header_from_hdr)
}

pub(crate) fn crypto_err(e: b::VaultErr) -> CryptoError {
    match e {
        b::VaultErr::EpochExhaustedRekeyRequired => CryptoError::EpochExhaustedRekeyRequired,
        b::VaultErr::EpochSpaceExhausted => CryptoError::EpochSpaceExhausted,
        b::VaultErr::BufferTooSmall => CryptoError::BufferTooSmall,
        b::VaultErr::ForeignTicket => CryptoError::ForeignTicket,
        b::VaultErr::CipherRejected => CryptoError::CipherRejected,
        other => unreachable!("unexpected VaultErr in AEAD seal/control path: {other:?}"),
    }
}

pub(crate) fn auth_err(e: b::VaultErr) -> AuthError {
    match e {
        b::VaultErr::MalformedHeader => AuthError::MalformedHeader,
        b::VaultErr::WrongEpoch { expected, found } => AuthError::WrongEpoch { expected, found },
        b::VaultErr::AuthenticationTagMismatch => AuthError::AuthenticationTagMismatch,
        other => unreachable!("unexpected VaultErr in AEAD open path: {other:?}"),
    }
}

pub(crate) fn kem_err(e: b::VaultErr) -> KemError {
    match e {
        b::VaultErr::BadShareLength { expected, found } => KemError::BadShareLength {
            expected: usize::try_from(expected).unwrap_or(usize::MAX),
            found: usize::try_from(found).unwrap_or(usize::MAX),
        },
        b::VaultErr::InvalidEncapsulationKey => KemError::InvalidEncapsulationKey,
        b::VaultErr::NonContributoryX25519 => KemError::NonContributoryX25519,
        b::VaultErr::Rng => KemError::Rng,
        b::VaultErr::IsaUnavailable { isa } => KemError::IsaUnavailable(isa_from_kind(isa)),
        other => unreachable!("unexpected VaultErr in KEM/RNG path: {other:?}"),
    }
}

/// Provide a mutable `needed`-byte scratch slice with zero heap allocations on
/// ordinary frame sizes (<= 16 KiB). Callers in `@vesper/vault` wipe plaintext
/// bytes before returning or raising.
pub(crate) fn with_work_buf<R>(needed: usize, f: impl FnOnce(&mut [u8]) -> R) -> R {
    if needed <= 64 {
        let mut buf = [0u8; 64];
        f(&mut buf[..needed])
    } else if needed <= 256 {
        let mut buf = [0u8; 256];
        f(&mut buf[..needed])
    } else if needed <= 512 {
        let mut buf = [0u8; 512];
        f(&mut buf[..needed])
    } else if needed <= 1536 {
        let mut buf = [0u8; 1536];
        f(&mut buf[..needed])
    } else if needed <= 4096 {
        let mut buf = [0u8; 4096];
        f(&mut buf[..needed])
    } else if needed <= 16384 {
        let mut buf = [0u8; 16384];
        f(&mut buf[..needed])
    } else {
        let mut buf = Zeroizing::new(vec![0u8; needed]);
        f(&mut buf[..needed])
    }
}
