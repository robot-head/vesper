//! X.509 certificate paths to a configured root (rustls-webpki) and raw
//! ECDSA verification of hardware signatures (ring).

use crate::error::{AttestError, ParseError};
use ring::signature::{self, UnparsedPublicKey};
use rustls_pki_types::{
    AlgorithmIdentifier, CertificateDer, InvalidSignature, SignatureVerificationAlgorithm,
    UnixTime, alg_id,
};
use spki::ObjectIdentifier;
use std::time::Duration;

const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");

/// Signature algorithms accepted on certificate paths and CRLs. RSA-PSS/
/// SHA-384 appears twice: once in the DER form webpki knows, and once in the
/// form AMD's KDS certificates and CRLs actually carry (see
/// [`AmdRsaPssSha384`]). The real Milan ARK → ASK → VCEK chains in
/// `tests/vendor_samples.rs` exercise the AMD form.
static CHAIN_ALGS: &[&dyn SignatureVerificationAlgorithm] = &[
    webpki::ring::ECDSA_P256_SHA256,
    webpki::ring::ECDSA_P256_SHA384,
    webpki::ring::ECDSA_P384_SHA256,
    webpki::ring::ECDSA_P384_SHA384,
    webpki::ring::RSA_PSS_2048_8192_SHA384_LEGACY_KEY,
    &AmdRsaPssSha384,
];

/// RSASSA-PSS with SHA-384, MGF1-SHA-384 and a 48-byte salt, in the exact
/// `AlgorithmIdentifier` encoding AMD uses for the ARK, ASK, ASVK, VCEK and
/// VLEK certificates and for the KDS CRLs.
///
/// AMD writes the `trailerField [3] INTEGER 1` explicitly. That value is the
/// ASN.1 DEFAULT, so DER requires it to be omitted, and webpki (which compares
/// algorithm identifiers byte for byte) does not recognise the AMD form. This
/// adapter accepts exactly that one encoding, with exactly these parameters,
/// and delegates the signature check to ring's `RSA_PSS_2048_8192_SHA384`.
/// No cryptography is implemented here.
#[derive(Debug)]
pub struct AmdRsaPssSha384;

/// `AlgorithmIdentifier` contents (without the outer SEQUENCE) of AMD's
/// RSASSA-PSS parameters, trailer field included.
const AMD_RSA_PSS_SHA384_ALG_ID: &[u8] = &[
    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a, // id-RSASSA-PSS
    0x30, 0x39, // RSASSA-PSS-params
    0xa0, 0x0f, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05,
    0x00, // hashAlgorithm sha384
    0xa1, 0x1c, 0x30, 0x1a, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x08, 0x30,
    0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05,
    0x00, // maskGenAlgorithm mgf1(sha384)
    0xa2, 0x03, 0x02, 0x01, 0x30, // saltLength 48
    0xa3, 0x03, 0x02, 0x01, 0x01, // trailerField 1 (the DEFAULT, written out)
];

impl SignatureVerificationAlgorithm for AmdRsaPssSha384 {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        UnparsedPublicKey::new(&signature::RSA_PSS_2048_8192_SHA384, public_key)
            .verify(message, signature)
            .map_err(|_| InvalidSignature)
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        AlgorithmIdentifier::from_slice(AMD_RSA_PSS_SHA384_ALG_ID)
    }

    fn fips(&self) -> bool {
        false
    }
}

/// Elliptic curves used by hardware attestation keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

/// Scalar (and coordinate) sizes of the NIST curves (SEC 2).
const P256_SCALAR_LEN: usize = 32;
const P384_SCALAR_LEN: usize = 48;
/// SEC 1 §2.3.3: an uncompressed point starts with `0x04`.
pub(crate) const SEC1_UNCOMPRESSED: u8 = 0x04;

/// The DER (ITU-T X.690) bytes `sequence_contents` reads: identifier octets
/// and short/long length forms.
mod der {
    pub(super) const SEQUENCE: u8 = 0x30;
    pub(super) const LONG_FORM: u8 = 0x80;
    pub(super) const LENGTH_OCTETS_MASK: u8 = 0x7f;
    /// A tag octet and a short-form length octet.
    pub(super) const SHORT_HEADER: usize = 2;
}

impl Curve {
    pub fn scalar_len(self) -> usize {
        match self {
            Curve::P256 => P256_SCALAR_LEN,
            Curve::P384 => P384_SCALAR_LEN,
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
        sec1.push(SEC1_UNCOMPRESSED);
        sec1.extend_from_slice(xy);
        Ok(EcPublicKey { curve, sec1 })
    }

    /// Build from an uncompressed SEC1 point (as in CCA realm tokens).
    pub fn from_sec1(curve: Curve, sec1: &[u8]) -> Result<Self, AttestError> {
        if sec1.len() != 1 + 2 * curve.scalar_len() || sec1[0] != SEC1_UNCOMPRESSED {
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
/// or a CCA endorsement root). See [`crate::roots`] for the pinned ones.
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
/// which must be on `curve`. No revocation check; see
/// [`verify_chain_with_crls`].
pub fn verify_chain(
    leaf: &[u8],
    intermediates: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    curve: Curve,
) -> Result<EcPublicKey, AttestError> {
    verify_path(leaf, intermediates, root, now_ns, None)?;
    leaf_ec_key(leaf, curve)
}

/// How certificate revocation lists apply to a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationScope {
    /// Every certificate below the root must be covered by one of the
    /// supplied CRLs (Intel: the PCK CRL covers the PCK, the root CA CRL
    /// covers the PCK and TCB-signing CAs).
    EveryCertificate,
    /// Only certificates whose issuer published one of the supplied CRLs
    /// are checked, and at least one CRL must be issued by the root (AMD:
    /// the ARK's CRL covers the ASK/ASVK; AMD publishes no CRL for VCEKs).
    IssuedByRoot,
}

/// Like [`verify_chain`], also checking revocation against `crls` (DER).
/// CRL signatures are verified against their issuer in the path, and an
/// expired CRL (`now` past `nextUpdate`) is an error.
pub fn verify_chain_with_crls(
    leaf: &[u8],
    intermediates: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    curve: Curve,
    crls: &[&[u8]],
    scope: RevocationScope,
) -> Result<EcPublicKey, AttestError> {
    verify_path(leaf, intermediates, root, now_ns, Some((crls, scope)))?;
    leaf_ec_key(leaf, curve)
}

/// `thisUpdate` of a DER CRL, in Unix seconds (signature not checked: use
/// it only on a CRL that [`verify_chain_with_crls`] accepted).
pub fn crl_this_update(der: &[u8]) -> Result<u64, AttestError> {
    let (rest, crl) =
        x509_parser::parse_x509_crl(der).map_err(|e| AttestError::Crl(format!("{e}")))?;
    if !rest.is_empty() {
        return Err(AttestError::Crl("trailing bytes".into()));
    }
    u64::try_from(crl.last_update().timestamp())
        .map_err(|_| AttestError::Crl("thisUpdate before 1970".into()))
}

/// Path validation only; returns nothing about the leaf key. Used for RSA
/// leaves (AMD intermediates) and by the typed wrappers above.
pub fn verify_path(
    leaf: &[u8],
    intermediates: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    revocation: Option<(&[&[u8]], RevocationScope)>,
) -> Result<(), AttestError> {
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
    let mut parsed = Vec::new();
    let scope = match revocation {
        None => None,
        Some((crls, scope)) => {
            if crls.is_empty() {
                return Err(AttestError::MissingCollateral { what: "CRL" });
            }
            for der in crls {
                let crl = webpki::BorrowedCertRevocationList::from_der(der)
                    .map_err(|e| AttestError::Crl(format!("{e:?}")))?;
                parsed.push(webpki::CertRevocationList::from(crl));
            }
            if scope == RevocationScope::IssuedByRoot
                && !parsed.iter().any(|c| c.issuer() == anchor.subject.as_ref())
            {
                return Err(AttestError::MissingCollateral {
                    what: "CRL issued by the root",
                });
            }
            Some(scope)
        }
    };
    let refs: Vec<&webpki::CertRevocationList<'_>> = parsed.iter().collect();
    let options = match scope {
        None => None,
        Some(scope) => Some(
            webpki::RevocationOptionsBuilder::new(&refs)
                .map_err(|_| AttestError::MissingCollateral { what: "CRL" })?
                .with_depth(webpki::RevocationCheckDepth::Chain)
                .with_status_policy(match scope {
                    RevocationScope::EveryCertificate => webpki::UnknownStatusPolicy::Deny,
                    RevocationScope::IssuedByRoot => webpki::UnknownStatusPolicy::Allow,
                })
                .with_expiration_policy(webpki::ExpirationPolicy::Enforce)
                .build(),
        ),
    };
    ee.verify_for_usage(
        CHAIN_ALGS,
        &[anchor],
        &inter,
        time,
        webpki::KeyUsage::client_auth(),
        options,
        None,
    )
    .map(|_| ())
    .map_err(map_webpki_error)
}

fn map_webpki_error(e: webpki::Error) -> AttestError {
    match e {
        webpki::Error::CertRevoked => AttestError::Revoked {
            what: "certificate on the CRL",
        },
        webpki::Error::UnknownRevocationStatus => AttestError::MissingCollateral {
            what: "CRL covering a certificate in the path",
        },
        webpki::Error::CrlExpired { .. } => AttestError::CollateralExpired { what: "CRL" },
        webpki::Error::CertExpired { .. } => AttestError::CertificateExpired,
        webpki::Error::CertNotValidYet { .. } => AttestError::CertificateNotYetValid,
        other => AttestError::CertificateChain(format!("{other:?}")),
    }
}

/// Check that `child` (DER) names `issuer` (DER) as its issuer and carries
/// a valid signature by `issuer`'s key, with one of the path algorithms.
/// No validity-period or extension checks: this is for pinned CA
/// certificates, which webpki will not verify as end entities.
pub fn verify_issued_by(child: &[u8], issuer: &[u8]) -> Result<(), AttestError> {
    let c = RawCert::parse(child)?;
    let i = RawCert::parse(issuer)?;
    if c.issuer != i.subject {
        return Err(AttestError::CertificateChain("issuer name mismatch".into()));
    }
    let spki = spki::SubjectPublicKeyInfoRef::try_from(i.spki)
        .map_err(|_| AttestError::CertificateChain("issuer SubjectPublicKeyInfo".into()))?;
    let key_alg = sequence_contents(i.spki_alg)?;
    let sig_alg = sequence_contents(c.signature_alg)?;
    let alg = CHAIN_ALGS
        .iter()
        .find(|a| {
            a.signature_alg_id().as_ref() == sig_alg && a.public_key_alg_id().as_ref() == key_alg
        })
        .ok_or(AttestError::UnsupportedAlgorithm {
            what: "certificate signature algorithm",
            value: 0,
        })?;
    let key = spki
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| AttestError::CertificateChain("issuer key bit string".into()))?;
    alg.verify_signature(key, c.tbs, c.signature)
        .map_err(|_| AttestError::BadSignature {
            what: "certificate signature",
        })
}

/// Borrowed DER pieces of an X.509 certificate.
struct RawCert<'a> {
    tbs: &'a [u8],
    signature_alg: &'a [u8],
    signature: &'a [u8],
    issuer: &'a [u8],
    subject: &'a [u8],
    spki: &'a [u8],
    spki_alg: &'a [u8],
}

const RAW_CERT_TBS_START: usize = 0;
const RAW_CERT_TBS_END: usize = 1;
const RAW_CERT_SIG_ALG_START: usize = 2;
const RAW_CERT_SIG_ALG_END: usize = 3;
const RAW_CERT_SIG_START: usize = 4;
const RAW_CERT_SIG_END: usize = 5;
const RAW_CERT_ISSUER_START: usize = 6;
const RAW_CERT_ISSUER_END: usize = 7;
const RAW_CERT_SUBJECT_START: usize = 8;
const RAW_CERT_SUBJECT_END: usize = 9;
const RAW_CERT_SPKI_START: usize = 10;
const RAW_CERT_SPKI_END: usize = 11;
const RAW_CERT_SPKI_ALG_START: usize = 12;
const RAW_CERT_SPKI_ALG_END: usize = 13;
const RAW_CERT_OUT_LEN: usize = 14;
const RAW_CERT_ERR_OUTER: u64 = 1;
const RAW_CERT_ERR_TRAILING: u64 = 2;
const RAW_CERT_ERR_TBS: u64 = 3;
const RAW_CERT_ERR_SIG_ALG: u64 = 4;
const RAW_CERT_ERR_SIG_VAL: u64 = 5;
const RAW_CERT_ERR_SIG_UNUSED: u64 = 6;
const RAW_CERT_ERR_TBS_FIELD: u64 = 7;
const RAW_CERT_ERR_SPKI: u64 = 8;

impl<'a> RawCert<'a> {
    fn parse(der: &'a [u8]) -> Result<Self, AttestError> {
        let bad = |what: &str| AttestError::CertificateChain(format!("certificate DER: {what}"));
        let mut out = [0u64; RAW_CERT_OUT_LEN];
        match crate::port::vesper::parse_raw_cert(der, &mut out) {
            0 => Ok(RawCert {
                tbs: &der[out[RAW_CERT_TBS_START] as usize..out[RAW_CERT_TBS_END] as usize],
                signature_alg: &der
                    [out[RAW_CERT_SIG_ALG_START] as usize..out[RAW_CERT_SIG_ALG_END] as usize],
                signature: &der[out[RAW_CERT_SIG_START] as usize..out[RAW_CERT_SIG_END] as usize],
                issuer: &der
                    [out[RAW_CERT_ISSUER_START] as usize..out[RAW_CERT_ISSUER_END] as usize],
                subject: &der
                    [out[RAW_CERT_SUBJECT_START] as usize..out[RAW_CERT_SUBJECT_END] as usize],
                spki: &der[out[RAW_CERT_SPKI_START] as usize..out[RAW_CERT_SPKI_END] as usize],
                spki_alg: &der
                    [out[RAW_CERT_SPKI_ALG_START] as usize..out[RAW_CERT_SPKI_ALG_END] as usize],
            }),
            RAW_CERT_ERR_OUTER => Err(bad("outer")),
            RAW_CERT_ERR_TRAILING => Err(bad("trailing bytes")),
            RAW_CERT_ERR_TBS => Err(bad("tbs")),
            RAW_CERT_ERR_SIG_ALG => Err(bad("signatureAlgorithm")),
            RAW_CERT_ERR_SIG_VAL => Err(bad("signatureValue")),
            RAW_CERT_ERR_SIG_UNUSED => Err(bad("signatureValue unused bits")),
            RAW_CERT_ERR_TBS_FIELD => Err(bad("tbs field")),
            RAW_CERT_ERR_SPKI => Err(bad("spki")),
            _ => Err(bad("outer")),
        }
    }
}

/// Benchmark entry point for `examples/abba.rs` (`der_cert` workload).
#[doc(hidden)]
pub fn parse_raw_cert_for_bench(der: &[u8]) -> Result<(usize, usize), AttestError> {
    let parsed = RawCert::parse(der)?;
    Ok((parsed.tbs.len(), parsed.spki_alg.len()))
}

fn contents(t: &[u8]) -> Option<&[u8]> {
    let header = if t[1] < der::LONG_FORM {
        der::SHORT_HEADER
    } else {
        der::SHORT_HEADER + usize::from(t[1] & der::LENGTH_OCTETS_MASK)
    };
    t.get(header..)
}

fn sequence_contents(t: &[u8]) -> Result<&[u8], AttestError> {
    if t.first() != Some(&der::SEQUENCE) {
        return Err(AttestError::CertificateChain("algorithm identifier".into()));
    }
    contents(t).ok_or_else(|| AttestError::CertificateChain("algorithm identifier".into()))
}

/// Verify an ECDSA signature by `cert`'s key (curve `curve`) over `message`;
/// `r_s` is fixed-width `r || s`.
pub fn verify_by_certificate(
    cert: &[u8],
    curve: Curve,
    message: &[u8],
    r_s: &[u8],
    what: &'static str,
) -> Result<(), AttestError> {
    leaf_ec_key(cert, curve)?.verify_fixed(message, r_s, what)
}

fn ec_key_from_spki(
    spki_der: &[u8],
    required_curve: Option<Curve>,
    what_spki: &'static str,
    what_alg: &'static str,
    what_curve: &'static str,
    what_bits: &'static str,
) -> Result<EcPublicKey, AttestError> {
    let spki = spki::SubjectPublicKeyInfoRef::try_from(spki_der)
        .map_err(|_| AttestError::UnexpectedKeyType { what: what_spki })?;
    if spki.algorithm.oid != ID_EC_PUBLIC_KEY {
        return Err(AttestError::UnexpectedKeyType { what: what_alg });
    }
    let curve = match (required_curve, spki.algorithm.parameters_oid()) {
        (Some(Curve::P256), Ok(o)) if o == SECP256R1 => Curve::P256,
        (Some(Curve::P384), Ok(o)) if o == SECP384R1 => Curve::P384,
        (None, Ok(o)) if o == SECP256R1 => Curve::P256,
        (None, Ok(o)) if o == SECP384R1 => Curve::P384,
        _ => return Err(AttestError::UnexpectedKeyType { what: what_curve }),
    };
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or(AttestError::UnexpectedKeyType { what: what_bits })?;
    EcPublicKey::from_sec1(curve, point)
}

fn leaf_ec_key(leaf: &[u8], curve: Curve) -> Result<EcPublicKey, AttestError> {
    let leaf_der = CertificateDer::from(leaf);
    let ee = webpki::EndEntityCert::try_from(&leaf_der)
        .map_err(|e| AttestError::CertificateChain(format!("leaf: {e:?}")))?;
    ec_key_from_spki(
        ee.subject_public_key_info().as_ref(),
        Some(curve),
        "leaf SubjectPublicKeyInfo",
        "leaf key is not id-ecPublicKey",
        "leaf key curve",
        "leaf key bit string",
    )
}

const PEM_PUBKEY_STACK_DER: usize = 256;

/// Decode a PEM `PUBLIC KEY` holding an uncompressed P-256 or P-384 point.
pub fn ec_public_key_from_pem(pem: &str) -> Result<EcPublicKey, AttestError> {
    let pem_bytes = pem.as_bytes();
    let mut stack_der = [0u8; PEM_PUBKEY_STACK_DER];
    let mut heap_der;
    let res;
    let der_slice: &[u8] = {
        let r = crate::port::vesper::decode_pem_public_key(pem_bytes, &mut stack_der);
        if (r & PEM_STATUS_MASK) == PEM_STATUS_TOO_SMALL {
            heap_der = vec![0u8; (pem_bytes.len() * PEM_B64_NUM) / PEM_B64_DEN + PEM_B64_PAD];
            res = crate::port::vesper::decode_pem_public_key(pem_bytes, &mut heap_der);
            &heap_der
        } else {
            res = r;
            &stack_der
        }
    };
    if (res & PEM_STATUS_MASK) != PEM_STATUS_OK {
        return Err(AttestError::UnexpectedKeyType {
            what: "PKIX public key is not a PEM PUBLIC KEY",
        });
    }
    let der_len = (res >> PEM_STATUS_SHIFT) as usize;
    ec_key_from_spki(
        &der_slice[..der_len],
        None,
        "PKIX public key DER",
        "PKIX public key is not an EC key",
        "PKIX public key curve (P-256 or P-384 only)",
        "PKIX public key bit string",
    )
}

/// Benchmark entry point for `examples/abba.rs` (`pem_pubkey` workload).
#[doc(hidden)]
pub fn decode_pem_public_key_for_bench(pem: &str) -> Result<Vec<u8>, AttestError> {
    let pem_bytes = pem.as_bytes();
    let mut stack_der = [0u8; PEM_PUBKEY_STACK_DER];
    let res = crate::port::vesper::decode_pem_public_key(pem_bytes, &mut stack_der);
    if (res & PEM_STATUS_MASK) != PEM_STATUS_OK {
        return Err(AttestError::UnexpectedKeyType {
            what: "PKIX public key is not a PEM PUBLIC KEY",
        });
    }
    let der_len = (res >> PEM_STATUS_SHIFT) as usize;
    Ok(stack_der[..der_len].to_vec())
}

const PEM_STATUS_MASK: u64 = 7;
const PEM_STATUS_SHIFT: u64 = 3;
const PEM_STATUS_OK: u64 = 0;
const PEM_STATUS_EMPTY: u64 = 2;
const PEM_STATUS_TOO_SMALL: u64 = 3;
const PEM_SPAN_STRIDE: usize = 2;
const PEM_INIT_CERT_CAP: usize = 8;
const PEM_B64_NUM: usize = 3;
const PEM_B64_DEN: usize = 4;
const PEM_B64_PAD: usize = 4;

/// Split concatenated PEM certificates (leaf first) into DER blobs.
pub fn pem_certificates(pem: &[u8]) -> Result<Vec<Vec<u8>>, ParseError> {
    let mut der_out = vec![0u8; (pem.len() * PEM_B64_NUM) / PEM_B64_DEN + PEM_B64_PAD];
    let mut spans = vec![0u64; PEM_INIT_CERT_CAP * PEM_SPAN_STRIDE];
    loop {
        let res = crate::port::vesper::decode_pem_certificates(pem, &mut der_out, &mut spans);
        match res & PEM_STATUS_MASK {
            PEM_STATUS_OK => {
                let count = (res >> PEM_STATUS_SHIFT) as usize;
                let mut out = Vec::with_capacity(count);
                for i in 0..count {
                    let s = spans[i * PEM_SPAN_STRIDE] as usize;
                    let e = spans[i * PEM_SPAN_STRIDE + 1] as usize;
                    out.push(der_out[s..e].to_vec());
                }
                return Ok(out);
            }
            PEM_STATUS_EMPTY => return Err(ParseError::Pem("no certificates")),
            PEM_STATUS_TOO_SMALL => {
                spans.resize(spans.len() * 2, 0);
            }
            _ => return Err(ParseError::Pem("malformed PEM certificate")),
        }
    }
}

#[cfg(test)]
mod der_cert_tests {
    use super::*;

    #[test]
    fn raw_cert_parse_handles_valid_and_truncated() {
        let cert = rcgen::generate_simple_self_signed(vec!["vesper.test".to_string()]).unwrap();
        let der = cert.cert.der().as_ref();
        let parsed = RawCert::parse(der).expect("valid self-signed cert");
        assert!(!parsed.tbs.is_empty());
        assert!(!parsed.signature.is_empty());
        assert_eq!(parsed.issuer, parsed.subject);
        assert!(!parsed.spki_alg.is_empty());

        for cut in 0..der.len() {
            assert!(RawCert::parse(&der[..cut]).is_err(), "accepted cut={cut}");
        }
        let mut trailing = der.to_vec();
        trailing.push(0);
        assert!(RawCert::parse(&trailing).is_err());
    }

    #[test]
    fn pem_certificates_matches_rust_reference() {
        use rustls_pki_types::pem::PemObject;
        let c1 = rcgen::generate_simple_self_signed(vec!["a.vesper.test".to_string()]).unwrap();
        let c2 = rcgen::generate_simple_self_signed(vec!["b.vesper.test".to_string()]).unwrap();
        let pem = format!("{}\n{}\n", c1.cert.pem(), c2.cert.pem());
        let rust_certs: Vec<Vec<u8>> = CertificateDer::pem_slice_iter(pem.as_bytes())
            .map(|item| item.unwrap().as_ref().to_vec())
            .collect();
        let vesper_certs = pem_certificates(pem.as_bytes()).unwrap();
        assert_eq!(vesper_certs, rust_certs);
        assert!(pem_certificates(b"").is_err());
        assert!(
            pem_certificates(b"-----BEGIN CERTIFICATE-----\n@@@\n-----END CERTIFICATE-----\n")
                .is_err()
        );
    }
}
