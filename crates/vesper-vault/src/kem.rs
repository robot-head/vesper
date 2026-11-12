//! §23.1.4 hybrid post-quantum key establishment: X25519MLKEM768.
//!
//! Layout and combiner follow draft-ietf-tls-ecdhe-mlkem (TLS group 0x11EC):
//!
//! * client share = ML-KEM-768 encapsulation key (1184 bytes) || X25519
//!   public key (32 bytes), 1216 bytes;
//! * server share = ML-KEM-768 ciphertext (1088 bytes) || X25519 public key
//!   (32 bytes), 1120 bytes;
//! * shared secret = ML-KEM-768 shared secret (32 bytes) || X25519 shared
//!   secret (32 bytes), 64 bytes, concatenated with no KDF (TLS 1.3 feeds it
//!   to its own key schedule).
//!
//! ML-KEM-768 (FIPS 203) is RustCrypto `ml-kem` 0.3.4 built without `alloc`;
//! X25519 is `x25519-dalek` 3.0.0. As the draft requires, the server rejects
//! an encapsulation key that fails the FIPS 203 modulus check (`ml-kem`
//! validates on decode) and both sides reject an all-zero X25519 output.
//! Secret seeds and shared secrets live in [`SecretSlot`]s. The design's
//! `eff [Isa[P]]` NTT acceleration is not implemented: `ml-kem` uses its own
//! portable arithmetic.

use core::fmt;

use ml_kem::array::Array;
use ml_kem::{Decapsulate, KeyExport, MlKem768};
use rand_core::TryCryptoRng;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::obligation::{ObligationKind, record_leak};
use crate::slot::{LockPolicy, SecretSlot, ShredReceipt, SlotError};

/// ML-KEM-768 encapsulation key length.
pub const MLKEM768_EK_LEN: usize = 1184;
/// ML-KEM-768 ciphertext length.
pub const MLKEM768_CT_LEN: usize = 1088;
/// X25519 public key length.
pub const X25519_LEN: usize = 32;
/// Client key share length.
pub const CLIENT_SHARE_LEN: usize = MLKEM768_EK_LEN + X25519_LEN;
/// Server key share length.
pub const SERVER_SHARE_LEN: usize = MLKEM768_CT_LEN + X25519_LEN;
/// Hybrid shared secret length.
pub const SHARED_SECRET_LEN: usize = 64;
/// TLS NamedGroup codepoint for X25519MLKEM768.
pub const X25519MLKEM768_GROUP: u16 = 0x11EC;

/// ML-KEM seed (`d || z`, 64 bytes) followed by the X25519 scalar.
const CLIENT_SECRET_LEN: usize = 64 + 32;

/// Key establishment failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KemError {
    /// A share had the wrong length.
    BadShareLength {
        /// Required length.
        expected: usize,
        /// Received length.
        found: usize,
    },
    /// The ML-KEM encapsulation key failed FIPS 203 input validation.
    InvalidEncapsulationKey,
    /// The X25519 output was all zero (a low-order peer key).
    NonContributoryX25519,
    /// The random number generator failed.
    Rng,
    /// A secret slot could not be created.
    Slot(SlotError),
}

impl fmt::Display for KemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KemError::BadShareLength { expected, found } => {
                write!(f, "key share is {found} bytes, expected {expected}")
            }
            KemError::InvalidEncapsulationKey => {
                f.write_str("invalid ML-KEM-768 encapsulation key")
            }
            KemError::NonContributoryX25519 => f.write_str("all-zero X25519 shared secret"),
            KemError::Rng => f.write_str("random number generator failed"),
            KemError::Slot(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for KemError {}

impl From<SlotError> for KemError {
    fn from(e: SlotError) -> Self {
        KemError::Slot(e)
    }
}

/// The 64-byte hybrid shared secret, in locked memory.
pub type HybridSharedSecret = SecretSlot<SHARED_SECRET_LEN>;

/// The client's ephemeral secrets. Linear: exits are
/// [`HybridClientSecret::decapsulate`] and [`HybridClientSecret::abandon`].
#[must_use = "a HybridClientSecret must be decapsulated or abandoned"]
pub struct HybridClientSecret {
    seeds: Option<SecretSlot<CLIENT_SECRET_LEN>>,
}

impl fmt::Debug for HybridClientSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HybridClientSecret(<secret>)")
    }
}

/// A failed decapsulation; the client secret comes back so the caller still
/// owns its exit.
#[derive(Debug)]
pub struct DecapError {
    /// Why decapsulation failed.
    pub error: KemError,
    /// The unconsumed client secret.
    pub secret: HybridClientSecret,
}

/// Draw randomness straight into `buf`.
fn fill<R: TryCryptoRng + ?Sized>(rng: &mut R, buf: &mut [u8]) -> Result<(), KemError> {
    rng.try_fill_bytes(buf).map_err(|_| KemError::Rng)
}

/// Expand the ML-KEM decapsulation key from the slot's seed, wiping the
/// local copy of the seed (the key zeroizes itself on drop).
fn mlkem_key(s: &[u8; CLIENT_SECRET_LEN]) -> ml_kem::DecapsulationKey<MlKem768> {
    let mut seed = [0u8; 64];
    seed.copy_from_slice(&s[..64]);
    let dk = ml_kem::DecapsulationKey::<MlKem768>::from_seed(Array::from(seed));
    seed.zeroize();
    dk
}

/// The X25519 scalar from the slot, wiping the local copy (`StaticSecret`
/// zeroizes itself on drop).
fn x25519_key(s: &[u8; CLIENT_SECRET_LEN]) -> StaticSecret {
    let mut x = [0u8; 32];
    x.copy_from_slice(&s[64..]);
    let sk = StaticSecret::from(x);
    x.zeroize();
    sk
}

impl HybridClientSecret {
    /// Generate fresh client secrets and the 1216-byte client share.
    pub fn generate<R: TryCryptoRng + ?Sized>(
        rng: &mut R,
        policy: LockPolicy,
    ) -> Result<(Self, [u8; CLIENT_SHARE_LEN]), KemError> {
        let mut slot = SecretSlot::<CLIENT_SECRET_LEN>::new(policy)?;
        if let Err(e) = slot.with_mut(|s| fill(rng, s)) {
            let _ = slot.shred();
            return Err(e);
        }
        let secret = Self { seeds: Some(slot) };
        let share = secret.share();
        Ok((secret, share))
    }

    /// Deterministic construction from explicit seeds, for known-answer
    /// tests only. `seeds` is zeroized.
    pub fn from_seeds_for_testing(
        mlkem_seed: &mut [u8; 64],
        x25519_secret: &mut [u8; 32],
        policy: LockPolicy,
    ) -> Result<(Self, [u8; CLIENT_SHARE_LEN]), KemError> {
        let slot = SecretSlot::<CLIENT_SECRET_LEN>::from_fn(policy, |s| {
            s[..64].copy_from_slice(mlkem_seed);
            s[64..].copy_from_slice(x25519_secret);
        });
        mlkem_seed.zeroize();
        x25519_secret.zeroize();
        let secret = Self { seeds: Some(slot?) };
        let share = secret.share();
        Ok((secret, share))
    }

    fn slot(&self) -> &SecretSlot<CLIENT_SECRET_LEN> {
        match &self.seeds {
            Some(s) => s,
            None => unreachable!("client secret used after consumption"),
        }
    }

    fn share(&self) -> [u8; CLIENT_SHARE_LEN] {
        self.slot().with(|s| {
            let dk = mlkem_key(s);
            let ek = dk.encapsulation_key().to_bytes();
            let mut out = [0u8; CLIENT_SHARE_LEN];
            out[..MLKEM768_EK_LEN].copy_from_slice(ek.as_slice());
            out[MLKEM768_EK_LEN..].copy_from_slice(PublicKey::from(&x25519_key(s)).as_bytes());
            out
        })
    }

    /// Complete the exchange with the server's 1120-byte share.
    pub fn decapsulate(
        mut self,
        server_share: &[u8],
        policy: LockPolicy,
    ) -> Result<HybridSharedSecret, DecapError> {
        if server_share.len() != SERVER_SHARE_LEN {
            return Err(DecapError {
                error: KemError::BadShareLength {
                    expected: SERVER_SHARE_LEN,
                    found: server_share.len(),
                },
                secret: self,
            });
        }
        let mut out = match SecretSlot::<SHARED_SECRET_LEN>::new(policy) {
            Ok(o) => o,
            Err(e) => {
                return Err(DecapError {
                    error: e.into(),
                    secret: self,
                });
            }
        };
        let ct: Array<u8, _> = match Array::try_from(&server_share[..MLKEM768_CT_LEN]) {
            Ok(c) => c,
            Err(_) => unreachable!("length checked"),
        };
        let peer: [u8; 32] = match server_share[MLKEM768_CT_LEN..].try_into() {
            Ok(p) => p,
            Err(_) => unreachable!("length checked"),
        };
        let contributory = self.slot().with(|s| {
            out.with_mut(|o| {
                let dk = mlkem_key(s);
                let k = dk.decapsulate(&ct);
                o[..32].copy_from_slice(k.as_slice());
                let ss = x25519_key(s).diffie_hellman(&PublicKey::from(peer));
                o[32..].copy_from_slice(ss.as_bytes());
                ss.was_contributory()
            })
        });
        if !contributory {
            let _ = out.shred();
            return Err(DecapError {
                error: KemError::NonContributoryX25519,
                secret: self,
            });
        }
        if let Some(s) = self.seeds.take() {
            let _ = s.shred();
        }
        Ok(out)
    }

    /// Abandon the exchange and shred the secrets.
    pub fn abandon(mut self) -> ShredReceipt {
        match self.seeds.take() {
            Some(s) => s.shred(),
            None => unreachable!("client secret consumed twice"),
        }
    }
}

impl Drop for HybridClientSecret {
    fn drop(&mut self) {
        if let Some(s) = self.seeds.take() {
            record_leak(ObligationKind::KemSecret);
            let _ = s.shred();
        }
    }
}

fn server_core(
    client_share: &[u8],
    m: &[u8; 32],
    x25519_eph: &[u8; 32],
    policy: LockPolicy,
) -> Result<([u8; SERVER_SHARE_LEN], HybridSharedSecret), KemError> {
    if client_share.len() != CLIENT_SHARE_LEN {
        return Err(KemError::BadShareLength {
            expected: CLIENT_SHARE_LEN,
            found: client_share.len(),
        });
    }
    let ek_bytes: Array<u8, _> = match Array::try_from(&client_share[..MLKEM768_EK_LEN]) {
        Ok(e) => e,
        Err(_) => unreachable!("length checked"),
    };
    let ek = ml_kem::EncapsulationKey::<MlKem768>::new(&ek_bytes)
        .map_err(|_| KemError::InvalidEncapsulationKey)?;
    let peer: [u8; 32] = match client_share[MLKEM768_EK_LEN..].try_into() {
        Ok(p) => p,
        Err(_) => unreachable!("length checked"),
    };
    let mut out = SecretSlot::<SHARED_SECRET_LEN>::new(policy)?;
    let mut share = [0u8; SERVER_SHARE_LEN];
    let contributory = out.with_mut(|o| {
        let (ct, k) = ek.encapsulate_deterministic(&Array::from(*m));
        share[..MLKEM768_CT_LEN].copy_from_slice(ct.as_slice());
        o[..32].copy_from_slice(k.as_slice());
        let sk = StaticSecret::from(*x25519_eph);
        share[MLKEM768_CT_LEN..].copy_from_slice(PublicKey::from(&sk).as_bytes());
        let ss = sk.diffie_hellman(&PublicKey::from(peer));
        o[32..].copy_from_slice(ss.as_bytes());
        ss.was_contributory()
    });
    if !contributory {
        let _ = out.shred();
        return Err(KemError::NonContributoryX25519);
    }
    Ok((share, out))
}

/// Server side: validate the client share, encapsulate, and return the
/// 1120-byte server share with the hybrid shared secret.
pub fn server_encapsulate<R: TryCryptoRng + ?Sized>(
    rng: &mut R,
    client_share: &[u8],
    policy: LockPolicy,
) -> Result<([u8; SERVER_SHARE_LEN], HybridSharedSecret), KemError> {
    let mut eph = SecretSlot::<64>::new(policy)?;
    let r = match eph.with_mut(|e| fill(rng, e)) {
        Ok(()) => eph.with(|e| {
            let m: [u8; 32] = match e[..32].try_into() {
                Ok(m) => m,
                Err(_) => unreachable!("32 bytes"),
            };
            let x: [u8; 32] = match e[32..].try_into() {
                Ok(x) => x,
                Err(_) => unreachable!("32 bytes"),
            };
            server_core(client_share, &m, &x, policy)
        }),
        Err(e) => Err(e),
    };
    let _ = eph.shred();
    r
}

/// Deterministic server encapsulation for known-answer tests only.
pub fn server_encapsulate_for_testing(
    client_share: &[u8],
    mlkem_m: &[u8; 32],
    x25519_ephemeral: &[u8; 32],
    policy: LockPolicy,
) -> Result<([u8; SERVER_SHARE_LEN], HybridSharedSecret), KemError> {
    server_core(client_share, mlkem_m, x25519_ephemeral, policy)
}

/// Operating-system randomness (`getrandom`) as a `rand_core` 0.10 fallible
/// cryptographic RNG.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRng;

impl rand_core::TryRng for SystemRng {
    type Error = getrandom::Error;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        getrandom::fill(dst)
    }
}

impl TryCryptoRng for SystemRng {}
