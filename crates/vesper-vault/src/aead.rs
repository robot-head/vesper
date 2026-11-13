//! §23.3 linear nonce tickets and the Doom-principle opener.
//!
//! An [`AeadSealer`] owns one key (in a [`SecretSlot`]), a key epoch and a
//! strictly increasing 64-bit sequence counter. [`AeadSealer::mint_nonce`]
//! (`&mut self`, so exclusive) is the only way to obtain a [`NonceTicket`],
//! and [`AeadSealer::seal_in_place`] consumes the ticket by value. Tickets are
//! not `Clone` or `Copy`, so one ticket cannot seal two frames, and a ticket
//! only seals under the sealer that minted it (checked at run time; a
//! mismatched ticket is handed back). The 96-bit nonce is
//! `key_epoch (u32 BE) || seq (u64 BE)`, so distinct tickets of one key always
//! give distinct nonces.
//!
//! Frames use the §23.3 28-byte wire header
//! `key_epoch: u32be, nonce_seq: u64be, tag: [u8; 16]` followed by the
//! payload, encrypted in place. [`AeadOpener::open_in_place`] consumes a
//! [`CiphertextFrame`] and either returns [`VerifiedPlaintext`] or zeroizes
//! the entire frame buffer before returning [`AuthError`], so unauthenticated
//! bytes are never left addressable.
//!
//! AES-256-GCM and ChaCha20-Poly1305 come from the RustCrypto `aes-gcm` and
//! `chacha20poly1305` crates already in the workspace. Their own tag checks
//! (via `subtle`) decide authenticity; this module adds the linear wrappers.

use core::fmt;
use core::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use zeroize::Zeroize;

use crate::obligation::{ObligationKind, record_leak};
use crate::slot::{SecretSlot, ShredReceipt};

/// Size of [`FrameHeader`] on the wire: 4 + 8 + 16 bytes, no padding.
pub const FRAME_HEADER_LEN: usize = 28;
/// AEAD key length for both algorithms.
pub const KEY_LEN: usize = 32;
/// AEAD tag length for both algorithms.
pub const TAG_LEN: usize = 16;
/// The §23.3 sequence bound: `mint_nonce` refuses once `next_seq` reaches it.
pub const SEQ_SPACE_LIMIT: u64 = 0xFFFF_FFFF_FFFF_FFFE;

mod private {
    pub trait Sealed {}
}

/// An AEAD cipher Vault can seal with (sealed trait).
pub trait AeadAlgorithm: private::Sealed + 'static {
    /// Human-readable name.
    const NAME: &'static str;
    /// Records one key may seal before it must be rotated.
    const RECORD_LIMIT: u64;
    #[doc(hidden)]
    fn seal_detached(
        key: &[u8; KEY_LEN],
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], aes_gcm::aead::Error>;
    #[doc(hidden)]
    fn open_detached(
        key: &[u8; KEY_LEN],
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), aes_gcm::aead::Error>;
}

/// AES-256-GCM. Record limit 2^24.5 (about 23.7 million) full-size records
/// per key, the RFC 8446 §5.5 confidentiality bound.
#[derive(Clone, Copy, Debug)]
pub enum Aes256Gcm {}

/// ChaCha20-Poly1305. Its RFC 8446 §5.5 bound exceeds 2^64 records, so the
/// §23.3 sequence space ([`SEQ_SPACE_LIMIT`]) is the limit.
#[derive(Clone, Copy, Debug)]
pub enum ChaCha20Poly1305 {}

impl private::Sealed for Aes256Gcm {}
impl private::Sealed for ChaCha20Poly1305 {}

impl AeadAlgorithm for Aes256Gcm {
    const NAME: &'static str = "AES-256-GCM";
    const RECORD_LIMIT: u64 = 23_726_566;

    fn seal_detached(
        key: &[u8; KEY_LEN],
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], aes_gcm::aead::Error> {
        let cipher = aes_gcm::Aes256Gcm::new(key.into());
        let tag = cipher.encrypt_in_place_detached(nonce.into(), aad, buf)?;
        Ok(tag.into())
    }

    fn open_detached(
        key: &[u8; KEY_LEN],
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), aes_gcm::aead::Error> {
        let cipher = aes_gcm::Aes256Gcm::new(key.into());
        cipher.decrypt_in_place_detached(nonce.into(), aad, buf, tag.into())
    }
}

impl AeadAlgorithm for ChaCha20Poly1305 {
    const NAME: &'static str = "ChaCha20-Poly1305";
    const RECORD_LIMIT: u64 = SEQ_SPACE_LIMIT;

    fn seal_detached(
        key: &[u8; KEY_LEN],
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], aes_gcm::aead::Error> {
        let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
        let tag = cipher.encrypt_in_place_detached(nonce.into(), aad, buf)?;
        Ok(tag.into())
    }

    fn open_detached(
        key: &[u8; KEY_LEN],
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), aes_gcm::aead::Error> {
        let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
        cipher.decrypt_in_place_detached(nonce.into(), aad, buf, tag.into())
    }
}

fn nonce_for(epoch: u32, seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&epoch.to_be_bytes());
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

/// Errors from sealing and key management.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The key reached its record limit; rotate before sealing more.
    EpochExhaustedRekeyRequired,
    /// The 32-bit key epoch space is exhausted.
    EpochSpaceExhausted,
    /// The frame buffer is shorter than the 28-byte header.
    BufferTooSmall,
    /// The ticket was minted by a different sealer.
    ForeignTicket,
    /// The underlying cipher refused the input (for example a payload over
    /// the algorithm's maximum length). The nonce is treated as spent.
    CipherRejected,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CryptoError::EpochExhaustedRekeyRequired => "record limit reached; rekey required",
            CryptoError::EpochSpaceExhausted => "key epoch space exhausted",
            CryptoError::BufferTooSmall => "frame buffer smaller than the 28-byte header",
            CryptoError::ForeignTicket => "nonce ticket belongs to another sealer",
            CryptoError::CipherRejected => "cipher rejected the input",
        };
        f.write_str(s)
    }
}

impl std::error::Error for CryptoError {}

static NEXT_SEALER_ID: AtomicU64 = AtomicU64::new(1);

/// A single-use nonce: `(epoch, seq)` under one sealer.
///
/// Linear: not `Clone`, not `Copy`; exits are [`AeadSealer::seal_in_place`]
/// and [`NonceTicket::burn`]. Dropping it otherwise is counted as a leak.
#[must_use = "a NonceTicket must seal exactly one frame or be burned"]
pub struct NonceTicket<A: AeadAlgorithm> {
    epoch: u32,
    seq: u64,
    sealer: u64,
    armed: bool,
    _alg: PhantomData<fn() -> A>,
}

impl<A: AeadAlgorithm> fmt::Debug for NonceTicket<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NonceTicket")
            .field("alg", &A::NAME)
            .field("epoch", &self.epoch)
            .field("seq", &self.seq)
            .finish()
    }
}

/// Record of a ticket burned on a pre-send abort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BurnedNonce {
    /// Key epoch.
    pub epoch: u32,
    /// Sequence number that will never be used.
    pub seq: u64,
}

impl<A: AeadAlgorithm> NonceTicket<A> {
    /// Key epoch of this nonce.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Sequence number of this nonce.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Explicitly abandon the nonce (it is never reused: the sealer's
    /// counter has already moved past it).
    pub fn burn(mut self) -> BurnedNonce {
        self.armed = false;
        BurnedNonce {
            epoch: self.epoch,
            seq: self.seq,
        }
    }

    fn defuse(mut self) -> (u32, u64) {
        self.armed = false;
        (self.epoch, self.seq)
    }
}

impl<A: AeadAlgorithm> Drop for NonceTicket<A> {
    fn drop(&mut self) {
        if self.armed {
            record_leak(ObligationKind::NonceTicket);
        }
    }
}

/// A failed seal. When the ticket was not spent it is handed back.
pub struct SealError<A: AeadAlgorithm> {
    /// Why sealing failed.
    pub error: CryptoError,
    /// The unspent ticket, if the cipher never saw its nonce.
    pub ticket: Option<NonceTicket<A>>,
}

impl<A: AeadAlgorithm> fmt::Debug for SealError<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealError")
            .field("error", &self.error)
            .field("ticket", &self.ticket)
            .finish()
    }
}

/// A successful seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sealed {
    /// Key epoch written to the header.
    pub epoch: u32,
    /// Sequence number written to the header.
    pub seq: u64,
    /// Total frame length (header plus payload).
    pub frame_len: usize,
}

/// A sealer for one key epoch. Linear: retire it with [`AeadSealer::retire`]
/// or replace its key with [`AeadSealer::rotate`].
#[must_use = "an AeadSealer must be retired so its key is shredded"]
pub struct AeadSealer<A: AeadAlgorithm> {
    key: Option<SecretSlot<KEY_LEN>>,
    epoch: u32,
    next_seq: u64,
    limit: u64,
    id: u64,
    _alg: PhantomData<fn() -> A>,
}

impl<A: AeadAlgorithm> fmt::Debug for AeadSealer<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AeadSealer")
            .field("alg", &A::NAME)
            .field("epoch", &self.epoch)
            .field("next_seq", &self.next_seq)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// A failed [`AeadSealer::rotate`]: both owners come back.
pub struct RotateError<A: AeadAlgorithm> {
    /// Why rotation failed.
    pub error: CryptoError,
    /// The unchanged sealer.
    pub sealer: AeadSealer<A>,
    /// The unused new key.
    pub new_key: SecretSlot<KEY_LEN>,
}

impl<A: AeadAlgorithm> fmt::Debug for RotateError<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RotateError")
            .field("error", &self.error)
            .field("sealer", &self.sealer)
            .finish_non_exhaustive()
    }
}

impl<A: AeadAlgorithm> AeadSealer<A> {
    /// A sealer for `key` at `epoch`, starting at sequence 0, limited to
    /// `A::RECORD_LIMIT` records.
    pub fn new(key: SecretSlot<KEY_LEN>, epoch: u32) -> Self {
        Self::with_record_limit(key, epoch, A::RECORD_LIMIT)
    }

    /// Like [`AeadSealer::new`] with a lower record limit (clamped to
    /// `A::RECORD_LIMIT`), for policies stricter than the algorithm's.
    pub fn with_record_limit(key: SecretSlot<KEY_LEN>, epoch: u32, limit: u64) -> Self {
        Self::with_start(key, epoch, 0, limit)
    }

    /// Resume a sealer whose counter is persisted (for example in a WAL):
    /// sequences below `next_seq` are never minted again.
    pub fn with_start(key: SecretSlot<KEY_LEN>, epoch: u32, next_seq: u64, limit: u64) -> Self {
        Self {
            key: Some(key),
            epoch,
            next_seq,
            limit: limit.min(A::RECORD_LIMIT).min(SEQ_SPACE_LIMIT),
            id: NEXT_SEALER_ID.fetch_add(1, Ordering::Relaxed),
            _alg: PhantomData,
        }
    }

    /// Current key epoch.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Next sequence number `mint_nonce` will hand out.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Tickets left before a rekey is required.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.next_seq)
    }

    /// Mint the next ticket. Strictly increasing per key; refuses at the
    /// record limit so the counter can never wrap.
    pub fn mint_nonce(&mut self) -> Result<NonceTicket<A>, CryptoError> {
        if self.next_seq >= self.limit {
            return Err(CryptoError::EpochExhaustedRekeyRequired);
        }
        let seq = self.next_seq;
        // Cannot overflow: seq < limit <= SEQ_SPACE_LIMIT < u64::MAX.
        self.next_seq = seq + 1;
        Ok(NonceTicket {
            epoch: self.epoch,
            seq,
            sealer: self.id,
            armed: true,
            _alg: PhantomData,
        })
    }

    fn key(&self) -> &SecretSlot<KEY_LEN> {
        match &self.key {
            Some(k) => k,
            None => unreachable!("sealer used after retire"),
        }
    }

    /// Seal `frame[28..]` in place under `ticket`, writing the header into
    /// `frame[..28]`. Allocation-free.
    pub fn seal_in_place(
        &self,
        ticket: NonceTicket<A>,
        aad: &[u8],
        frame: &mut [u8],
    ) -> Result<Sealed, SealError<A>> {
        if ticket.sealer != self.id {
            return Err(SealError {
                error: CryptoError::ForeignTicket,
                ticket: Some(ticket),
            });
        }
        if frame.len() < FRAME_HEADER_LEN {
            return Err(SealError {
                error: CryptoError::BufferTooSmall,
                ticket: Some(ticket),
            });
        }
        let (epoch, seq) = ticket.defuse();
        let nonce = nonce_for(epoch, seq);
        let (hdr, payload) = frame.split_at_mut(FRAME_HEADER_LEN);
        let tag = self
            .key()
            .with(|k| A::seal_detached(k, &nonce, aad, payload));
        match tag {
            Ok(tag) => {
                hdr[0..4].copy_from_slice(&epoch.to_be_bytes());
                hdr[4..12].copy_from_slice(&seq.to_be_bytes());
                hdr[12..28].copy_from_slice(&tag);
                Ok(Sealed {
                    epoch,
                    seq,
                    frame_len: frame.len(),
                })
            }
            Err(_) => {
                frame.zeroize();
                Err(SealError {
                    error: CryptoError::CipherRejected,
                    ticket: None,
                })
            }
        }
    }

    /// Replace the key with `new_key` at `epoch + 1`, restarting the counter
    /// and shredding the old key.
    pub fn rotate(
        mut self,
        new_key: SecretSlot<KEY_LEN>,
    ) -> Result<(Self, ShredReceipt), Box<RotateError<A>>> {
        let Some(epoch) = self.epoch.checked_add(1) else {
            return Err(Box::new(RotateError {
                error: CryptoError::EpochSpaceExhausted,
                sealer: self,
                new_key,
            }));
        };
        let limit = self.limit;
        let old = self.key.take();
        let receipt = match old {
            Some(k) => k.shred(),
            None => unreachable!("sealer used after retire"),
        };
        Ok((Self::with_record_limit(new_key, epoch, limit), receipt))
    }

    /// Shred the key and end the sealer.
    pub fn retire(mut self) -> ShredReceipt {
        match self.key.take() {
            Some(k) => k.shred(),
            None => unreachable!("sealer retired twice"),
        }
    }
}

impl<A: AeadAlgorithm> Drop for AeadSealer<A> {
    fn drop(&mut self) {
        if let Some(k) = self.key.take() {
            record_leak(ObligationKind::AeadKey);
            let _ = k.shred();
        }
    }
}

/// The parsed §23.3 `EncryptedFrameWireHeader`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// `key_epoch: u32be`.
    pub key_epoch: u32,
    /// `nonce_seq: u64be`.
    pub nonce_seq: u64,
    /// `tag: ByteArray[16]`.
    pub tag: [u8; TAG_LEN],
}

impl FrameHeader {
    /// Parse the first 28 bytes of `frame`.
    pub fn parse(frame: &[u8]) -> Option<Self> {
        let hdr = frame.get(..FRAME_HEADER_LEN)?;
        Some(Self {
            key_epoch: u32::from_be_bytes(hdr[0..4].try_into().ok()?),
            nonce_seq: u64::from_be_bytes(hdr[4..12].try_into().ok()?),
            tag: hdr[12..28].try_into().ok()?,
        })
    }
}

/// A received, not yet authenticated frame. Linear: exits are
/// [`AeadOpener::open_in_place`] and [`CiphertextFrame::discard`]; both
/// leave nothing unauthenticated readable.
#[must_use = "a CiphertextFrame must be opened or discarded"]
pub struct CiphertextFrame<'a> {
    buf: Option<&'a mut [u8]>,
}

impl fmt::Debug for CiphertextFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.buf.as_ref().map_or(0, |b| b.len());
        write!(f, "CiphertextFrame({len} bytes)")
    }
}

impl<'a> CiphertextFrame<'a> {
    /// Quarantine a received buffer. Its bytes are not readable through the
    /// frame until authenticated.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf: Some(buf) }
    }

    /// Buffer length (public).
    pub fn len(&self) -> usize {
        self.buf.as_ref().map_or(0, |b| b.len())
    }

    /// Is the buffer empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Zeroize and give up the frame without opening it.
    pub fn discard(mut self) {
        if let Some(b) = self.buf.take() {
            b.zeroize();
        }
    }

    fn take(mut self) -> &'a mut [u8] {
        match self.buf.take() {
            Some(b) => b,
            None => unreachable!("frame consumed twice"),
        }
    }
}

impl Drop for CiphertextFrame<'_> {
    fn drop(&mut self) {
        if let Some(b) = self.buf.take() {
            record_leak(ObligationKind::CiphertextFrame);
            b.zeroize();
        }
    }
}

/// Authenticated plaintext, borrowed from the frame buffer.
pub struct VerifiedPlaintext<'a> {
    header: FrameHeader,
    payload: &'a mut [u8],
}

impl fmt::Debug for VerifiedPlaintext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedPlaintext")
            .field("epoch", &self.header.key_epoch)
            .field("seq", &self.header.nonce_seq)
            .field("len", &self.payload.len())
            .finish()
    }
}

impl<'a> VerifiedPlaintext<'a> {
    /// The authenticated header.
    pub fn header(&self) -> FrameHeader {
        self.header
    }

    /// The authenticated plaintext.
    pub fn bytes(&self) -> &[u8] {
        self.payload
    }

    /// The authenticated plaintext, mutably.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.payload
    }

    /// Give back the plaintext slice.
    pub fn into_inner(self) -> &'a mut [u8] {
        self.payload
    }
}

/// Why a frame failed to open. The buffer has been zeroized in every case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Shorter than the 28-byte header.
    MalformedHeader,
    /// The header names a key epoch this opener does not hold.
    WrongEpoch {
        /// The opener's epoch.
        expected: u32,
        /// The header's epoch.
        found: u32,
    },
    /// Poly1305 or GHASH tag mismatch.
    AuthenticationTagMismatch,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::MalformedHeader => f.write_str("frame shorter than its header"),
            AuthError::WrongEpoch { expected, found } => {
                write!(f, "frame key epoch {found}, opener holds {expected}")
            }
            AuthError::AuthenticationTagMismatch => f.write_str("authentication tag mismatch"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The receive side of one key epoch. Linear like [`AeadSealer`].
#[must_use = "an AeadOpener must be retired so its key is shredded"]
pub struct AeadOpener<A: AeadAlgorithm> {
    key: Option<SecretSlot<KEY_LEN>>,
    epoch: u32,
    _alg: PhantomData<fn() -> A>,
}

impl<A: AeadAlgorithm> fmt::Debug for AeadOpener<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AeadOpener")
            .field("alg", &A::NAME)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl<A: AeadAlgorithm> AeadOpener<A> {
    /// An opener for `key` at `epoch`.
    pub fn new(key: SecretSlot<KEY_LEN>, epoch: u32) -> Self {
        Self {
            key: Some(key),
            epoch,
            _alg: PhantomData,
        }
    }

    /// The key epoch this opener accepts.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Authenticate and decrypt in place. On any failure the whole frame
    /// buffer is zeroized before the error is returned. Allocation-free.
    pub fn open_in_place<'a>(
        &self,
        aad: &[u8],
        frame: CiphertextFrame<'a>,
    ) -> Result<VerifiedPlaintext<'a>, AuthError> {
        let buf = frame.take();
        let Some(header) = FrameHeader::parse(buf) else {
            buf.zeroize();
            return Err(AuthError::MalformedHeader);
        };
        if header.key_epoch != self.epoch {
            buf.zeroize();
            return Err(AuthError::WrongEpoch {
                expected: self.epoch,
                found: header.key_epoch,
            });
        }
        let nonce = nonce_for(header.key_epoch, header.nonce_seq);
        let key = match &self.key {
            Some(k) => k,
            None => unreachable!("opener used after retire"),
        };
        let (_, payload) = buf.split_at_mut(FRAME_HEADER_LEN);
        let ok = key.with(|k| A::open_detached(k, &nonce, aad, payload, &header.tag));
        if ok.is_err() {
            // Doom principle: scrub before the error is observable.
            buf.zeroize();
            return Err(AuthError::AuthenticationTagMismatch);
        }
        Ok(VerifiedPlaintext {
            header,
            payload: &mut buf[FRAME_HEADER_LEN..],
        })
    }

    /// Shred the key and end the opener.
    pub fn retire(mut self) -> ShredReceipt {
        match self.key.take() {
            Some(k) => k.shred(),
            None => unreachable!("opener retired twice"),
        }
    }
}

impl<A: AeadAlgorithm> Drop for AeadOpener<A> {
    fn drop(&mut self) {
        if let Some(k) = self.key.take() {
            record_leak(ObligationKind::AeadKey);
            let _ = k.shred();
        }
    }
}
