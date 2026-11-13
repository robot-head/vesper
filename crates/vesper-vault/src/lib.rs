//! Vesper Vault (design §23): constant-time cryptography, fenced
//! zeroization and nonce-safe AEAD, as a host library.
//!
//! * [`verifier`]: the §23.5 Stage 5 constant-time SSA taint verifier over
//!   real `cranelift_codegen::ir::Function`s.
//! * [`secret`]: §23.2 opaque secret scalars and masks.
//! * [`slot`]: `SecretSlot[K]` in page-aligned, `mlock`ed,
//!   `MADV_DONTDUMP` memory with a consuming `shred`.
//! * [`aead`]: §23.3 linear `AeadSealer`/`NonceTicket` and the Doom-principle
//!   `open_in_place` over AES-256-GCM and ChaCha20-Poly1305.
//! * [`kem`]: §23.1.4 hybrid X25519MLKEM768 key establishment.
//! * [`consttime`]: `ConstTime::scope` and AArch64 `PSTATE.DIT`.
//! * [`obligation`]: the leak auditor for linear owners.
//!
//! The linear types here are host-library discipline (non-Clone, non-Copy,
//! `#[must_use]` owners with counted leaks), not an implementation of the
//! Vesper source typing rules that make misuse a compile-time error. Unsafe
//! code is confined to the gated `sys` module (mlock/madvise and DIT).
//!
//! # What the types refuse at compile time
//!
//! A nonce ticket cannot be duplicated:
//!
//! ```compile_fail,E0599
//! use vesper_vault::{Aes256Gcm, AeadSealer, LockPolicy, SecretSlot};
//! let key = SecretSlot::<32>::new(LockPolicy::BestEffort).unwrap();
//! let mut sealer = AeadSealer::<Aes256Gcm>::new(key, 0);
//! let ticket = sealer.mint_nonce().unwrap();
//! let copy = ticket.clone();
//! ```
//!
//! nor seal twice:
//!
//! ```compile_fail,E0382
//! use vesper_vault::{Aes256Gcm, AeadSealer, FRAME_HEADER_LEN, LockPolicy, SecretSlot};
//! let key = SecretSlot::<32>::new(LockPolicy::BestEffort).unwrap();
//! let mut sealer = AeadSealer::<Aes256Gcm>::new(key, 0);
//! let ticket = sealer.mint_nonce().unwrap();
//! let mut a = [0u8; FRAME_HEADER_LEN + 4];
//! let mut b = [0u8; FRAME_HEADER_LEN + 4];
//! let _ = sealer.seal_in_place(ticket, b"", &mut a);
//! let _ = sealer.seal_in_place(ticket, b"", &mut b);
//! ```
//!
//! nor be minted without exclusive access to the sealer:
//!
//! ```compile_fail,E0596
//! use vesper_vault::{Aes256Gcm, AeadSealer, LockPolicy, SecretSlot};
//! let key = SecretSlot::<32>::new(LockPolicy::BestEffort).unwrap();
//! let sealer = AeadSealer::<Aes256Gcm>::new(key, 0);
//! let shared = &sealer;
//! let ticket = shared.mint_nonce();
//! ```
//!
//! Secret scalars have no equality, ordering or division:
//!
//! ```compile_fail,E0369
//! use vesper_vault::SecU64;
//! let same = SecU64::new(1) == SecU64::new(1);
//! ```
//!
//! ```compile_fail,E0369
//! use vesper_vault::SecU64;
//! let q = SecU64::new(10) / SecU64::new(3);
//! ```
//!
//! and a secret slot cannot be copied or read without a projection:
//!
//! ```compile_fail,E0599
//! use vesper_vault::{LockPolicy, SecretSlot};
//! let slot = SecretSlot::<32>::new(LockPolicy::BestEffort).unwrap();
//! let copy = slot.clone();
//! ```
//!
//! The positive path, for contrast:
//!
//! ```
//! use vesper_vault::{
//!     Aes256Gcm, AeadOpener, AeadSealer, CiphertextFrame, FRAME_HEADER_LEN, LockPolicy, SecretSlot,
//! };
//! let key = || SecretSlot::<32>::from_fn(LockPolicy::BestEffort, |k| k.fill(1)).unwrap();
//! let mut sealer = AeadSealer::<Aes256Gcm>::new(key(), 0);
//! let opener = AeadOpener::<Aes256Gcm>::new(key(), 0);
//! let mut frame = [0u8; FRAME_HEADER_LEN + 5];
//! frame[FRAME_HEADER_LEN..].copy_from_slice(b"hello");
//! let ticket = sealer.mint_nonce().unwrap();
//! sealer.seal_in_place(ticket, b"aad", &mut frame).unwrap();
//! let plain = opener.open_in_place(b"aad", CiphertextFrame::new(&mut frame)).unwrap();
//! assert_eq!(plain.bytes(), b"hello");
//! let _ = sealer.retire();
//! let _ = opener.retire();
//! ```
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod aead;
pub mod consttime;
pub mod kem;
pub mod obligation;
pub mod secret;
pub mod slot;
mod sys;
pub mod verifier;

pub use aead::{
    AeadAlgorithm, AeadOpener, AeadSealer, Aes256Gcm, AuthError, BurnedNonce, ChaCha20Poly1305,
    CiphertextFrame, CryptoError, FRAME_HEADER_LEN, FrameHeader, NonceTicket, SealError, Sealed,
    VerifiedPlaintext,
};
pub use consttime::{ConstTime, HardwareCtMode};
pub use kem::{HybridClientSecret, HybridSharedSecret, KemError, SystemRng, server_encapsulate};
pub use obligation::{LeakAuditor, LeakReport, ObligationKind};
pub use secret::{SecMask64, SecU8, SecU32, SecU64, SecVec, verify_tag_16_ct};
pub use slot::{LockPolicy, SecretSlot, ShredReceipt, SlotError, SlotProtection};
pub use sys::Protection;
pub use verifier::{CtPolicy, DECLASSIFY_MARKER, VerifierFinding, verify_constant_time};
