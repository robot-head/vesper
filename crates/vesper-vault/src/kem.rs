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
//! ML-KEM-768 (FIPS 203) is Cryspen `libcrux-ml-kem` 0.0.11, built without
//! `std`, `alloc` or `rand`; X25519 is `x25519-dalek` 3.0.0. As the draft
//! requires, the server rejects an encapsulation key that fails the FIPS 203
//! modulus check and both sides reject an all-zero X25519 output. Secret
//! seeds and shared secrets live in [`SecretSlot`]s.
//!
//! # Vector NTT (`eff [Isa[P]]`)
//!
//! The design asks for the ML-KEM polynomial NTT to run on vector units.
//! Vault does not implement any of that arithmetic itself (the shared rules
//! forbid hand-rolled primitives): it delegates to libcrux, whose portable,
//! AVX2 and NEON backends are formally verified with hax and F* for panic
//! freedom, functional correctness against the FIPS 203 specification and
//! secret independence. [`KemIsa`] is the explicit ISA witness: every
//! exchange records the backend that ran it, the `_on` entry points pin one,
//! a backend the CPU lacks is refused with [`KemError::IsaUnavailable`]
//! instead of being run, and the default is [`KemIsa::detect`]. Vault itself
//! contains no `unsafe`; the intrinsics calls are inside libcrux.

use core::fmt;

use libcrux_ml_kem::mlkem768::{
    MlKem768Ciphertext, MlKem768KeyPair, MlKem768PrivateKey, MlKem768PublicKey,
};
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
    /// The requested ML-KEM backend is not available on this CPU or was not
    /// compiled for this architecture.
    IsaUnavailable(KemIsa),
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
            KemError::IsaUnavailable(isa) => {
                write!(f, "ML-KEM backend {} is not available here", isa.name())
            }
        }
    }
}

impl std::error::Error for KemError {}

impl From<SlotError> for KemError {
    fn from(e: SlotError) -> Self {
        KemError::Slot(e)
    }
}

/// The instruction set that runs the ML-KEM-768 polynomial arithmetic
/// (NTT, inverse NTT, base-case multiplication, sampling and compression):
/// the `P` of the design's `eff [Isa[P]]`. Every backend is libcrux's and all
/// of them produce identical bytes; only speed differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KemIsa {
    /// Portable scalar code, available everywhere.
    Portable,
    /// x86-64 AVX2 (256-bit vectors, sixteen 16-bit coefficients per lane).
    Avx2,
    /// AArch64 NEON (128-bit vectors).
    Neon,
}

impl KemIsa {
    /// Every backend, fastest last.
    pub const ALL: [KemIsa; 3] = [KemIsa::Portable, KemIsa::Neon, KemIsa::Avx2];

    /// Short lowercase name (`portable`, `avx2`, `neon`).
    pub fn name(self) -> &'static str {
        match self {
            KemIsa::Portable => "portable",
            KemIsa::Avx2 => "avx2",
            KemIsa::Neon => "neon",
        }
    }

    /// Whether this backend was compiled in and the running CPU supports it.
    pub fn is_available(self) -> bool {
        match self {
            KemIsa::Portable => true,
            KemIsa::Avx2 => avx2_available(),
            KemIsa::Neon => neon_available(),
        }
    }

    /// The fastest available backend.
    pub fn detect() -> KemIsa {
        if KemIsa::Avx2.is_available() {
            KemIsa::Avx2
        } else if KemIsa::Neon.is_available() {
            KemIsa::Neon
        } else {
            KemIsa::Portable
        }
    }

    /// The backends this CPU can run, portable first.
    pub fn available() -> impl Iterator<Item = KemIsa> {
        KemIsa::ALL.into_iter().filter(|i| i.is_available())
    }

    fn require(self) -> Result<KemIsa, KemError> {
        if self.is_available() {
            Ok(self)
        } else {
            Err(KemError::IsaUnavailable(self))
        }
    }

    /// The ML-KEM-768 encapsulation key for a 64-byte `d || z` seed, on this
    /// backend. For known-answer and cross-backend tests.
    pub fn mlkem768_encapsulation_key_for_testing(
        self,
        seed: &[u8; 64],
    ) -> Result<[u8; MLKEM768_EK_LEN], KemError> {
        let kp = ml::keygen(self.require()?, seed);
        let (sk, pk) = kp.into_parts();
        drop(WipedKey(sk));
        Ok(*pk.as_slice())
    }

    /// Plain ML-KEM-768 encapsulation with explicit message `m`, on this
    /// backend; rejects an encapsulation key that fails the FIPS 203 modulus
    /// check. Returns the ciphertext and the shared secret in ordinary
    /// memory: known-answer and cross-backend tests only.
    pub fn mlkem768_encapsulate_for_testing(
        self,
        ek: &[u8; MLKEM768_EK_LEN],
        m: &[u8; 32],
    ) -> Result<([u8; MLKEM768_CT_LEN], [u8; 32]), KemError> {
        let isa = self.require()?;
        let pk = MlKem768PublicKey::from(ek);
        if !ml::validate(isa, &pk) {
            return Err(KemError::InvalidEncapsulationKey);
        }
        let (ct, k) = ml::encaps(isa, &pk, m);
        Ok((*ct.as_slice(), k))
    }

    /// Plain ML-KEM-768 decapsulation (with implicit rejection) under the
    /// key expanded from `seed`, on this backend. Returns the shared secret
    /// in ordinary memory: known-answer and cross-backend tests only.
    pub fn mlkem768_decapsulate_for_testing(
        self,
        seed: &[u8; 64],
        ct: &[u8; MLKEM768_CT_LEN],
    ) -> Result<[u8; 32], KemError> {
        let isa = self.require()?;
        let (sk, _) = ml::keygen(isa, seed).into_parts();
        let sk = WipedKey(sk);
        Ok(ml::decaps(isa, &sk.0, &MlKem768Ciphertext::from(ct)))
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_available() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(not(target_arch = "x86_64"))]
fn avx2_available() -> bool {
    false
}

#[cfg(target_arch = "aarch64")]
fn neon_available() -> bool {
    std::arch::is_aarch64_feature_detected!("neon")
}

#[cfg(not(target_arch = "aarch64"))]
fn neon_available() -> bool {
    false
}

/// An expanded ML-KEM-768 decapsulation key (2400 bytes), wiped on drop.
struct WipedKey(MlKem768PrivateKey);

impl Drop for WipedKey {
    fn drop(&mut self) {
        self.0[0..].zeroize();
    }
}

/// Dispatch to libcrux's per-ISA instantiations. Callers pass a backend
/// that [`KemIsa::require`] accepted, so an arm for a backend that is not
/// compiled for this architecture is never reached.
mod ml {
    use super::KemIsa;
    use libcrux_ml_kem::mlkem768::{
        self as m, MlKem768Ciphertext, MlKem768KeyPair, MlKem768PrivateKey, MlKem768PublicKey,
    };
    use zeroize::Zeroize;

    pub(super) fn keygen(isa: KemIsa, seed: &[u8; 64]) -> MlKem768KeyPair {
        let mut s = *seed;
        let kp = match isa {
            #[cfg(target_arch = "x86_64")]
            KemIsa::Avx2 => m::avx2::generate_key_pair(s),
            #[cfg(target_arch = "aarch64")]
            KemIsa::Neon => m::neon::generate_key_pair(s),
            KemIsa::Portable => m::portable::generate_key_pair(s),
            #[allow(unreachable_patterns)]
            _ => unreachable!("backend not compiled for this architecture"),
        };
        s.zeroize();
        kp
    }

    pub(super) fn validate(isa: KemIsa, pk: &MlKem768PublicKey) -> bool {
        match isa {
            #[cfg(target_arch = "x86_64")]
            KemIsa::Avx2 => m::avx2::validate_public_key(pk),
            #[cfg(target_arch = "aarch64")]
            KemIsa::Neon => m::neon::validate_public_key(pk),
            KemIsa::Portable => m::portable::validate_public_key(pk),
            #[allow(unreachable_patterns)]
            _ => unreachable!("backend not compiled for this architecture"),
        }
    }

    pub(super) fn encaps(
        isa: KemIsa,
        pk: &MlKem768PublicKey,
        msg: &[u8; 32],
    ) -> (MlKem768Ciphertext, [u8; 32]) {
        let mut r = *msg;
        let out = match isa {
            #[cfg(target_arch = "x86_64")]
            KemIsa::Avx2 => m::avx2::encapsulate(pk, r),
            #[cfg(target_arch = "aarch64")]
            KemIsa::Neon => m::neon::encapsulate(pk, r),
            KemIsa::Portable => m::portable::encapsulate(pk, r),
            #[allow(unreachable_patterns)]
            _ => unreachable!("backend not compiled for this architecture"),
        };
        r.zeroize();
        out
    }

    pub(super) fn decaps(
        isa: KemIsa,
        sk: &MlKem768PrivateKey,
        ct: &MlKem768Ciphertext,
    ) -> [u8; 32] {
        match isa {
            #[cfg(target_arch = "x86_64")]
            KemIsa::Avx2 => m::avx2::decapsulate(sk, ct),
            #[cfg(target_arch = "aarch64")]
            KemIsa::Neon => m::neon::decapsulate(sk, ct),
            KemIsa::Portable => m::portable::decapsulate(sk, ct),
            #[allow(unreachable_patterns)]
            _ => unreachable!("backend not compiled for this architecture"),
        }
    }
}

/// The 64-byte hybrid shared secret, in locked memory.
pub type HybridSharedSecret = SecretSlot<SHARED_SECRET_LEN>;

/// The client's ephemeral secrets. Linear: exits are
/// [`HybridClientSecret::decapsulate`] and [`HybridClientSecret::abandon`].
#[must_use = "a HybridClientSecret must be decapsulated or abandoned"]
pub struct HybridClientSecret {
    seeds: Option<SecretSlot<CLIENT_SECRET_LEN>>,
    isa: KemIsa,
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

/// Expand the ML-KEM key pair from the slot's seed, wiping the local copy
/// of the seed; the expanded decapsulation key is wiped when dropped.
fn mlkem_key(isa: KemIsa, s: &[u8; CLIENT_SECRET_LEN]) -> (WipedKey, MlKem768PublicKey) {
    let mut seed = [0u8; 64];
    seed.copy_from_slice(&s[..64]);
    let kp: MlKem768KeyPair = ml::keygen(isa, &seed);
    seed.zeroize();
    let (sk, pk) = kp.into_parts();
    (WipedKey(sk), pk)
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
    /// Generate fresh client secrets and the 1216-byte client share, on the
    /// [`KemIsa::detect`] backend.
    pub fn generate<R: TryCryptoRng + ?Sized>(
        rng: &mut R,
        policy: LockPolicy,
    ) -> Result<(Self, [u8; CLIENT_SHARE_LEN]), KemError> {
        Self::generate_on(KemIsa::detect(), rng, policy)
    }

    /// [`HybridClientSecret::generate`] on an explicit ML-KEM backend; the
    /// same backend runs [`HybridClientSecret::decapsulate`].
    pub fn generate_on<R: TryCryptoRng + ?Sized>(
        isa: KemIsa,
        rng: &mut R,
        policy: LockPolicy,
    ) -> Result<(Self, [u8; CLIENT_SHARE_LEN]), KemError> {
        let isa = isa.require()?;
        let mut slot = SecretSlot::<CLIENT_SECRET_LEN>::new(policy)?;
        if let Err(e) = slot.with_mut(|s| fill(rng, s)) {
            let _ = slot.shred();
            return Err(e);
        }
        let secret = Self {
            seeds: Some(slot),
            isa,
        };
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
        Self::from_seeds_for_testing_on(KemIsa::detect(), mlkem_seed, x25519_secret, policy)
    }

    /// [`HybridClientSecret::from_seeds_for_testing`] on an explicit ML-KEM
    /// backend. The seeds are zeroized even when the backend is refused.
    pub fn from_seeds_for_testing_on(
        isa: KemIsa,
        mlkem_seed: &mut [u8; 64],
        x25519_secret: &mut [u8; 32],
        policy: LockPolicy,
    ) -> Result<(Self, [u8; CLIENT_SHARE_LEN]), KemError> {
        let isa = match isa.require() {
            Ok(i) => i,
            Err(e) => {
                mlkem_seed.zeroize();
                x25519_secret.zeroize();
                return Err(e);
            }
        };
        let slot = SecretSlot::<CLIENT_SECRET_LEN>::from_fn(policy, |s| {
            s[..64].copy_from_slice(mlkem_seed);
            s[64..].copy_from_slice(x25519_secret);
        });
        mlkem_seed.zeroize();
        x25519_secret.zeroize();
        let secret = Self {
            seeds: Some(slot?),
            isa,
        };
        let share = secret.share();
        Ok((secret, share))
    }

    /// The ML-KEM backend this exchange runs on.
    pub fn isa(&self) -> KemIsa {
        self.isa
    }

    fn slot(&self) -> &SecretSlot<CLIENT_SECRET_LEN> {
        match &self.seeds {
            Some(s) => s,
            None => unreachable!("client secret used after consumption"),
        }
    }

    fn share(&self) -> [u8; CLIENT_SHARE_LEN] {
        self.slot().with(|s| {
            let (_sk, ek) = mlkem_key(self.isa, s);
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
        let ct = match MlKem768Ciphertext::try_from(&server_share[..MLKEM768_CT_LEN]) {
            Ok(c) => c,
            Err(_) => unreachable!("length checked"),
        };
        let peer: [u8; 32] = match server_share[MLKEM768_CT_LEN..].try_into() {
            Ok(p) => p,
            Err(_) => unreachable!("length checked"),
        };
        let contributory = self.slot().with(|s| {
            out.with_mut(|o| {
                let (sk, _) = mlkem_key(self.isa, s);
                let mut k = ml::decaps(self.isa, &sk.0, &ct);
                drop(sk);
                o[..32].copy_from_slice(&k);
                k.zeroize();
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
    isa: KemIsa,
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
    let ek = match MlKem768PublicKey::try_from(&client_share[..MLKEM768_EK_LEN]) {
        Ok(e) => e,
        Err(_) => unreachable!("length checked"),
    };
    if !ml::validate(isa, &ek) {
        return Err(KemError::InvalidEncapsulationKey);
    }
    let peer: [u8; 32] = match client_share[MLKEM768_EK_LEN..].try_into() {
        Ok(p) => p,
        Err(_) => unreachable!("length checked"),
    };
    let mut out = SecretSlot::<SHARED_SECRET_LEN>::new(policy)?;
    let mut share = [0u8; SERVER_SHARE_LEN];
    let contributory = out.with_mut(|o| {
        let (ct, mut k) = ml::encaps(isa, &ek, m);
        share[..MLKEM768_CT_LEN].copy_from_slice(ct.as_slice());
        o[..32].copy_from_slice(&k);
        k.zeroize();
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
/// 1120-byte server share with the hybrid shared secret, on the
/// [`KemIsa::detect`] backend.
pub fn server_encapsulate<R: TryCryptoRng + ?Sized>(
    rng: &mut R,
    client_share: &[u8],
    policy: LockPolicy,
) -> Result<([u8; SERVER_SHARE_LEN], HybridSharedSecret), KemError> {
    server_encapsulate_on(KemIsa::detect(), rng, client_share, policy)
}

/// [`server_encapsulate`] on an explicit ML-KEM backend.
pub fn server_encapsulate_on<R: TryCryptoRng + ?Sized>(
    isa: KemIsa,
    rng: &mut R,
    client_share: &[u8],
    policy: LockPolicy,
) -> Result<([u8; SERVER_SHARE_LEN], HybridSharedSecret), KemError> {
    let isa = isa.require()?;
    let mut eph = SecretSlot::<64>::new(policy)?;
    let r = match eph.with_mut(|e| fill(rng, e)) {
        Ok(()) => eph.with(|e| {
            // Borrow both halves in place: no unwiped stack copies.
            let m: &[u8; 32] = match e[..32].try_into() {
                Ok(m) => m,
                Err(_) => unreachable!("32 bytes"),
            };
            let x: &[u8; 32] = match e[32..].try_into() {
                Ok(x) => x,
                Err(_) => unreachable!("32 bytes"),
            };
            server_core(isa, client_share, m, x, policy)
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
    server_encapsulate_for_testing_on(
        KemIsa::detect(),
        client_share,
        mlkem_m,
        x25519_ephemeral,
        policy,
    )
}

/// [`server_encapsulate_for_testing`] on an explicit ML-KEM backend.
pub fn server_encapsulate_for_testing_on(
    isa: KemIsa,
    client_share: &[u8],
    mlkem_m: &[u8; 32],
    x25519_ephemeral: &[u8; 32],
    policy: LockPolicy,
) -> Result<([u8; SERVER_SHARE_LEN], HybridSharedSecret), KemError> {
    server_core(
        isa.require()?,
        client_share,
        mlkem_m,
        x25519_ephemeral,
        policy,
    )
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
