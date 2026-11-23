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
//! Under `cfg(vesper_port)`, hybrid encapsulation, decapsulation, validation,
//! ISA dispatch, and OS CSPRNG run in the compiled Vesper port `@vesper/vault`
//! over `@vesper/crypto` (`vesper-crypto-sys`), while secret seeds and shared
//! secrets stay in [`SecretSlot`]s. `vesper-vault-seed` (`cfg(not(vesper_port))`)
//! runs the frozen Rust engine in `src/reference/`.

use core::fmt;

use rand_core::TryCryptoRng;
use zeroize::Zeroize;

use crate::obligation::{ObligationKind, record_leak};
#[cfg(vesper_port)]
use crate::port;
#[cfg(not(vesper_port))]
use crate::reference::kem as ref_kem;
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
pub(crate) const CLIENT_SECRET_LEN: usize = 64 + 32;
/// ML-KEM-768 expanded decapsulation key length.
#[cfg(vesper_port)]
pub(crate) const MLKEM768_DK_LEN: usize = 2400;

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
/// the `P` of the design's `eff [Isa[P]]`. Every backend produces identical
/// bytes; only speed differs.
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
        #[cfg(vesper_port)]
        {
            port::b::kem_isa_available(port::isa_kind(self))
        }
        #[cfg(not(vesper_port))]
        {
            ref_kem::is_isa_available(self)
        }
    }

    /// The fastest available backend.
    pub fn detect() -> KemIsa {
        #[cfg(vesper_port)]
        {
            port::isa_from_kind(port::b::kem_isa_detect())
        }
        #[cfg(not(vesper_port))]
        {
            ref_kem::detect_isa()
        }
    }

    /// The backends this CPU can run, portable first.
    pub fn available() -> impl Iterator<Item = KemIsa> {
        KemIsa::ALL.into_iter().filter(|i| i.is_available())
    }

    fn require(self) -> Result<KemIsa, KemError> {
        #[cfg(vesper_port)]
        {
            port::b::kem_require_isa(port::isa_kind(self)).map_err(port::kem_err)?;
            Ok(self)
        }
        #[cfg(not(vesper_port))]
        {
            ref_kem::require_isa(self)
        }
    }

    /// The ML-KEM-768 encapsulation key for a 64-byte `d || z` seed, on this
    /// backend. For known-answer and cross-backend tests.
    pub fn mlkem768_encapsulation_key_for_testing(
        self,
        seed: &[u8; 64],
    ) -> Result<[u8; MLKEM768_EK_LEN], KemError> {
        #[cfg(vesper_port)]
        {
            let mut ek = [0u8; MLKEM768_EK_LEN];
            let mut dk_scratch = [0u8; MLKEM768_DK_LEN];
            port::b::kem_mlkem768_ek(port::isa_kind(self), seed, &mut ek, &mut dk_scratch)
                .map_err(port::kem_err)?;
            Ok(ek)
        }
        #[cfg(not(vesper_port))]
        {
            ref_kem::mlkem768_encapsulation_key(self, seed)
        }
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
        #[cfg(vesper_port)]
        {
            let mut ct = [0u8; MLKEM768_CT_LEN];
            let mut ss = [0u8; 32];
            port::b::kem_mlkem768_encaps(port::isa_kind(self), ek, m, &mut ct, &mut ss)
                .map_err(port::kem_err)?;
            Ok((ct, ss))
        }
        #[cfg(not(vesper_port))]
        {
            ref_kem::mlkem768_encapsulate(self, ek, m)
        }
    }

    /// Plain ML-KEM-768 decapsulation (with implicit rejection) under the
    /// key expanded from `seed`, on this backend. Returns the shared secret
    /// in ordinary memory: known-answer and cross-backend tests only.
    pub fn mlkem768_decapsulate_for_testing(
        self,
        seed: &[u8; 64],
        ct: &[u8; MLKEM768_CT_LEN],
    ) -> Result<[u8; 32], KemError> {
        #[cfg(vesper_port)]
        {
            let mut ss = [0u8; 32];
            let mut ek_scratch = [0u8; MLKEM768_EK_LEN];
            let mut dk_scratch = [0u8; MLKEM768_DK_LEN];
            port::b::kem_mlkem768_decaps(
                port::isa_kind(self),
                seed,
                ct,
                &mut ss,
                &mut ek_scratch,
                &mut dk_scratch,
            )
            .map_err(port::kem_err)?;
            Ok(ss)
        }
        #[cfg(not(vesper_port))]
        {
            ref_kem::mlkem768_decapsulate(self, seed, ct)
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
        #[cfg(vesper_port)]
        {
            self.slot().with(|s| {
                let mut out = [0u8; CLIENT_SHARE_LEN];
                let mut dk_scratch = [0u8; MLKEM768_DK_LEN];
                port::b::kem_client_share(port::isa_kind(self.isa), s, &mut out, &mut dk_scratch)
                    .expect("backend already verified at construction");
                out
            })
        }
        #[cfg(not(vesper_port))]
        {
            self.slot().with(|s| ref_kem::client_share(self.isa, s))
        }
    }

    /// Complete the exchange with the server's 1120-byte share.
    pub fn decapsulate(
        mut self,
        server_share: &[u8],
        policy: LockPolicy,
    ) -> Result<HybridSharedSecret, DecapError> {
        #[cfg(vesper_port)]
        {
            let share_len = u64::try_from(server_share.len()).unwrap_or(u64::MAX);
            if let Err(e) = port::b::kem_check_server_share(share_len).map_err(port::kem_err) {
                return Err(DecapError {
                    error: e,
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
            let mut scratch = [0u8; MLKEM768_EK_LEN + MLKEM768_DK_LEN];
            let res = self.slot().with(|s| {
                out.with_mut(|o| {
                    port::b::kem_client_decapsulate(
                        port::isa_kind(self.isa),
                        s,
                        server_share,
                        o,
                        &mut scratch,
                    )
                })
            });
            scratch.zeroize();
            if let Err(e) = res.map_err(port::kem_err) {
                let _ = out.shred();
                return Err(DecapError {
                    error: e,
                    secret: self,
                });
            }
            if let Some(s) = self.seeds.take() {
                let _ = s.shred();
            }
            Ok(out)
        }
        #[cfg(not(vesper_port))]
        {
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
            let res = self.slot().with(|s| {
                out.with_mut(|o| ref_kem::client_decapsulate(self.isa, s, server_share, o))
            });
            if let Err(error) = res {
                let _ = out.shred();
                return Err(DecapError {
                    error,
                    secret: self,
                });
            }
            if let Some(s) = self.seeds.take() {
                let _ = s.shred();
            }
            Ok(out)
        }
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
    #[cfg(vesper_port)]
    {
        port::b::kem_server_validate(port::isa_kind(isa), client_share).map_err(port::kem_err)?;
        let mut out = SecretSlot::<SHARED_SECRET_LEN>::new(policy)?;
        let mut share = [0u8; SERVER_SHARE_LEN];
        let res = out.with_mut(|o| {
            port::b::kem_server_encapsulate(
                port::isa_kind(isa),
                client_share,
                m,
                x25519_eph,
                &mut share,
                o,
            )
        });
        if let Err(e) = res.map_err(port::kem_err) {
            let _ = out.shred();
            return Err(e);
        }
        Ok((share, out))
    }
    #[cfg(not(vesper_port))]
    {
        ref_kem::server_validate(isa, client_share)?;
        let mut out = SecretSlot::<SHARED_SECRET_LEN>::new(policy)?;
        let mut share = [0u8; SERVER_SHARE_LEN];
        if let Err(e) = out.with_mut(|o| {
            ref_kem::server_encapsulate(isa, client_share, m, x25519_eph, &mut share, o)
        }) {
            let _ = out.shred();
            return Err(e);
        }
        Ok((share, out))
    }
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

fn fill_random(dst: &mut [u8]) -> Result<(), KemError> {
    #[cfg(vesper_port)]
    {
        port::b::fill_random(dst).map_err(port::kem_err)
    }
    #[cfg(not(vesper_port))]
    {
        ref_kem::fill_random(dst)
    }
}

/// Operating-system randomness (`vesper-crypto-sys` `vcr_random` on the port,
/// `getrandom` on the Rust seed) as a `rand_core` 0.10 fallible cryptographic RNG.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRng;

impl rand_core::TryRng for SystemRng {
    type Error = KemError;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        fill_random(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        fill_random(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        fill_random(dst)
    }
}

impl TryCryptoRng for SystemRng {}
