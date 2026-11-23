//! Frozen Rust §23.3 AEAD framing, sealer state, and Doom-principle opener
//! backed by RustCrypto `aes-gcm` and `chacha20poly1305`.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use zeroize::Zeroize;

use crate::aead::{
    AuthError, CryptoError, FRAME_HEADER_LEN, FrameHeader, KEY_LEN, SEQ_SPACE_LIMIT, TAG_LEN,
};

pub(crate) const AES256_RECORD_LIMIT: u64 = 23_726_566;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alg {
    Aes256Gcm,
    ChaCha20Poly1305,
}

impl Alg {
    pub(crate) fn from_code(code: u32) -> Self {
        match code {
            2 => Self::Aes256Gcm,
            3 => Self::ChaCha20Poly1305,
            _ => unreachable!("invalid AEAD algorithm code {code}"),
        }
    }

    pub(crate) fn record_limit(self) -> u64 {
        match self {
            Self::Aes256Gcm => AES256_RECORD_LIMIT,
            Self::ChaCha20Poly1305 => SEQ_SPACE_LIMIT,
        }
    }
}

pub(crate) fn seal_detached(
    alg: Alg,
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    buf: &mut [u8],
) -> Result<[u8; TAG_LEN], ()> {
    match alg {
        Alg::Aes256Gcm => {
            let cipher = aes_gcm::Aes256Gcm::new(key.into());
            let tag = cipher
                .encrypt_in_place_detached(nonce.into(), aad, buf)
                .map_err(|_| ())?;
            Ok(tag.into())
        }
        Alg::ChaCha20Poly1305 => {
            let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
            let tag = cipher
                .encrypt_in_place_detached(nonce.into(), aad, buf)
                .map_err(|_| ())?;
            Ok(tag.into())
        }
    }
}

pub(crate) fn open_detached(
    alg: Alg,
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    buf: &mut [u8],
    tag: &[u8; TAG_LEN],
) -> Result<(), ()> {
    match alg {
        Alg::Aes256Gcm => {
            let cipher = aes_gcm::Aes256Gcm::new(key.into());
            cipher
                .decrypt_in_place_detached(nonce.into(), aad, buf, tag.into())
                .map_err(|_| ())
        }
        Alg::ChaCha20Poly1305 => {
            let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
            cipher
                .decrypt_in_place_detached(nonce.into(), aad, buf, tag.into())
                .map_err(|_| ())
        }
    }
}

pub(crate) fn nonce_for(epoch: u32, seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&epoch.to_be_bytes());
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

pub(crate) fn frame_header_parse(frame: &[u8]) -> Option<FrameHeader> {
    let hdr = frame.get(..FRAME_HEADER_LEN)?;
    Some(FrameHeader {
        key_epoch: u32::from_be_bytes(hdr[0..4].try_into().ok()?),
        nonce_seq: u64::from_be_bytes(hdr[4..12].try_into().ok()?),
        tag: hdr[12..28].try_into().ok()?,
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SealerState {
    pub(crate) id: u64,
    pub(crate) alg: Alg,
    pub(crate) epoch: u32,
    pub(crate) next_seq: u64,
    pub(crate) limit: u64,
}

impl SealerState {
    pub(crate) fn new(id: u64, alg: Alg, epoch: u32, next_seq: u64, limit: u64) -> Self {
        Self {
            id,
            alg,
            epoch,
            next_seq,
            limit: limit.min(alg.record_limit()).min(SEQ_SPACE_LIMIT),
        }
    }

    pub(crate) fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.next_seq)
    }

    pub(crate) fn mint_nonce(&mut self) -> Result<u64, CryptoError> {
        if self.next_seq >= self.limit {
            return Err(CryptoError::EpochExhaustedRekeyRequired);
        }
        let seq = self.next_seq;
        self.next_seq = seq + 1;
        Ok(seq)
    }

    pub(crate) fn rotate_epoch(&self) -> Result<u32, CryptoError> {
        self.epoch
            .checked_add(1)
            .ok_or(CryptoError::EpochSpaceExhausted)
    }
}

pub(crate) fn seal_frame(
    st: &SealerState,
    key: &[u8; KEY_LEN],
    ticket_sealer: u64,
    ticket_epoch: u32,
    ticket_seq: u64,
    aad: &[u8],
    frame: &mut [u8],
) -> Result<usize, CryptoError> {
    if ticket_sealer != st.id {
        return Err(CryptoError::ForeignTicket);
    }
    if frame.len() < FRAME_HEADER_LEN {
        return Err(CryptoError::BufferTooSmall);
    }
    let nonce = nonce_for(ticket_epoch, ticket_seq);
    let (hdr, payload) = frame.split_at_mut(FRAME_HEADER_LEN);
    match seal_detached(st.alg, key, &nonce, aad, payload) {
        Ok(tag) => {
            hdr[0..4].copy_from_slice(&ticket_epoch.to_be_bytes());
            hdr[4..12].copy_from_slice(&ticket_seq.to_be_bytes());
            hdr[12..28].copy_from_slice(&tag);
            Ok(frame.len())
        }
        Err(()) => {
            frame.zeroize();
            Err(CryptoError::CipherRejected)
        }
    }
}

pub(crate) fn open_frame(
    alg: Alg,
    expected_epoch: u32,
    key: &[u8; KEY_LEN],
    aad: &[u8],
    buf: &mut [u8],
) -> Result<FrameHeader, AuthError> {
    let Some(header) = frame_header_parse(buf) else {
        buf.zeroize();
        return Err(AuthError::MalformedHeader);
    };
    if header.key_epoch != expected_epoch {
        buf.zeroize();
        return Err(AuthError::WrongEpoch {
            expected: expected_epoch,
            found: header.key_epoch,
        });
    }
    let nonce = nonce_for(header.key_epoch, header.nonce_seq);
    let (_, payload) = buf.split_at_mut(FRAME_HEADER_LEN);
    if open_detached(alg, key, &nonce, aad, payload, &header.tag).is_err() {
        buf.zeroize();
        return Err(AuthError::AuthenticationTagMismatch);
    }
    Ok(header)
}
