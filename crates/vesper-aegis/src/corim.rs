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

use crate::cca::{CcaEndorsement, VerifiedCcaToken, verify_cca_endorsed, verify_sign1};
use crate::chain::EcPublicKey;
pub use crate::chain::ec_public_key_from_pem;
use crate::error::{AttestError, ParseError};
use crate::time::unix_seconds;

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

impl Corim {
    /// Parse an unsigned CoRIM (tag 501).
    pub fn parse_unsigned(bytes: &[u8]) -> Result<Self, ParseError> {
        crate::port::corim_parse(bytes)
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
        let sign1 = crate::port::cose_sign1_parse(
            bytes,
            crate::port::CBOR_SIGNED_CORIM,
            crate::port::CBOR_REASON_MALFORMED_SIGNED_COSE,
        )?;
        if sign1.content_type.as_deref() != Some(CORIM_CONTENT_TYPE) {
            return Err(ParseError::Cbor {
                field: "signed CoRIM",
                reason: "content type is not application/rim+cbor",
            }
            .into());
        }
        let meta = match &sign1.corim_meta {
            Some(m) => m,
            None => {
                return Err(ParseError::Cbor {
                    field: "signed CoRIM",
                    reason: "missing corim-meta",
                }
                .into());
            }
        };
        verify_sign1(&sign1, key, "signed CoRIM")?;
        let meta_bytes = meta.as_ref().map_err(|()| ParseError::Cbor {
            field: "corim-meta",
            reason: "expected byte string",
        })?;
        let signer = crate::port::corim_meta_parse(meta_bytes)?;
        if let Some(v) = &signer.signature_validity {
            v.check(now_ns, "signed CoRIM signature validity")?;
        }
        let payload = sign1.payload.as_deref().ok_or(ParseError::Cbor {
            field: "signed CoRIM",
            reason: "detached payload",
        })?;
        let mut corim = Self::parse_unsigned(payload)?;
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
    pub rems: [Option<Vec<u8>>; crate::cca::CCA_REMS],
    pub personalization: Option<Vec<u8>>,
}

/// Reference values and endorsed platform keys from accepted CCA CoRIMs.
#[derive(Clone, Debug, Default)]
pub struct CcaCorimStore {
    pub platform: Vec<CcaPlatformReference>,
    pub realm: Vec<CcaRealmReference>,
    pub endorsements: Vec<CcaEndorsement>,
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
        crate::port::corim_validate_cca_and_add(self, corim, now_ns)
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
    crate::port::cca_appraise_with_options(verified, store, opts)
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
