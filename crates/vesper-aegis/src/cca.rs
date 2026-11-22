//! Arm CCA attestation token: a platform COSE_Sign1 token (key 44234) and
//! a realm COSE_Sign1 token (key 44241), following the RMM/CCA token claim
//! keys, in either collection encoding:
//!
//! - tag 399 (RMM 1.0 / `draft-ffm-rats-cca-token-00`), whose values are
//!   the tagged COSE_Sign1 byte strings;
//! - tag 907, a CMW collection (`draft-ffm-rats-cca-token-03`, profiles
//!   `tag:arm.com,2024:…#2.0.0`), whose values are CMW records
//!   `[263, bstr]` (CoAP content format 263, `application/eat+cwt`).
//!
//! Recognised profiles: platform `http://arm.com/CCA-SSD/1.0.0`
//! (pre-standard, as TF-RMM emits it), `tag:arm.com,2023:cca_platform#1.0.0`
//! and `tag:arm.com,2024:cca_platform#2.0.0`; realm: absent (pre-standard),
//! `tag:arm.com,2023:realm#1.0.0` and `tag:arm.com,2024:realm#2.0.0`. Any
//! other profile is a typed error. Software components (2399) and the
//! platform configuration (2401) are parsed for CoRIM appraisal
//! ([`crate::corim`]); other claims the verifier does not use (verification
//! service, attestation-key claims) are ignored; duplicated keys are errors.
//!
//! Platform keys: there is no public Arm CCA root. A platform attestation
//! key (CPAK, formerly IAK) is either certified by a configured X.509 root
//! ([`verify_cca_token`]) or endorsed directly by instance ID, as
//! Veraison's trust anchors do ([`verify_cca_endorsed`]).

use crate::chain::{Curve, EcPublicKey, SEC1_UNCOMPRESSED, TrustRoot, verify_chain};
use crate::claims::{
    AttestationClaims, ENCLAVE_FLAG_DEBUG_DISABLED, GuestRuntimeClaims, RegisterHash,
    SupplyChainEvidence, TeeArch, ct_eq, single_extend,
};
use crate::error::{AttestError, ParseError};
use ring::digest;

pub const CCA_COLLECTION_TAG: u64 = 399;
/// CMW collection tag (`draft-ffm-rats-cca-token-03`).
pub const CCA_CMW_COLLECTION_TAG: u64 = 907;
/// CoAP content format of each CMW record: `application/eat+cwt`.
pub const CCA_CMW_CONTENT_FORMAT: u64 = 263;
/// RMM measurement buffers are 64 bytes regardless of the hash algorithm;
/// measurement claims are accepted at the algorithm's length or at this one.
pub const CCA_MAX_MEASUREMENT_LEN: usize = 64;
pub const CCA_PLATFORM_TOKEN_KEY: i64 = 44234;
pub const CCA_REALM_TOKEN_KEY: i64 = 44241;
/// Largest token accepted (real tokens are a few KiB).
pub const MAX_CCA_TOKEN_LEN: usize = 64 * 1024;

pub mod keys {
    pub const CHALLENGE: i64 = 10;
    pub const PROFILE: i64 = 265;
    pub const REALM_PERSONALIZATION: i64 = 44235;
    pub const REALM_HASH_ALGO: i64 = 44236;
    pub const REALM_PUBLIC_KEY: i64 = 44237;
    pub const REALM_INITIAL_MEASUREMENT: i64 = 44238;
    pub const REALM_EXTENSIBLE_MEASUREMENTS: i64 = 44239;
    pub const REALM_PUBLIC_KEY_HASH_ALGO: i64 = 44240;
    pub const PLATFORM_INSTANCE_ID: i64 = 256;
    pub const PLATFORM_LIFECYCLE: i64 = 2395;
    pub const PLATFORM_IMPLEMENTATION_ID: i64 = 2396;
    pub const PLATFORM_HASH_ALGO: i64 = 2402;
    pub const PLATFORM_SW_COMPONENTS: i64 = 2399;
    pub const PLATFORM_CONFIG: i64 = 2401;
    pub const PLATFORM_MANUFACTURING_CONFIG: i64 = 2403;
    pub const PLATFORM_TBB_ROTPK: i64 = 2405;
    /// TBB RoTPK entry keys.
    pub const ROTPK_NAME: i64 = 1;
    pub const ROTPK_INDEX: i64 = 2;
    pub const ROTPK_HASH: i64 = 3;
    /// Software-component map keys (PSA/CCA token).
    pub const SW_MEASUREMENT_TYPE: i64 = 1;
    pub const SW_MEASUREMENT_VALUE: i64 = 2;
    pub const SW_VERSION: i64 = 4;
    pub const SW_SIGNER_ID: i64 = 5;
    pub const SW_MEASUREMENT_DESCRIPTION: i64 = 6;
}

/// Most software components accepted in one platform token.
pub const MAX_CCA_SW_COMPONENTS: usize = 64;
/// Realm extensible measurement registers (RMM `REM[0..4]`).
pub const CCA_REMS: usize = 4;
/// An uncompressed P-384 SEC1 point: `0x04 || X || Y`, 48 bytes each.
const P384_COORD_LEN: usize = 48;
const P384_SEC1_LEN: usize = 1 + 2 * P384_COORD_LEN;

/// Security lifecycle states `0x3000..=0x30FF` are "secured"; every other
/// state (including `0x5000` non-recoverable debug) counts as debug.
pub const CCA_LIFECYCLE_SECURED: std::ops::RangeInclusive<u64> = 0x3000..=0x30FF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaRealmClaims {
    pub challenge: [u8; 64],
    pub personalization: [u8; 64],
    pub hash_algo: RegisterHash,
    /// Realm Attestation Key exactly as encoded in claim 44237: a CBOR
    /// `COSE_Key` (EC2, P-384), or, as in early RMM releases, a raw
    /// uncompressed SEC1 point. The platform challenge hashes these bytes.
    pub public_key: Vec<u8>,
    pub public_key_hash_algo: RegisterHash,
    pub initial_measurement: Vec<u8>,
    pub extensible_measurements: [Vec<u8>; CCA_REMS],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaPlatformClaims {
    /// Hash of the realm public key under the realm's public-key hash algorithm.
    pub challenge: Vec<u8>,
    pub instance_id: Vec<u8>,
    pub implementation_id: Vec<u8>,
    pub lifecycle: u64,
    pub hash_algo: RegisterHash,
    /// Claim 2399, in token order (empty when the claim is absent).
    pub sw_components: Vec<CcaSwComponent>,
    /// Claim 2401 (platform configuration), when present.
    pub config: Option<Vec<u8>>,
    /// Claim 2403 (2024 profile: platform manufacturing configuration).
    pub manufacturing_config: Option<Vec<u8>>,
    /// Claim 2405 (2024 profile: hashes of the trusted-board-boot root of
    /// trust public keys), in token order.
    pub tbb_rotpks: Vec<CcaTbbRotpk>,
}

/// One entry of the TBB RoTPK claim (2405).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaTbbRotpk {
    /// `"CM"` (chip manufacturer) or `"DM"` (device manufacturer).
    pub name: String,
    pub index: u64,
    pub hash: Vec<u8>,
}

impl CcaTbbRotpk {
    fn encode_cbor(&self, out: &mut Vec<u8>) {
        use keys::*;
        cbor_head(out, 5, 3);
        cbor_int(out, ROTPK_NAME);
        cbor_text(out, &self.name);
        cbor_int(out, ROTPK_INDEX);
        cbor_head(out, 0, self.index);
        cbor_int(out, ROTPK_HASH);
        cbor_bytes(out, &self.hash);
    }
}

/// One entry of the platform software-components claim (2399).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaSwComponent {
    /// Key 1, e.g. `"BL"` or `"RSE_BL1_2"`.
    pub measurement_type: Option<String>,
    /// Key 2: the component digest.
    pub measurement_value: Vec<u8>,
    /// Key 4.
    pub version: Option<String>,
    /// Key 5: hash of the signing authority's key.
    pub signer_id: Vec<u8>,
    /// Key 6: the hash algorithm name, e.g. `"sha-256"`.
    pub measurement_description: Option<String>,
}

impl CcaSwComponent {
    fn encode_cbor(&self, out: &mut Vec<u8>) {
        use keys::*;
        let n = usize::from(self.measurement_type.is_some())
            + 2
            + usize::from(self.version.is_some())
            + usize::from(self.measurement_description.is_some());
        cbor_head(out, 5, n as u64);
        if let Some(t) = &self.measurement_type {
            cbor_int(out, SW_MEASUREMENT_TYPE);
            cbor_text(out, t);
        }
        cbor_int(out, SW_MEASUREMENT_VALUE);
        cbor_bytes(out, &self.measurement_value);
        if let Some(t) = &self.version {
            cbor_int(out, SW_VERSION);
            cbor_text(out, t);
        }
        cbor_int(out, SW_SIGNER_ID);
        cbor_bytes(out, &self.signer_id);
        if let Some(t) = &self.measurement_description {
            cbor_int(out, SW_MEASUREMENT_DESCRIPTION);
            cbor_text(out, t);
        }
    }
}

fn algo_name(h: RegisterHash) -> &'static str {
    match h {
        RegisterHash::Sha256 => "sha-256",
        RegisterHash::Sha384 => "sha-384",
        RegisterHash::Sha512 => "sha-512",
    }
}

fn cbor_head(out: &mut Vec<u8>, major: u8, n: u64) {
    let base = major << 5;
    if n <= 23 {
        out.push(base | n as u8);
    } else if n <= u8::MAX as u64 {
        out.push(base | 24);
        out.push(n as u8);
    } else if n <= u16::MAX as u64 {
        out.push(base | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= u32::MAX as u64 {
        out.push(base | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(base | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

fn cbor_int(out: &mut Vec<u8>, v: i64) {
    if v >= 0 {
        cbor_head(out, 0, v as u64);
    } else {
        cbor_head(out, 1, (-1i128 - i128::from(v)) as u64);
    }
}

fn cbor_bytes(out: &mut Vec<u8>, b: &[u8]) {
    cbor_head(out, 2, b.len() as u64);
    out.extend_from_slice(b);
}

fn cbor_text(out: &mut Vec<u8>, s: &str) {
    cbor_head(out, 3, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// The digest of a measurement: a 64-byte encoding of a shorter digest
/// whose padding is all zero is cut to the digest length, so both
/// encodings of one RIM normalize alike. Other values are returned whole.
fn unpadded_measurement(m: &[u8], algo: RegisterHash) -> &[u8] {
    let n = algo.len();
    if m.len() == CCA_MAX_MEASUREMENT_LEN && n < m.len() && m[n..].iter().all(|&b| b == 0) {
        &m[..n]
    } else {
        m
    }
}

/// Compare a reported measurement with an expected digest. A register in
/// the 64-byte encoding matches when it is the digest followed by zero
/// padding; any nonzero padding byte is a mismatch.
pub(crate) fn measurement_eq(reported: &[u8], expected: &[u8]) -> bool {
    if reported.len() == expected.len() {
        return ct_eq(reported, expected);
    }
    if reported.len() != CCA_MAX_MEASUREMENT_LEN || expected.len() > reported.len() {
        return false;
    }
    let (digest, padding) = reported.split_at(expected.len());
    let zeros = vec![0u8; padding.len()];
    ct_eq(digest, expected) & ct_eq(padding, &zeros)
}

/// The platform token profile (claim 265).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcaPlatformProfile {
    /// `http://arm.com/CCA-SSD/1.0.0` (pre-standard, TF-RMM).
    CcaSsd1,
    /// `tag:arm.com,2023:cca_platform#1.0.0`.
    Platform2023,
    /// `tag:arm.com,2024:cca_platform#2.0.0`.
    Platform2024,
}

/// The realm token profile (claim 265; absent in pre-standard tokens).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcaRealmProfile {
    Unspecified,
    /// `tag:arm.com,2023:realm#1.0.0`.
    Realm2023,
    /// `tag:arm.com,2024:realm#2.0.0`.
    Realm2024,
}

/// How the two tokens are wrapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcaCollection {
    /// Tag 399, byte-string values.
    Tag399,
    /// Tag 907 CMW collection, `[263, bstr]` values.
    Cmw907,
}

/// Algorithm carried in a parsed `COSE_Sign1` protected header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoseAlg {
    None,
    Assigned(i64),
    Private(i64),
    Text,
}

/// Parsed tagged `COSE_Sign1` envelope (RFC 8152 tag 18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoseSign1 {
    pub protected_raw: Vec<u8>,
    pub alg: CoseAlg,
    pub content_type: Option<String>,
    pub(crate) corim_meta: Option<Result<Vec<u8>, ()>>,
    pub payload: Option<Vec<u8>>,
    pub signature: Vec<u8>,
}

impl CoseSign1 {
    /// Decode a tagged `COSE_Sign1` (RFC 8152 tag 18) via the Vesper port.
    pub fn from_tagged_slice(bytes: &[u8]) -> Result<Self, ParseError> {
        crate::port::cose_sign1_parse(bytes, 33, 25)
    }

    /// Encode RFC 8152 §4.4 `Sig_structure` TBS bytes via the Vesper port.
    pub fn tbs_data(&self, aad: &[u8]) -> Vec<u8> {
        let pay = self.payload.as_deref().unwrap_or(&[]);
        crate::port::cose_sig_structure(&self.protected_raw, aad, pay)
    }
}

impl CcaRealmClaims {
    pub fn from_cbor(payload: &[u8]) -> Result<Self, ParseError> {
        use crate::port::{CBOR_REALM_CLAIMS, CborFields, cbor_call, read_realm_claims, vesper};
        let ((), w) = cbor_call(payload, CBOR_REALM_CLAIMS, |n, st, w| {
            vesper::cca_realm_claims(payload, n, st, w)
        })?;
        let mut f = CborFields::new(&w, payload);
        Ok(read_realm_claims(&mut f))
    }

    pub fn to_cbor(&self) -> Vec<u8> {
        use keys::*;
        let mut out = Vec::with_capacity(512);
        cbor_head(&mut out, 5, 8);
        cbor_int(&mut out, PROFILE);
        cbor_text(&mut out, "tag:arm.com,2023:realm#1.0.0");
        cbor_int(&mut out, CHALLENGE);
        cbor_bytes(&mut out, &self.challenge);
        cbor_int(&mut out, REALM_PERSONALIZATION);
        cbor_bytes(&mut out, &self.personalization);
        cbor_int(&mut out, REALM_HASH_ALGO);
        cbor_text(&mut out, algo_name(self.hash_algo));
        cbor_int(&mut out, REALM_PUBLIC_KEY);
        cbor_bytes(&mut out, &self.public_key);
        cbor_int(&mut out, REALM_PUBLIC_KEY_HASH_ALGO);
        cbor_text(&mut out, algo_name(self.public_key_hash_algo));
        cbor_int(&mut out, REALM_INITIAL_MEASUREMENT);
        cbor_bytes(&mut out, &self.initial_measurement);
        cbor_int(&mut out, REALM_EXTENSIBLE_MEASUREMENTS);
        cbor_head(&mut out, 4, self.extensible_measurements.len() as u64);
        for rem in &self.extensible_measurements {
            cbor_bytes(&mut out, rem);
        }
        out
    }
}

impl CcaPlatformClaims {
    pub fn from_cbor(payload: &[u8]) -> Result<Self, ParseError> {
        use crate::port::{
            CBOR_PLATFORM_CLAIMS, CborFields, cbor_call, read_platform_claims, vesper,
        };
        let ((), w) = cbor_call(payload, CBOR_PLATFORM_CLAIMS, |n, st, w| {
            vesper::cca_platform_claims(payload, n, st, w)
        })?;
        let mut f = CborFields::new(&w, payload);
        Ok(read_platform_claims(&mut f))
    }

    pub fn to_cbor(&self) -> Vec<u8> {
        use keys::*;
        let mut out = Vec::with_capacity(512);
        let n = 6
            + usize::from(!self.sw_components.is_empty())
            + usize::from(self.config.is_some())
            + usize::from(self.manufacturing_config.is_some())
            + usize::from(!self.tbb_rotpks.is_empty());
        cbor_head(&mut out, 5, n as u64);
        cbor_int(&mut out, PROFILE);
        cbor_text(&mut out, "tag:arm.com,2023:cca_platform#1.0.0");
        cbor_int(&mut out, CHALLENGE);
        cbor_bytes(&mut out, &self.challenge);
        cbor_int(&mut out, PLATFORM_IMPLEMENTATION_ID);
        cbor_bytes(&mut out, &self.implementation_id);
        cbor_int(&mut out, PLATFORM_INSTANCE_ID);
        cbor_bytes(&mut out, &self.instance_id);
        cbor_int(&mut out, PLATFORM_LIFECYCLE);
        cbor_head(&mut out, 0, self.lifecycle);
        cbor_int(&mut out, PLATFORM_HASH_ALGO);
        cbor_text(&mut out, algo_name(self.hash_algo));
        if !self.sw_components.is_empty() {
            cbor_int(&mut out, PLATFORM_SW_COMPONENTS);
            cbor_head(&mut out, 4, self.sw_components.len() as u64);
            for c in &self.sw_components {
                c.encode_cbor(&mut out);
            }
        }
        if let Some(c) = &self.config {
            cbor_int(&mut out, PLATFORM_CONFIG);
            cbor_bytes(&mut out, c);
        }
        if let Some(c) = &self.manufacturing_config {
            cbor_int(&mut out, PLATFORM_MANUFACTURING_CONFIG);
            cbor_bytes(&mut out, c);
        }
        if !self.tbb_rotpks.is_empty() {
            cbor_int(&mut out, PLATFORM_TBB_ROTPK);
            cbor_head(&mut out, 4, self.tbb_rotpks.len() as u64);
            for r in &self.tbb_rotpks {
                r.encode_cbor(&mut out);
            }
        }
        out
    }
}

/// Encode a CCA token collection from two tagged COSE_Sign1 encodings.
pub fn encode_collection(platform_sign1: &[u8], realm_sign1: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(platform_sign1.len() + realm_sign1.len() + 32);
    cbor_head(&mut out, 6, CCA_COLLECTION_TAG);
    cbor_head(&mut out, 5, 2);
    cbor_int(&mut out, CCA_PLATFORM_TOKEN_KEY);
    cbor_bytes(&mut out, platform_sign1);
    cbor_int(&mut out, CCA_REALM_TOKEN_KEY);
    cbor_bytes(&mut out, realm_sign1);
    out
}

/// The two tokens of a collection, signatures not yet checked.
#[derive(Clone, Debug)]
pub struct CcaToken {
    pub collection: CcaCollection,
    pub platform_profile: CcaPlatformProfile,
    pub realm_profile: CcaRealmProfile,
    pub platform: CoseSign1,
    pub platform_claims: CcaPlatformClaims,
    pub realm: CoseSign1,
    pub realm_claims: CcaRealmClaims,
}

pub(crate) fn sign1_curve(sign1: &CoseSign1, what: &'static str) -> Result<Curve, AttestError> {
    match sign1.alg {
        CoseAlg::Assigned(-7) => Ok(Curve::P256),
        CoseAlg::Assigned(-35) => Ok(Curve::P384),
        CoseAlg::Assigned(a) => Err(AttestError::UnsupportedAlgorithm { what, value: a }),
        _ => Err(AttestError::UnsupportedAlgorithm { what, value: 0 }),
    }
}

pub(crate) fn verify_sign1(
    sign1: &CoseSign1,
    key: &EcPublicKey,
    what: &'static str,
) -> Result<(), AttestError> {
    if sign1_curve(sign1, what)? != key.curve {
        return Err(AttestError::UnexpectedKeyType { what });
    }
    let tbs = sign1.tbs_data(b"");
    key.verify_fixed(&tbs, &sign1.signature, what)
}

impl CcaToken {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        crate::port::cca_token_parse(bytes)
    }
}

/// Decode the RAK claim: a `COSE_Key` (kty EC2, crv P-384, uncompressed
/// x/y) or a raw 97-byte SEC1 point.
pub fn realm_public_key(encoded: &[u8]) -> Result<EcPublicKey, AttestError> {
    let sec1 = crate::port::cose_key_p384(encoded)?;
    EcPublicKey::from_sec1(Curve::P384, &sec1)
}

/// Encode an uncompressed P-384 SEC1 point as a `COSE_Key` (for simulators
/// and tests).
pub fn encode_realm_cose_key(sec1: &[u8]) -> Vec<u8> {
    assert!(
        sec1.len() == P384_SEC1_LEN && sec1[0] == SEC1_UNCOMPRESSED,
        "uncompressed P-384 point"
    );
    let mut out = Vec::with_capacity(112);
    cbor_head(&mut out, 5, 4);
    cbor_int(&mut out, 1);
    cbor_int(&mut out, 2);
    cbor_int(&mut out, -1);
    cbor_int(&mut out, 2);
    cbor_int(&mut out, -2);
    cbor_bytes(&mut out, &sec1[1..=P384_COORD_LEN]);
    cbor_int(&mut out, -3);
    cbor_bytes(&mut out, &sec1[1 + P384_COORD_LEN..]);
    out
}

/// Normalized 48-byte measurement for a CCA RIM:
/// `SHA-384("vesper-aegis/cca-rim/v1\0" || algo || 0 || rim)`.
pub fn cca_rim_measurement(hash_algo: RegisterHash, rim: &[u8]) -> [u8; 48] {
    let mut ctx = digest::Context::new(&digest::SHA384);
    ctx.update(b"vesper-aegis/cca-rim/v1\0");
    ctx.update(algo_name(hash_algo).as_bytes());
    ctx.update(&[0]);
    ctx.update(rim);
    let mut out = [0u8; 48];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

/// Hash of the realm public key, which the platform challenge must equal.
pub fn rak_hash(algo: RegisterHash, rak: &[u8]) -> Vec<u8> {
    digest::digest(algo.algorithm(), rak).as_ref().to_vec()
}

/// A platform attestation key endorsed for one platform instance, as a
/// verifier's trust anchor store holds it (Veraison: instance ID,
/// implementation ID and the CPAK public key).
#[derive(Clone, Debug)]
pub struct CcaEndorsement {
    /// Claim 256, including its leading type byte (`0x01`).
    pub instance_id: Vec<u8>,
    /// Claim 2396; `None` accepts any implementation.
    pub implementation_id: Option<Vec<u8>>,
    pub cpak: EcPublicKey,
}

/// A CCA token whose platform and realm signatures and RAK binding
/// verified. Report data and REMs are not interpreted here.
#[derive(Clone, Debug)]
pub struct VerifiedCcaToken {
    pub token: CcaToken,
}

/// Realm signature by the RAK in the realm claims, and the platform
/// challenge binding that RAK. Shared by every platform-key model.
fn verify_realm_and_binding(token: &CcaToken) -> Result<(), AttestError> {
    let rc = &token.realm_claims;
    let rak = realm_public_key(&rc.public_key)?;
    verify_sign1(&token.realm, &rak, "CCA realm token (RAK)")?;
    if !ct_eq(
        &token.platform_claims.challenge,
        &rak_hash(rc.public_key_hash_algo, &rc.public_key),
    ) {
        return Err(AttestError::BindingMismatch {
            what: "platform challenge does not bind the realm attestation key",
        });
    }
    Ok(())
}

/// Verify a CCA token against an X.509-certified platform key: the CPAK
/// chain (`iak_chain`, leaf first) to `root` at `now_ns`, the platform
/// COSE_Sign1 (ES256/ES384) by it, the realm COSE_Sign1 (ES384) by the RAK,
/// and the platform challenge binding the RAK.
pub fn verify_cca_certified(
    bytes: &[u8],
    iak_chain: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
) -> Result<VerifiedCcaToken, AttestError> {
    let token = CcaToken::parse(bytes)?;
    let (leaf, rest) = iak_chain
        .split_first()
        .ok_or(AttestError::CertificateChain("empty IAK chain".into()))?;
    let iak_curve = sign1_curve(&token.platform, "CCA platform token")?;
    let iak = verify_chain(leaf, rest, root, now_ns, iak_curve)?;
    verify_sign1(&token.platform, &iak, "CCA platform token (IAK)")?;
    verify_realm_and_binding(&token)?;
    Ok(VerifiedCcaToken { token })
}

/// Verify a CCA token against endorsed platform keys: the endorsement whose
/// instance ID equals claim 256 (exactly one must match) supplies the CPAK,
/// its implementation ID (if set) must equal claim 2396, and the platform
/// and realm signatures and the RAK binding must verify.
pub fn verify_cca_endorsed(
    bytes: &[u8],
    endorsements: &[CcaEndorsement],
) -> Result<VerifiedCcaToken, AttestError> {
    let token = CcaToken::parse(bytes)?;
    let pc = &token.platform_claims;
    let mut matching = endorsements
        .iter()
        .filter(|e| ct_eq(&e.instance_id, &pc.instance_id));
    let endorsement = match (matching.next(), matching.next()) {
        (Some(e), None) => e,
        (None, _) => {
            return Err(AttestError::MissingCollateral {
                what: "CCA endorsement for the platform instance ID",
            });
        }
        (Some(_), Some(_)) => {
            return Err(AttestError::Collateral {
                what: "CCA endorsements",
                reason: "more than one endorsement for the instance ID".into(),
            });
        }
    };
    if let Some(impl_id) = &endorsement.implementation_id
        && !ct_eq(impl_id, &pc.implementation_id)
    {
        return Err(AttestError::CollateralMismatch {
            what: "CCA implementation ID vs endorsement",
        });
    }
    verify_sign1(
        &token.platform,
        &endorsement.cpak,
        "CCA platform token (CPAK)",
    )?;
    verify_realm_and_binding(&token)?;
    Ok(VerifiedCcaToken { token })
}

/// Check the Vesper binding of a verified token and normalize it: realm
/// challenge equals the Vesper binding of `runtime`; REM[2]/REM[3] equal
/// one extend with the claimed digests.
pub fn cca_vesper_claims(
    verified: &VerifiedCcaToken,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    let token = &verified.token;
    let rc = &token.realm_claims;
    if !ct_eq(&rc.challenge, &runtime.report_data()) {
        return Err(AttestError::BindingMismatch {
            what: "realm challenge does not match runtime claims",
        });
    }
    if !measurement_eq(
        &rc.extensible_measurements[2],
        &single_extend(rc.hash_algo, 64, &runtime.loom_lock_blake3),
    ) {
        return Err(AttestError::RegisterReplayMismatch { register: "REM[2]" });
    }
    if !measurement_eq(
        &rc.extensible_measurements[3],
        &single_extend(rc.hash_algo, 64, &runtime.olean_spec_mif),
    ) {
        return Err(AttestError::RegisterReplayMismatch { register: "REM[3]" });
    }
    let flags = if CCA_LIFECYCLE_SECURED.contains(&token.platform_claims.lifecycle) {
        ENCLAVE_FLAG_DEBUG_DISABLED
    } else {
        0
    };
    Ok(AttestationClaims::from_runtime(
        TeeArch::ArmCcaRealm,
        0,
        flags,
        cca_rim_measurement(
            rc.hash_algo,
            unpadded_measurement(&rc.initial_measurement, rc.hash_algo),
        ),
        runtime,
        SupplyChainEvidence::RegisterReplay,
    ))
}

/// Verify a CCA token collection end to end and normalize it.
///
/// `iak_chain` is the platform attestation key certificate chain (leaf
/// first) to `root`. Checks: [`verify_cca_certified`] (IAK chain at
/// `now_ns`, platform and realm signatures, RAK binding), then
/// [`cca_vesper_claims`] (realm challenge equals the Vesper binding of
/// `runtime`; REM[2]/REM[3] equal one extend with the claimed digests).
/// Not checked: platform software-component endorsements and reference
/// values (Veraison-style CoRIM).
pub fn verify_cca_token(
    bytes: &[u8],
    iak_chain: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    let verified = verify_cca_certified(bytes, iak_chain, root, now_ns)?;
    cca_vesper_claims(&verified, runtime)
}

/// Encode a tagged COSE_Sign1 with an ES256/ES384 protected header, signing
/// with `sign` (which returns fixed-width `r || s`). Used by simulators and
/// tests to produce tokens; verification never calls it.
pub fn build_sign1(curve: Curve, payload: Vec<u8>, sign: impl FnOnce(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let prot: &[u8] = match curve {
        Curve::P256 => &[0xa1, 0x01, 0x26],
        Curve::P384 => &[0xa1, 0x01, 0x38, 0x22],
    };
    let tbs = crate::port::cose_sig_structure(prot, b"", &payload);
    let sig = sign(&tbs);
    let mut out = Vec::with_capacity(prot.len() + payload.len() + sig.len() + 24);
    cbor_head(&mut out, 6, 18);
    cbor_head(&mut out, 4, 4);
    cbor_bytes(&mut out, prot);
    cbor_head(&mut out, 5, 0);
    cbor_bytes(&mut out, &payload);
    cbor_bytes(&mut out, &sig);
    out
}
