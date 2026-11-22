//! Retry packet integrity (RFC 9001 §5.8, QUIC v1).
//!
//! Retry handling is off the hot path, so the Retry pseudo-packet is built
//! in a heap buffer.

use aes_gcm::Aes128Gcm;
use aes_gcm::aead::{AeadInPlace, KeyInit};

use crate::{CryptoError, MAX_CID_LEN, TAG_LEN};

const RETRY_KEY_V1: [u8; 16] = [
    0xbe, 0x0c, 0x69, 0x0b, 0x9f, 0x66, 0x57, 0x5a, 0x1d, 0x76, 0x6b, 0x54, 0xe3, 0x68, 0xc8, 0x4e,
];
const RETRY_NONCE_V1: [u8; 12] = [
    0x46, 0x15, 0x99, 0xd3, 0x5d, 0x63, 0x2b, 0xf2, 0x23, 0x98, 0x25, 0xbb,
];

fn pseudo_packet(odcid: &[u8], retry_without_tag: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if odcid.len() > MAX_CID_LEN {
        return Err(CryptoError::ConnectionIdTooLong(odcid.len()));
    }
    let mut p = Vec::with_capacity(1 + odcid.len() + retry_without_tag.len());
    p.push(odcid.len() as u8); // <= 20
    p.extend_from_slice(odcid);
    p.extend_from_slice(retry_without_tag);
    Ok(p)
}

/// The Retry Integrity Tag over the Retry pseudo-packet: AEAD_AES_128_GCM
/// with the fixed v1 key and nonce, empty plaintext, and the pseudo-packet
/// (`odcid_len || odcid || retry_without_tag`) as associated data.
pub fn retry_integrity_tag(
    odcid: &[u8],
    retry_without_tag: &[u8],
) -> Result<[u8; TAG_LEN], CryptoError> {
    let aad = pseudo_packet(odcid, retry_without_tag)?;
    let aead = Aes128Gcm::new((&RETRY_KEY_V1).into());
    let tag = aead
        .encrypt_in_place_detached((&RETRY_NONCE_V1).into(), &aad, &mut [])
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    Ok(tag.into())
}

/// Verifies a received Retry packet (tag included) against the client's
/// original Destination Connection ID. The tag comparison is the AEAD's
/// constant-time check.
pub fn verify_retry_integrity(odcid: &[u8], retry_packet: &[u8]) -> Result<(), CryptoError> {
    let body_len = retry_packet
        .len()
        .checked_sub(TAG_LEN)
        .ok_or(CryptoError::BufferTooShort {
            needed: TAG_LEN,
            available: retry_packet.len(),
        })?;
    let (body, tag) = retry_packet.split_at(body_len);
    let aad = pseudo_packet(odcid, body)?;
    let tag: &[u8; TAG_LEN] = tag.try_into().expect("TAG_LEN slice");
    Aes128Gcm::new((&RETRY_KEY_V1).into())
        .decrypt_in_place_detached((&RETRY_NONCE_V1).into(), &aad, &mut [], tag.into())
        .map_err(|_| CryptoError::AuthenticationFailed)
}
