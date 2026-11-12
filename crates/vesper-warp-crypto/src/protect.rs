//! In-place AEAD (RFC 9001 §5.3) and header protection (RFC 9001 §5.4).
//!
//! All offsets come from the packet core as plain `usize`s and are checked
//! against the buffer before any byte is touched. Every arithmetic step on
//! them is checked.

use aes::cipher::{BlockEncrypt, KeyIvInit, StreamCipher, StreamCipherSeek};
use aes::{Aes128, Aes256};
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20::ChaCha20;
use chacha20poly1305::ChaCha20Poly1305;
use zeroize::Zeroize;

use crate::{Cipher, CryptoError, IV_LEN, SAMPLE_LEN, TAG_LEN, check_pn};

/// First-byte bit that marks a long header (RFC 9000 §17.2).
pub const LONG_HEADER_BIT: u8 = 0x80;
/// Short-header key-phase bit (RFC 9000 §17.3.1), protected by HP.
pub const KEY_PHASE_BIT: u8 = 0x04;

fn checked_end(start: usize, len: usize, available: usize) -> Result<usize, CryptoError> {
    let needed = start.checked_add(len).ok_or(CryptoError::BufferTooShort {
        needed: usize::MAX,
        available,
    })?;
    if needed > available {
        Err(CryptoError::BufferTooShort { needed, available })
    } else {
        Ok(needed)
    }
}

// The AEAD states differ in size by a few hundred bytes; keeping them inline
// avoids an allocation per key and keys are created off the hot path.
#[allow(clippy::large_enum_variant)]
enum Aead {
    Aes128(Aes128Gcm),
    Aes256(Aes256Gcm),
    ChaCha(ChaCha20Poly1305),
}

/// An AEAD packet-protection key with its IV. Zeroized on drop (see the
/// crate docs for which part each dependency zeroizes).
pub struct PacketKey {
    cipher: Cipher,
    aead: Aead,
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
        let aead = match cipher {
            Cipher::Aes128Gcm => Aead::Aes128(Aes128Gcm::new_from_slice(key).expect("key length")),
            Cipher::Aes256Gcm => Aead::Aes256(Aes256Gcm::new_from_slice(key).expect("key length")),
            Cipher::ChaCha20Poly1305 => {
                Aead::ChaCha(ChaCha20Poly1305::new_from_slice(key).expect("key length"))
            }
        };
        Self { cipher, aead, iv }
    }

    pub fn cipher(&self) -> Cipher {
        self.cipher
    }

    /// RFC 9001 §5.3: the IV XORed with the packet number, left-padded to
    /// the IV length in network byte order.
    pub fn nonce(&self, pn: u64) -> [u8; IV_LEN] {
        let mut nonce = self.iv;
        for (n, p) in nonce[IV_LEN - 8..].iter_mut().zip(pn.to_be_bytes()) {
            *n ^= p;
        }
        nonce
    }

    /// Seals `buf[header_len..header_len + payload_len]` in place with
    /// `buf[..header_len]` as associated data and writes the tag after it.
    /// Returns the protected packet length. Nothing is written on error.
    pub(crate) fn seal(
        &self,
        pn: u64,
        buf: &mut [u8],
        header_len: usize,
        payload_len: usize,
    ) -> Result<usize, CryptoError> {
        check_pn(pn)?;
        let payload_end = checked_end(header_len, payload_len, buf.len())?;
        let end = checked_end(payload_end, TAG_LEN, buf.len())?;
        let (header, rest) = buf[..end].split_at_mut(header_len);
        let (payload, tag_out) = rest.split_at_mut(payload_len);
        let nonce = self.nonce(pn);
        let tag = match &self.aead {
            Aead::Aes128(k) => k.encrypt_in_place_detached((&nonce).into(), header, payload),
            Aead::Aes256(k) => k.encrypt_in_place_detached((&nonce).into(), header, payload),
            Aead::ChaCha(k) => k.encrypt_in_place_detached((&nonce).into(), header, payload),
        }
        // Only fails for payloads beyond the AEADs' multi-GiB maxima.
        .map_err(|_| CryptoError::PayloadTooLarge)?;
        tag_out.copy_from_slice(&tag);
        Ok(end)
    }

    /// Opens `buf[header_len..]` (ciphertext followed by the 16-byte tag)
    /// in place. Returns the plaintext length. The dependency AEADs compare
    /// the tag in constant time before decrypting, so on error the buffer is
    /// unchanged.
    pub(crate) fn open(
        &self,
        pn: u64,
        buf: &mut [u8],
        header_len: usize,
    ) -> Result<usize, CryptoError> {
        check_pn(pn)?;
        let min = checked_end(header_len, TAG_LEN, buf.len())?;
        let plain_len = buf.len() - min;
        let (header, rest) = buf.split_at_mut(header_len);
        let (payload, tag) = rest.split_at_mut(plain_len);
        let nonce = self.nonce(pn);
        let tag: &[u8; TAG_LEN] = (&*tag).try_into().expect("tag slice is TAG_LEN");
        match &self.aead {
            Aead::Aes128(k) => {
                k.decrypt_in_place_detached((&nonce).into(), header, payload, tag.into())
            }
            Aead::Aes256(k) => {
                k.decrypt_in_place_detached((&nonce).into(), header, payload, tag.into())
            }
            Aead::ChaCha(k) => {
                k.decrypt_in_place_detached((&nonce).into(), header, payload, tag.into())
            }
        }
        .map_err(|_| CryptoError::AuthenticationFailed)?;
        Ok(plain_len)
    }
}

#[allow(clippy::large_enum_variant)] // inline to keep key creation allocation-free
enum Hp {
    Aes128(Aes128),
    Aes256(Aes256),
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
    mask: [u8; 5],
}

impl Unprotected {
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
        let bits = 8 * self.pn_len as u32;
        full_pn & ((1u64 << bits) - 1) == self.truncated_pn
    }

    /// Re-applies the removed protection, restoring `buf` to its original
    /// bytes. `buf` must be the buffer `HeaderKey::remove` operated on.
    pub fn reapply(self, buf: &mut [u8]) {
        apply_mask(buf, self.pn_offset, self.pn_len, &self.mask);
    }
}

fn first_byte_mask(first: u8) -> u8 {
    if first & LONG_HEADER_BIT != 0 {
        0x0f
    } else {
        0x1f
    }
}

fn apply_mask(buf: &mut [u8], pn_offset: usize, pn_len: usize, mask: &[u8; 5]) {
    buf[0] ^= mask[0] & first_byte_mask(buf[0]);
    for (b, m) in buf[pn_offset..pn_offset + pn_len]
        .iter_mut()
        .zip(&mask[1..])
    {
        *b ^= m;
    }
}

/// Checks the RFC 9001 §5.4.2 sample bounds and returns the sample range.
fn sample_range(len: usize, pn_offset: usize) -> Result<(usize, usize), CryptoError> {
    if pn_offset == 0 {
        return Err(CryptoError::InvalidPacketNumberOffset);
    }
    let start = pn_offset
        .checked_add(4)
        .ok_or(CryptoError::SampleOutOfBounds {
            sample_end: usize::MAX,
            packet_len: len,
        })?;
    let end = start
        .checked_add(SAMPLE_LEN)
        .ok_or(CryptoError::SampleOutOfBounds {
            sample_end: usize::MAX,
            packet_len: len,
        })?;
    if end > len {
        return Err(CryptoError::SampleOutOfBounds {
            sample_end: end,
            packet_len: len,
        });
    }
    Ok((start, end))
}

impl HeaderKey {
    pub(crate) fn new(cipher: Cipher, hp: &[u8]) -> Self {
        let hp = match cipher {
            Cipher::Aes128Gcm => Hp::Aes128(Aes128::new_from_slice(hp).expect("hp length")),
            Cipher::Aes256Gcm => Hp::Aes256(Aes256::new_from_slice(hp).expect("hp length")),
            Cipher::ChaCha20Poly1305 => Hp::ChaCha(hp.try_into().expect("hp length")),
        };
        Self { hp }
    }

    /// The 5-byte mask for a 16-byte sample.
    pub fn mask(&self, sample: &[u8; SAMPLE_LEN]) -> [u8; 5] {
        let mut out = [0u8; 5];
        match &self.hp {
            Hp::Aes128(k) => {
                let mut block = (*sample).into();
                k.encrypt_block(&mut block);
                out.copy_from_slice(&block[..5]);
            }
            Hp::Aes256(k) => {
                let mut block = (*sample).into();
                k.encrypt_block(&mut block);
                out.copy_from_slice(&block[..5]);
            }
            Hp::ChaCha(k) => {
                // RFC 9001 §5.4.4: counter = sample[0..4] (little endian),
                // nonce = sample[4..16]; mask = ChaCha20 keystream bytes 0..5.
                let counter = u32::from_le_bytes(sample[..4].try_into().expect("4 bytes"));
                let nonce: [u8; 12] = sample[4..].try_into().expect("12 bytes");
                let mut c = ChaCha20::new(k.into(), (&nonce).into());
                c.seek(u64::from(counter) * 64);
                c.apply_keystream(&mut out);
            }
        }
        out
    }

    /// Applies header protection in place. The first byte's low two bits
    /// (still unprotected) give the packet-number length. Validates the
    /// sample bounds before writing.
    pub fn apply(&self, buf: &mut [u8], pn_offset: usize) -> Result<(), CryptoError> {
        let (start, end) = sample_range(buf.len(), pn_offset)?;
        let pn_len = usize::from(buf[0] & 0x03) + 1;
        let sample: &[u8; SAMPLE_LEN] = buf[start..end].try_into().expect("sample length");
        let mask = self.mask(sample);
        apply_mask(buf, pn_offset, pn_len, &mask);
        Ok(())
    }

    /// Removes header protection in place (RFC 9001 §5.4.1) and reports the
    /// packet-number length and truncated value. On error nothing is
    /// written.
    pub fn remove(&self, buf: &mut [u8], pn_offset: usize) -> Result<Unprotected, CryptoError> {
        let (start, end) = sample_range(buf.len(), pn_offset)?;
        let sample: &[u8; SAMPLE_LEN] = buf[start..end].try_into().expect("sample length");
        let mask = self.mask(sample);
        let first_byte = buf[0] ^ (mask[0] & first_byte_mask(buf[0]));
        let pn_len = usize::from(first_byte & 0x03) + 1;
        // pn_offset + 4 <= start < buf.len(), so the pn field is in bounds.
        apply_mask(buf, pn_offset, pn_len, &mask);
        let truncated_pn = buf[pn_offset..pn_offset + pn_len]
            .iter()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        Ok(Unprotected {
            pn_offset,
            pn_len,
            truncated_pn,
            first_byte,
            mask,
        })
    }
}

/// Encodes the low `pn_len` bytes of `pn` (helper for writers and tests).
pub fn truncated_matches(buf: &[u8], pn_offset: usize, pn_len: usize, pn: u64) -> bool {
    let Some(field) = buf.get(pn_offset..pn_offset.saturating_add(pn_len)) else {
        return false;
    };
    let be = pn.to_be_bytes();
    field == &be[8 - pn_len..]
}
