//! RFC 9001 §5.1/§5.2 key schedule: HKDF-Expand-Label, Initial secrets and
//! per-direction key material, and the §6.1 "quic ku" secret update. The
//! derivations are Vesper (`vesper/src/lib.ves`); the secrets stay here.

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::port::{b, err, scratch};
use crate::protect::{HeaderKey, PacketKey};
use crate::{Cipher, CryptoError, IV_LEN};

/// QUIC v1 Initial salt (RFC 9001 §5.2).
pub const INITIAL_SALT_V1: [u8; 20] = [
    0x38, 0x76, 0x2c, 0xf7, 0xf5, 0x59, 0x34, 0xb3, 0x4d, 0x17, 0x9a, 0xe6, 0xa4, 0xc8, 0x0c, 0xad,
    0xcc, 0xbb, 0x7f, 0x0a,
];

const MAX_SECRET_LEN: usize = 48;
const MAX_KEY_LEN: usize = 32;

/// Which endpoint a set of keys belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Server,
}

/// A derivation whose PRK length and output sizes this crate fixed cannot
/// fail; a shim failure here is a broken invariant.
fn derived<E: std::fmt::Debug>(r: Result<(), E>) {
    r.expect("HKDF-Expand-Label with a validated PRK and short output");
}

/// A TLS traffic secret for one direction and one key phase. Zeroized on
/// drop; never `Clone` or `Debug`-printable.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct TrafficSecret {
    #[zeroize(skip)]
    cipher: Cipher,
    bytes: [u8; MAX_SECRET_LEN],
}

impl TrafficSecret {
    /// Wraps a secret supplied by a TLS stack. Its length must equal the
    /// cipher suite's hash output length.
    pub fn new(cipher: Cipher, secret: &[u8]) -> Result<Self, CryptoError> {
        b::secret_check(cipher.into(), secret.len() as u64).map_err(err)?;
        let mut bytes = [0u8; MAX_SECRET_LEN];
        bytes[..secret.len()].copy_from_slice(secret);
        Ok(Self { cipher, bytes })
    }

    pub fn cipher(&self) -> Cipher {
        self.cipher
    }

    /// The secret bytes. Exposed for test vectors and TLS integration.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.cipher.secret_len()]
    }

    /// RFC 9001 §6.1: the secret for the next key phase,
    /// `HKDF-Expand-Label(secret, "quic ku", "", Hash.length)`.
    pub fn next_phase(&self) -> TrafficSecret {
        let mut next = TrafficSecret {
            cipher: self.cipher,
            bytes: [0u8; MAX_SECRET_LEN],
        };
        let n = self.cipher.secret_len();
        derived(b::next_phase(
            self.cipher.into(),
            self.as_bytes(),
            &mut next.bytes[..n],
            &mut scratch(),
        ));
        next
    }

    /// RFC 9001 §5.1 packet protection key material for this secret.
    pub fn key_material(&self) -> KeyMaterial {
        let c = self.cipher;
        let mut m = KeyMaterial {
            cipher: c,
            key: [0; MAX_KEY_LEN],
            iv: [0; IV_LEN],
            hp: [0; MAX_KEY_LEN],
        };
        let n = c.key_len();
        derived(b::key_material(
            c.into(),
            self.as_bytes(),
            &mut m.key[..n],
            &mut m.iv,
            &mut m.hp[..n],
            &mut scratch(),
        ));
        m
    }

    /// Derived packet and header keys for this secret.
    pub fn keys(&self) -> DirectionalKeys {
        self.key_material().keys()
    }
}

/// Raw key, IV and header-protection key derived from a secret. Zeroized
/// on drop. Exposed so test vectors can be checked byte-for-byte.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct KeyMaterial {
    #[zeroize(skip)]
    cipher: Cipher,
    key: [u8; MAX_KEY_LEN],
    iv: [u8; IV_LEN],
    hp: [u8; MAX_KEY_LEN],
}

impl KeyMaterial {
    pub fn cipher(&self) -> Cipher {
        self.cipher
    }
    pub fn key(&self) -> &[u8] {
        &self.key[..self.cipher.key_len()]
    }
    pub fn iv(&self) -> &[u8; IV_LEN] {
        &self.iv
    }
    pub fn hp(&self) -> &[u8] {
        &self.hp[..self.cipher.key_len()]
    }

    /// Instantiates the AEAD and header-protection ciphers.
    pub fn keys(&self) -> DirectionalKeys {
        DirectionalKeys {
            packet: PacketKey::new(self.cipher, self.key(), self.iv),
            header: HeaderKey::new(self.cipher, self.hp()),
        }
    }
}

/// Packet and header keys for one direction.
pub struct DirectionalKeys {
    pub packet: PacketKey,
    pub header: HeaderKey,
}

/// Client and server Initial secrets for one Destination Connection ID.
pub struct InitialSecrets {
    pub client: TrafficSecret,
    pub server: TrafficSecret,
}

/// The Initial derivations raise `ConnectionIdTooLong` or a shim failure.
fn initial_error<E>(e: E, to_wc: impl FnOnce(E) -> Option<b::WcErr>) -> CryptoError {
    match to_wc(e) {
        Some(e) => err(e),
        None => panic!("HKDF over the fixed Initial salt cannot fail"),
    }
}

impl InitialSecrets {
    /// RFC 9001 §5.2 for QUIC v1: `initial_secret = HKDF-Extract(salt,
    /// client_dst_connection_id)`, then `"client in"` / `"server in"`.
    pub fn new_v1(client_dcid: &[u8]) -> Result<Self, CryptoError> {
        let c = Cipher::Aes128Gcm;
        let mut client = TrafficSecret {
            cipher: c,
            bytes: [0; MAX_SECRET_LEN],
        };
        let mut server = TrafficSecret {
            cipher: c,
            bytes: [0; MAX_SECRET_LEN],
        };
        let mut prk = [0u8; 32];
        b::initial_secrets(
            client_dcid,
            &mut client.bytes[..32],
            &mut server.bytes[..32],
            &mut prk,
            &mut scratch(),
        )
        .map_err(|e| {
            initial_error(e, |e| match e {
                b::InitialSecretsError::WcErr(e) => Some(e),
                _ => None,
            })
        })?;
        Ok(Self { client, server })
    }

    /// The common `initial_secret` (HKDF-Extract output), for test vectors.
    pub fn initial_secret_v1(client_dcid: &[u8]) -> Result<[u8; 32], CryptoError> {
        let mut out = [0u8; 32];
        b::initial_secret(client_dcid, &mut out, &mut scratch()).map_err(|e| {
            initial_error(e, |e| match e {
                b::InitialSecretError::WcErr(e) => Some(e),
                _ => None,
            })
        })?;
        Ok(out)
    }

    /// `(seal, open)` keys for `side`: an endpoint seals with its own secret
    /// and opens with its peer's.
    pub fn keys_for(&self, side: Side) -> (DirectionalKeys, DirectionalKeys) {
        let (local, remote) = match side {
            Side::Client => (&self.client, &self.server),
            Side::Server => (&self.server, &self.client),
        };
        (local.keys(), remote.keys())
    }
}
