//! X.509 certificate paths to a configured root (rustls-webpki) and raw
//! ECDSA verification of hardware signatures (ring).

use crate::error::{AttestError, ParseError};
use ring::signature::{self, UnparsedPublicKey};
use rustls_pki_types::{CertificateDer, UnixTime};
use spki::ObjectIdentifier;
use std::time::Duration;

const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");

/// Signature algorithms accepted on certificate paths. RSA-PSS/SHA-384 is
/// listed because AMD's ARK/ASK use it; no test exercises it (no real AMD
/// chain is included), so it is accepted on webpki's word only.
static CHAIN_ALGS: &[&dyn rustls_pki_types::SignatureVerificationAlgorithm] = &[
    webpki::ring::ECDSA_P256_SHA256,
    webpki::ring::ECDSA_P256_SHA384,
    webpki::ring::ECDSA_P384_SHA256,
    webpki::ring::ECDSA_P384_SHA384,
    webpki::ring::RSA_PSS_2048_8192_SHA384_LEGACY_KEY,
];

/// Elliptic curves used by hardware attestation keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Curve {
    pub fn scalar_len(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }

    fn fixed_alg(self) -> &'static signature::EcdsaVerificationAlgorithm {
        match self {
            Curve::P256 => &signature::ECDSA_P256_SHA256_FIXED,
            Curve::P384 => &signature::ECDSA_P384_SHA384_FIXED,
        }
    }
}

/// An uncompressed SEC1 elliptic-curve public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcPublicKey {
    pub curve: Curve,
    /// `0x04 || X || Y`.
    pub sec1: Vec<u8>,
}

impl EcPublicKey {
    /// Build from raw `X || Y` (as embedded in TDX quotes).
    pub fn from_raw_xy(curve: Curve, xy: &[u8]) -> Result<Self, AttestError> {
        if xy.len() != 2 * curve.scalar_len() {
            return Err(AttestError::UnexpectedKeyType {
                what: "raw EC point length",
            });
        }
        let mut sec1 = Vec::with_capacity(xy.len() + 1);
        sec1.push(4);
        sec1.extend_from_slice(xy);
        Ok(EcPublicKey { curve, sec1 })
    }

    /// Build from an uncompressed SEC1 point (as in CCA realm tokens).
    pub fn from_sec1(curve: Curve, sec1: &[u8]) -> Result<Self, AttestError> {
        if sec1.len() != 1 + 2 * curve.scalar_len() || sec1[0] != 4 {
            return Err(AttestError::UnexpectedKeyType {
                what: "uncompressed SEC1 point",
            });
        }
        Ok(EcPublicKey {
            curve,
            sec1: sec1.to_vec(),
        })
    }

    /// Verify a fixed-width `r || s` ECDSA signature (SHA-256 for P-256,
    /// SHA-384 for P-384). The curve-point check is done by ring.
    pub fn verify_fixed(
        &self,
        message: &[u8],
        r_s: &[u8],
        what: &'static str,
    ) -> Result<(), AttestError> {
        UnparsedPublicKey::new(self.curve.fixed_alg(), &self.sec1)
            .verify(message, r_s)
            .map_err(|_| AttestError::BadSignature { what })
    }
}

/// A configured trust anchor (for example Intel's SGX root CA, AMD's ARK,
/// or a CCA endorsement root). Tests use generated roots only.
#[derive(Clone, Debug)]
pub struct TrustRoot {
    der: Vec<u8>,
}

impl TrustRoot {
    pub fn from_der(der: impl Into<Vec<u8>>) -> Result<Self, AttestError> {
        let der = der.into();
        let cert = CertificateDer::from(der.as_slice());
        webpki::anchor_from_trusted_cert(&cert)
            .map_err(|e| AttestError::CertificateChain(format!("trust root: {e:?}")))?;
        Ok(TrustRoot { der })
    }

    pub fn der(&self) -> &[u8] {
        &self.der
    }
}

/// Verify `leaf` through `intermediates` to `root` at `now_ns` (Unix-epoch
/// nanoseconds on a caller-supplied clock) and return the leaf's EC key,
/// which must be on `curve`.
pub fn verify_chain(
    leaf: &[u8],
    intermediates: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    curve: Curve,
) -> Result<EcPublicKey, AttestError> {
    let root_der = CertificateDer::from(root.der.as_slice());
    let anchor = webpki::anchor_from_trusted_cert(&root_der)
        .map_err(|e| AttestError::CertificateChain(format!("trust root: {e:?}")))?;
    let leaf_der = CertificateDer::from(leaf);
    let ee = webpki::EndEntityCert::try_from(&leaf_der)
        .map_err(|e| AttestError::CertificateChain(format!("leaf: {e:?}")))?;
    let inter: Vec<CertificateDer<'_>> = intermediates
        .iter()
        .map(|d| CertificateDer::from(d.as_slice()))
        .collect();
    let time = UnixTime::since_unix_epoch(Duration::from_nanos(now_ns));
    ee.verify_for_usage(
        CHAIN_ALGS,
        &[anchor],
        &inter,
        time,
        webpki::KeyUsage::client_auth(),
        None,
        None,
    )
    .map_err(|e| AttestError::CertificateChain(format!("{e:?}")))?;
    leaf_ec_key(&ee, curve)
}

fn leaf_ec_key(ee: &webpki::EndEntityCert<'_>, curve: Curve) -> Result<EcPublicKey, AttestError> {
    let spki_der = ee.subject_public_key_info();
    let spki = spki::SubjectPublicKeyInfoRef::try_from(spki_der.as_ref()).map_err(|_| {
        AttestError::UnexpectedKeyType {
            what: "leaf SubjectPublicKeyInfo",
        }
    })?;
    if spki.algorithm.oid != ID_EC_PUBLIC_KEY {
        return Err(AttestError::UnexpectedKeyType {
            what: "leaf key is not id-ecPublicKey",
        });
    }
    let want = match curve {
        Curve::P256 => SECP256R1,
        Curve::P384 => SECP384R1,
    };
    match spki.algorithm.parameters_oid() {
        Ok(oid) if oid == want => {}
        _ => {
            return Err(AttestError::UnexpectedKeyType {
                what: "leaf key curve",
            });
        }
    }
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or(AttestError::UnexpectedKeyType {
            what: "leaf key bit string",
        })?;
    EcPublicKey::from_sec1(curve, point)
}

/// Split concatenated PEM certificates (leaf first) into DER blobs.
pub fn pem_certificates(pem: &[u8]) -> Result<Vec<Vec<u8>>, ParseError> {
    use rustls_pki_types::pem::PemObject;
    let mut out = Vec::new();
    for item in CertificateDer::pem_slice_iter(pem) {
        let cert = item.map_err(|_| ParseError::Pem("malformed PEM certificate"))?;
        out.push(cert.as_ref().to_vec());
    }
    if out.is_empty() {
        return Err(ParseError::Pem("no certificates"));
    }
    Ok(out)
}
