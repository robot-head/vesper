//! The one C-ABI shim over vetted cryptography (Rust-exit wave SH6, lane 17,
//! #1412).
//!
//! The repository never hand-rolls cryptography. Every primitive Vesper code
//! needs is one `vcr_*` call here, and each call is a direct call into a
//! vetted library with no logic of its own beyond checking lengths:
//!
//! | Family | Library |
//! |---|---|
//! | AES-128/256-GCM, ChaCha20-Poly1305 | RustCrypto `aes-gcm`, `chacha20poly1305` |
//! | AES block, ChaCha20 block (QUIC header protection) | RustCrypto `aes`, `chacha20` |
//! | SHA-256/384/512, HMAC, HKDF | RustCrypto `sha2`, `hmac`, `hkdf` |
//! | X25519 | `x25519-dalek` |
//! | Ed25519 (strict verification) | `ed25519-dalek` |
//! | ML-KEM-768 (FIPS 203) | Cryspen `libcrux-ml-kem` (portable, AVX2, NEON) |
//! | ECDSA P-256/P-384 verification | `ring` |
//! | Operating-system randomness | `getrandom` |
//! | Constant-time equality, zeroization | `subtle`, `zeroize` |
//!
//! Everything around those primitives (the QUIC and TLS key schedules,
//! packet protection, nonce discipline, key updates, the hybrid KEM, the
//! attestation quorum) is Vesper, in `packages/vesper-crypto` and the ports
//! that import it.
//!
//! ABI:
//! - Vesper imports these functions through the `.vsys` library `Crypto` in
//!   `packages/vesper-crypto/crypto.vsys`. A `CSlice[u8]` is a `(ptr, len)`
//!   pair, a `u32`/`u64` is itself, and a handle is a pointer.
//! - Three opaque handles, each a `linear extern type` on the Vesper side:
//!   [`Aead`] (an AEAD key schedule), [`Block`] (an AES block cipher key
//!   schedule) and [`Hash`] (a running SHA-2 state). Each has exactly one
//!   finalizer; the key schedules zeroize themselves when freed (the `aes`,
//!   `polyval`, `poly1305` and `chacha20` `zeroize` features).
//! - Calls that can fail return a status: [`OK`], [`LENGTH`] (a buffer has
//!   the wrong length), [`AUTH`] (authentication or verification failed),
//!   [`FAILED`] (the library failed, or the OS had no randomness) or
//!   [`UNSUPPORTED`] (an unknown algorithm code, or an ML-KEM backend this
//!   CPU cannot run). On any status but [`OK`], no output byte is written.
//! - Every `vcr_*` function is `unsafe`: its `(ptr, len)` pairs must be valid
//!   for the call and its handles live (or null), which the `.vsys`
//!   declarations guarantee to Vesper. No panic crosses the boundary: every
//!   call runs under `catch_unwind` and reports [`FAILED`].
#![allow(clippy::missing_safety_doc)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, Ordering};

use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipherCore, StreamCipherSeekCore};
use aes_gcm::aead::AeadInPlace;
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20::ChaChaCore;
use chacha20::cipher::consts::U10;
use chacha20poly1305::ChaCha20Poly1305;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha384, Sha512};
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// Success.
pub const OK: u32 = 0;
/// A buffer or key has the wrong length for the algorithm.
pub const LENGTH: u32 = 1;
/// Authentication or signature verification failed.
pub const AUTH: u32 = 2;
/// The library failed (or the operating system had no randomness).
pub const FAILED: u32 = 3;
/// An unknown algorithm code, or a backend this CPU cannot run.
pub const UNSUPPORTED: u32 = 4;

/// AEAD algorithm codes.
pub mod aead {
    pub const AES_128_GCM: u32 = 1;
    pub const AES_256_GCM: u32 = 2;
    pub const CHACHA20_POLY1305: u32 = 3;
}

/// Hash algorithm codes (also HMAC and HKDF).
pub mod hash {
    pub const SHA256: u32 = 1;
    pub const SHA384: u32 = 2;
    pub const SHA512: u32 = 3;
}

/// ML-KEM-768 backend codes.
pub mod isa {
    pub const PORTABLE: u32 = 0;
    pub const AVX2: u32 = 1;
    pub const NEON: u32 = 2;
}

/// ECDSA verification algorithm codes (ring's algorithms of the same name).
pub mod ecdsa {
    pub const P256_SHA256_FIXED: u32 = 1;
    pub const P384_SHA384_FIXED: u32 = 2;
    pub const P256_SHA256_ASN1: u32 = 3;
    pub const P384_SHA384_ASN1: u32 = 4;
}

/// The AEAD tag length of every supported algorithm.
pub const TAG_LEN: usize = 16;
/// The AEAD nonce length of every supported algorithm.
pub const NONCE_LEN: usize = 12;
/// ML-KEM-768 sizes (FIPS 203).
pub const MLKEM768_EK_LEN: usize = 1184;
pub const MLKEM768_DK_LEN: usize = 2400;
pub const MLKEM768_CT_LEN: usize = 1088;

// ---------------------------------------------------------------------------
// Live-handle counters: the leak tests read them through `live_handles()`.

static LIVE: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];

/// Live `(Aead, Block, Hash)` handles, for leak tests.
pub fn live_handles() -> [i64; 3] {
    [0, 1, 2].map(|i| LIVE[i].load(Ordering::SeqCst))
}

fn born<T>(kind: usize, value: T) -> *mut T {
    LIVE[kind].fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(value))
}

/// # Safety
/// `p` is null or a live handle of kind `kind` made by [`born`].
unsafe fn die<T>(kind: usize, p: *mut T) {
    if !p.is_null() {
        LIVE[kind].fetch_sub(1, Ordering::SeqCst);
        // SAFETY: the sink import consumes the handle exactly once.
        drop(unsafe { Box::from_raw(p) });
    }
}

/// # Safety
/// `(ptr, len)` is valid for reads (or `len` is 0).
unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || ptr.is_null() {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// # Safety
/// `(ptr, len)` is valid for reads and writes (or `len` is 0).
unsafe fn slice_mut<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
    if len == 0 || ptr.is_null() {
        &mut []
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }
}

/// Runs `f`, turning a panic into [`FAILED`].
fn guard(f: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(FAILED)
}

fn array<const N: usize>(b: &[u8]) -> Option<[u8; N]> {
    b.try_into().ok()
}

// ---------------------------------------------------------------------------
// Randomness, constant-time equality, zeroization.

/// Fills `out` from the operating system's CSPRNG (`getrandom`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_random(out: *mut u8, out_len: usize) -> u32 {
    // SAFETY: the `.vsys` contract.
    let out = unsafe { slice_mut(out, out_len) };
    guard(|| match getrandom::fill(out) {
        Ok(()) => OK,
        Err(_) => FAILED,
    })
}

/// 1 if `a == b` (same length and bytes), compared in constant time for
/// equal lengths (`subtle`), else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_ct_eq(a: *const u8, a_len: usize, b: *const u8, b_len: usize) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (a, b) = unsafe { (slice(a, a_len), slice(b, b_len)) };
    u32::from(bool::from(a.ct_eq(b)))
}

/// Overwrites `buf` with zeros in a way the optimizer keeps (`zeroize`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_zeroize(buf: *mut u8, buf_len: usize) {
    // SAFETY: the `.vsys` contract.
    unsafe { slice_mut(buf, buf_len) }.zeroize();
}

// ---------------------------------------------------------------------------
// Hashes, HMAC, HKDF.

/// The digest length of hash `alg`, or 0 for an unknown code.
#[unsafe(no_mangle)]
pub extern "C" fn vcr_hash_len(alg: u32) -> u64 {
    match alg {
        hash::SHA256 => 32,
        hash::SHA384 => 48,
        hash::SHA512 => 64,
        _ => 0,
    }
}

fn digest_len(alg: u32) -> Option<usize> {
    match vcr_hash_len(alg) {
        0 => None,
        n => Some(n as usize),
    }
}

/// `out = H(data)`; `out` must be exactly the digest length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hash(
    alg: u32,
    data: *const u8,
    data_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (data, out) = unsafe { (slice(data, data_len), slice_mut(out, out_len)) };
    guard(|| {
        let Some(n) = digest_len(alg) else {
            return UNSUPPORTED;
        };
        if out.len() != n {
            return LENGTH;
        }
        match alg {
            hash::SHA256 => out.copy_from_slice(&Sha256::digest(data)),
            hash::SHA384 => out.copy_from_slice(&Sha384::digest(data)),
            _ => out.copy_from_slice(&Sha512::digest(data)),
        }
        OK
    })
}

/// A running SHA-2 state (a TLS transcript hash).
#[derive(Clone)]
pub enum Hash {
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

/// A new running hash, or null for an unknown code.
#[unsafe(no_mangle)]
pub extern "C" fn vcr_hash_new(alg: u32) -> *mut Hash {
    let h = match alg {
        hash::SHA256 => Hash::Sha256(Sha256::new()),
        hash::SHA384 => Hash::Sha384(Sha384::new()),
        hash::SHA512 => Hash::Sha512(Sha512::new()),
        _ => return std::ptr::null_mut(),
    };
    born(2, h)
}

/// A copy of a running hash (null if `h` is null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hash_clone(h: *const Hash) -> *mut Hash {
    // SAFETY: a live handle or null.
    match unsafe { h.as_ref() } {
        Some(h) => born(2, h.clone()),
        None => std::ptr::null_mut(),
    }
}

/// Feeds `data` to the running hash.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hash_update(h: *mut Hash, data: *const u8, data_len: usize) {
    // SAFETY: the `.vsys` contract.
    let (h, data) = unsafe { (h.as_mut(), slice(data, data_len)) };
    match h {
        Some(Hash::Sha256(s)) => s.update(data),
        Some(Hash::Sha384(s)) => s.update(data),
        Some(Hash::Sha512(s)) => s.update(data),
        None => {}
    }
}

/// The digest of everything fed so far; the state keeps running. `out`
/// must be exactly the digest length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hash_digest(h: *const Hash, out: *mut u8, out_len: usize) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (h, out) = unsafe { (h.as_ref(), slice_mut(out, out_len)) };
    guard(|| {
        let Some(h) = h else { return FAILED };
        let want = match h {
            Hash::Sha256(_) => 32,
            Hash::Sha384(_) => 48,
            Hash::Sha512(_) => 64,
        };
        if out.len() != want {
            return LENGTH;
        }
        match h.clone() {
            Hash::Sha256(s) => out.copy_from_slice(&s.finalize()),
            Hash::Sha384(s) => out.copy_from_slice(&s.finalize()),
            Hash::Sha512(s) => out.copy_from_slice(&s.finalize()),
        }
        OK
    })
}

/// Releases a running hash.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hash_free(h: *mut Hash) {
    // SAFETY: the `.vsys` contract (sink).
    unsafe { die(2, h) }
}

/// `out = HMAC-H(key, data)`; `out` must be exactly the digest length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hmac(
    alg: u32,
    key: *const u8,
    key_len: usize,
    data: *const u8,
    data_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (key, data, out) = unsafe {
        (
            slice(key, key_len),
            slice(data, data_len),
            slice_mut(out, out_len),
        )
    };
    guard(|| {
        let Some(n) = digest_len(alg) else {
            return UNSUPPORTED;
        };
        if out.len() != n {
            return LENGTH;
        }
        // HMAC accepts keys of every length.
        match alg {
            hash::SHA256 => {
                let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
                m.update(data);
                out.copy_from_slice(&m.finalize().into_bytes());
            }
            hash::SHA384 => {
                let mut m = <Hmac<Sha384> as Mac>::new_from_slice(key).expect("any key length");
                m.update(data);
                out.copy_from_slice(&m.finalize().into_bytes());
            }
            _ => {
                let mut m = <Hmac<Sha512> as Mac>::new_from_slice(key).expect("any key length");
                m.update(data);
                out.copy_from_slice(&m.finalize().into_bytes());
            }
        }
        OK
    })
}

/// `prk = HKDF-Extract(salt, ikm)` (RFC 5869 §2.2); `prk` must be exactly
/// the digest length. An empty salt is the RFC's default salt.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hkdf_extract(
    alg: u32,
    salt: *const u8,
    salt_len: usize,
    ikm: *const u8,
    ikm_len: usize,
    prk: *mut u8,
    prk_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (salt, ikm, out) = unsafe {
        (
            slice(salt, salt_len),
            slice(ikm, ikm_len),
            slice_mut(prk, prk_len),
        )
    };
    guard(|| {
        let Some(n) = digest_len(alg) else {
            return UNSUPPORTED;
        };
        if out.len() != n {
            return LENGTH;
        }
        let salt = (!salt.is_empty()).then_some(salt);
        match alg {
            hash::SHA256 => {
                let (mut k, _) = hkdf::Hkdf::<Sha256>::extract(salt, ikm);
                out.copy_from_slice(&k);
                k.as_mut_slice().zeroize();
            }
            hash::SHA384 => {
                let (mut k, _) = hkdf::Hkdf::<Sha384>::extract(salt, ikm);
                out.copy_from_slice(&k);
                k.as_mut_slice().zeroize();
            }
            _ => {
                let (mut k, _) = hkdf::Hkdf::<Sha512>::extract(salt, ikm);
                out.copy_from_slice(&k);
                k.as_mut_slice().zeroize();
            }
        }
        OK
    })
}

/// `out = HKDF-Expand(prk, info, len(out))` (RFC 5869 §2.3). The PRK must
/// be at least the digest length and `out` at most 255 digests long.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_hkdf_expand(
    alg: u32,
    prk: *const u8,
    prk_len: usize,
    info: *const u8,
    info_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (prk, info, out) = unsafe {
        (
            slice(prk, prk_len),
            slice(info, info_len),
            slice_mut(out, out_len),
        )
    };
    guard(|| {
        let r = match alg {
            hash::SHA256 => hkdf::Hkdf::<Sha256>::from_prk(prk)
                .map_err(|_| LENGTH)
                .and_then(|h| h.expand(info, out).map_err(|_| LENGTH)),
            hash::SHA384 => hkdf::Hkdf::<Sha384>::from_prk(prk)
                .map_err(|_| LENGTH)
                .and_then(|h| h.expand(info, out).map_err(|_| LENGTH)),
            hash::SHA512 => hkdf::Hkdf::<Sha512>::from_prk(prk)
                .map_err(|_| LENGTH)
                .and_then(|h| h.expand(info, out).map_err(|_| LENGTH)),
            _ => Err(UNSUPPORTED),
        };
        r.err().unwrap_or(OK)
    })
}

// ---------------------------------------------------------------------------
// AEADs.

/// An AEAD key schedule. The AES and ChaCha20 states zeroize on drop
/// through the dependencies' `zeroize` features.
#[allow(clippy::large_enum_variant)]
pub enum Aead {
    Aes128(Aes128Gcm),
    Aes256(Aes256Gcm),
    ChaCha(ChaCha20Poly1305),
}

/// A new AEAD key for `alg`, or null if the code is unknown or `key` has
/// the wrong length (16 bytes for AES-128-GCM, 32 otherwise).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aead_new(alg: u32, key: *const u8, key_len: usize) -> *mut Aead {
    // SAFETY: the `.vsys` contract.
    let key = unsafe { slice(key, key_len) };
    let a = match alg {
        aead::AES_128_GCM => Aes128Gcm::new_from_slice(key).ok().map(Aead::Aes128),
        aead::AES_256_GCM => Aes256Gcm::new_from_slice(key).ok().map(Aead::Aes256),
        aead::CHACHA20_POLY1305 => ChaCha20Poly1305::new_from_slice(key).ok().map(Aead::ChaCha),
        _ => None,
    };
    a.map_or(std::ptr::null_mut(), |a| born(0, a))
}

/// Releases an AEAD key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aead_free(a: *mut Aead) {
    // SAFETY: the `.vsys` contract (sink).
    unsafe { die(0, a) }
}

/// Seals in place over the record layout `buf = aad || plaintext || tag`:
/// `buf[..aad_len]` is the associated data, the next `msg_len` bytes are
/// encrypted in place, and the 16-byte tag is written right after them.
/// `buf` must hold `aad_len + msg_len + 16` bytes (more are left alone).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aead_seal(
    a: *const Aead,
    nonce: *const u8,
    nonce_len: usize,
    buf: *mut u8,
    buf_len: usize,
    aad_len: u64,
    msg_len: u64,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (a, nonce, buf) = unsafe { (a.as_ref(), slice(nonce, nonce_len), slice_mut(buf, buf_len)) };
    guard(|| {
        let Some(a) = a else { return FAILED };
        let Some(nonce) = array::<NONCE_LEN>(nonce) else {
            return LENGTH;
        };
        let (Ok(h), Ok(m)) = (usize::try_from(aad_len), usize::try_from(msg_len)) else {
            return LENGTH;
        };
        let Some(end) = h.checked_add(m).and_then(|e| e.checked_add(TAG_LEN)) else {
            return LENGTH;
        };
        if end > buf.len() {
            return LENGTH;
        }
        let (aad, rest) = buf[..end].split_at_mut(h);
        let (msg, tag_out) = rest.split_at_mut(m);
        let n = (&nonce).into();
        let tag = match a {
            Aead::Aes128(k) => k.encrypt_in_place_detached(n, aad, msg),
            Aead::Aes256(k) => k.encrypt_in_place_detached(n, aad, msg),
            Aead::ChaCha(k) => k.encrypt_in_place_detached(n, aad, msg),
        };
        match tag {
            Ok(tag) => {
                tag_out.copy_from_slice(&tag);
                OK
            }
            // Only for messages beyond the AEADs' multi-GiB maxima.
            Err(_) => LENGTH,
        }
    })
}

/// Opens in place over the record layout `buf = aad || ciphertext || tag`
/// (the whole of `buf`): checks the tag in constant time and only then
/// decrypts the ciphertext in place. On [`AUTH`] the buffer is unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aead_open(
    a: *const Aead,
    nonce: *const u8,
    nonce_len: usize,
    buf: *mut u8,
    buf_len: usize,
    aad_len: u64,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (a, nonce, buf) = unsafe { (a.as_ref(), slice(nonce, nonce_len), slice_mut(buf, buf_len)) };
    guard(|| {
        let Some(a) = a else { return FAILED };
        let Some(nonce) = array::<NONCE_LEN>(nonce) else {
            return LENGTH;
        };
        let Ok(h) = usize::try_from(aad_len) else {
            return LENGTH;
        };
        let Some(min) = h.checked_add(TAG_LEN) else {
            return LENGTH;
        };
        if min > buf.len() {
            return LENGTH;
        }
        let plain = buf.len() - min;
        let (aad, rest) = buf.split_at_mut(h);
        let (msg, tag) = rest.split_at_mut(plain);
        let tag: &[u8; TAG_LEN] = (&*tag).try_into().expect("TAG_LEN bytes");
        let n = (&nonce).into();
        let r = match a {
            Aead::Aes128(k) => k.decrypt_in_place_detached(n, aad, msg, tag.into()),
            Aead::Aes256(k) => k.decrypt_in_place_detached(n, aad, msg, tag.into()),
            Aead::ChaCha(k) => k.decrypt_in_place_detached(n, aad, msg, tag.into()),
        };
        if r.is_ok() { OK } else { AUTH }
    })
}

// ---------------------------------------------------------------------------
// Block ciphers for QUIC header protection (RFC 9001 §5.4).

#[allow(clippy::large_enum_variant)] // boxed as a whole behind the handle
/// An AES block cipher key schedule (zeroized on drop by `aes`).
pub enum Block {
    Aes128(aes::Aes128),
    Aes256(aes::Aes256),
}

/// A new AES key (16 or 32 bytes), or null for any other length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aes_new(key: *const u8, key_len: usize) -> *mut Block {
    // SAFETY: the `.vsys` contract.
    let key = unsafe { slice(key, key_len) };
    let b = match key.len() {
        16 => aes::Aes128::new_from_slice(key).ok().map(Block::Aes128),
        32 => aes::Aes256::new_from_slice(key).ok().map(Block::Aes256),
        _ => None,
    };
    b.map_or(std::ptr::null_mut(), |b| born(1, b))
}

/// Releases an AES key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aes_free(b: *mut Block) {
    // SAFETY: the `.vsys` contract (sink).
    unsafe { die(1, b) }
}

/// Encrypts one 16-byte block in place.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_aes_encrypt_block(
    b: *const Block,
    block: *mut u8,
    block_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (b, block) = unsafe { (b.as_ref(), slice_mut(block, block_len)) };
    guard(|| {
        let Some(b) = b else { return FAILED };
        if block.len() != 16 {
            return LENGTH;
        }
        let blk = aes::Block::from_mut_slice(block);
        match b {
            Block::Aes128(k) => k.encrypt_block(blk),
            Block::Aes256(k) => k.encrypt_block(blk),
        }
        OK
    })
}

/// Writes the 64-byte ChaCha20 keystream block at 32-bit block `counter`
/// for a 32-byte `key` and 12-byte `nonce` (RFC 8439 §2.3). The block core
/// is called directly: the buffered stream wrapper refuses counter
/// `2^32 - 1`, which an attacker-chosen QUIC sample can select.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_chacha20_block(
    key: *const u8,
    key_len: usize,
    counter: u32,
    nonce: *const u8,
    nonce_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (key, nonce, out) = unsafe {
        (
            slice(key, key_len),
            slice(nonce, nonce_len),
            slice_mut(out, out_len),
        )
    };
    guard(|| {
        let (Some(k), Some(n)) = (array::<32>(key), array::<12>(nonce)) else {
            return LENGTH;
        };
        if out.len() != 64 {
            return LENGTH;
        }
        let mut k = k;
        let mut core = ChaChaCore::<U10>::new((&k).into(), (&n).into());
        core.set_block_pos(counter);
        let mut block = Default::default();
        core.write_keystream_block(&mut block);
        out.copy_from_slice(&block);
        block.as_mut_slice().zeroize();
        k.zeroize();
        OK
    })
}

// ---------------------------------------------------------------------------
// X25519 (RFC 7748).

/// `out = X25519(scalar, point)`. Returns [`AUTH`] (and writes nothing)
/// when the result is all zeros, i.e. the peer's point has small order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_x25519(
    scalar: *const u8,
    scalar_len: usize,
    point: *const u8,
    point_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (s, p, out) = unsafe {
        (
            slice(scalar, scalar_len),
            slice(point, point_len),
            slice_mut(out, out_len),
        )
    };
    guard(|| {
        let (Some(mut s), Some(p)) = (array::<32>(s), array::<32>(p)) else {
            return LENGTH;
        };
        if out.len() != 32 {
            return LENGTH;
        }
        let sk = x25519_dalek::StaticSecret::from(s);
        s.zeroize();
        let ss = sk.diffie_hellman(&x25519_dalek::PublicKey::from(p));
        if !ss.was_contributory() {
            return AUTH;
        }
        out.copy_from_slice(ss.as_bytes());
        OK
    })
}

/// `out = X25519(scalar, 9)`: the public key of `scalar`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_x25519_base(
    scalar: *const u8,
    scalar_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (s, out) = unsafe { (slice(scalar, scalar_len), slice_mut(out, out_len)) };
    guard(|| {
        let Some(mut s) = array::<32>(s) else {
            return LENGTH;
        };
        if out.len() != 32 {
            return LENGTH;
        }
        let sk = x25519_dalek::StaticSecret::from(s);
        s.zeroize();
        out.copy_from_slice(x25519_dalek::PublicKey::from(&sk).as_bytes());
        OK
    })
}

// ---------------------------------------------------------------------------
// Ed25519 (RFC 8032).

/// The 32-byte public key of a 32-byte secret seed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_ed25519_public(
    seed: *const u8,
    seed_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (seed, out) = unsafe { (slice(seed, seed_len), slice_mut(out, out_len)) };
    guard(|| {
        let Some(mut s) = array::<32>(seed) else {
            return LENGTH;
        };
        if out.len() != 32 {
            return LENGTH;
        }
        let k = ed25519_dalek::SigningKey::from_bytes(&s);
        s.zeroize();
        out.copy_from_slice(k.verifying_key().as_bytes());
        OK
    })
}

/// The 64-byte signature of `msg` under a 32-byte secret seed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_ed25519_sign(
    seed: *const u8,
    seed_len: usize,
    msg: *const u8,
    msg_len: usize,
    out: *mut u8,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (seed, msg, out) = unsafe {
        (
            slice(seed, seed_len),
            slice(msg, msg_len),
            slice_mut(out, out_len),
        )
    };
    guard(|| {
        use ed25519_dalek::Signer;
        let Some(mut s) = array::<32>(seed) else {
            return LENGTH;
        };
        if out.len() != 64 {
            return LENGTH;
        }
        let k = ed25519_dalek::SigningKey::from_bytes(&s);
        s.zeroize();
        out.copy_from_slice(&k.sign(msg).to_bytes());
        OK
    })
}

/// Strict verification (`verify_strict`: no small-order keys, canonical
/// `S`) of a 64-byte signature under a 32-byte public key. [`AUTH`] for an
/// invalid key or signature.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_ed25519_verify(
    public: *const u8,
    public_len: usize,
    msg: *const u8,
    msg_len: usize,
    sig: *const u8,
    sig_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (pk, msg, sig) = unsafe {
        (
            slice(public, public_len),
            slice(msg, msg_len),
            slice(sig, sig_len),
        )
    };
    guard(|| {
        let (Some(pk), Some(sig)) = (array::<32>(pk), array::<64>(sig)) else {
            return LENGTH;
        };
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else {
            return AUTH;
        };
        let sig = ed25519_dalek::Signature::from_bytes(&sig);
        if key.verify_strict(msg, &sig).is_ok() {
            OK
        } else {
            AUTH
        }
    })
}

// ---------------------------------------------------------------------------
// ML-KEM-768 (FIPS 203), libcrux.

fn isa_available(isa: u32) -> bool {
    match isa {
        isa::PORTABLE => true,
        #[cfg(target_arch = "x86_64")]
        isa::AVX2 => std::arch::is_x86_feature_detected!("avx2"),
        #[cfg(target_arch = "aarch64")]
        isa::NEON => std::arch::is_aarch64_feature_detected!("neon"),
        _ => false,
    }
}

/// 1 if ML-KEM backend `isa` is compiled in and this CPU can run it.
#[unsafe(no_mangle)]
pub extern "C" fn vcr_mlkem768_isa_available(isa: u32) -> u32 {
    u32::from(isa_available(isa))
}

/// Dispatches `$call` to libcrux's instantiation for `$isa` (already
/// checked available).
macro_rules! on_isa {
    ($isa:expr, $f:ident ( $($arg:expr),* )) => {{
        use libcrux_ml_kem::mlkem768 as m;
        match $isa {
            #[cfg(target_arch = "x86_64")]
            isa::AVX2 => m::avx2::$f($($arg),*),
            #[cfg(target_arch = "aarch64")]
            isa::NEON => m::neon::$f($($arg),*),
            _ => m::portable::$f($($arg),*),
        }
    }};
}

/// Expands a 64-byte seed `d || z` into the encapsulation key (1184 bytes)
/// and the decapsulation key (2400 bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_mlkem768_keygen(
    isa: u32,
    seed: *const u8,
    seed_len: usize,
    ek: *mut u8,
    ek_len: usize,
    dk: *mut u8,
    dk_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (seed, ek, dk) = unsafe {
        (
            slice(seed, seed_len),
            slice_mut(ek, ek_len),
            slice_mut(dk, dk_len),
        )
    };
    guard(|| {
        if !isa_available(isa) {
            return UNSUPPORTED;
        }
        let Some(s) = array::<64>(seed) else {
            return LENGTH;
        };
        if ek.len() != MLKEM768_EK_LEN || dk.len() != MLKEM768_DK_LEN {
            return LENGTH;
        }
        let kp = on_isa!(isa, generate_key_pair(s));
        let (mut sk, pk) = kp.into_parts();
        ek.copy_from_slice(pk.as_slice());
        dk.copy_from_slice(sk.as_slice());
        sk[0..MLKEM768_DK_LEN].zeroize();
        OK
    })
}

/// FIPS 203 encapsulation-key check (modulus check): [`OK`] or [`AUTH`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_mlkem768_validate(isa: u32, ek: *const u8, ek_len: usize) -> u32 {
    // SAFETY: the `.vsys` contract.
    let ek = unsafe { slice(ek, ek_len) };
    guard(|| {
        if !isa_available(isa) {
            return UNSUPPORTED;
        }
        let Some(ek) = array::<MLKEM768_EK_LEN>(ek) else {
            return LENGTH;
        };
        let pk = libcrux_ml_kem::mlkem768::MlKem768PublicKey::from(ek);
        if on_isa!(isa, validate_public_key(&pk)) {
            OK
        } else {
            AUTH
        }
    })
}

/// Encapsulates to `ek` with the 32-byte randomness `m`: writes the
/// ciphertext (1088 bytes) and the 32-byte shared secret. Callers validate
/// `ek` first ([`vcr_mlkem768_validate`]).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_mlkem768_encaps(
    isa: u32,
    ek: *const u8,
    ek_len: usize,
    m: *const u8,
    m_len: usize,
    ct: *mut u8,
    ct_len: usize,
    ss: *mut u8,
    ss_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (ek, m, ct, ss) = unsafe {
        (
            slice(ek, ek_len),
            slice(m, m_len),
            slice_mut(ct, ct_len),
            slice_mut(ss, ss_len),
        )
    };
    guard(|| {
        if !isa_available(isa) {
            return UNSUPPORTED;
        }
        let (Some(ek), Some(mut r)) = (array::<MLKEM768_EK_LEN>(ek), array::<32>(m)) else {
            return LENGTH;
        };
        if ct.len() != MLKEM768_CT_LEN || ss.len() != 32 {
            return LENGTH;
        }
        let pk = libcrux_ml_kem::mlkem768::MlKem768PublicKey::from(ek);
        let (c, mut k) = on_isa!(isa, encapsulate(&pk, r));
        r.zeroize();
        ct.copy_from_slice(c.as_slice());
        ss.copy_from_slice(&k);
        k.zeroize();
        OK
    })
}

/// Decapsulates `ct` with the 2400-byte decapsulation key: writes the
/// 32-byte shared secret (FIPS 203 implicit rejection: a bad ciphertext
/// gives a pseudorandom secret, not an error).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_mlkem768_decaps(
    isa: u32,
    dk: *const u8,
    dk_len: usize,
    ct: *const u8,
    ct_len: usize,
    ss: *mut u8,
    ss_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (dk, ct, ss) = unsafe { (slice(dk, dk_len), slice(ct, ct_len), slice_mut(ss, ss_len)) };
    guard(|| {
        if !isa_available(isa) {
            return UNSUPPORTED;
        }
        let (Some(mut d), Some(c)) = (array::<MLKEM768_DK_LEN>(dk), array::<MLKEM768_CT_LEN>(ct))
        else {
            return LENGTH;
        };
        if ss.len() != 32 {
            return LENGTH;
        }
        let mut sk = libcrux_ml_kem::mlkem768::MlKem768PrivateKey::from(d);
        d.zeroize();
        let ct = libcrux_ml_kem::mlkem768::MlKem768Ciphertext::from(c);
        let mut k = on_isa!(isa, decapsulate(&sk, &ct));
        sk[0..MLKEM768_DK_LEN].zeroize();
        ss.copy_from_slice(&k);
        k.zeroize();
        OK
    })
}

// ---------------------------------------------------------------------------
// ECDSA verification (ring).

/// Verifies an ECDSA signature over `msg` with an uncompressed SEC1 public
/// point, using ring's algorithm `alg` ([`ecdsa`] codes). [`AUTH`] for a
/// bad key or signature.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcr_ecdsa_verify(
    alg: u32,
    public: *const u8,
    public_len: usize,
    msg: *const u8,
    msg_len: usize,
    sig: *const u8,
    sig_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` contract.
    let (pk, msg, sig) = unsafe {
        (
            slice(public, public_len),
            slice(msg, msg_len),
            slice(sig, sig_len),
        )
    };
    guard(|| {
        use ring::signature as s;
        let a: &'static dyn s::VerificationAlgorithm = match alg {
            ecdsa::P256_SHA256_FIXED => &s::ECDSA_P256_SHA256_FIXED,
            ecdsa::P384_SHA384_FIXED => &s::ECDSA_P384_SHA384_FIXED,
            ecdsa::P256_SHA256_ASN1 => &s::ECDSA_P256_SHA256_ASN1,
            ecdsa::P384_SHA384_ASN1 => &s::ECDSA_P384_SHA384_ASN1,
            _ => return UNSUPPORTED,
        };
        match s::UnparsedPublicKey::new(a, pk).verify(msg, sig) {
            Ok(()) => OK,
            Err(_) => AUTH,
        }
    })
}

/// Every `vcr_*` symbol with its address, for hosts that bind the `.vsys`
/// library `Crypto` without a linker (the JIT tests).
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vcr_random", vcr_random as *const u8),
        ("vcr_ct_eq", vcr_ct_eq as *const u8),
        ("vcr_zeroize", vcr_zeroize as *const u8),
        ("vcr_hash_len", vcr_hash_len as *const u8),
        ("vcr_hash", vcr_hash as *const u8),
        ("vcr_hash_new", vcr_hash_new as *const u8),
        ("vcr_hash_clone", vcr_hash_clone as *const u8),
        ("vcr_hash_update", vcr_hash_update as *const u8),
        ("vcr_hash_digest", vcr_hash_digest as *const u8),
        ("vcr_hash_free", vcr_hash_free as *const u8),
        ("vcr_hmac", vcr_hmac as *const u8),
        ("vcr_hkdf_extract", vcr_hkdf_extract as *const u8),
        ("vcr_hkdf_expand", vcr_hkdf_expand as *const u8),
        ("vcr_aead_new", vcr_aead_new as *const u8),
        ("vcr_aead_free", vcr_aead_free as *const u8),
        ("vcr_aead_seal", vcr_aead_seal as *const u8),
        ("vcr_aead_open", vcr_aead_open as *const u8),
        ("vcr_aes_new", vcr_aes_new as *const u8),
        ("vcr_aes_free", vcr_aes_free as *const u8),
        ("vcr_aes_encrypt_block", vcr_aes_encrypt_block as *const u8),
        ("vcr_chacha20_block", vcr_chacha20_block as *const u8),
        ("vcr_x25519", vcr_x25519 as *const u8),
        ("vcr_x25519_base", vcr_x25519_base as *const u8),
        ("vcr_ed25519_public", vcr_ed25519_public as *const u8),
        ("vcr_ed25519_sign", vcr_ed25519_sign as *const u8),
        ("vcr_ed25519_verify", vcr_ed25519_verify as *const u8),
        (
            "vcr_mlkem768_isa_available",
            vcr_mlkem768_isa_available as *const u8,
        ),
        ("vcr_mlkem768_keygen", vcr_mlkem768_keygen as *const u8),
        ("vcr_mlkem768_validate", vcr_mlkem768_validate as *const u8),
        ("vcr_mlkem768_encaps", vcr_mlkem768_encaps as *const u8),
        ("vcr_mlkem768_decaps", vcr_mlkem768_decaps as *const u8),
        ("vcr_ecdsa_verify", vcr_ecdsa_verify as *const u8),
    ]
}
