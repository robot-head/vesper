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
use coset::cbor::value::Value;
use coset::{CoseSign1, TaggedCborSerializable, iana};
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
    fn to_value(&self) -> Value {
        use keys::*;
        Value::Map(vec![
            (ROTPK_NAME.into(), Value::Text(self.name.clone())),
            (ROTPK_INDEX.into(), Value::Integer(self.index.into())),
            (ROTPK_HASH.into(), Value::Bytes(self.hash.clone())),
        ])
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
    fn to_value(&self) -> Value {
        use keys::*;
        let mut m = Vec::new();
        if let Some(t) = &self.measurement_type {
            m.push((SW_MEASUREMENT_TYPE.into(), Value::Text(t.clone())));
        }
        m.push((
            SW_MEASUREMENT_VALUE.into(),
            Value::Bytes(self.measurement_value.clone()),
        ));
        if let Some(t) = &self.version {
            m.push((SW_VERSION.into(), Value::Text(t.clone())));
        }
        m.push((SW_SIGNER_ID.into(), Value::Bytes(self.signer_id.clone())));
        if let Some(t) = &self.measurement_description {
            m.push((SW_MEASUREMENT_DESCRIPTION.into(), Value::Text(t.clone())));
        }
        Value::Map(m)
    }
}

fn algo_name(h: RegisterHash) -> &'static str {
    match h {
        RegisterHash::Sha256 => "sha-256",
        RegisterHash::Sha384 => "sha-384",
        RegisterHash::Sha512 => "sha-512",
    }
}

fn cbor_err(field: &'static str, reason: &'static str) -> ParseError {
    ParseError::Cbor { field, reason }
}

fn bytes<'v>(v: &'v Value, field: &'static str) -> Result<&'v [u8], ParseError> {
    v.as_bytes()
        .map(Vec::as_slice)
        .ok_or_else(|| cbor_err(field, "expected byte string"))
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

fn platform_profile(payload: &[u8]) -> Result<CcaPlatformProfile, ParseError> {
    use crate::port::{CBOR_PLATFORM_CLAIMS, cbor_call, vesper};
    let (p, _) = cbor_call(payload, CBOR_PLATFORM_CLAIMS, |n, st, _| {
        vesper::cca_platform_profile(payload, n, st)
    })?;
    Ok(match p {
        0 => CcaPlatformProfile::CcaSsd1,
        1 => CcaPlatformProfile::Platform2023,
        2 => CcaPlatformProfile::Platform2024,
        p => unreachable!("platform profile {p} from the Vesper port"),
    })
}

fn realm_profile(payload: &[u8]) -> Result<CcaRealmProfile, ParseError> {
    use crate::port::{CBOR_REALM_CLAIMS, cbor_call, vesper};
    let (p, _) = cbor_call(payload, CBOR_REALM_CLAIMS, |n, st, _| {
        vesper::cca_realm_profile(payload, n, st)
    })?;
    Ok(match p {
        0 => CcaRealmProfile::Unspecified,
        1 => CcaRealmProfile::Realm2023,
        2 => CcaRealmProfile::Realm2024,
        p => unreachable!("realm profile {p} from the Vesper port"),
    })
}

/// An integer-keyed claims map with duplicate keys rejected.
struct ClaimMap<'v>(Vec<(i64, &'v Value)>);

impl<'v> ClaimMap<'v> {
    fn new(v: &'v Value, what: &'static str) -> Result<Self, ParseError> {
        let entries = v.as_map().ok_or_else(|| cbor_err(what, "expected map"))?;
        let mut out: Vec<(i64, &'v Value)> = Vec::with_capacity(entries.len());
        for (k, val) in entries {
            let key = k
                .as_integer()
                .and_then(|i| i64::try_from(i).ok())
                .ok_or_else(|| cbor_err(what, "non-integer claim key"))?;
            if out.iter().any(|(seen, _)| *seen == key) {
                return Err(cbor_err(what, "duplicate claim key"));
            }
            out.push((key, val));
        }
        Ok(ClaimMap(out))
    }

    fn get(&self, key: i64, field: &'static str) -> Result<&'v Value, ParseError> {
        self.0
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .ok_or_else(|| cbor_err(field, "missing claim"))
    }
}

pub(crate) fn decode_value(bytes: &[u8], what: &'static str) -> Result<Value, ParseError> {
    let (v, used) =
        crate::port::cbor_value(bytes).ok_or_else(|| cbor_err(what, "malformed CBOR"))?;
    if used != bytes.len() {
        return Err(cbor_err(what, "trailing bytes"));
    }
    Ok(v)
}

fn encode_value(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    coset::cbor::ser::into_writer(v, &mut out).expect("CBOR encoding into Vec cannot fail");
    out
}

impl CcaRealmClaims {
    pub fn from_cbor(payload: &[u8]) -> Result<Self, ParseError> {
        use crate::port::{CBOR_REALM_CLAIMS, CborFields, cbor_call, vesper};
        let ((), w) = cbor_call(payload, CBOR_REALM_CLAIMS, |n, st, w| {
            vesper::cca_realm_claims(payload, n, st, w)
        })?;
        let mut f = CborFields::new(&w, payload);
        let hash_algo = f.hash();
        let initial_measurement = f.bytes();
        let extensible_measurements = std::array::from_fn(|_| f.bytes());
        Ok(CcaRealmClaims {
            challenge: f.array(),
            personalization: f.array(),
            hash_algo,
            public_key: f.bytes(),
            public_key_hash_algo: f.hash(),
            initial_measurement,
            extensible_measurements,
        })
    }

    pub fn to_cbor(&self) -> Vec<u8> {
        use keys::*;
        let b = |x: &[u8]| Value::Bytes(x.to_vec());
        encode_value(&Value::Map(vec![
            (
                PROFILE.into(),
                Value::Text("tag:arm.com,2023:realm#1.0.0".into()),
            ),
            (CHALLENGE.into(), b(&self.challenge)),
            (REALM_PERSONALIZATION.into(), b(&self.personalization)),
            (
                REALM_HASH_ALGO.into(),
                Value::Text(algo_name(self.hash_algo).into()),
            ),
            (REALM_PUBLIC_KEY.into(), b(&self.public_key)),
            (
                REALM_PUBLIC_KEY_HASH_ALGO.into(),
                Value::Text(algo_name(self.public_key_hash_algo).into()),
            ),
            (
                REALM_INITIAL_MEASUREMENT.into(),
                b(&self.initial_measurement),
            ),
            (
                REALM_EXTENSIBLE_MEASUREMENTS.into(),
                Value::Array(self.extensible_measurements.iter().map(|r| b(r)).collect()),
            ),
        ]))
    }
}

impl CcaPlatformClaims {
    pub fn from_cbor(payload: &[u8]) -> Result<Self, ParseError> {
        use crate::port::{CBOR_PLATFORM_CLAIMS, CborFields, cbor_call, vesper};
        let ((), w) = cbor_call(payload, CBOR_PLATFORM_CLAIMS, |n, st, w| {
            vesper::cca_platform_claims(payload, n, st, w)
        })?;
        let mut f = CborFields::new(&w, payload);
        let lifecycle = f.word();
        let sw_components = (0..f.word())
            .map(|_| CcaSwComponent {
                measurement_type: f.opt_text(),
                measurement_value: f.bytes(),
                version: f.opt_text(),
                signer_id: f.bytes(),
                measurement_description: f.opt_text(),
            })
            .collect();
        let config = f.opt_bytes();
        let manufacturing_config = f.opt_bytes();
        let tbb_rotpks = (0..f.word())
            .map(|_| CcaTbbRotpk {
                name: f.text(),
                index: f.word(),
                hash: f.bytes(),
            })
            .collect();
        Ok(CcaPlatformClaims {
            sw_components,
            config,
            manufacturing_config,
            tbb_rotpks,
            challenge: f.bytes(),
            instance_id: f.bytes(),
            implementation_id: f.bytes(),
            lifecycle,
            hash_algo: f.hash(),
        })
    }

    pub fn to_cbor(&self) -> Vec<u8> {
        use keys::*;
        let b = |x: &[u8]| Value::Bytes(x.to_vec());
        let mut m = vec![
            (
                PROFILE.into(),
                Value::Text("tag:arm.com,2023:cca_platform#1.0.0".into()),
            ),
            (CHALLENGE.into(), b(&self.challenge)),
            (
                PLATFORM_IMPLEMENTATION_ID.into(),
                b(&self.implementation_id),
            ),
            (PLATFORM_INSTANCE_ID.into(), b(&self.instance_id)),
            (
                PLATFORM_LIFECYCLE.into(),
                Value::Integer(self.lifecycle.into()),
            ),
            (
                PLATFORM_HASH_ALGO.into(),
                Value::Text(algo_name(self.hash_algo).into()),
            ),
        ];
        if !self.sw_components.is_empty() {
            m.push((
                PLATFORM_SW_COMPONENTS.into(),
                Value::Array(self.sw_components.iter().map(|c| c.to_value()).collect()),
            ));
        }
        if let Some(c) = &self.config {
            m.push((PLATFORM_CONFIG.into(), b(c)));
        }
        if let Some(c) = &self.manufacturing_config {
            m.push((PLATFORM_MANUFACTURING_CONFIG.into(), b(c)));
        }
        if !self.tbb_rotpks.is_empty() {
            m.push((
                PLATFORM_TBB_ROTPK.into(),
                Value::Array(self.tbb_rotpks.iter().map(|r| r.to_value()).collect()),
            ));
        }
        encode_value(&Value::Map(m))
    }
}

/// Encode a CCA token collection from two tagged COSE_Sign1 encodings.
pub fn encode_collection(platform_sign1: &[u8], realm_sign1: &[u8]) -> Vec<u8> {
    encode_value(&Value::Tag(
        CCA_COLLECTION_TAG,
        Box::new(Value::Map(vec![
            (
                CCA_PLATFORM_TOKEN_KEY.into(),
                Value::Bytes(platform_sign1.to_vec()),
            ),
            (
                CCA_REALM_TOKEN_KEY.into(),
                Value::Bytes(realm_sign1.to_vec()),
            ),
        ])),
    ))
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
    use coset::{Algorithm, RegisteredLabelWithPrivate};
    match &sign1.protected.header.alg {
        Some(Algorithm::Assigned(iana::Algorithm::ES256)) => Ok(Curve::P256),
        Some(Algorithm::Assigned(iana::Algorithm::ES384)) => Ok(Curve::P384),
        Some(RegisteredLabelWithPrivate::Assigned(a)) => Err(AttestError::UnsupportedAlgorithm {
            what,
            value: *a as i64,
        }),
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
    sign1.verify_signature(b"", |sig, data| key.verify_fixed(data, sig, what))
}

impl CcaToken {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() > MAX_CCA_TOKEN_LEN {
            return Err(ParseError::OverBudget {
                format: crate::error::Format::CcaToken,
                field: "token",
                limit: MAX_CCA_TOKEN_LEN,
                value: bytes.len() as u64,
            });
        }
        let v = decode_value(bytes, "token collection")?;
        let (collection, inner) = match v {
            Value::Tag(CCA_COLLECTION_TAG, inner) => (CcaCollection::Tag399, *inner),
            Value::Tag(CCA_CMW_COLLECTION_TAG, inner) => (CcaCollection::Cmw907, *inner),
            Value::Tag(..) => return Err(cbor_err("token collection", "wrong tag")),
            _ => return Err(cbor_err("token collection", "missing tag 399 or 907")),
        };
        let m = ClaimMap::new(&inner, "token collection")?;
        if m.0.len() != 2 {
            return Err(cbor_err("token collection", "expected exactly two tokens"));
        }
        let member = |key, field| -> Result<&[u8], ParseError> {
            let v = m.get(key, field)?;
            match collection {
                CcaCollection::Tag399 => bytes_of(v, field),
                CcaCollection::Cmw907 => cmw_record(v, field),
            }
        };
        let p = member(CCA_PLATFORM_TOKEN_KEY, "platform token")?;
        let r = member(CCA_REALM_TOKEN_KEY, "realm token")?;
        let platform = CoseSign1::from_tagged_slice(p)
            .map_err(|_| cbor_err("platform token", "malformed COSE_Sign1"))?;
        let realm = CoseSign1::from_tagged_slice(r)
            .map_err(|_| cbor_err("realm token", "malformed COSE_Sign1"))?;
        let platform_payload = platform
            .payload
            .as_deref()
            .ok_or_else(|| cbor_err("platform token", "detached payload"))?;
        let realm_payload = realm
            .payload
            .as_deref()
            .ok_or_else(|| cbor_err("realm token", "detached payload"))?;
        let platform_claims = CcaPlatformClaims::from_cbor(platform_payload)?;
        let realm_claims = CcaRealmClaims::from_cbor(realm_payload)?;
        Ok(CcaToken {
            collection,
            platform_profile: platform_profile(platform_payload)?,
            realm_profile: realm_profile(realm_payload)?,
            platform,
            platform_claims,
            realm,
            realm_claims,
        })
    }
}

fn bytes_of<'v>(v: &'v Value, field: &'static str) -> Result<&'v [u8], ParseError> {
    bytes(v, field)
}

/// A CMW record `[263, bstr]`.
fn cmw_record<'v>(v: &'v Value, field: &'static str) -> Result<&'v [u8], ParseError> {
    let a = v
        .as_array()
        .ok_or_else(|| cbor_err(field, "CMW record is not an array"))?;
    match a.as_slice() {
        [cf, b] => {
            let cf = cf
                .as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .ok_or_else(|| cbor_err(field, "CMW content format is not an integer"))?;
            if cf != CCA_CMW_CONTENT_FORMAT {
                return Err(cbor_err(field, "CMW content format is not 263"));
            }
            bytes(b, field)
        }
        _ => Err(cbor_err(field, "CMW record is not [type, value]")),
    }
}

/// Decode the RAK claim: a `COSE_Key` (kty EC2, crv P-384, uncompressed
/// x/y) or a raw 97-byte SEC1 point.
pub fn realm_public_key(encoded: &[u8]) -> Result<EcPublicKey, AttestError> {
    use coset::{CborSerializable, CoseKey, KeyType, Label};
    if encoded.len() == P384_SEC1_LEN && encoded[0] == SEC1_UNCOMPRESSED {
        return EcPublicKey::from_sec1(Curve::P384, encoded);
    }
    let bad = |what| AttestError::UnexpectedKeyType { what };
    let key = CoseKey::from_slice(encoded).map_err(|_| bad("RAK is neither COSE_Key nor SEC1"))?;
    if key.kty != KeyType::Assigned(iana::KeyType::EC2) {
        return Err(bad("RAK COSE_Key kty is not EC2"));
    }
    let param = |label: iana::Ec2KeyParameter| {
        let mut found = key
            .params
            .iter()
            .filter(|(l, _)| *l == Label::Int(label as i64));
        match (found.next(), found.next()) {
            (Some((_, v)), None) => Some(v),
            _ => None,
        }
    };
    let crv = param(iana::Ec2KeyParameter::Crv)
        .and_then(Value::as_integer)
        .and_then(|i| i64::try_from(i).ok());
    if crv != Some(iana::EllipticCurve::P_384 as i64) {
        return Err(bad("RAK COSE_Key curve is not P-384"));
    }
    let coord = |label| {
        param(label)
            .and_then(Value::as_bytes)
            .filter(|b| b.len() == P384_COORD_LEN)
            .ok_or(bad("RAK COSE_Key coordinate"))
    };
    let mut sec1 = Vec::with_capacity(P384_SEC1_LEN);
    sec1.push(SEC1_UNCOMPRESSED);
    sec1.extend_from_slice(coord(iana::Ec2KeyParameter::X)?);
    sec1.extend_from_slice(coord(iana::Ec2KeyParameter::Y)?);
    EcPublicKey::from_sec1(Curve::P384, &sec1)
}

/// Encode an uncompressed P-384 SEC1 point as a `COSE_Key` (for simulators
/// and tests).
pub fn encode_realm_cose_key(sec1: &[u8]) -> Vec<u8> {
    use coset::CborSerializable;
    assert!(
        sec1.len() == P384_SEC1_LEN && sec1[0] == SEC1_UNCOMPRESSED,
        "uncompressed P-384 point"
    );
    coset::CoseKeyBuilder::new_ec2_pub_key(
        iana::EllipticCurve::P_384,
        sec1[1..=P384_COORD_LEN].to_vec(),
        sec1[1 + P384_COORD_LEN..].to_vec(),
    )
    .build()
    .to_vec()
    .expect("COSE_Key encoding into Vec cannot fail")
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
    let alg = match curve {
        Curve::P256 => iana::Algorithm::ES256,
        Curve::P384 => iana::Algorithm::ES384,
    };
    let protected = coset::HeaderBuilder::new().algorithm(alg).build();
    let mut sign = Some(sign);
    coset::CoseSign1Builder::new()
        .protected(protected)
        .payload(payload)
        .create_signature(b"", |data| (sign.take().expect("called once"))(data))
        .build()
        .to_tagged_vec()
        .expect("COSE_Sign1 encoding into Vec cannot fail")
}
