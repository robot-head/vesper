//! [`crate::lifecycle`]'s keyed tokens in Rust: address-validation tokens
//! sealed with AES-256-GCM and stateless reset tokens from HMAC-SHA-256.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::frame::RESET_TOKEN_LEN;
use crate::lifecycle::{
    MAX_TOKEN_LEN, NONCE_LEN, PeerAddr, TAG_LEN, TOKEN_NEW, TOKEN_PLAIN_MAX, TOKEN_RETRY,
    TokenError, TokenKind, encode_addr, random,
};

/// Seals and opens address-validation tokens (AES-256-GCM, random
/// 96-bit nonces). Every endpoint that should accept another's tokens
/// must share the key ([`TokenKey::from_bytes`]).
pub struct TokenKey {
    aead: Aes256Gcm,
}

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenKey(..)")
    }
}

impl TokenKey {
    pub fn random() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        random(&mut k[..]);
        Self::from_bytes(&k)
    }

    pub fn from_bytes(key: &[u8; 32]) -> Self {
        Self {
            aead: Aes256Gcm::new(key.into()),
        }
    }

    /// Writes a token into `out`; returns its length.
    pub(crate) fn seal(
        &self,
        kind: TokenKind,
        addr: Option<PeerAddr>,
        now_ns: u64,
        out: &mut [u8; MAX_TOKEN_LEN],
    ) -> usize {
        let (k, odcid): (u8, &[u8]) = match &kind {
            TokenKind::Retry { odcid, len } => (TOKEN_RETRY, &odcid[..usize::from(*len)]),
            TokenKind::NewToken => (TOKEN_NEW, &[]),
        };
        out[0] = k;
        random(&mut out[1..1 + NONCE_LEN]);
        let mut plain = [0u8; TOKEN_PLAIN_MAX];
        plain[..8].copy_from_slice(&now_ns.to_be_bytes());
        let mut a = [0u8; 19];
        encode_addr(addr, &mut a);
        plain[8..27].copy_from_slice(&a);
        plain[27] = odcid.len() as u8;
        plain[28..28 + odcid.len()].copy_from_slice(odcid);
        let plain_len = 28 + odcid.len();
        let start = 1 + NONCE_LEN;
        out[start..start + plain_len].copy_from_slice(&plain[..plain_len]);
        let (head, body) = out.split_at_mut(start);
        let nonce = Nonce::from_slice(&head[1..]);
        // Only fails for multi-GiB payloads.
        let tag = self
            .aead
            .encrypt_in_place_detached(nonce, &head[..1], &mut body[..plain_len])
            .expect("short token");
        body[plain_len..plain_len + TAG_LEN].copy_from_slice(&tag);
        start + plain_len + TAG_LEN
    }

    /// Opens and checks a token received from `addr` at `now_ns`.
    pub(crate) fn open(
        &self,
        token: &[u8],
        addr: Option<PeerAddr>,
        now_ns: u64,
        lifetimes: (u64, u64),
    ) -> Result<TokenKind, TokenError> {
        let start = 1 + NONCE_LEN;
        if token.len() < start + 28 + TAG_LEN || token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Invalid);
        }
        let kind = token[0];
        if kind != TOKEN_RETRY && kind != TOKEN_NEW {
            return Err(TokenError::Invalid);
        }
        let plain_len = token.len() - start - TAG_LEN;
        let mut plain = [0u8; TOKEN_PLAIN_MAX];
        plain[..plain_len].copy_from_slice(&token[start..start + plain_len]);
        let tag = aes_gcm::Tag::from_slice(&token[start + plain_len..]);
        self.aead
            .decrypt_in_place_detached(
                Nonce::from_slice(&token[1..start]),
                &token[..1],
                &mut plain[..plain_len],
                tag,
            )
            .map_err(|_| TokenError::Invalid)?;
        let issued = u64::from_be_bytes(plain[..8].try_into().expect("8 bytes"));
        let lifetime = if kind == TOKEN_RETRY {
            lifetimes.0
        } else {
            lifetimes.1
        };
        if now_ns < issued || now_ns - issued > lifetime {
            return Err(TokenError::Expired);
        }
        let mut a = [0u8; 19];
        encode_addr(addr, &mut a);
        if !bool::from(plain[8..27].ct_eq(&a)) {
            return Err(TokenError::WrongAddress);
        }
        let len = plain[27];
        if usize::from(len) + 28 != plain_len || len > 20 {
            return Err(TokenError::Invalid);
        }
        if kind == TOKEN_NEW {
            return if len == 0 {
                Ok(TokenKind::NewToken)
            } else {
                Err(TokenError::Invalid)
            };
        }
        let mut odcid = [0u8; 20];
        odcid[..usize::from(len)].copy_from_slice(&plain[28..28 + usize::from(len)]);
        Ok(TokenKind::Retry { odcid, len })
    }
}

/// Derives stateless reset tokens: HMAC-SHA-256(key, connection ID),
/// truncated to 16 bytes (RFC 9000 §10.3.2). Endpoints behind one
/// address must share the key so any of them can reset for the others.
pub struct ResetKey {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for ResetKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResetKey(..)")
    }
}

impl ResetKey {
    pub fn random() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        random(&mut k[..]);
        Self { key: k }
    }

    pub fn from_bytes(key: &[u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(*key),
        }
    }

    /// The reset token for one of our connection IDs.
    pub fn token(&self, cid: &[u8]) -> [u8; RESET_TOKEN_LEN] {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.key[..]).expect("HMAC takes any key");
        mac.update(cid);
        let full = mac.finalize().into_bytes();
        let mut t = [0u8; RESET_TOKEN_LEN];
        t.copy_from_slice(&full[..RESET_TOKEN_LEN]);
        t
    }
}

/// Constant-time token comparison.
pub(crate) fn token_eq(a: &[u8; RESET_TOKEN_LEN], b: &[u8]) -> bool {
    b.len() == RESET_TOKEN_LEN && bool::from(a[..].ct_eq(b))
}
