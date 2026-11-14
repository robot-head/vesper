//! CoRIM/CoMID (`draft-ietf-rats-corim`) parsing, signed-CoRIM
//! verification, and reference-value and endorsement appraisal of Arm CCA
//! tokens under the Arm CCA endorsement profiles
//! `tag:arm.com,2025:cca_platform#1.0.0` and `tag:arm.com,2025:cca_realm#1.0.0`
//! (`draft-ydb-rats-cca-endorsements`, as implemented by Veraison's
//! `corim/profiles/cca` and `services/scheme/arm-cca`).
//!
//! What is parsed: unsigned CoRIMs (tag 501) and signed CoRIMs (COSE_Sign1,
//! tag 18, content type `application/rim+cbor`, with `corim-meta`); the
//! CoRIM id, profile, validity, entities and CoMID tags (tag 506); in each
//! CoMID the tag identity, reference-value triples (key 0) and
//! attestation-key triples (key 3). Other triples are counted, not
//! interpreted (and the CCA appraisal does not use them). Inside a
//! measurement, the version, digests, raw value (tag 560 bytes or tag 563
//! masked bytes), name and cryptographic keys are parsed; any other
//! measurement-value key is recorded and makes the CCA profile validation
//! fail, so a constraint this verifier cannot evaluate is never ignored.
//!
//! The CCA appraisal ([`appraise_cca`], [`verify_cca_with_corim`]):
//!
//! - **Platform**: the CoMIDs whose class ID equals the token's
//!   implementation ID supply reference values. Every software component in
//!   the token must match a `cca.software-component` reference: equal
//!   digest (and algorithm, when the token names one), equal signer ID
//!   (unless [`CcaAppraisalOptions::compare_signer_id`] is off, as in
//!   Veraison), and equal name and version where the reference sets them. A
//!   `cca.platform-config` / `cca.platform-manufacturing-config`
//!   reference requires the token's claim to equal its value exactly, and
//!   `cca.rotpk.{CM,DM}.n` references require every TBB RoTPK the token
//!   reports to be listed, and the token to report them.
//! - **Platform key**: attestation-key triples (class ID = implementation
//!   ID, instance = UEID instance ID, one PKIX public key) become
//!   [`CcaEndorsement`]s; the token's platform signature must verify under
//!   the one whose instance ID equals the token's.
//! - **Realm**: the CoMIDs whose class ID equals the token's RIM supply
//!   reference values: `cca.rim` must equal the RIM, each `cca.remN` that is
//!   present must equal REM[N], and `cca.rpv`, when present, must equal the
//!   personalization value. With several references for one RIM, one must
//!   match in full.
//!
//! Masks on raw values are validated (same length as the value) but never
//! loosen the comparison: Veraison compares the raw bytes, and a published
//! all-zero mask would otherwise match every configuration.
//!
//! Every failure is a typed error; malformed, duplicated, over-long or
//! over-budget input never panics.

use crate::cca::{
    CcaEndorsement, CcaToken, VerifiedCcaToken, decode_value, measurement_eq, verify_cca_endorsed,
    verify_sign1,
};
use crate::chain::{Curve, EcPublicKey};
use crate::claims::ct_eq;
use crate::error::{AttestError, ParseError};
use crate::time::unix_seconds;
use coset::cbor::value::Value;
use coset::{CoseSign1, TaggedCborSerializable};

pub const CORIM_TAG: u64 = 501;
pub const COMID_TAG: u64 = 506;
pub const COSE_SIGN1_TAG: u64 = 18;
pub const CORIM_CONTENT_TYPE: &str = "application/rim+cbor";
/// `corim-meta` protected-header label of a signed CoRIM.
pub const CORIM_META_LABEL: i64 = 8;
pub const TAG_UUID: u64 = 37;
pub const TAG_OID: u64 = 111;
pub const TAG_UEID: u64 = 550;
pub const TAG_PKIX_BASE64_KEY: u64 = 554;
pub const TAG_BYTES: u64 = 560;
pub const TAG_MASKED_RAW_VALUE: u64 = 563;
pub const TAG_URI: u64 = 32;
pub const TAG_EPOCH_TIME: u64 = 1;
/// Largest CoRIM accepted (real ones are a few KiB).
pub const MAX_CORIM_LEN: usize = 256 * 1024;
/// Largest array accepted anywhere in a CoRIM.
pub const MAX_CORIM_ITEMS: usize = 1024;

pub const CCA_PLATFORM_ENDORSEMENT_PROFILE: &str = "tag:arm.com,2025:cca_platform#1.0.0";
pub const CCA_REALM_ENDORSEMENT_PROFILE: &str = "tag:arm.com,2025:cca_realm#1.0.0";

/// Named Information hash algorithm IDs (IANA): sha-256, sha-384, sha-512.
pub const NI_SHA256: i64 = 1;
pub const NI_SHA384: i64 = 7;
pub const NI_SHA512: i64 = 8;

// ------------------------------------------------------------ model --

/// A tag or CoRIM identifier: text or a 16-byte UUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorimId {
    Text(String),
    Uuid([u8; 16]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorimProfile {
    Uri(String),
    Oid(Vec<u8>),
}

/// Validity in Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorimValidity {
    pub not_before: Option<u64>,
    pub not_after: u64,
}

impl CorimValidity {
    fn check(&self, now_ns: u64, what: &'static str) -> Result<(), AttestError> {
        let now = unix_seconds(now_ns);
        if self.not_before.is_some_and(|nb| now < nb) || now >= self.not_after {
            return Err(AttestError::CollateralExpired { what });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorimEntity {
    pub name: String,
    pub regid: Option<String>,
    pub roles: Vec<u64>,
}

/// Who signed a signed CoRIM, from its `corim-meta` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorimSigner {
    pub name: String,
    pub uri: Option<String>,
    pub signature_validity: Option<CorimValidity>,
}

/// A parsed CoRIM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Corim {
    pub id: CorimId,
    pub profile: Option<CorimProfile>,
    pub validity: Option<CorimValidity>,
    pub entities: Vec<CorimEntity>,
    pub comids: Vec<Comid>,
    /// Tags that are not CoMIDs (CoSWID, CoTL, ...), not interpreted.
    pub other_tags: usize,
    /// `Some` for a signed CoRIM whose signature verified.
    pub signer: Option<CorimSigner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comid {
    pub tag_id: CorimId,
    pub tag_version: u64,
    pub reference_values: Vec<ReferenceTriple>,
    pub attest_keys: Vec<AttestKeyTriple>,
    /// Keys of other triple kinds present (endorsed values, identity,
    /// dependency, ...), not interpreted.
    pub other_triples: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassId {
    Uuid([u8; 16]),
    Oid(Vec<u8>),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instance {
    Ueid(Vec<u8>),
    Uuid([u8; 16]),
    Bytes(Vec<u8>),
    /// A cryptographic-key instance or another tagged form (its tag).
    Other(u64),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub class_id: Option<ClassId>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub layer: Option<u64>,
    pub index: Option<u64>,
    /// Whether the environment has a `class` map at all.
    pub has_class: bool,
    pub instance: Option<Instance>,
    /// Groups are parsed only to be refused by the CCA profile.
    pub has_group: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mkey {
    Text(String),
    Uint(u64),
    Uuid([u8; 16]),
    Oid(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digest {
    /// Named Information algorithm ID (text names are mapped to it).
    pub alg: i64,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawValue {
    pub value: Vec<u8>,
    /// Tag 563 only.
    pub mask: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CryptoKey {
    /// Tag 554: a PEM `PUBLIC KEY` (SubjectPublicKeyInfo).
    PkixBase64Key(String),
    /// Tag 560: raw bytes (CCA signer IDs, RoTPK hashes).
    Bytes(Vec<u8>),
    /// Another key form (certificate, thumbprint, COSE_Key): its tag.
    Other(u64),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeasurementValues {
    pub version: Option<String>,
    pub version_has_scheme: bool,
    pub digests: Vec<Digest>,
    pub raw_value: Option<RawValue>,
    pub name: Option<String>,
    pub cryptokeys: Vec<CryptoKey>,
    /// Measurement-value keys this parser does not interpret.
    pub other_keys: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measurement {
    pub mkey: Option<Mkey>,
    pub values: MeasurementValues,
    pub has_authorized_by: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceTriple {
    pub environment: Environment,
    pub measurements: Vec<Measurement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestKeyTriple {
    pub environment: Environment,
    pub keys: Vec<CryptoKey>,
    pub has_conditions: bool,
}

// ------------------------------------------------------- CBOR helpers --

fn err(field: &'static str, reason: &'static str) -> ParseError {
    ParseError::Cbor { field, reason }
}

/// An integer-keyed map with duplicates rejected.
struct IntMap<'v>(Vec<(i64, &'v Value)>);

impl<'v> IntMap<'v> {
    fn new(v: &'v Value, what: &'static str) -> Result<Self, ParseError> {
        let entries = v.as_map().ok_or_else(|| err(what, "expected map"))?;
        if entries.len() > MAX_CORIM_ITEMS {
            return Err(err(what, "too many entries"));
        }
        let mut out: Vec<(i64, &'v Value)> = Vec::with_capacity(entries.len());
        for (k, val) in entries {
            let key = k
                .as_integer()
                .and_then(|i| i64::try_from(i).ok())
                .ok_or_else(|| err(what, "non-integer key"))?;
            if out.iter().any(|(seen, _)| *seen == key) {
                return Err(err(what, "duplicate key"));
            }
            out.push((key, val));
        }
        Ok(IntMap(out))
    }

    fn get(&self, key: i64) -> Option<&'v Value> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    fn req(&self, key: i64, field: &'static str) -> Result<&'v Value, ParseError> {
        self.get(key).ok_or_else(|| err(field, "missing"))
    }

    fn only(&self, allowed: &[i64], what: &'static str) -> Result<(), ParseError> {
        if self.0.iter().any(|(k, _)| !allowed.contains(k)) {
            return Err(err(what, "unknown key"));
        }
        Ok(())
    }
}

fn array<'v>(v: &'v Value, field: &'static str) -> Result<&'v [Value], ParseError> {
    let a = v.as_array().ok_or_else(|| err(field, "expected array"))?;
    if a.len() > MAX_CORIM_ITEMS {
        return Err(err(field, "too many items"));
    }
    Ok(a)
}

fn text(v: &Value, field: &'static str) -> Result<String, ParseError> {
    v.as_text()
        .map(str::to_owned)
        .ok_or_else(|| err(field, "expected text"))
}

fn bstr<'v>(v: &'v Value, field: &'static str) -> Result<&'v [u8], ParseError> {
    v.as_bytes()
        .map(Vec::as_slice)
        .ok_or_else(|| err(field, "expected byte string"))
}

fn uint(v: &Value, field: &'static str) -> Result<u64, ParseError> {
    v.as_integer()
        .and_then(|i| u64::try_from(i).ok())
        .ok_or_else(|| err(field, "expected unsigned integer"))
}

fn tagged(v: &Value) -> Option<(u64, &Value)> {
    match v {
        Value::Tag(t, inner) => Some((*t, inner)),
        _ => None,
    }
}

fn uuid(v: &Value, field: &'static str) -> Result<[u8; 16], ParseError> {
    bstr(v, field)?
        .try_into()
        .map_err(|_| err(field, "UUID is not 16 bytes"))
}

fn id(v: &Value, field: &'static str) -> Result<CorimId, ParseError> {
    match v {
        Value::Text(t) => Ok(CorimId::Text(t.clone())),
        Value::Bytes(_) => Ok(CorimId::Uuid(uuid(v, field)?)),
        Value::Tag(TAG_UUID, inner) => Ok(CorimId::Uuid(uuid(inner, field)?)),
        _ => Err(err(field, "expected text or UUID")),
    }
}

fn time(v: &Value, field: &'static str) -> Result<u64, ParseError> {
    match tagged(v) {
        Some((TAG_EPOCH_TIME, inner)) => uint(inner, field),
        _ => Err(err(field, "expected tag-1 integer time")),
    }
}

fn validity(v: &Value, field: &'static str) -> Result<CorimValidity, ParseError> {
    let m = IntMap::new(v, field)?;
    m.only(&[0, 1], field)?;
    let not_before = m.get(0).map(|t| time(t, field)).transpose()?;
    let not_after = time(m.req(1, field)?, field)?;
    if not_before.is_some_and(|nb| nb > not_after) {
        return Err(err(field, "not-before after not-after"));
    }
    Ok(CorimValidity {
        not_before,
        not_after,
    })
}

fn uri(v: &Value, field: &'static str) -> Result<String, ParseError> {
    match tagged(v) {
        Some((TAG_URI, inner)) => text(inner, field),
        None => text(v, field),
        _ => Err(err(field, "expected URI")),
    }
}

// ----------------------------------------------------------- parsing --

fn parse_entities(v: &Value) -> Result<Vec<CorimEntity>, ParseError> {
    array(v, "entities")?
        .iter()
        .map(|e| {
            let m = IntMap::new(e, "entity")?;
            let roles = match m.get(2) {
                None => Vec::new(),
                Some(r) => array(r, "entity roles")?
                    .iter()
                    .map(|x| uint(x, "entity role"))
                    .collect::<Result<_, _>>()?,
            };
            Ok(CorimEntity {
                name: text(m.req(0, "entity name")?, "entity name")?,
                regid: m.get(1).map(|r| uri(r, "entity regid")).transpose()?,
                roles,
            })
        })
        .collect()
}

fn parse_class(v: &Value, env: &mut Environment) -> Result<(), ParseError> {
    let m = IntMap::new(v, "class")?;
    m.only(&[0, 1, 2, 3, 4], "class")?;
    env.has_class = true;
    env.class_id = match m.get(0) {
        None => None,
        Some(c) => Some(match tagged(c) {
            Some((TAG_UUID, inner)) => ClassId::Uuid(uuid(inner, "class id")?),
            Some((TAG_OID, inner)) => ClassId::Oid(bstr(inner, "class id")?.to_vec()),
            Some((TAG_BYTES, inner)) => ClassId::Bytes(bstr(inner, "class id")?.to_vec()),
            _ => return Err(err("class id", "expected tagged UUID, OID or bytes")),
        }),
    };
    env.vendor = m.get(1).map(|x| text(x, "class vendor")).transpose()?;
    env.model = m.get(2).map(|x| text(x, "class model")).transpose()?;
    env.layer = m.get(3).map(|x| uint(x, "class layer")).transpose()?;
    env.index = m.get(4).map(|x| uint(x, "class index")).transpose()?;
    Ok(())
}

fn parse_environment(v: &Value) -> Result<Environment, ParseError> {
    let m = IntMap::new(v, "environment")?;
    m.only(&[0, 1, 2], "environment")?;
    let mut env = Environment::default();
    if let Some(c) = m.get(0) {
        parse_class(c, &mut env)?;
    }
    env.instance = match m.get(1) {
        None => None,
        Some(i) => Some(match tagged(i) {
            Some((TAG_UEID, inner)) => Instance::Ueid(bstr(inner, "instance")?.to_vec()),
            Some((TAG_UUID, inner)) => Instance::Uuid(uuid(inner, "instance")?),
            Some((TAG_BYTES, inner)) => Instance::Bytes(bstr(inner, "instance")?.to_vec()),
            Some((t, _)) => Instance::Other(t),
            None => return Err(err("instance", "expected tagged instance")),
        }),
    };
    env.has_group = m.get(2).is_some();
    if !env.has_class && env.instance.is_none() && !env.has_group {
        return Err(err("environment", "empty"));
    }
    Ok(env)
}

fn alg_id(v: &Value) -> Result<i64, ParseError> {
    match v {
        Value::Integer(i) => i64::try_from(*i).map_err(|_| err("digest alg", "out of range")),
        Value::Text(t) => match t.as_str() {
            "sha-256" => Ok(NI_SHA256),
            "sha-384" => Ok(NI_SHA384),
            "sha-512" => Ok(NI_SHA512),
            _ => Err(err("digest alg", "unknown algorithm name")),
        },
        _ => Err(err("digest alg", "expected integer or text")),
    }
}

fn parse_crypto_key(v: &Value) -> Result<CryptoKey, ParseError> {
    match tagged(v) {
        Some((TAG_PKIX_BASE64_KEY, inner)) => {
            Ok(CryptoKey::PkixBase64Key(text(inner, "pkix-base64-key")?))
        }
        Some((TAG_BYTES, inner)) => Ok(CryptoKey::Bytes(bstr(inner, "crypto key")?.to_vec())),
        Some((t, _)) => Ok(CryptoKey::Other(t)),
        None => Err(err("crypto key", "expected tagged key")),
    }
}

fn parse_mval(v: &Value) -> Result<MeasurementValues, ParseError> {
    let m = IntMap::new(v, "measurement values")?;
    let mut out = MeasurementValues::default();
    for (k, val) in &m.0 {
        match *k {
            0 => {
                let vm = IntMap::new(val, "version")?;
                vm.only(&[0, 1], "version")?;
                out.version = Some(text(vm.req(0, "version")?, "version")?);
                out.version_has_scheme = vm.get(1).is_some();
            }
            2 => {
                let ds = array(val, "digests")?;
                if ds.is_empty() {
                    return Err(err("digests", "empty"));
                }
                for d in ds {
                    match array(d, "digest")? {
                        [alg, value] => out.digests.push(Digest {
                            alg: alg_id(alg)?,
                            value: bstr(value, "digest value")?.to_vec(),
                        }),
                        _ => return Err(err("digest", "expected [alg, value]")),
                    }
                }
            }
            4 => {
                out.raw_value = Some(match tagged(val) {
                    Some((TAG_BYTES, inner)) => RawValue {
                        value: bstr(inner, "raw value")?.to_vec(),
                        mask: None,
                    },
                    Some((TAG_MASKED_RAW_VALUE, inner)) => {
                        match array(inner, "masked raw value")? {
                            [value, mask] => RawValue {
                                value: bstr(value, "masked raw value")?.to_vec(),
                                mask: Some(bstr(mask, "raw value mask")?.to_vec()),
                            },
                            _ => return Err(err("masked raw value", "expected [value, mask]")),
                        }
                    }
                    _ => return Err(err("raw value", "expected tag 560 or 563")),
                });
            }
            11 => out.name = Some(text(val, "measurement name")?),
            13 => {
                out.cryptokeys = array(val, "cryptokeys")?
                    .iter()
                    .map(parse_crypto_key)
                    .collect::<Result<_, _>>()?;
            }
            other => out.other_keys.push(other),
        }
    }
    Ok(out)
}

fn parse_measurement(v: &Value) -> Result<Measurement, ParseError> {
    let m = IntMap::new(v, "measurement")?;
    m.only(&[0, 1, 2], "measurement")?;
    let mkey = match m.get(0) {
        None => None,
        Some(Value::Text(t)) => Some(Mkey::Text(t.clone())),
        Some(Value::Integer(i)) => Some(Mkey::Uint(
            u64::try_from(*i).map_err(|_| err("mkey", "negative integer"))?,
        )),
        Some(Value::Tag(TAG_UUID, inner)) => Some(Mkey::Uuid(uuid(inner, "mkey")?)),
        Some(Value::Tag(TAG_OID, inner)) => Some(Mkey::Oid(bstr(inner, "mkey")?.to_vec())),
        Some(_) => return Err(err("mkey", "unsupported type")),
    };
    Ok(Measurement {
        mkey,
        values: parse_mval(m.req(1, "measurement values")?)?,
        has_authorized_by: m.get(2).is_some(),
    })
}

fn parse_comid(bytes: &[u8]) -> Result<Comid, ParseError> {
    let v = decode_value(bytes, "CoMID")?;
    let m = IntMap::new(&v, "CoMID")?;
    let ti = IntMap::new(m.req(1, "CoMID tag identity")?, "CoMID tag identity")?;
    ti.only(&[0, 1], "CoMID tag identity")?;
    let triples = IntMap::new(m.req(4, "CoMID triples")?, "CoMID triples")?;
    let mut comid = Comid {
        tag_id: id(ti.req(0, "CoMID tag id")?, "CoMID tag id")?,
        tag_version: ti
            .get(1)
            .map(|x| uint(x, "tag version"))
            .transpose()?
            .unwrap_or(0),
        reference_values: Vec::new(),
        attest_keys: Vec::new(),
        other_triples: Vec::new(),
    };
    for (k, val) in &triples.0 {
        match *k {
            0 => {
                for t in array(val, "reference triples")? {
                    match array(t, "reference triple")? {
                        [env, ms] => comid.reference_values.push(ReferenceTriple {
                            environment: parse_environment(env)?,
                            measurements: {
                                let ms = array(ms, "measurements")?;
                                if ms.is_empty() {
                                    return Err(err("measurements", "empty"));
                                }
                                ms.iter().map(parse_measurement).collect::<Result<_, _>>()?
                            },
                        }),
                        _ => return Err(err("reference triple", "expected [env, measurements]")),
                    }
                }
            }
            3 => {
                for t in array(val, "attest-key triples")? {
                    let (env, keys, has_conditions) = match array(t, "attest-key triple")? {
                        [env, keys] => (env, keys, false),
                        [env, keys, _conditions] => (env, keys, true),
                        _ => return Err(err("attest-key triple", "expected [env, keys]")),
                    };
                    comid.attest_keys.push(AttestKeyTriple {
                        environment: parse_environment(env)?,
                        keys: array(keys, "attest keys")?
                            .iter()
                            .map(parse_crypto_key)
                            .collect::<Result<_, _>>()?,
                        has_conditions,
                    });
                }
            }
            other => comid.other_triples.push(other),
        }
    }
    Ok(comid)
}

fn parse_corim_value(v: &Value) -> Result<Corim, ParseError> {
    let inner = match tagged(v) {
        Some((CORIM_TAG, inner)) => inner,
        _ => return Err(err("CoRIM", "missing tag 501")),
    };
    let m = IntMap::new(inner, "CoRIM")?;
    let mut comids = Vec::new();
    let mut other_tags = 0;
    let tags = array(m.req(1, "CoRIM tags")?, "CoRIM tags")?;
    if tags.is_empty() {
        return Err(err("CoRIM tags", "empty"));
    }
    for t in tags {
        match tagged(t) {
            Some((COMID_TAG, b)) => comids.push(parse_comid(bstr(b, "CoMID")?)?),
            Some(_) => other_tags += 1,
            None => return Err(err("CoRIM tags", "untagged entry")),
        }
    }
    let profile = match m.get(3) {
        None => None,
        Some(p) => Some(match tagged(p) {
            Some((TAG_URI, inner)) => CorimProfile::Uri(text(inner, "profile")?),
            Some((TAG_OID, inner)) => CorimProfile::Oid(bstr(inner, "profile")?.to_vec()),
            _ => return Err(err("profile", "expected tagged URI or OID")),
        }),
    };
    Ok(Corim {
        id: id(m.req(0, "CoRIM id")?, "CoRIM id")?,
        profile,
        validity: m
            .get(4)
            .map(|x| validity(x, "CoRIM validity"))
            .transpose()?,
        entities: m
            .get(5)
            .map(parse_entities)
            .transpose()?
            .unwrap_or_default(),
        comids,
        other_tags,
        signer: None,
    })
}

impl Corim {
    /// Parse an unsigned CoRIM (tag 501).
    pub fn parse_unsigned(bytes: &[u8]) -> Result<Self, ParseError> {
        check_len(bytes)?;
        parse_corim_value(&decode_value(bytes, "CoRIM")?)
    }

    /// Verify a signed CoRIM (COSE_Sign1, tag 18) with `key` and parse its
    /// payload: the protected header must name ES256/ES384 matching the
    /// key's curve, content type `application/rim+cbor` and carry
    /// `corim-meta`, whose signature validity (if any) must hold at
    /// `now_ns`; the payload must be an attached tag-501 CoRIM.
    pub fn verify_signed(
        bytes: &[u8],
        key: &EcPublicKey,
        now_ns: u64,
    ) -> Result<Self, AttestError> {
        check_len(bytes)?;
        let sign1 = CoseSign1::from_tagged_slice(bytes)
            .map_err(|_| err("signed CoRIM", "malformed tagged COSE_Sign1"))?;
        let h = &sign1.protected.header;
        match &h.content_type {
            Some(coset::ContentType::Text(t)) if t == CORIM_CONTENT_TYPE => {}
            _ => {
                return Err(err("signed CoRIM", "content type is not application/rim+cbor").into());
            }
        }
        let meta = h
            .rest
            .iter()
            .find(|(l, _)| *l == coset::Label::Int(CORIM_META_LABEL))
            .map(|(_, v)| v)
            .ok_or_else(|| err("signed CoRIM", "missing corim-meta"))?;
        verify_sign1(&sign1, key, "signed CoRIM")?;
        let signer = parse_meta(bstr(meta, "corim-meta")?)?;
        if let Some(v) = &signer.signature_validity {
            v.check(now_ns, "signed CoRIM signature validity")?;
        }
        let payload = sign1
            .payload
            .as_deref()
            .ok_or_else(|| err("signed CoRIM", "detached payload"))?;
        let mut corim = parse_corim_value(&decode_value(payload, "CoRIM")?)?;
        corim.signer = Some(signer);
        Ok(corim)
    }

    pub fn profile_uri(&self) -> Option<&str> {
        match &self.profile {
            Some(CorimProfile::Uri(u)) => Some(u),
            _ => None,
        }
    }
}

fn check_len(bytes: &[u8]) -> Result<(), ParseError> {
    if bytes.len() > MAX_CORIM_LEN {
        return Err(ParseError::OverBudget {
            format: crate::error::Format::Corim,
            field: "CoRIM",
            limit: MAX_CORIM_LEN,
            value: bytes.len() as u64,
        });
    }
    Ok(())
}

fn parse_meta(bytes: &[u8]) -> Result<CorimSigner, ParseError> {
    let v = decode_value(bytes, "corim-meta")?;
    let m = IntMap::new(&v, "corim-meta")?;
    m.only(&[0, 1], "corim-meta")?;
    let s = IntMap::new(m.req(0, "corim-meta signer")?, "corim-meta signer")?;
    s.only(&[0, 1], "corim-meta signer")?;
    Ok(CorimSigner {
        name: text(s.req(0, "signer name")?, "signer name")?,
        uri: s.get(1).map(|u| uri(u, "signer uri")).transpose()?,
        signature_validity: m
            .get(1)
            .map(|x| validity(x, "signature validity"))
            .transpose()?,
    })
}

// --------------------------------------------------- PKIX public key --

/// Decode a PEM `PUBLIC KEY` holding an uncompressed P-256 or P-384 point.
pub fn ec_public_key_from_pem(pem: &str) -> Result<EcPublicKey, AttestError> {
    use rustls_pki_types::SubjectPublicKeyInfoDer;
    use rustls_pki_types::pem::PemObject;
    let bad = |what| AttestError::UnexpectedKeyType { what };
    let der = SubjectPublicKeyInfoDer::from_pem_slice(pem.as_bytes())
        .map_err(|_| bad("PKIX public key is not a PEM PUBLIC KEY"))?;
    let spki = spki::SubjectPublicKeyInfoRef::try_from(der.as_ref())
        .map_err(|_| bad("PKIX public key DER"))?;
    const EC: spki::ObjectIdentifier = spki::ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
    const P256: spki::ObjectIdentifier = spki::ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
    const P384: spki::ObjectIdentifier = spki::ObjectIdentifier::new_unwrap("1.3.132.0.34");
    if spki.algorithm.oid != EC {
        return Err(bad("PKIX public key is not an EC key"));
    }
    let curve = match spki.algorithm.parameters_oid() {
        Ok(o) if o == P256 => Curve::P256,
        Ok(o) if o == P384 => Curve::P384,
        _ => return Err(bad("PKIX public key curve (P-256 or P-384 only)")),
    };
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| bad("PKIX public key bit string"))?;
    EcPublicKey::from_sec1(curve, point)
}

// ------------------------------------------------------- CCA profile --

/// A `cca.software-component` reference value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaSwReference {
    pub name: Option<String>,
    pub version: Option<String>,
    pub digests: Vec<Digest>,
    pub signer_id: Vec<u8>,
}

/// A TBB RoTPK reference value (`cca.rotpk.CM.n` / `cca.rotpk.DM.n`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaRotpkReference {
    pub name: String,
    pub index: u64,
    pub hashes: Vec<Vec<u8>>,
}

/// The reference values of one platform reference-value triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaPlatformReference {
    pub implementation_id: Vec<u8>,
    pub sw_components: Vec<CcaSwReference>,
    pub config: Option<RawValue>,
    pub manufacturing_config: Option<RawValue>,
    pub rotpks: Vec<CcaRotpkReference>,
}

/// The reference values of one realm reference-value triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaRealmReference {
    pub rim: Vec<u8>,
    pub rems: [Option<Vec<u8>>; 4],
    pub personalization: Option<Vec<u8>>,
}

/// Reference values and endorsed platform keys from accepted CCA CoRIMs.
#[derive(Clone, Debug, Default)]
pub struct CcaCorimStore {
    pub platform: Vec<CcaPlatformReference>,
    pub realm: Vec<CcaRealmReference>,
    pub endorsements: Vec<CcaEndorsement>,
}

fn profile_err(reason: impl Into<String>) -> AttestError {
    AttestError::Collateral {
        what: "CCA CoRIM profile",
        reason: reason.into(),
    }
}

fn digest_len_ok(b: &[u8]) -> bool {
    matches!(b.len(), 32 | 48 | 64)
}

fn alg_len(alg: i64) -> Option<usize> {
    match alg {
        NI_SHA256 => Some(32),
        NI_SHA384 => Some(48),
        NI_SHA512 => Some(64),
        _ => None,
    }
}

fn check_digests(ds: &[Digest], exactly_one: bool, what: &str) -> Result<(), AttestError> {
    if ds.is_empty() {
        return Err(profile_err(format!("{what}: digests field is mandatory")));
    }
    if exactly_one && ds.len() != 1 {
        return Err(profile_err(format!("{what}: exactly one digest required")));
    }
    for d in ds {
        match alg_len(d.alg) {
            Some(n) if n == d.value.len() => {}
            Some(_) => return Err(profile_err(format!("{what}: digest length vs algorithm"))),
            None => {
                return Err(profile_err(format!(
                    "{what}: unsupported digest algorithm {}",
                    d.alg
                )));
            }
        }
    }
    Ok(())
}

fn mkey_text(m: &Measurement, at: usize) -> Result<&str, AttestError> {
    match &m.mkey {
        Some(Mkey::Text(t)) => Ok(t),
        None => Err(profile_err(format!("measurement {at}: mkey is mandatory"))),
        Some(_) => Err(profile_err(format!(
            "measurement {at}: mkey must be a string"
        ))),
    }
}

fn class_bytes<'a>(env: &'a Environment, what: &str) -> Result<&'a [u8], AttestError> {
    if !env.has_class {
        return Err(profile_err(format!(
            "{what}: environment.class is required"
        )));
    }
    match &env.class_id {
        Some(ClassId::Bytes(b)) => Ok(b),
        Some(_) => Err(profile_err(format!(
            "{what}: class id must be tagged bytes"
        ))),
        None => Err(profile_err(format!("{what}: class id is required"))),
    }
}

fn check_plain_values(
    v: &MeasurementValues,
    allowed: &[&str],
    what: &str,
) -> Result<(), AttestError> {
    if !v.other_keys.is_empty() {
        return Err(AttestError::UnsupportedReferenceValue {
            what: "CCA measurement with measurement-value keys this verifier does not evaluate",
        });
    }
    let present = [
        ("version", v.version.is_some()),
        ("digests", !v.digests.is_empty()),
        ("raw-value", v.raw_value.is_some()),
        ("name", v.name.is_some()),
        ("cryptokeys", !v.cryptokeys.is_empty()),
    ];
    for (k, p) in present {
        if p && !allowed.contains(&k) {
            return Err(profile_err(format!("{what}: {k} is not allowed here")));
        }
    }
    Ok(())
}

fn masked_raw(rv: &Option<RawValue>, what: &str) -> Result<RawValue, AttestError> {
    let rv = rv
        .as_ref()
        .ok_or_else(|| profile_err(format!("{what}: raw-value is mandatory")))?;
    match &rv.mask {
        Some(m) if !rv.value.is_empty() && m.len() == rv.value.len() => Ok(rv.clone()),
        Some(_) => Err(profile_err(format!(
            "{what}: raw-value and mask lengths differ or are empty"
        ))),
        None => Err(profile_err(format!(
            "{what}: raw-value must be masked (tag 563)"
        ))),
    }
}

fn rotpk_key(k: &str) -> Option<(String, u64)> {
    let rest = k.strip_prefix("cca.rotpk.")?;
    let (name, idx) = rest.split_once('.')?;
    if (name != "CM" && name != "DM") || idx.len() != 1 {
        return None;
    }
    let i = idx.parse::<u64>().ok().filter(|i| *i <= 5)?;
    Some((name.to_owned(), i))
}

impl CcaCorimStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate a CoRIM against its CCA endorsement profile (platform or
    /// realm, from the CoRIM's profile URI) and add its reference values and
    /// attestation keys. The CoRIM's validity, if any, must hold at
    /// `now_ns`. Validation follows Veraison's `corim/profiles/cca`; a
    /// CoRIM with any profile violation adds nothing.
    pub fn add(&mut self, corim: &Corim, now_ns: u64) -> Result<(), AttestError> {
        if let Some(v) = &corim.validity {
            v.check(now_ns, "CoRIM validity")?;
        }
        let mut next = self.clone();
        match corim.profile_uri() {
            Some(CCA_PLATFORM_ENDORSEMENT_PROFILE) => {
                for c in &corim.comids {
                    next.add_platform_comid(c)?;
                }
            }
            Some(CCA_REALM_ENDORSEMENT_PROFILE) => {
                for c in &corim.comids {
                    next.add_realm_comid(c)?;
                }
            }
            _ => {
                return Err(profile_err(
                    "not a CCA platform or realm endorsement profile",
                ));
            }
        }
        *self = next;
        Ok(())
    }

    fn add_platform_comid(&mut self, c: &Comid) -> Result<(), AttestError> {
        for (i, t) in c.reference_values.iter().enumerate() {
            let what = format!("platform reference value {i}");
            if t.environment.instance.is_some() || t.environment.has_group {
                return Err(profile_err(format!("{what}: instance/group not allowed")));
            }
            let impl_id = class_bytes(&t.environment, &what)?;
            if impl_id.len() != 32 {
                return Err(profile_err(format!(
                    "{what}: implementation-id must be 32 bytes"
                )));
            }
            let mut r = CcaPlatformReference {
                implementation_id: impl_id.to_vec(),
                sw_components: Vec::new(),
                config: None,
                manufacturing_config: None,
                rotpks: Vec::new(),
            };
            for (j, m) in t.measurements.iter().enumerate() {
                if m.has_authorized_by {
                    return Err(AttestError::UnsupportedReferenceValue {
                        what: "CCA measurement with authorized-by",
                    });
                }
                let key = mkey_text(m, j)?;
                let v = &m.values;
                match key {
                    "cca.software-component" => {
                        check_plain_values(
                            v,
                            &["version", "digests", "name", "cryptokeys"],
                            &what,
                        )?;
                        check_digests(&v.digests, false, &what)?;
                        if v.version_has_scheme {
                            return Err(profile_err(format!(
                                "{what}: version-scheme must not be present"
                            )));
                        }
                        let signer_id = match v.cryptokeys.as_slice() {
                            [CryptoKey::Bytes(b)] if digest_len_ok(b) => b.clone(),
                            [CryptoKey::Bytes(_)] => {
                                return Err(profile_err(format!("{what}: signer-id length")));
                            }
                            [] => {
                                return Err(profile_err(format!("{what}: signer-id is mandatory")));
                            }
                            [_] => {
                                return Err(profile_err(format!(
                                    "{what}: cryptokeys (signer-id) must be of type bytes"
                                )));
                            }
                            _ => return Err(profile_err(format!("{what}: exactly one signer-id"))),
                        };
                        r.sw_components.push(CcaSwReference {
                            name: v.name.clone(),
                            version: v.version.clone(),
                            digests: v.digests.clone(),
                            signer_id,
                        });
                    }
                    "cca.platform-config" | "cca.platform-manufacturing-config" => {
                        check_plain_values(v, &["raw-value"], &what)?;
                        let rv = masked_raw(&v.raw_value, key)?;
                        let slot = if key == "cca.platform-config" {
                            &mut r.config
                        } else {
                            &mut r.manufacturing_config
                        };
                        if slot.replace(rv).is_some() {
                            return Err(profile_err(format!("{what}: duplicate {key}")));
                        }
                    }
                    k => {
                        let Some((name, index)) = rotpk_key(k) else {
                            return Err(profile_err(format!("{what}: invalid mkey {k:?}")));
                        };
                        check_plain_values(v, &["cryptokeys"], &what)?;
                        if v.cryptokeys.is_empty() || v.cryptokeys.len() > 48 {
                            return Err(profile_err(format!("{what}: RoTPK keys count")));
                        }
                        let hashes = v
                            .cryptokeys
                            .iter()
                            .map(|k| match k {
                                CryptoKey::Bytes(b) if digest_len_ok(b) => Ok(b.clone()),
                                _ => Err(profile_err(format!(
                                    "{what}: RoTPK keys must be hash bytes"
                                ))),
                            })
                            .collect::<Result<_, _>>()?;
                        r.rotpks.push(CcaRotpkReference {
                            name,
                            index,
                            hashes,
                        });
                    }
                }
            }
            if r.sw_components.is_empty() {
                return Err(profile_err(format!(
                    "{what}: at least one software component measurement is required"
                )));
            }
            self.platform.push(r);
        }
        for (i, t) in c.attest_keys.iter().enumerate() {
            let what = format!("platform attestation key {i}");
            if t.has_conditions || t.environment.has_group {
                return Err(AttestError::UnsupportedReferenceValue {
                    what: "CCA attestation-key triple with conditions or group",
                });
            }
            let impl_id = class_bytes(&t.environment, &what)?;
            if impl_id.len() != 32 {
                return Err(profile_err(format!(
                    "{what}: implementation-id must be 32 bytes"
                )));
            }
            let instance = match &t.environment.instance {
                Some(Instance::Ueid(u)) if u.len() == 33 && u[0] == 0x01 => u.clone(),
                Some(Instance::Ueid(_)) => {
                    return Err(profile_err(format!(
                        "{what}: instance-id must be 33 bytes starting 0x01"
                    )));
                }
                Some(_) => {
                    return Err(profile_err(format!(
                        "{what}: instance-id must be of type ueid"
                    )));
                }
                None => {
                    return Err(profile_err(format!(
                        "{what}: environment.instance (instance-id) is required"
                    )));
                }
            };
            let cpak = match t.keys.as_slice() {
                [CryptoKey::PkixBase64Key(pem)] => ec_public_key_from_pem(pem)?,
                [_] => {
                    return Err(profile_err(format!(
                        "{what}: verification-key must be of type pkix-base64-key"
                    )));
                }
                _ => return Err(profile_err(format!("{what}: exactly one verification key"))),
            };
            self.endorsements.push(CcaEndorsement {
                instance_id: instance,
                implementation_id: Some(impl_id.to_vec()),
                cpak,
            });
        }
        Ok(())
    }

    fn add_realm_comid(&mut self, c: &Comid) -> Result<(), AttestError> {
        if !c.attest_keys.is_empty() {
            return Err(profile_err("realm CoMID with attestation keys"));
        }
        for (i, t) in c.reference_values.iter().enumerate() {
            let what = format!("realm reference value {i}");
            if t.environment.has_group || t.environment.instance.is_some() {
                return Err(profile_err(format!("{what}: instance/group not allowed")));
            }
            if !t.environment.has_class {
                return Err(profile_err(format!(
                    "{what}: environment.class is required for CCA Realm profile"
                )));
            }
            let rim = match &t.environment.class_id {
                Some(ClassId::Bytes(b)) if digest_len_ok(b) => b.clone(),
                Some(ClassId::Bytes(_)) => {
                    return Err(profile_err(format!(
                        "{what}: RIM must be 32, 48 or 64 bytes"
                    )));
                }
                Some(_) => return Err(profile_err(format!("{what}: RIM must be of type bytes"))),
                None => return Err(profile_err(format!("{what}: class id (RIM) is required"))),
            };
            let mut r = CcaRealmReference {
                rim: rim.clone(),
                rems: Default::default(),
                personalization: None,
            };
            let mut has_rim = false;
            for (j, m) in t.measurements.iter().enumerate() {
                if m.has_authorized_by {
                    return Err(AttestError::UnsupportedReferenceValue {
                        what: "CCA measurement with authorized-by",
                    });
                }
                let key = mkey_text(m, j)?;
                let v = &m.values;
                let rem = match key {
                    "cca.rem0" => Some(0),
                    "cca.rem1" => Some(1),
                    "cca.rem2" => Some(2),
                    "cca.rem3" => Some(3),
                    _ => None,
                };
                match (key, rem) {
                    ("cca.rim", _) | (_, Some(_)) => {
                        check_plain_values(v, &["digests"], &what)?;
                        check_digests(&v.digests, true, &format!("{what} {key}"))?;
                        let d = v.digests[0].value.clone();
                        if let Some(n) = rem {
                            if r.rems[n].replace(d).is_some() {
                                return Err(profile_err(format!("{what}: duplicate {key}")));
                            }
                        } else {
                            if has_rim {
                                return Err(profile_err(format!("{what}: duplicate cca.rim")));
                            }
                            if !ct_eq(&d, &rim) {
                                return Err(profile_err(format!(
                                    "{what}: cca.rim differs from the class id"
                                )));
                            }
                            has_rim = true;
                        }
                    }
                    ("cca.rpv", _) => {
                        check_plain_values(v, &["raw-value"], &what)?;
                        let rv = v.raw_value.as_ref().ok_or_else(|| {
                            profile_err(format!("{what}: raw-value is mandatory for cca.rpv"))
                        })?;
                        if rv.mask.is_some() {
                            return Err(AttestError::UnsupportedReferenceValue {
                                what: "masked cca.rpv",
                            });
                        }
                        if rv.value.is_empty() || rv.value.len() > 64 {
                            return Err(profile_err(format!("{what}: cca.rpv length")));
                        }
                        if r.personalization.replace(rv.value.clone()).is_some() {
                            return Err(profile_err(format!("{what}: duplicate cca.rpv")));
                        }
                    }
                    (k, _) => return Err(profile_err(format!("{what}: invalid mkey {k:?}"))),
                }
            }
            if !has_rim {
                return Err(profile_err(format!(
                    "{what}: RIM (cca.rim) measurement is mandatory but not found"
                )));
            }
            self.realm.push(r);
        }
        Ok(())
    }
}

/// The outcome of a successful CCA appraisal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcaAppraisal {
    /// Index into [`CcaCorimStore::platform`] of the reference that matched.
    pub platform_reference: usize,
    /// Software components in the token (all matched).
    pub sw_components_matched: usize,
    pub config_matched: bool,
    pub manufacturing_config_matched: bool,
    pub rotpks_matched: usize,
    /// Index into [`CcaCorimStore::realm`] of the reference that matched.
    pub realm_reference: usize,
    /// Which REMs the matching realm reference constrained.
    pub rems_constrained: [bool; 4],
    pub personalization_matched: bool,
}

fn description_alg(d: &str) -> Result<i64, AttestError> {
    match d {
        "sha-256" => Ok(NI_SHA256),
        "sha-384" => Ok(NI_SHA384),
        "sha-512" => Ok(NI_SHA512),
        _ => Err(AttestError::UnsupportedReferenceValue {
            what: "software component measurement description (hash algorithm)",
        }),
    }
}

fn exact_config(
    reference: &Option<RawValue>,
    evidence: &Option<Vec<u8>>,
    what: &'static str,
) -> Result<bool, AttestError> {
    match (reference, evidence) {
        (None, _) => Ok(false),
        (Some(_), None) => Err(AttestError::ReferenceValueMissing { what }),
        (Some(r), Some(e)) if ct_eq(&r.value, e) => Ok(true),
        (Some(_), Some(_)) => Err(AttestError::ReferenceValueMismatch { what }),
    }
}

/// Appraisal options. The default is the strict reading of the CCA
/// endorsement profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CcaAppraisalOptions {
    /// Require each software component's signer ID to equal the
    /// reference's `cryptokeys` signer ID. Veraison's `arm-cca` scheme does
    /// not compare signer IDs (and its `cca-good` test corpus relies on
    /// that); `false` reproduces its verdicts.
    pub compare_signer_id: bool,
}

impl Default for CcaAppraisalOptions {
    fn default() -> Self {
        CcaAppraisalOptions {
            compare_signer_id: true,
        }
    }
}

fn appraise_platform(
    token: &CcaToken,
    r: &CcaPlatformReference,
    opts: &CcaAppraisalOptions,
) -> Result<(usize, bool, bool, usize), AttestError> {
    let pc = &token.platform_claims;
    if pc.sw_components.is_empty() {
        return Err(AttestError::ReferenceValueMissing {
            what: "CCA platform token has no software components",
        });
    }
    for sw in &pc.sw_components {
        let alg = sw
            .measurement_description
            .as_deref()
            .map(description_alg)
            .transpose()?;
        let found = r.sw_components.iter().any(|rv| {
            rv.digests.iter().any(|d| {
                // The store's fields are public, so the algorithm and length
                // are re-checked here, not only when a CoRIM is added.
                alg.is_none_or(|a| a == d.alg)
                    && alg_len(d.alg) == Some(d.value.len())
                    && ct_eq(&d.value, &sw.measurement_value)
            }) && (!opts.compare_signer_id || ct_eq(&rv.signer_id, &sw.signer_id))
                && rv
                    .name
                    .as_ref()
                    .is_none_or(|n| sw.measurement_type.as_ref() == Some(n))
                && rv
                    .version
                    .as_ref()
                    .is_none_or(|v| sw.version.as_ref() == Some(v))
        });
        if !found {
            return Err(AttestError::ReferenceValueMismatch {
                what: "CCA platform software component",
            });
        }
    }
    let config = exact_config(&r.config, &pc.config, "CCA platform configuration")?;
    let mfg = exact_config(
        &r.manufacturing_config,
        &pc.manufacturing_config,
        "CCA platform manufacturing configuration",
    )?;
    let mut rotpks = 0;
    if !r.rotpks.is_empty() {
        if pc.tbb_rotpks.is_empty() {
            return Err(AttestError::ReferenceValueMissing {
                what: "CCA TBB RoTPK claim",
            });
        }
        for e in &pc.tbb_rotpks {
            let ok = r.rotpks.iter().any(|rv| {
                rv.name == e.name
                    && rv.index == e.index
                    && rv.hashes.iter().any(|h| ct_eq(h, &e.hash))
            });
            if !ok {
                return Err(AttestError::ReferenceValueMismatch {
                    what: "CCA TBB RoTPK",
                });
            }
            rotpks += 1;
        }
    }
    Ok((pc.sw_components.len(), config, mfg, rotpks))
}

fn appraise_realm(
    token: &CcaToken,
    r: &CcaRealmReference,
) -> Result<([bool; 4], bool), AttestError> {
    let rc = &token.realm_claims;
    if let Some(p) = &r.personalization
        && !ct_eq(p, &rc.personalization)
    {
        return Err(AttestError::ReferenceValueMismatch {
            what: "CCA realm personalization value",
        });
    }
    let mut constrained = [false; 4];
    for (i, want) in r.rems.iter().enumerate() {
        if let Some(w) = want {
            if !measurement_eq(&rc.extensible_measurements[i], w) {
                return Err(AttestError::ReferenceValueMismatch {
                    what: "CCA realm extensible measurement",
                });
            }
            constrained[i] = true;
        }
    }
    Ok((constrained, r.personalization.is_some()))
}

/// Appraise a verified CCA token against the store's reference values (see
/// the module documentation for the rules) with the default (strict)
/// options. The token's signatures must already have been checked
/// ([`verify_cca_with_corim`] does both).
pub fn appraise_cca(
    verified: &VerifiedCcaToken,
    store: &CcaCorimStore,
) -> Result<CcaAppraisal, AttestError> {
    appraise_cca_with_options(verified, store, &CcaAppraisalOptions::default())
}

/// [`appraise_cca`] with explicit options.
pub fn appraise_cca_with_options(
    verified: &VerifiedCcaToken,
    store: &CcaCorimStore,
    opts: &CcaAppraisalOptions,
) -> Result<CcaAppraisal, AttestError> {
    let token = &verified.token;
    let pc = &token.platform_claims;
    let candidates: Vec<usize> = (0..store.platform.len())
        .filter(|&i| ct_eq(&store.platform[i].implementation_id, &pc.implementation_id))
        .collect();
    if candidates.is_empty() {
        return Err(AttestError::ReferenceValueMissing {
            what: "CCA platform reference values for the implementation ID",
        });
    }
    let mut last = None;
    let mut platform = None;
    for i in candidates {
        match appraise_platform(token, &store.platform[i], opts) {
            Ok(r) => {
                platform = Some((i, r));
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some((pi, (sw, config, mfg, rotpks))) = platform else {
        return Err(last.expect("at least one candidate"));
    };

    let rc = &token.realm_claims;
    let candidates: Vec<usize> = (0..store.realm.len())
        .filter(|&i| measurement_eq(&rc.initial_measurement, &store.realm[i].rim))
        .collect();
    if candidates.is_empty() {
        return Err(AttestError::ReferenceValueMissing {
            what: "CCA realm reference values for the RIM",
        });
    }
    let mut last = None;
    for i in candidates {
        match appraise_realm(token, &store.realm[i]) {
            Ok((rems, rpv)) => {
                return Ok(CcaAppraisal {
                    platform_reference: pi,
                    sw_components_matched: sw,
                    config_matched: config,
                    manufacturing_config_matched: mfg,
                    rotpks_matched: rotpks,
                    realm_reference: i,
                    rems_constrained: rems,
                    personalization_matched: rpv,
                });
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one candidate"))
}

/// Verify a CCA token with the platform keys endorsed by the store's
/// attestation-key triples ([`verify_cca_endorsed`]: CPAK by instance ID,
/// implementation ID, platform and realm signatures, RAK binding), then
/// appraise it against the store's reference values ([`appraise_cca`]).
pub fn verify_cca_with_corim(
    bytes: &[u8],
    store: &CcaCorimStore,
) -> Result<(VerifiedCcaToken, CcaAppraisal), AttestError> {
    verify_cca_with_corim_options(bytes, store, &CcaAppraisalOptions::default())
}

/// [`verify_cca_with_corim`] with explicit appraisal options.
pub fn verify_cca_with_corim_options(
    bytes: &[u8],
    store: &CcaCorimStore,
    opts: &CcaAppraisalOptions,
) -> Result<(VerifiedCcaToken, CcaAppraisal), AttestError> {
    let verified = verify_cca_endorsed(bytes, &store.endorsements)?;
    let appraisal = appraise_cca_with_options(&verified, store, opts)?;
    Ok((verified, appraisal))
}
