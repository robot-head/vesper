//! [`crate::lifecycle`]'s keyed tokens forwarded to `vesper/src/lib.ves`.
//! The token layout, its checks (length, kind, lifetime, address, ODCID)
//! and the constant-time reset-token comparison are Vesper; AES-256-GCM,
//! HMAC-SHA-256 and the CSPRNG stay Rust behind the `.vsys` library
//! `WarpKeys` ([`keys`]), which the Vesper code imports.

use std::sync::Arc;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::KeyInit;
use zeroize::Zeroizing;

use super::binding as b;
use crate::frame::RESET_TOKEN_LEN;
use crate::header::MAX_CID_LEN;
use crate::lifecycle::{
    ADDR_LEN, MAX_TOKEN_LEN, PeerAddr, TOKEN_NEW, TOKEN_RETRY, TokenError, TokenKind, encode_addr,
    random,
};

/// Seals and opens address-validation tokens (AES-256-GCM, random
/// 96-bit nonces). Every endpoint that should accept another's tokens
/// must share the key ([`TokenKey::from_bytes`]).
pub struct TokenKey {
    aead: Arc<Aes256Gcm>,
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
            aead: Arc::new(Aes256Gcm::new(key.into())),
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
        let mut a = [0u8; ADDR_LEN];
        encode_addr(addr, &mut a);
        keys::with_key(&self.aead, || {
            b::token_seal(k, odcid, &a, now_ns, &mut out[..]) as usize
        })
    }

    /// Opens and checks a token received from `addr` at `now_ns`.
    pub(crate) fn open(
        &self,
        token: &[u8],
        addr: Option<PeerAddr>,
        now_ns: u64,
        lifetimes: (u64, u64),
    ) -> Result<TokenKind, TokenError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Invalid);
        }
        // Opened in place, so on a copy: the caller's token stays as sent.
        let mut buf = [0u8; MAX_TOKEN_LEN];
        let buf = &mut buf[..token.len()];
        buf.copy_from_slice(token);
        let mut a = [0u8; ADDR_LEN];
        encode_addr(addr, &mut a);
        let mut odcid = [0u8; MAX_CID_LEN];
        let r = keys::with_key(&self.aead, || {
            b::token_open(buf, &a, now_ns, lifetimes.0, lifetimes.1, &mut odcid)
        });
        match r.code {
            0 => Ok(TokenKind::Retry {
                odcid,
                len: r.len as u8,
            }),
            1 => Ok(TokenKind::NewToken),
            3 => Err(TokenError::Expired),
            4 => Err(TokenError::WrongAddress),
            _ => Err(TokenError::Invalid),
        }
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
        let mut t = [0u8; RESET_TOKEN_LEN];
        b::reset_token(&self.key[..], cid, &mut t);
        t
    }
}

/// Constant-time token comparison.
pub(crate) fn token_eq(a: &[u8; RESET_TOKEN_LEN], b: &[u8]) -> bool {
    b::reset_token_eq(a, b)
}

/// The `.vsys` library `WarpKeys` that `vesper/src/lib.ves` imports. The
/// token key is lent to the one synchronous call through a per-thread slot
/// ([`with_key`]); the HMAC key and every buffer arrive as slices.
pub(crate) mod keys {
    use std::cell::RefCell;
    use std::sync::Arc;

    use aes_gcm::aead::AeadInPlace;
    use aes_gcm::{Aes256Gcm, Nonce, Tag};
    use hkdf::hmac::{Hmac, Mac};
    use sha2::Sha256;

    use crate::lifecycle::{NONCE_LEN, TAG_LEN};

    thread_local! {
        static KEY: RefCell<Option<Arc<Aes256Gcm>>> = const { RefCell::new(None) };
    }

    /// Runs `f` (one binding call) with `key` as the token key.
    pub(crate) fn with_key<R>(key: &Arc<Aes256Gcm>, f: impl FnOnce() -> R) -> R {
        let previous = KEY.with(|k| k.replace(Some(Arc::clone(key))));
        let r = f();
        KEY.with(|k| *k.borrow_mut() = previous);
        r
    }

    /// The slice a `nocapture` `CSlice[u8]` lends for the call.
    #[allow(unsafe_code)]
    fn lent<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
        if len == 0 {
            return &mut [];
        }
        // SAFETY: the Vesper caller lends `len` initialized bytes at `ptr`
        // (a `nocapture` slice of its own buffer) for the duration of this
        // call and touches them through nothing else meanwhile.
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }

    #[allow(unsafe_code)]
    fn lent_ref<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
        if len == 0 {
            return &[];
        }
        // SAFETY: as in `lent`, read-only.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    /// Fills the slice from rustls's provider CSPRNG.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_warp_keys_random(ptr: *mut u8, len: usize) {
        crate::lifecycle::random(lent(ptr, len));
    }

    /// A token's associated data (the kind byte), nonce, body and tag.
    type Parts<'a> = (&'a [u8], &'a [u8], &'a mut [u8], &'a mut [u8]);

    /// Splits a token into its [`Parts`].
    fn parts(token: &mut [u8]) -> Option<Parts<'_>> {
        if token.len() < 1 + NONCE_LEN + TAG_LEN {
            return None;
        }
        let (head, rest) = token.split_at_mut(1 + NONCE_LEN);
        let (aad, nonce) = head.split_at(1);
        let at = rest.len() - TAG_LEN;
        let (body, tag) = rest.split_at_mut(at);
        Some((aad, nonce, body, tag))
    }

    /// Seals the token's body in place and writes its tag; 0 on success.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_warp_keys_seal(ptr: *mut u8, len: usize) -> u32 {
        let token = lent(ptr, len);
        let Some((aad, nonce, body, tag)) = parts(token) else {
            return 1;
        };
        KEY.with(|k| {
            let k = k.borrow();
            let Some(aead) = k.as_ref() else {
                return 1;
            };
            match aead.encrypt_in_place_detached(Nonce::from_slice(nonce), aad, body) {
                Ok(t) => {
                    tag.copy_from_slice(&t);
                    0
                }
                Err(_) => 1,
            }
        })
    }

    /// Opens the token's body in place; 0 when the tag verifies.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_warp_keys_open(ptr: *mut u8, len: usize) -> u32 {
        let token = lent(ptr, len);
        let Some((aad, nonce, body, tag)) = parts(token) else {
            return 1;
        };
        KEY.with(|k| {
            let k = k.borrow();
            let Some(aead) = k.as_ref() else {
                return 1;
            };
            let tag = Tag::clone_from_slice(tag);
            match aead.decrypt_in_place_detached(Nonce::from_slice(nonce), aad, body, &tag) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        })
    }

    /// HMAC-SHA-256(key, msg), truncated to the output slice.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_warp_keys_hmac16(
        key: *const u8,
        key_len: usize,
        msg: *const u8,
        msg_len: usize,
        out: *mut u8,
        out_len: usize,
    ) {
        let out = lent(out, out_len);
        let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(lent_ref(key, key_len)) else {
            out.fill(0);
            return;
        };
        mac.update(lent_ref(msg, msg_len));
        let full = mac.finalize().into_bytes();
        let n = out.len().min(full.len());
        out[..n].copy_from_slice(&full[..n]);
    }
}
