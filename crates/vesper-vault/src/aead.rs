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
//! Under `cfg(vesper_port)`, nonce accounting, header framing, limits, and the
//! Doom-principle opener run in the compiled Vesper port `@vesper/vault` over
//! `@vesper/crypto` (`vesper-crypto-sys`), while [`SecretSlot`] holds the locked
//! key material and `vesper_crypto_sys::Aead` stays inline in the sealer/opener
//! so steady-state `seal_in_place` and `open_in_place` make zero heap
//! allocations. `vesper-vault-seed` (`cfg(not(vesper_port))`) runs the frozen
//! Rust engine in `src/reference/`.

use core::fmt;
use core::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use zeroize::Zeroize;

use crate::obligation::{ObligationKind, record_leak};
#[cfg(vesper_port)]
use crate::port;
#[cfg(not(vesper_port))]
use crate::reference::aead as ref_aead;
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
    const ALG_CODE: u32;
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
    const ALG_CODE: u32 = 2;
}

impl AeadAlgorithm for ChaCha20Poly1305 {
    const NAME: &'static str = "ChaCha20-Poly1305";
    const RECORD_LIMIT: u64 = SEQ_SPACE_LIMIT;
    const ALG_CODE: u32 = 3;
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

#[cfg(vesper_port)]
fn expand_aead<A: AeadAlgorithm>(key: &SecretSlot<KEY_LEN>) -> vesper_crypto_sys::Aead {
    let alg = port::b::alg_code(port::aead_kind(A::ALG_CODE));
    key.with(|k| {
        vesper_crypto_sys::Aead::new(alg, k)
            .expect("32-byte key matches Aes256Gcm or ChaCha20Poly1305")
    })
}

/// A sealer for one key epoch. Linear: retire it with [`AeadSealer::retire`]
/// or replace its key with [`AeadSealer::rotate`].
#[must_use = "an AeadSealer must be retired so its key is shredded"]
pub struct AeadSealer<A: AeadAlgorithm> {
    key: Option<SecretSlot<KEY_LEN>>,
    #[cfg(vesper_port)]
    aead: Option<vesper_crypto_sys::Aead>,
    #[cfg(vesper_port)]
    st: port::b::repr::SealerSt,
    #[cfg(not(vesper_port))]
    st: ref_aead::SealerState,
    _alg: PhantomData<fn() -> A>,
}

impl<A: AeadAlgorithm> fmt::Debug for AeadSealer<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AeadSealer")
            .field("alg", &A::NAME)
            .field("epoch", &self.epoch())
            .field("next_seq", &self.st.next_seq)
            .field("limit", &self.st.limit)
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
        let id = NEXT_SEALER_ID.fetch_add(1, Ordering::Relaxed);
        #[cfg(vesper_port)]
        {
            let aead = expand_aead::<A>(&key);
            let st = port::b::sealer_new(id, port::aead_kind(A::ALG_CODE), epoch, next_seq, limit);
            Self {
                key: Some(key),
                aead: Some(aead),
                st,
                _alg: PhantomData,
            }
        }
        #[cfg(not(vesper_port))]
        {
            let st = ref_aead::SealerState::new(
                id,
                ref_aead::Alg::from_code(A::ALG_CODE),
                epoch,
                next_seq,
                limit,
            );
            Self {
                key: Some(key),
                st,
                _alg: PhantomData,
            }
        }
    }

    /// Current key epoch.
    pub fn epoch(&self) -> u32 {
        #[cfg(vesper_port)]
        {
            self.st.epoch.get()
        }
        #[cfg(not(vesper_port))]
        {
            self.st.epoch
        }
    }

    /// Next sequence number `mint_nonce` will hand out.
    pub fn next_seq(&self) -> u64 {
        self.st.next_seq
    }

    /// Tickets left before a rekey is required.
    pub fn remaining(&self) -> u64 {
        #[cfg(vesper_port)]
        {
            port::b::sealer_remaining(&self.st)
        }
        #[cfg(not(vesper_port))]
        {
            self.st.remaining()
        }
    }

    /// Mint the next ticket. Strictly increasing per key; refuses at the
    /// record limit so the counter can never wrap.
    pub fn mint_nonce(&mut self) -> Result<NonceTicket<A>, CryptoError> {
        #[cfg(vesper_port)]
        let seq = port::b::sealer_mint_nonce(&mut self.st).map_err(port::crypto_err)?;
        #[cfg(not(vesper_port))]
        let seq = self.st.mint_nonce()?;
        Ok(NonceTicket {
            epoch: self.epoch(),
            seq,
            sealer: self.st.id,
            armed: true,
            _alg: PhantomData,
        })
    }

    #[cfg(not(vesper_port))]
    fn key(&self) -> &SecretSlot<KEY_LEN> {
        match &self.key {
            Some(k) => k,
            None => unreachable!("sealer used after retire"),
        }
    }

    /// Seal `frame[28..]` in place under `ticket`, writing the header into
    /// `frame[..28]`. Allocation-free on frames up to 16 KiB.
    pub fn seal_in_place(
        &self,
        ticket: NonceTicket<A>,
        aad: &[u8],
        frame: &mut [u8],
    ) -> Result<Sealed, SealError<A>> {
        #[cfg(vesper_port)]
        {
            let frame_len = frame.len();
            let payload_len = frame_len.saturating_sub(FRAME_HEADER_LEN);
            let work_len = aad
                .len()
                .saturating_add(payload_len)
                .saturating_add(TAG_LEN);
            let mut nonce = [0u8; 12];
            let res = port::with_work_buf(work_len, |work| {
                let aead = self.aead.as_ref().expect("sealer used after retire");
                port::b::sealer_seal_frame(
                    &self.st,
                    ticket.sealer,
                    ticket.epoch,
                    ticket.seq,
                    port::lend_aead(aead),
                    aad,
                    frame,
                    &mut nonce,
                    work,
                )
            });
            nonce.zeroize();
            match res {
                Ok(len) => {
                    let (epoch, seq) = ticket.defuse();
                    Ok(Sealed {
                        epoch,
                        seq,
                        frame_len: usize::try_from(len).unwrap_or(frame_len),
                    })
                }
                Err(e) => {
                    let error = port::crypto_err(e);
                    match error {
                        CryptoError::ForeignTicket | CryptoError::BufferTooSmall => {
                            Err(SealError {
                                error,
                                ticket: Some(ticket),
                            })
                        }
                        _ => {
                            let _ = ticket.defuse();
                            Err(SealError {
                                error,
                                ticket: None,
                            })
                        }
                    }
                }
            }
        }
        #[cfg(not(vesper_port))]
        {
            let ticket_sealer = ticket.sealer;
            let ticket_epoch = ticket.epoch;
            let ticket_seq = ticket.seq;
            let res = self.key().with(|k| {
                ref_aead::seal_frame(
                    &self.st,
                    k,
                    ticket_sealer,
                    ticket_epoch,
                    ticket_seq,
                    aad,
                    frame,
                )
            });
            match res {
                Ok(frame_len) => {
                    let (epoch, seq) = ticket.defuse();
                    Ok(Sealed {
                        epoch,
                        seq,
                        frame_len,
                    })
                }
                Err(error @ (CryptoError::ForeignTicket | CryptoError::BufferTooSmall)) => {
                    Err(SealError {
                        error,
                        ticket: Some(ticket),
                    })
                }
                Err(error) => {
                    let _ = ticket.defuse();
                    Err(SealError {
                        error,
                        ticket: None,
                    })
                }
            }
        }
    }

    /// Replace the key with `new_key` at `epoch + 1`, restarting the counter
    /// and shredding the old key.
    pub fn rotate(
        mut self,
        new_key: SecretSlot<KEY_LEN>,
    ) -> Result<(Self, ShredReceipt), Box<RotateError<A>>> {
        #[cfg(vesper_port)]
        let epoch_res = port::b::sealer_rotate_epoch(&self.st).map_err(port::crypto_err);
        #[cfg(not(vesper_port))]
        let epoch_res = self.st.rotate_epoch();
        let epoch = match epoch_res {
            Ok(e) => e,
            Err(error) => {
                return Err(Box::new(RotateError {
                    error,
                    sealer: self,
                    new_key,
                }));
            }
        };
        let limit = self.st.limit;
        #[cfg(vesper_port)]
        drop(self.aead.take());
        let old = self.key.take();
        let receipt = match old {
            Some(k) => k.shred(),
            None => unreachable!("sealer used after retire"),
        };
        Ok((Self::with_record_limit(new_key, epoch, limit), receipt))
    }

    /// Shred the key and end the sealer.
    pub fn retire(mut self) -> ShredReceipt {
        #[cfg(vesper_port)]
        drop(self.aead.take());
        match self.key.take() {
            Some(k) => k.shred(),
            None => unreachable!("sealer retired twice"),
        }
    }
}

impl<A: AeadAlgorithm> Drop for AeadSealer<A> {
    fn drop(&mut self) {
        #[cfg(vesper_port)]
        drop(self.aead.take());
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
        #[cfg(vesper_port)]
        {
            port::frame_header_parse(frame)
        }
        #[cfg(not(vesper_port))]
        {
            ref_aead::frame_header_parse(frame)
        }
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
    #[cfg(vesper_port)]
    aead: Option<vesper_crypto_sys::Aead>,
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
        #[cfg(vesper_port)]
        let aead = Some(expand_aead::<A>(&key));
        Self {
            key: Some(key),
            #[cfg(vesper_port)]
            aead,
            epoch,
            _alg: PhantomData,
        }
    }

    /// The key epoch this opener accepts.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Authenticate and decrypt in place. On any failure the whole frame
    /// buffer is zeroized before the error is returned. Allocation-free on
    /// frames up to 16 KiB.
    pub fn open_in_place<'a>(
        &self,
        aad: &[u8],
        frame: CiphertextFrame<'a>,
    ) -> Result<VerifiedPlaintext<'a>, AuthError> {
        let buf = frame.take();
        #[cfg(vesper_port)]
        {
            let frame_len = buf.len();
            let ct_len = frame_len.saturating_sub(FRAME_HEADER_LEN);
            let work_len = aad.len().saturating_add(ct_len).saturating_add(TAG_LEN);
            let mut nonce = [0u8; 12];
            let res = port::with_work_buf(work_len, |work| {
                let aead = self.aead.as_ref().expect("opener used after retire");
                port::b::opener_open_frame(
                    self.epoch,
                    port::lend_aead(aead),
                    aad,
                    buf,
                    &mut nonce,
                    work,
                )
            });
            nonce.zeroize();
            match res {
                Ok(hdr) => Ok(VerifiedPlaintext {
                    header: port::frame_header_from_hdr(hdr),
                    payload: &mut buf[FRAME_HEADER_LEN..],
                }),
                Err(e) => Err(port::auth_err(e)),
            }
        }
        #[cfg(not(vesper_port))]
        {
            let key = match &self.key {
                Some(k) => k,
                None => unreachable!("opener used after retire"),
            };
            let header = key.with(|k| {
                ref_aead::open_frame(
                    ref_aead::Alg::from_code(A::ALG_CODE),
                    self.epoch,
                    k,
                    aad,
                    buf,
                )
            })?;
            Ok(VerifiedPlaintext {
                header,
                payload: &mut buf[FRAME_HEADER_LEN..],
            })
        }
    }

    /// Shred the key and end the opener.
    pub fn retire(mut self) -> ShredReceipt {
        #[cfg(vesper_port)]
        drop(self.aead.take());
        match self.key.take() {
            Some(k) => k.shred(),
            None => unreachable!("opener retired twice"),
        }
    }
}

impl<A: AeadAlgorithm> Drop for AeadOpener<A> {
    fn drop(&mut self) {
        #[cfg(vesper_port)]
        drop(self.aead.take());
        if let Some(k) = self.key.take() {
            record_leak(ObligationKind::AeadKey);
            let _ = k.shred();
        }
    }
}
