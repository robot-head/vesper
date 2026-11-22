//! Retry packet integrity (RFC 9001 §5.8, QUIC v1), in Vesper
//! (`retry_integrity_tag` and `verify_retry_integrity` in
//! `vesper/src/lib.ves`). Retry handling is off the hot path, so the Retry
//! pseudo-packet is built in a fresh buffer there.

use crate::port::{b, err};
use crate::{CryptoError, TAG_LEN};

/// The Retry Integrity Tag over the Retry pseudo-packet: AEAD_AES_128_GCM
/// with the fixed v1 key and nonce, empty plaintext, and the pseudo-packet
/// (`odcid_len || odcid || retry_without_tag`) as associated data.
pub fn retry_integrity_tag(
    odcid: &[u8],
    retry_without_tag: &[u8],
) -> Result<[u8; TAG_LEN], CryptoError> {
    let mut tag = [0u8; TAG_LEN];
    b::retry_integrity_tag(odcid, retry_without_tag, &mut tag).map_err(err)?;
    Ok(tag)
}

/// Verifies a received Retry packet (tag included) against the client's
/// original Destination Connection ID. The tag comparison is the AEAD's
/// constant-time check.
pub fn verify_retry_integrity(odcid: &[u8], retry_packet: &[u8]) -> Result<(), CryptoError> {
    b::verify_retry_integrity(odcid, retry_packet).map_err(err)
}
