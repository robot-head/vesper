//! Arm CCA attestation token: a CBOR collection (tag 399) holding a
//! platform COSE_Sign1 token (key 44234) and a realm COSE_Sign1 token (key
//! 44241), following the RMM/CCA token claim keys.

use crate::chain::{Curve, EcPublicKey, TrustRoot, verify_chain};
use crate::claims::{
    AttestationClaims, ENCLAVE_FLAG_DEBUG_DISABLED, GuestRuntimeClaims, RegisterHash,
    SupplyChainEvidence, TeeArch, ct_eq, single_extend,
};
use crate::error::{AttestError, ParseError};
use coset::cbor::value::Value;
use coset::{CoseSign1, TaggedCborSerializable, iana};
use ring::digest;

pub const CCA_COLLECTION_TAG: u64 = 399;
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
}

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
    pub extensible_measurements: [Vec<u8>; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaPlatformClaims {
    /// Hash of the realm public key under the realm's public-key hash algorithm.
    pub challenge: Vec<u8>,
    pub instance_id: Vec<u8>,
    pub implementation_id: Vec<u8>,
    pub lifecycle: u64,
    pub hash_algo: RegisterHash,
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

fn parse_algo(v: &Value, field: &'static str) -> Result<RegisterHash, ParseError> {
    match v.as_text() {
        Some("sha-256") => Ok(RegisterHash::Sha256),
        Some("sha-384") => Ok(RegisterHash::Sha384),
        Some("sha-512") => Ok(RegisterHash::Sha512),
        Some(_) => Err(cbor_err(field, "unsupported hash algorithm")),
        None => Err(cbor_err(field, "expected text")),
    }
}

fn bytes<'v>(v: &'v Value, field: &'static str) -> Result<&'v [u8], ParseError> {
    v.as_bytes()
        .map(Vec::as_slice)
        .ok_or_else(|| cbor_err(field, "expected byte string"))
}

fn fixed<const N: usize>(v: &Value, field: &'static str) -> Result<[u8; N], ParseError> {
    bytes(v, field)?
        .try_into()
        .map_err(|_| cbor_err(field, "wrong byte-string length"))
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

fn decode_value(bytes: &[u8], what: &'static str) -> Result<Value, ParseError> {
    let mut rest = bytes;
    let v: Value =
        coset::cbor::de::from_reader(&mut rest).map_err(|_| cbor_err(what, "malformed CBOR"))?;
    if !rest.is_empty() {
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
        use keys::*;
        let v = decode_value(payload, "realm claims")?;
        let m = ClaimMap::new(&v, "realm claims")?;
        let hash_algo = parse_algo(
            m.get(REALM_HASH_ALGO, "realm hash algo")?,
            "realm hash algo",
        )?;
        let initial_measurement = bytes(m.get(REALM_INITIAL_MEASUREMENT, "RIM")?, "RIM")?.to_vec();
        if initial_measurement.len() != hash_algo.len() {
            return Err(cbor_err("RIM", "length does not match hash algorithm"));
        }
        let rems = m
            .get(REALM_EXTENSIBLE_MEASUREMENTS, "REMs")?
            .as_array()
            .ok_or_else(|| cbor_err("REMs", "expected array"))?;
        if rems.len() != 4 {
            return Err(cbor_err("REMs", "expected four registers"));
        }
        let mut regs: [Vec<u8>; 4] = Default::default();
        for (dst, src) in regs.iter_mut().zip(rems) {
            let b = bytes(src, "REM")?;
            if b.len() != hash_algo.len() {
                return Err(cbor_err("REM", "length does not match hash algorithm"));
            }
            *dst = b.to_vec();
        }
        Ok(CcaRealmClaims {
            challenge: fixed(m.get(CHALLENGE, "realm challenge")?, "realm challenge")?,
            personalization: fixed(
                m.get(REALM_PERSONALIZATION, "realm personalization")?,
                "realm personalization",
            )?,
            hash_algo,
            public_key: bytes(m.get(REALM_PUBLIC_KEY, "RAK")?, "RAK")?.to_vec(),
            public_key_hash_algo: parse_algo(
                m.get(REALM_PUBLIC_KEY_HASH_ALGO, "RAK hash algo")?,
                "RAK hash algo",
            )?,
            initial_measurement,
            extensible_measurements: regs,
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
        use keys::*;
        let v = decode_value(payload, "platform claims")?;
        let m = ClaimMap::new(&v, "platform claims")?;
        let lifecycle = m
            .get(PLATFORM_LIFECYCLE, "lifecycle")?
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .ok_or_else(|| cbor_err("lifecycle", "expected unsigned integer"))?;
        Ok(CcaPlatformClaims {
            challenge: bytes(
                m.get(CHALLENGE, "platform challenge")?,
                "platform challenge",
            )?
            .to_vec(),
            instance_id: bytes(m.get(PLATFORM_INSTANCE_ID, "instance id")?, "instance id")?
                .to_vec(),
            implementation_id: bytes(
                m.get(PLATFORM_IMPLEMENTATION_ID, "implementation id")?,
                "implementation id",
            )?
            .to_vec(),
            lifecycle,
            hash_algo: parse_algo(
                m.get(PLATFORM_HASH_ALGO, "platform hash algo")?,
                "platform hash algo",
            )?,
        })
    }

    pub fn to_cbor(&self) -> Vec<u8> {
        use keys::*;
        let b = |x: &[u8]| Value::Bytes(x.to_vec());
        encode_value(&Value::Map(vec![
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
        ]))
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
    pub platform: CoseSign1,
    pub platform_claims: CcaPlatformClaims,
    pub realm: CoseSign1,
    pub realm_claims: CcaRealmClaims,
}

fn sign1_curve(sign1: &CoseSign1, what: &'static str) -> Result<Curve, AttestError> {
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

fn verify_sign1(
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
        let inner = match v {
            Value::Tag(CCA_COLLECTION_TAG, inner) => *inner,
            Value::Tag(..) => return Err(cbor_err("token collection", "wrong tag")),
            _ => return Err(cbor_err("token collection", "missing tag 399")),
        };
        let m = ClaimMap::new(&inner, "token collection")?;
        if m.0.len() != 2 {
            return Err(cbor_err("token collection", "expected exactly two tokens"));
        }
        let p = bytes_of(
            m.get(CCA_PLATFORM_TOKEN_KEY, "platform token")?,
            "platform token",
        )?;
        let r = bytes_of(m.get(CCA_REALM_TOKEN_KEY, "realm token")?, "realm token")?;
        let platform = CoseSign1::from_tagged_slice(p)
            .map_err(|_| cbor_err("platform token", "malformed COSE_Sign1"))?;
        let realm = CoseSign1::from_tagged_slice(r)
            .map_err(|_| cbor_err("realm token", "malformed COSE_Sign1"))?;
        let platform_claims = CcaPlatformClaims::from_cbor(
            platform
                .payload
                .as_deref()
                .ok_or_else(|| cbor_err("platform token", "detached payload"))?,
        )?;
        let realm_claims = CcaRealmClaims::from_cbor(
            realm
                .payload
                .as_deref()
                .ok_or_else(|| cbor_err("realm token", "detached payload"))?,
        )?;
        Ok(CcaToken {
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

/// Decode the RAK claim: a `COSE_Key` (kty EC2, crv P-384, uncompressed
/// x/y) or a raw 97-byte SEC1 point.
pub fn realm_public_key(encoded: &[u8]) -> Result<EcPublicKey, AttestError> {
    use coset::{CborSerializable, CoseKey, KeyType, Label};
    if encoded.len() == 97 && encoded[0] == 4 {
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
            .filter(|b| b.len() == 48)
            .ok_or(bad("RAK COSE_Key coordinate"))
    };
    let mut sec1 = Vec::with_capacity(97);
    sec1.push(4);
    sec1.extend_from_slice(coord(iana::Ec2KeyParameter::X)?);
    sec1.extend_from_slice(coord(iana::Ec2KeyParameter::Y)?);
    EcPublicKey::from_sec1(Curve::P384, &sec1)
}

/// Encode an uncompressed P-384 SEC1 point as a `COSE_Key` (for simulators
/// and tests).
pub fn encode_realm_cose_key(sec1: &[u8]) -> Vec<u8> {
    use coset::CborSerializable;
    assert!(sec1.len() == 97 && sec1[0] == 4, "uncompressed P-384 point");
    coset::CoseKeyBuilder::new_ec2_pub_key(
        iana::EllipticCurve::P_384,
        sec1[1..49].to_vec(),
        sec1[49..].to_vec(),
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

/// Verify a CCA token collection end to end and normalize it.
///
/// `iak_chain` is the platform attestation key certificate chain (leaf
/// first) to `root`. Checks: IAK chain at `now_ns`; platform COSE_Sign1
/// (ES256/ES384) by the IAK; realm COSE_Sign1 (ES384) by the RAK in the
/// realm claims; platform challenge equals the RAK hash; realm challenge
/// equals the Vesper binding of `runtime`; REM[2]/REM[3] equal one extend
/// with the claimed digests. Not checked: platform software-component
/// endorsements and reference values (Veraison-style CoRIM).
pub fn verify_cca_token(
    bytes: &[u8],
    iak_chain: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    let token = CcaToken::parse(bytes)?;
    let (leaf, rest) = iak_chain
        .split_first()
        .ok_or(AttestError::CertificateChain("empty IAK chain".into()))?;
    let iak_curve = sign1_curve(&token.platform, "CCA platform token")?;
    let iak = verify_chain(leaf, rest, root, now_ns, iak_curve)?;
    verify_sign1(&token.platform, &iak, "CCA platform token (IAK)")?;

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
    if !ct_eq(&rc.challenge, &runtime.report_data()) {
        return Err(AttestError::BindingMismatch {
            what: "realm challenge does not match runtime claims",
        });
    }
    if !ct_eq(
        &rc.extensible_measurements[2],
        &single_extend(rc.hash_algo, 64, &runtime.loom_lock_blake3),
    ) {
        return Err(AttestError::RegisterReplayMismatch { register: "REM[2]" });
    }
    if !ct_eq(
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
        cca_rim_measurement(rc.hash_algo, &rc.initial_measurement),
        runtime,
        SupplyChainEvidence::RegisterReplay,
    ))
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
