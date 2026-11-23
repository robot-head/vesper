//! Frozen Rust §23.1.4 hybrid X25519MLKEM768 backed by `libcrux-ml-kem`,
//! `x25519-dalek`, and `getrandom`.

use libcrux_ml_kem::mlkem768::{
    MlKem768Ciphertext, MlKem768KeyPair, MlKem768PrivateKey, MlKem768PublicKey,
};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::kem::{
    CLIENT_SECRET_LEN, CLIENT_SHARE_LEN, KemError, KemIsa, MLKEM768_CT_LEN, MLKEM768_EK_LEN,
    SERVER_SHARE_LEN, SHARED_SECRET_LEN,
};

#[cfg(target_arch = "x86_64")]
pub(crate) fn avx2_available() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(not(target_arch = "x86_64"))]
pub(crate) fn avx2_available() -> bool {
    false
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn neon_available() -> bool {
    std::arch::is_aarch64_feature_detected!("neon")
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn neon_available() -> bool {
    false
}

pub(crate) fn is_isa_available(isa: KemIsa) -> bool {
    match isa {
        KemIsa::Portable => true,
        KemIsa::Avx2 => avx2_available(),
        KemIsa::Neon => neon_available(),
    }
}

pub(crate) fn detect_isa() -> KemIsa {
    if is_isa_available(KemIsa::Avx2) {
        KemIsa::Avx2
    } else if is_isa_available(KemIsa::Neon) {
        KemIsa::Neon
    } else {
        KemIsa::Portable
    }
}

pub(crate) fn require_isa(isa: KemIsa) -> Result<KemIsa, KemError> {
    if is_isa_available(isa) {
        Ok(isa)
    } else {
        Err(KemError::IsaUnavailable(isa))
    }
}

struct WipedKey(MlKem768PrivateKey);

impl Drop for WipedKey {
    fn drop(&mut self) {
        self.0[0..].zeroize();
    }
}

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

fn mlkem_key(isa: KemIsa, s: &[u8; CLIENT_SECRET_LEN]) -> (WipedKey, MlKem768PublicKey) {
    let mut seed = [0u8; 64];
    seed.copy_from_slice(&s[..64]);
    let kp: MlKem768KeyPair = ml::keygen(isa, &seed);
    seed.zeroize();
    let (sk, pk) = kp.into_parts();
    (WipedKey(sk), pk)
}

fn x25519_key(s: &[u8; CLIENT_SECRET_LEN]) -> StaticSecret {
    let mut x = [0u8; 32];
    x.copy_from_slice(&s[64..]);
    let sk = StaticSecret::from(x);
    x.zeroize();
    sk
}

pub(crate) fn mlkem768_encapsulation_key(
    isa: KemIsa,
    seed: &[u8; 64],
) -> Result<[u8; MLKEM768_EK_LEN], KemError> {
    let kp = ml::keygen(require_isa(isa)?, seed);
    let (sk, pk) = kp.into_parts();
    drop(WipedKey(sk));
    Ok(*pk.as_slice())
}

pub(crate) fn mlkem768_encapsulate(
    isa: KemIsa,
    ek: &[u8; MLKEM768_EK_LEN],
    m: &[u8; 32],
) -> Result<([u8; MLKEM768_CT_LEN], [u8; 32]), KemError> {
    let isa = require_isa(isa)?;
    let pk = MlKem768PublicKey::from(ek);
    if !ml::validate(isa, &pk) {
        return Err(KemError::InvalidEncapsulationKey);
    }
    let (ct, k) = ml::encaps(isa, &pk, m);
    Ok((*ct.as_slice(), k))
}

pub(crate) fn mlkem768_decapsulate(
    isa: KemIsa,
    seed: &[u8; 64],
    ct: &[u8; MLKEM768_CT_LEN],
) -> Result<[u8; 32], KemError> {
    let isa = require_isa(isa)?;
    let (sk, _) = ml::keygen(isa, seed).into_parts();
    let sk = WipedKey(sk);
    Ok(ml::decaps(isa, &sk.0, &MlKem768Ciphertext::from(ct)))
}

pub(crate) fn client_share(isa: KemIsa, seeds: &[u8; CLIENT_SECRET_LEN]) -> [u8; CLIENT_SHARE_LEN] {
    let (_sk, ek) = mlkem_key(isa, seeds);
    let mut out = [0u8; CLIENT_SHARE_LEN];
    out[..MLKEM768_EK_LEN].copy_from_slice(ek.as_slice());
    out[MLKEM768_EK_LEN..].copy_from_slice(PublicKey::from(&x25519_key(seeds)).as_bytes());
    out
}

pub(crate) fn client_decapsulate(
    isa: KemIsa,
    seeds: &[u8; CLIENT_SECRET_LEN],
    server_share: &[u8],
    out_ss: &mut [u8; SHARED_SECRET_LEN],
) -> Result<(), KemError> {
    let isa = require_isa(isa)?;
    if server_share.len() != SERVER_SHARE_LEN {
        return Err(KemError::BadShareLength {
            expected: SERVER_SHARE_LEN,
            found: server_share.len(),
        });
    }
    let ct = match MlKem768Ciphertext::try_from(&server_share[..MLKEM768_CT_LEN]) {
        Ok(c) => c,
        Err(_) => unreachable!("length checked"),
    };
    let peer: [u8; 32] = match server_share[MLKEM768_CT_LEN..].try_into() {
        Ok(p) => p,
        Err(_) => unreachable!("length checked"),
    };
    let (sk, _) = mlkem_key(isa, seeds);
    let mut k = ml::decaps(isa, &sk.0, &ct);
    drop(sk);
    out_ss[..32].copy_from_slice(&k);
    k.zeroize();
    let ss = x25519_key(seeds).diffie_hellman(&PublicKey::from(peer));
    out_ss[32..].copy_from_slice(ss.as_bytes());
    if !ss.was_contributory() {
        out_ss.zeroize();
        return Err(KemError::NonContributoryX25519);
    }
    Ok(())
}

pub(crate) fn server_validate(isa: KemIsa, client_share: &[u8]) -> Result<(), KemError> {
    let isa = require_isa(isa)?;
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
    Ok(())
}

pub(crate) fn server_encapsulate(
    isa: KemIsa,
    client_share: &[u8],
    m: &[u8; 32],
    x25519_eph: &[u8; 32],
    server_share: &mut [u8; SERVER_SHARE_LEN],
    out_ss: &mut [u8; SHARED_SECRET_LEN],
) -> Result<(), KemError> {
    server_validate(isa, client_share)?;
    let ek = match MlKem768PublicKey::try_from(&client_share[..MLKEM768_EK_LEN]) {
        Ok(e) => e,
        Err(_) => unreachable!("length checked"),
    };
    let peer: [u8; 32] = match client_share[MLKEM768_EK_LEN..].try_into() {
        Ok(p) => p,
        Err(_) => unreachable!("length checked"),
    };
    let (ct, mut k) = ml::encaps(isa, &ek, m);
    server_share[..MLKEM768_CT_LEN].copy_from_slice(ct.as_slice());
    out_ss[..32].copy_from_slice(&k);
    k.zeroize();
    let sk = StaticSecret::from(*x25519_eph);
    server_share[MLKEM768_CT_LEN..].copy_from_slice(PublicKey::from(&sk).as_bytes());
    let ss = sk.diffie_hellman(&PublicKey::from(peer));
    out_ss[32..].copy_from_slice(ss.as_bytes());
    if !ss.was_contributory() {
        out_ss.zeroize();
        server_share.zeroize();
        return Err(KemError::NonContributoryX25519);
    }
    Ok(())
}

pub(crate) fn fill_random(dst: &mut [u8]) -> Result<(), KemError> {
    getrandom::fill(dst).map_err(|_| KemError::Rng)
}
