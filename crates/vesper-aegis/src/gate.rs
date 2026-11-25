//! The §33.3 confidential inference gate: snapshot a host frame once into
//! private memory, authenticate and decrypt it there, evaluate, seal the
//! reply under a fresh nonce ticket, and let only ciphertext cross back.
//! Every control path discharges the bounce grant (flush or scrub).
//!
//! [`VaultEnclaveGate`] is the §33.1 item 3 form: keys in Vault secret
//! slots, Vault nonce tickets and frames, and a receive-side replay check.
//! After warm-up, `process_attested_enclave_req` performs no heap
//! allocation (`tests/zero_alloc.rs` measures this with a counting
//! allocator). The deprecated gate with gate-held ChaCha20-Poly1305 keys
//! (`EnclaveGate`, kept until the end of wave six) is gone.

use crate::error::TeeError;
use crate::host::{
    BounceAperture, CiphertextSlice, HostBounceGrant, PrivateRegion, UntrustedHostVal,
};

pub const REPLY_LEN: usize = 64;

/// Result of one gate run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateReply {
    pub plaintext_len: usize,
    /// The reply plaintext (only its ciphertext reached the host).
    pub reply: [u8; REPLY_LEN],
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
