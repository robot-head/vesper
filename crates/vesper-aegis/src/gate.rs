//! The §33.3 confidential inference gate: snapshot a host frame once into
//! private memory, authenticate and decrypt it there, evaluate, seal the
//! reply under a fresh nonce ticket, and let only ciphertext cross back.
//! Every control path discharges the bounce grant (flush or scrub).
//!
//! Frame layout in the bounce slot (both directions):
//! `nonce[12] || ciphertext || tag[16]`, AEAD = ChaCha20-Poly1305 with the
//! 48-byte channel transcript hash as associated data.
//!
//! After warm-up, `process_attested_enclave_req` performs no heap
//! allocation (`tests/zero_alloc.rs` measures this with a counting
//! allocator).
//!
//! [`VaultEnclaveGate`] is the §33.1 item 3 form: keys in Vault secret
//! slots, Vault nonce tickets and frames, and a receive-side replay check.
//! [`EnclaveGate`] (gate-held ChaCha20-Poly1305 keys) is deprecated and
//! kept until the end of wave six for API stability.

use crate::error::TeeError;
use crate::host::{
    BounceAperture, CiphertextSlice, HostBounceGrant, PrivateRegion, SealedFrame, UntrustedHostVal,
};
use crate::linear::{Ledger, Obligation};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use std::sync::Arc;

pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
pub const FRAME_OVERHEAD: usize = NONCE_LEN + TAG_LEN;
pub const REPLY_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnclaveError {
    /// The host-reported length or slot was invalid (Iago attempt).
    HostIagoAttempt(TeeError),
    /// Snapshot shorter than nonce + tag.
    FrameTooShort {
        len: usize,
    },
    UnauthenticatedPayload,
    /// The transmit nonce sequence is exhausted.
    RekeyRequired,
    SealFailed,
    HostEgress(TeeError),
}

/// Linear single-use nonce. Consumed by sealing (even when sealing fails,
/// since the nonce may have been used).
#[must_use = "a NonceTicket must be consumed by sealing"]
#[derive(Debug)]
pub struct NonceTicket {
    nonce: [u8; NONCE_LEN],
    obligation: Obligation,
}

/// Counter-based nonce source: `prefix[4] || counter_be[8]`, never reused.
#[derive(Debug)]
pub struct NonceSequence {
    prefix: [u8; 4],
    next: u64,
    limit: u64,
    ledger: Arc<Ledger>,
}

impl NonceSequence {
    pub fn new(prefix: [u8; 4], limit: u64) -> Self {
        NonceSequence {
            prefix,
            next: 0,
            limit,
            ledger: Ledger::new(),
        }
    }

    pub fn mint(&mut self) -> Result<NonceTicket, EnclaveError> {
        if self.next >= self.limit {
            return Err(EnclaveError::RekeyRequired);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..4].copy_from_slice(&self.prefix);
        nonce[4..].copy_from_slice(&self.next.to_be_bytes());
        self.next += 1;
        Ok(NonceTicket {
            nonce,
            obligation: Obligation::new(&self.ledger),
        })
    }

    /// Auditor: tickets dropped without sealing.
    pub fn leaked_tickets(&self) -> u64 {
        self.ledger.leaked()
    }
}

/// Result of one gate run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateReply {
    pub plaintext_len: usize,
    /// The reply plaintext (only its ciphertext reached the host).
    pub reply: [u8; REPLY_LEN],
}

#[deprecated(note = "keys are held outside Vault; use VaultEnclaveGate")]
pub struct EnclaveGate {
    rx: ChaCha20Poly1305,
    tx: ChaCha20Poly1305,
    aad: [u8; 48],
    tx_nonces: NonceSequence,
}

#[allow(deprecated)]
impl EnclaveGate {
    pub fn new(
        rx_key: &[u8; 32],
        tx_key: &[u8; 32],
        aad_transcript: [u8; 48],
        tx: NonceSequence,
    ) -> Self {
        EnclaveGate {
            rx: ChaCha20Poly1305::new(Key::from_slice(rx_key)),
            tx: ChaCha20Poly1305::new(Key::from_slice(tx_key)),
            aad: aad_transcript,
            tx_nonces: tx,
        }
    }

    pub fn nonces(&self) -> &NonceSequence {
        &self.tx_nonces
    }

    fn seal(
        &self,
        ticket: NonceTicket,
        buf: &mut [u8],
    ) -> Result<([u8; NONCE_LEN], [u8; TAG_LEN]), EnclaveError> {
        let nonce = ticket.nonce;
        ticket.obligation.discharge();
        let tag = self
            .tx
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), &self.aad, buf)
            .map_err(|_| EnclaveError::SealFailed)?;
        Ok((nonce, tag.into()))
    }

    /// Process one request (design §33.3).
    pub fn process_attested_enclave_req(
        &mut self,
        aperture: &mut BounceAperture,
        scratch: &mut PrivateRegion,
        bounce_slot: HostBounceGrant,
        host_reported_len: UntrustedHostVal<u32>,
        evaluate: &mut dyn FnMut(&[u8], &mut [u8; REPLY_LEN]),
    ) -> Result<GateReply, EnclaveError> {
        self.process_observed(
            aperture,
            scratch,
            bounce_slot,
            host_reported_len,
            evaluate,
            &mut |_| {},
        )
    }

    /// As [`Self::process_attested_enclave_req`], calling `observe` with the
    /// private snapshot right after it is taken (audit and test hook; the
    /// snapshot is enclave-private data).
    pub fn process_observed(
        &mut self,
        aperture: &mut BounceAperture,
        scratch: &mut PrivateRegion,
        bounce_slot: HostBounceGrant,
        host_reported_len: UntrustedHostVal<u32>,
        evaluate: &mut dyn FnMut(&[u8], &mut [u8; REPLY_LEN]),
        observe: &mut dyn FnMut(&[u8]),
    ) -> Result<GateReply, EnclaveError> {
        scratch.reset();
        let result = self.run(
            aperture,
            scratch,
            bounce_slot,
            host_reported_len,
            evaluate,
            observe,
        );
        scratch.reset();
        result
    }

    fn run(
        &mut self,
        aperture: &mut BounceAperture,
        scratch: &mut PrivateRegion,
        bounce_slot: HostBounceGrant,
        host_reported_len: UntrustedHostVal<u32>,
        evaluate: &mut dyn FnMut(&[u8], &mut [u8; REPLY_LEN]),
        observe: &mut dyn FnMut(&[u8]),
    ) -> Result<GateReply, EnclaveError> {
        // 1. Single fetch into private memory.
        let snap =
            match aperture.snapshot_from_host_bounce(&bounce_slot, host_reported_len, scratch) {
                Ok(s) => s,
                Err(e) => {
                    aperture.abort_and_scrub_bounce(bounce_slot);
                    return Err(EnclaveError::HostIagoAttempt(e));
                }
            };
        observe(snap.as_slice());
        let frame = snap.into_mut();
        let len = frame.len();
        if len < FRAME_OVERHEAD {
            aperture.abort_and_scrub_bounce(bounce_slot);
            return Err(EnclaveError::FrameTooShort { len });
        }
        // 2. Authenticate and decrypt inside private memory.
        let (nonce, rest) = frame.split_at_mut(NONCE_LEN);
        let (body, tag) = rest.split_at_mut(rest.len() - TAG_LEN);
        let tag = Tag::clone_from_slice(tag);
        if self
            .rx
            .decrypt_in_place_detached(Nonce::from_slice(nonce), &self.aad, body, &tag)
            .is_err()
        {
            zeroize::Zeroize::zeroize(body);
            aperture.abort_and_scrub_bounce(bounce_slot);
            return Err(EnclaveError::UnauthenticatedPayload);
        }
        // 3. Evaluate and seal the reply with a fresh nonce.
        let mut reply = [0u8; REPLY_LEN];
        evaluate(body, &mut reply);
        let plaintext_len = body.len();
        let ticket = match self.tx_nonces.mint() {
            Ok(t) => t,
            Err(e) => {
                aperture.abort_and_scrub_bounce(bounce_slot);
                return Err(e);
            }
        };
        let mut sealed = reply;
        let (nonce, tag) = match self.seal(ticket, &mut sealed) {
            Ok(v) => v,
            Err(e) => {
                aperture.abort_and_scrub_bounce(bounce_slot);
                return Err(e);
            }
        };
        // 4. Only ciphertext crosses back.
        let frame = SealedFrame {
            nonce,
            ciphertext: &sealed,
            tag,
        };
        if let Err((grant, e)) =
            aperture.flush_ciphertext_and_release(bounce_slot, CiphertextSlice::from_sealed(&frame))
        {
            aperture.abort_and_scrub_bounce(grant);
            return Err(EnclaveError::HostEgress(e));
        }
        Ok(GateReply {
            plaintext_len,
            reply,
        })
    }
}

/// Tenant side: seal a request frame `nonce || ct || tag`.
pub fn tenant_seal_frame(
    key: &[u8; 32],
    nonce: [u8; NONCE_LEN],
    aad: &[u8; 48],
    plaintext: &[u8],
) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut body = plaintext.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), aad, &mut body)
        .expect("ChaCha20-Poly1305 seal of an in-memory buffer");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&tag);
    out
}

/// Tenant side: open a frame; `None` if it does not authenticate.
pub fn tenant_open_frame(key: &[u8; 32], aad: &[u8; 48], frame: &[u8]) -> Option<Vec<u8>> {
    if frame.len() < FRAME_OVERHEAD {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let (nonce, rest) = frame.split_at(NONCE_LEN);
    let (body, tag) = rest.split_at(rest.len() - TAG_LEN);
    let mut body = body.to_vec();
    cipher
        .decrypt_in_place_detached(
            Nonce::from_slice(nonce),
            aad,
            &mut body,
            Tag::from_slice(tag),
        )
        .ok()?;
    Some(body)
}

// ------------------------------------------------------ Vault-keyed gate --

/// Errors from [`VaultEnclaveGate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultGateError {
    /// The host-reported length or slot was invalid (Iago attempt).
    HostIagoAttempt(TeeError),
    /// Snapshot shorter than the 28-byte Vault frame header.
    FrameTooShort {
        len: usize,
    },
    /// Vault refused the frame (tag mismatch or wrong key epoch); the
    /// private copy was zeroized by Vault before this error.
    UnauthenticatedPayload(vesper_vault::AuthError),
    /// The frame's `(epoch, seq)` is not above the last accepted one: a
    /// replayed or reordered request.
    ReplayedNonce {
        epoch: u32,
        seq: u64,
        last: u64,
    },
    /// The transmit sealer hit its record limit; rotate its key.
    RekeyRequired,
    /// Sealing failed (the ticket was spent or burned).
    SealFailed(vesper_vault::CryptoError),
    HostEgress(TeeError),
}

/// The §33.3 gate with keys held in Vault (§23.2/§23.3): requests are
/// opened by an [`AeadOpener`](vesper_vault::AeadOpener) and replies sealed
/// by an [`AeadSealer`](vesper_vault::AeadSealer), whose keys live in
/// locked, non-dumpable [`SecretSlot`](vesper_vault::SecretSlot)s and are
/// shredded by [`VaultEnclaveGate::retire`]. Nonces are Vault
/// [`NonceTicket`](vesper_vault::NonceTicket)s (strictly increasing per key
/// epoch, refused at the record limit), and received sequence numbers must
/// strictly increase, so a replayed request frame is rejected.
///
/// Frames in both directions use Vault's layout:
/// `key_epoch u32be || nonce_seq u64be || tag[16] || ciphertext`, with the
/// 48-byte channel transcript hash as associated data.
///
/// Linear like its Vault parts: retire it; dropping it records Vault
/// `AeadKey` leaks.
#[must_use = "a VaultEnclaveGate must be retired so its keys are shredded"]
pub struct VaultEnclaveGate<A: vesper_vault::AeadAlgorithm> {
    rx: vesper_vault::AeadOpener<A>,
    tx: vesper_vault::AeadSealer<A>,
    aad: [u8; 48],
    last_rx_seq: Option<u64>,
}

impl<A: vesper_vault::AeadAlgorithm> std::fmt::Debug for VaultEnclaveGate<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultEnclaveGate")
            .field("rx", &self.rx)
            .field("tx", &self.tx)
            .field("last_rx_seq", &self.last_rx_seq)
            .finish_non_exhaustive()
    }
}

/// Plaintext length of a gate reply.
pub const VAULT_REPLY_FRAME_LEN: usize = vesper_vault::FRAME_HEADER_LEN + REPLY_LEN;

impl<A: vesper_vault::AeadAlgorithm> VaultEnclaveGate<A> {
    pub fn new(
        rx: vesper_vault::AeadOpener<A>,
        tx: vesper_vault::AeadSealer<A>,
        aad_transcript: [u8; 48],
    ) -> Self {
        VaultEnclaveGate {
            rx,
            tx,
            aad: aad_transcript,
            last_rx_seq: None,
        }
    }

    /// The transmit sealer (epoch, next sequence, remaining tickets).
    pub fn sealer(&self) -> &vesper_vault::AeadSealer<A> {
        &self.tx
    }

    /// Shred both keys.
    pub fn retire(self) -> (vesper_vault::ShredReceipt, vesper_vault::ShredReceipt) {
        (self.rx.retire(), self.tx.retire())
    }

    /// Process one request (design §33.3), with the snapshot observer of
    /// [`EnclaveGate::process_observed`].
    pub fn process_observed(
        &mut self,
        aperture: &mut BounceAperture,
        scratch: &mut PrivateRegion,
        bounce_slot: HostBounceGrant,
        host_reported_len: UntrustedHostVal<u32>,
        evaluate: &mut dyn FnMut(&[u8], &mut [u8; REPLY_LEN]),
        observe: &mut dyn FnMut(&[u8]),
    ) -> Result<GateReply, VaultGateError> {
        scratch.reset();
        let result = self.run(
            aperture,
            scratch,
            bounce_slot,
            host_reported_len,
            evaluate,
            observe,
        );
        scratch.reset();
        result
    }

    /// Process one request (design §33.3).
    pub fn process_attested_enclave_req(
        &mut self,
        aperture: &mut BounceAperture,
        scratch: &mut PrivateRegion,
        bounce_slot: HostBounceGrant,
        host_reported_len: UntrustedHostVal<u32>,
        evaluate: &mut dyn FnMut(&[u8], &mut [u8; REPLY_LEN]),
    ) -> Result<GateReply, VaultGateError> {
        self.process_observed(
            aperture,
            scratch,
            bounce_slot,
            host_reported_len,
            evaluate,
            &mut |_| {},
        )
    }

    fn run(
        &mut self,
        aperture: &mut BounceAperture,
        scratch: &mut PrivateRegion,
        bounce_slot: HostBounceGrant,
        host_reported_len: UntrustedHostVal<u32>,
        evaluate: &mut dyn FnMut(&[u8], &mut [u8; REPLY_LEN]),
        observe: &mut dyn FnMut(&[u8]),
    ) -> Result<GateReply, VaultGateError> {
        // 1. Single fetch into private memory.
        let snap =
            match aperture.snapshot_from_host_bounce(&bounce_slot, host_reported_len, scratch) {
                Ok(s) => s,
                Err(e) => {
                    aperture.abort_and_scrub_bounce(bounce_slot);
                    return Err(VaultGateError::HostIagoAttempt(e));
                }
            };
        observe(snap.as_slice());
        let frame = snap.into_mut();
        let len = frame.len();
        let Some(header) = vesper_vault::FrameHeader::parse(frame) else {
            zeroize::Zeroize::zeroize(frame);
            aperture.abort_and_scrub_bounce(bounce_slot);
            return Err(VaultGateError::FrameTooShort { len });
        };
        // 2. Replay check on the (authenticated below) sequence number, then
        //    authenticate and decrypt inside private memory.
        if let Some(last) = self.last_rx_seq
            && header.nonce_seq <= last
        {
            zeroize::Zeroize::zeroize(frame);
            aperture.abort_and_scrub_bounce(bounce_slot);
            return Err(VaultGateError::ReplayedNonce {
                epoch: header.key_epoch,
                seq: header.nonce_seq,
                last,
            });
        }
        let plain = match self
            .rx
            .open_in_place(&self.aad, vesper_vault::CiphertextFrame::new(frame))
        {
            Ok(p) => p,
            Err(e) => {
                aperture.abort_and_scrub_bounce(bounce_slot);
                return Err(VaultGateError::UnauthenticatedPayload(e));
            }
        };
        // Only an authenticated sequence number advances the window.
        self.last_rx_seq = Some(header.nonce_seq);
        // 3. Evaluate and seal the reply under a fresh Vault ticket.
        let mut reply = [0u8; REPLY_LEN];
        evaluate(plain.bytes(), &mut reply);
        let plaintext_len = plain.bytes().len();
        zeroize::Zeroize::zeroize(plain.into_inner());
        let ticket = match self.tx.mint_nonce() {
            Ok(t) => t,
            Err(_) => {
                aperture.abort_and_scrub_bounce(bounce_slot);
                return Err(VaultGateError::RekeyRequired);
            }
        };
        let mut out = [0u8; VAULT_REPLY_FRAME_LEN];
        out[vesper_vault::FRAME_HEADER_LEN..].copy_from_slice(&reply);
        if let Err(e) = self.tx.seal_in_place(ticket, &self.aad, &mut out) {
            if let Some(t) = e.ticket {
                let _ = t.burn();
            }
            aperture.abort_and_scrub_bounce(bounce_slot);
            return Err(VaultGateError::SealFailed(e.error));
        }
        // 4. Only ciphertext crosses back.
        let (hdr, ct) = out.split_at(vesper_vault::FRAME_HEADER_LEN);
        let mut header = [0u8; vesper_vault::FRAME_HEADER_LEN];
        header.copy_from_slice(hdr);
        let sealed = crate::host::SealedVaultFrame {
            header,
            ciphertext: ct,
        };
        if let Err((grant, e)) = aperture
            .flush_ciphertext_and_release(bounce_slot, CiphertextSlice::from_vault_sealed(&sealed))
        {
            aperture.abort_and_scrub_bounce(grant);
            return Err(VaultGateError::HostEgress(e));
        }
        Ok(GateReply {
            plaintext_len,
            reply,
        })
    }
}
