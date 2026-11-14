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

impl<'a> RawCert<'a> {
    fn parse(der: &'a [u8]) -> Result<Self, AttestError> {
        let bad = |what: &str| AttestError::CertificateChain(format!("certificate DER: {what}"));
        let (outer, rest) = tlv(der).ok_or_else(|| bad("outer"))?;
        if !rest.is_empty() {
            return Err(bad("trailing bytes"));
        }
        let body = contents(outer).ok_or_else(|| bad("outer"))?;
        let (tbs, body) = tlv(body).ok_or_else(|| bad("tbs"))?;
        let (signature_alg, body) = tlv(body).ok_or_else(|| bad("signatureAlgorithm"))?;
        let (sig, body) = tlv(body).ok_or_else(|| bad("signatureValue"))?;
        if !body.is_empty() || sig[0] != 0x03 {
            return Err(bad("signatureValue"));
        }
        let sig = contents(sig).ok_or_else(|| bad("signatureValue"))?;
        let (&unused, signature) = sig.split_first().ok_or_else(|| bad("signatureValue"))?;
        if unused != 0 {
            return Err(bad("signatureValue unused bits"));
        }
        let mut t = contents(tbs).ok_or_else(|| bad("tbs"))?;
        let mut fields = Vec::with_capacity(8);
        while !t.is_empty() {
            let (f, rest) = tlv(t).ok_or_else(|| bad("tbs field"))?;
            fields.push(f);
            t = rest;
        }
        // [0] version present (v3) shifts the fixed fields by one.
        let o = usize::from(fields.first().is_some_and(|f| f[0] == 0xa0));
        let field = |i: usize| fields.get(o + i).copied().ok_or_else(|| bad("tbs field"));
        let spki = field(5)?;
        let (spki_alg, _) =
            tlv(contents(spki).ok_or_else(|| bad("spki"))?).ok_or_else(|| bad("spki"))?;
        Ok(RawCert {
            tbs,
            signature_alg,
            signature,
            issuer: field(2)?,
            subject: field(4)?,
            spki,
            spki_alg,
        })
    }
}

/// Split one DER TLV (definite length, single-byte tag) off `b`.
fn tlv(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let first_len = *b.get(1)?;
    let (len, header) = if first_len < 0x80 {
        (usize::from(first_len), 2)
    } else {
        let n = usize::from(first_len & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for &x in b.get(2..2 + n)? {
            len = (len << 8) | usize::from(x);
        }
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some(b.split_at(end))
}

fn contents(t: &[u8]) -> Option<&[u8]> {
    let header = if t[1] < 0x80 {
        2
    } else {
        2 + usize::from(t[1] & 0x7f)
    };
    t.get(header..)
}

fn sequence_contents(t: &[u8]) -> Result<&[u8], AttestError> {
    if t.first() != Some(&0x30) {
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

fn leaf_ec_key(leaf: &[u8], curve: Curve) -> Result<EcPublicKey, AttestError> {
    let leaf_der = CertificateDer::from(leaf);
    let ee = webpki::EndEntityCert::try_from(&leaf_der)
        .map_err(|e| AttestError::CertificateChain(format!("leaf: {e:?}")))?;
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
