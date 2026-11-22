//! In-place AEAD (RFC 9001 §5.3) and header protection (RFC 9001 §5.4).
//!
//! All offsets come from the packet core as plain `usize`s; the Vesper
//! calls check them against the buffer, with every arithmetic step
//! checked, before any byte is touched.

use vesper_crypto_sys as sys;
use zeroize::Zeroize;

use crate::port::{aead_alg, b, err, lend_aead, lend_block, scratch};
use crate::{Cipher, CryptoError, IV_LEN, SAMPLE_LEN};

/// First-byte bit that marks a long header (RFC 9000 §17.2).
pub const LONG_HEADER_BIT: u8 = 0x80;
/// Short-header key-phase bit (RFC 9000 §17.3.1), protected by HP.
pub const KEY_PHASE_BIT: u8 = 0x04;

/// An AEAD packet-protection key with its IV. Zeroized on drop (the shim's
/// key schedules zeroize themselves).
pub struct PacketKey {
    cipher: Cipher,
    aead: sys::Aead,
    iv: [u8; IV_LEN],
}

impl Drop for PacketKey {
    fn drop(&mut self) {
        self.iv.zeroize();
    }
}

impl PacketKey {
    /// `key.len()` must equal `cipher.key_len()`; callers inside this crate
    /// derive it from the schedule, so the length always matches.
    pub(crate) fn new(cipher: Cipher, key: &[u8], iv: [u8; IV_LEN]) -> Self {
        let aead = sys::Aead::new(aead_alg(cipher), key).expect("key length");
        Self { cipher, aead, iv }
    }

    pub fn cipher(&self) -> Cipher {
        self.cipher
    }

    /// RFC 9001 §5.3: the IV XORed with the packet number, left-padded to
    /// the IV length in network byte order.
    pub fn nonce(&self, pn: u64) -> [u8; IV_LEN] {
        let mut nonce = [0u8; IV_LEN];
        b::nonce(&self.iv, pn, &mut nonce);
        nonce
    }

    pub(crate) fn lend(&self) -> b::Aead<'_> {
        lend_aead(&self.aead)
    }

    pub(crate) fn iv(&self) -> &[u8; IV_LEN] {
        &self.iv
    }

    /// Opens `buf[header_len..]` (ciphertext followed by the 16-byte tag)
    /// in place and returns the plaintext length; on error the buffer is
    /// unchanged.
    #[cfg(test)]
    pub(crate) fn open(
        &self,
        pn: u64,
        buf: &mut [u8],
        header_len: usize,
    ) -> Result<usize, CryptoError> {
        b::packet_open(
            self.lend(),
            &self.iv,
            pn,
            buf,
            header_len as u64,
            &mut scratch(),
        )
        .map(|n| n as usize)
        .map_err(err)
    }

    /// Seals `buf[header_len..header_len + payload_len]` in place with
    /// `buf[..header_len]` as associated data and writes the tag after it.
    #[cfg(test)]
    pub(crate) fn seal(
        &self,
        pn: u64,
        buf: &mut [u8],
        header_len: usize,
        payload_len: usize,
    ) -> Result<usize, CryptoError> {
        b::packet_seal(
            self.lend(),
            &self.iv,
            pn,
            buf,
            header_len as u64,
            payload_len as u64,
            &mut scratch(),
        )
        .map(|n| n as usize)
        .map_err(err)
    }
}

#[allow(clippy::large_enum_variant)] // inline to keep key creation allocation-free
enum Hp {
    Aes(sys::Block),
    ChaCha([u8; 32]),
}

/// A header-protection key (RFC 9001 §5.4). Not changed by key updates.
pub struct HeaderKey {
    hp: Hp,
}

impl Drop for HeaderKey {
    fn drop(&mut self) {
        if let Hp::ChaCha(k) = &mut self.hp {
            k.zeroize();
        }
        // AES key schedules zeroize themselves (aes "zeroize" feature).
    }
}

/// Result of removing header protection: everything the packet core needs
/// to reconstruct the full packet number, plus the mask so protection can
/// be restored if the packet is rejected.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub struct Unprotected {
    pub pn_offset: usize,
    /// Encoded packet-number length, 1–4.
    pub pn_len: usize,
    /// The truncated packet number as it appears on the wire.
    pub truncated_pn: u64,
    /// The unprotected first byte.
    pub first_byte: u8,
    mask: u64,
}

impl Unprotected {
    fn from_repr(h: b::repr::Unprot) -> Self {
        Self {
            pn_offset: h.pn_offset as usize,
            pn_len: h.pn_len as usize,
            truncated_pn: h.truncated_pn,
            first_byte: h.first_byte.get(),
            mask: h.mask,
        }
    }

    pub(crate) fn to_repr(&self) -> b::repr::Unprot {
        b::repr::Unprot {
            pn_offset: self.pn_offset as u64,
            pn_len: self.pn_len as u64,
            truncated_pn: self.truncated_pn,
            first_byte: vesper_port_abi::Word::new(self.first_byte),
            mask: self.mask,
        }
    }

    /// `pn_offset + pn_len`: the header (AEAD associated data) length.
    pub fn header_len(&self) -> usize {
        self.pn_offset + self.pn_len
    }

    /// The short-header key-phase bit, or `None` for long headers.
    pub fn key_phase(&self) -> Option<bool> {
        (self.first_byte & LONG_HEADER_BIT == 0).then_some(self.first_byte & KEY_PHASE_BIT != 0)
    }

    /// Whether `full_pn`'s low `pn_len` bytes equal the truncated value.
    pub fn encodes(&self, full_pn: u64) -> bool {
        b::unprot_encodes(&self.to_repr(), full_pn)
    }

    /// Re-applies the removed protection, restoring `buf` to its original
    /// bytes. `buf` must be the buffer `HeaderKey::remove` operated on.
    pub fn reapply(self, buf: &mut [u8]) {
        b::hp_reapply(buf, &self.to_repr());
    }
}

impl HeaderKey {
    pub(crate) fn new(cipher: Cipher, hp: &[u8]) -> Self {
        let hp = match cipher {
            Cipher::Aes128Gcm | Cipher::Aes256Gcm => {
                Hp::Aes(sys::Block::new(hp).expect("hp length"))
            }
            Cipher::ChaCha20Poly1305 => Hp::ChaCha(hp.try_into().expect("hp length")),
        };
        Self { hp }
    }

    /// The packed mask for the sample of the packet `buf` whose packet
    /// number starts at `pn_offset`, after the sample bounds checks.
    fn mask_at(&self, buf: &[u8], pn_offset: usize) -> Result<u64, CryptoError> {
        let mut s = scratch();
        match &self.hp {
            Hp::Aes(k) => b::hp_mask_aes(lend_block(k), buf, pn_offset as u64, &mut s),
            Hp::ChaCha(k) => b::hp_mask_chacha(k, buf, pn_offset as u64, &mut s),
        }
        .map_err(err)
    }

    /// The 5-byte mask for a 16-byte sample.
    pub fn mask(&self, sample: &[u8; SAMPLE_LEN]) -> [u8; 5] {
        // A one-byte header with the packet number at 1: the sample is at 5.
        let mut packet = [0u8; 5 + SAMPLE_LEN];
        packet[5..].copy_from_slice(sample);
        let m = self.mask_at(&packet, 1).expect("the sample fits");
        let bytes = m.to_be_bytes();
        [bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]
    }

    /// Applies header protection in place. The first byte's low two bits
    /// (still unprotected) give the packet-number length. Validates the
    /// sample bounds before writing.
    pub fn apply(&self, buf: &mut [u8], pn_offset: usize) -> Result<(), CryptoError> {
        let mut s = scratch();
        match &self.hp {
            Hp::Aes(k) => b::hp_apply_aes(lend_block(k), buf, pn_offset as u64, &mut s),
            Hp::ChaCha(k) => b::hp_apply_chacha(k, buf, pn_offset as u64, &mut s),
        }
        .map_err(err)
    }

    /// Removes header protection in place (RFC 9001 §5.4.1) and reports the
    /// packet-number length and truncated value. On error nothing is
    /// written.
    pub fn remove(&self, buf: &mut [u8], pn_offset: usize) -> Result<Unprotected, CryptoError> {
        let mut s = scratch();
        match &self.hp {
            Hp::Aes(k) => b::hp_remove_aes(lend_block(k), buf, pn_offset as u64, &mut s),
            Hp::ChaCha(k) => b::hp_remove_chacha(k, buf, pn_offset as u64, &mut s),
        }
        .map(Unprotected::from_repr)
        .map_err(err)
    }
}

/// Whether the packet-number field of `buf` encodes the low `pn_len` bytes
/// of `pn` (helper for writers and tests).
pub fn truncated_matches(buf: &[u8], pn_offset: usize, pn_len: usize, pn: u64) -> bool {
    b::truncated_matches(buf, pn_offset as u64, pn_len as u64, pn)
}
