//! RFC 9001 §5.1/§5.2 key schedule: HKDF-Expand-Label, Initial secrets and
//! per-direction key material, and the §6.1 "quic ku" secret update.

use hkdf::Hkdf;
use sha2::{Sha256, Sha384};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::protect::{HeaderKey, PacketKey};
use crate::{Cipher, CryptoError, IV_LEN, MAX_CID_LEN};

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

/// TLS 1.3 HKDF-Expand-Label with an empty context (RFC 8446 §7.1), built
/// without allocation. `out.len()` must fit in a `u16` and `label` (with the
/// `"tls13 "` prefix) in a `u8`; both hold for every label used here.
fn expand_label(cipher: Cipher, secret: &[u8], label: &[u8], out: &mut [u8]) {
    let len = u16::try_from(out.len()).expect("internal output length fits u16");
    let label_len = u8::try_from(6 + label.len()).expect("internal label fits u8");
    let info: [&[u8]; 5] = [&len.to_be_bytes(), &[label_len], b"tls13 ", label, &[0]];
    // The secret is used as the PRK; its length always equals the hash
    // output length (checked by `TrafficSecret::new`), and every requested
    // output is far below 255 * HashLen, so neither call can fail.
    let ok = match cipher {
        Cipher::Aes128Gcm | Cipher::ChaCha20Poly1305 => Hkdf::<Sha256>::from_prk(secret)
            .ok()
            .and_then(|h| h.expand_multi_info(&info, out).ok()),
        Cipher::Aes256Gcm => Hkdf::<Sha384>::from_prk(secret)
            .ok()
            .and_then(|h| h.expand_multi_info(&info, out).ok()),
    };
    ok.expect("HKDF-Expand-Label with a validated PRK and short output");
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
        if secret.len() != cipher.secret_len() {
            return Err(CryptoError::SecretLength {
                expected: cipher.secret_len(),
                actual: secret.len(),
            });
        }
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
        expand_label(
            self.cipher,
            self.as_bytes(),
            b"quic ku",
            &mut next.bytes[..n],
        );
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
        let s = self.as_bytes();
        expand_label(c, s, b"quic key", &mut m.key[..c.key_len()]);
        expand_label(c, s, b"quic iv", &mut m.iv);
        expand_label(c, s, b"quic hp", &mut m.hp[..c.key_len()]);
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

impl InitialSecrets {
    /// RFC 9001 §5.2 for QUIC v1: `initial_secret = HKDF-Extract(salt,
    /// client_dst_connection_id)`, then `"client in"` / `"server in"`.
    pub fn new_v1(client_dcid: &[u8]) -> Result<Self, CryptoError> {
        if client_dcid.len() > MAX_CID_LEN {
            return Err(CryptoError::ConnectionIdTooLong(client_dcid.len()));
        }
        let (mut prk, _) = Hkdf::<Sha256>::extract(Some(&INITIAL_SALT_V1), client_dcid);
        let c = Cipher::Aes128Gcm;
        let mut client = TrafficSecret {
            cipher: c,
            bytes: [0; MAX_SECRET_LEN],
        };
        let mut server = TrafficSecret {
            cipher: c,
            bytes: [0; MAX_SECRET_LEN],
        };
        expand_label(c, &prk, b"client in", &mut client.bytes[..32]);
        expand_label(c, &prk, b"server in", &mut server.bytes[..32]);
        prk.as_mut_slice().zeroize();
        Ok(Self { client, server })
    }

    /// The common `initial_secret` (HKDF-Extract output), for test vectors.
    pub fn initial_secret_v1(client_dcid: &[u8]) -> Result<[u8; 32], CryptoError> {
        if client_dcid.len() > MAX_CID_LEN {
            return Err(CryptoError::ConnectionIdTooLong(client_dcid.len()));
        }
        let (prk, _) = Hkdf::<Sha256>::extract(Some(&INITIAL_SALT_V1), client_dcid);
        Ok(prk.into())
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
