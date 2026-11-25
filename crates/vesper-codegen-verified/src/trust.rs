//! The auditor's own copy of the builder-attestation rules it checks.
//!
//! The gate used to link the frozen Rust seeds of `vesper-loom-calculus`
//! (the package-name rule), `vesper-loom-cas` (object and derivation ids) and
//! `vesper-loom-trust` (the attestation types, signing message and Ed25519
//! quorum verifier). Those seeds go when the Rust compiler retires (#1397),
//! and the auditor must not call the Vesper libraries instead: diverse
//! double-compiling needs an implementation that the compiler under audit
//! did not build. So this module carries the subset the gate uses, written
//! to the same rules. `tests/trust_vectors.rs` pins it to the seeds' outputs
//! (signing bytes, object ids, accepted and rejected bundles) through frozen
//! vectors, so a change to either side shows up as a test failure.
//!
//! Wire formats (all frozen at schema version 1):
//! - package names: `@org/name` or the reserved `std/name`, each component
//!   `[a-z][a-z0-9_-]*`;
//! - object ids: `b3v1:<domain>:<kind>:<64 lowercase hex>`, the digest a
//!   BLAKE3 `derive_key` under `vesper-loom-cas 2026-11 v1 <domain> <kind>`;
//! - derivation ids: `drv:b3v1:<64 lowercase hex>`;
//! - the signing message: `vesper.loom.builder-attestation.v1\0`, the
//!   payload's length as a little-endian u64, then the compact JSON of the
//!   [`Statement`] in declared field order.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_POLICY_KEYS: usize = 64;
pub const MAX_ATTESTATIONS: usize = 128;

const SIGNING_DOMAIN: &[u8] = b"vesper.loom.builder-attestation.v1\0";

/// A scoped package name: `@org/name`, or `std/name` for the platform.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageName(String);

impl PackageName {
    pub fn parse(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        if !package_name_valid(&name) {
            return Err(format!(
                "invalid package name '{name}'; use @org/name or reserved std/name"
            ));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PackageName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn package_name_valid(name: &str) -> bool {
    let scoped = name.strip_prefix('@');
    let mut parts = scoped.unwrap_or(name).split('/');
    let (Some(org), Some(package), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let component = |s: &str| {
        s.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    };
    component(org) && component(package) && (scoped.is_some() || org == "std")
}

/// Object domains of the content store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Source,
    Interface,
    Artifact,
}

impl Domain {
    const ALL: [Domain; 3] = [Domain::Source, Domain::Interface, Domain::Artifact];

    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Source => "source",
            Domain::Interface => "interface",
            Domain::Artifact => "artifact",
        }
    }
}

/// Object kinds of the content store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Blob,
    Tree,
}

impl ObjectKind {
    const ALL: [ObjectKind; 2] = [ObjectKind::Blob, ObjectKind::Tree];

    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
        }
    }
}

/// A store object identity: domain, kind and BLAKE3 digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId {
    pub domain: Domain,
    pub kind: ObjectKind,
    pub digest: [u8; 32],
}

impl ObjectId {
    pub fn new(domain: Domain, kind: ObjectKind, digest: [u8; 32]) -> Self {
        Self {
            domain,
            kind,
            digest,
        }
    }

    /// Strict parser of the text form; no other spelling is accepted.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let (Some("b3v1"), Some(domain), Some(kind), Some(digest), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        Some(Self {
            domain: Domain::ALL.into_iter().find(|d| d.as_str() == domain)?,
            kind: ObjectKind::ALL.into_iter().find(|k| k.as_str() == kind)?,
            digest: hex::<32>(digest)?,
        })
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "b3v1:{}:{}:{}",
            self.domain.as_str(),
            self.kind.as_str(),
            lower_hex(&self.digest)
        )
    }
}

/// The identity of `bytes` stored as a blob in `domain`.
pub fn blob_id(domain: Domain, bytes: &[u8]) -> ObjectId {
    let context = format!("vesper-loom-cas 2026-11 v1 {} blob", domain.as_str());
    let mut hasher = blake3::Hasher::new_derive_key(&context);
    hasher.update(bytes);
    ObjectId::new(domain, ObjectKind::Blob, *hasher.finalize().as_bytes())
}

/// The canonical text of a derivation id over `digest`.
pub fn derivation_id(digest: &[u8; 32]) -> String {
    format!("drv:b3v1:{}", lower_hex(digest))
}

/// Complete build identity the builders sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildIdentity {
    pub package: PackageName,
    pub version: String,
    pub source_tree: String,
    pub derivation: String,
    pub target: String,
    /// Exact builder/compiler implementation digest.
    pub builder_blake3: String,
    pub sandbox_contract: String,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub schema_version: u32,
    pub key_id: String,
    pub identity: BuildIdentity,
    pub issued_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub statement: Statement,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestationBundle {
    pub schema_version: u32,
    pub attestations: Vec<Attestation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedBuilder {
    pub key_id: String,
    /// Independence group assigned by the trusted policy maintainer.
    pub operator_id: String,
    pub public_key: String,
    pub valid_from: u64,
    pub valid_until: u64,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilderPolicy {
    pub schema_version: u32,
    pub minimum_operators: usize,
    pub max_statement_ttl: u64,
    pub builders: Vec<TrustedBuilder>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diagnostic {
    pub code: String,
    pub key_id: Option<String>,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub accepted: bool,
    pub verified_keys: BTreeSet<String>,
    pub verified_operators: BTreeSet<String>,
    pub diagnostics: Vec<Diagnostic>,
}

fn label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 256
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/@".contains(&b))
}

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exactly `2 * N` lowercase hex digits.
fn hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let text = text.as_bytes();
    if text.len() != N * 2 {
        return None;
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (digit(text[2 * i])? << 4) | digit(text[2 * i + 1])?;
    }
    Some(out)
}

fn identity_valid(identity: &BuildIdentity) -> Result<(), String> {
    if identity.package.as_str().len() > 256 {
        return Err("package identity is too long".into());
    }
    if identity.version.len() > 256
        || identity.source_tree.len() > 128
        || identity.derivation.len() > 128
        || identity.output.len() > 128
    {
        return Err("build identity field exceeds its size limit".into());
    }
    let version =
        semver::Version::parse(&identity.version).map_err(|_| "invalid exact SemVer version")?;
    if version.to_string() != identity.version {
        return Err("version must use canonical exact SemVer".into());
    }
    let source = ObjectId::parse(&identity.source_tree).ok_or("invalid source object identity")?;
    if source.domain != Domain::Source || source.kind != ObjectKind::Tree {
        return Err("source_tree must be a source-domain tree object".into());
    }
    // `hex` accepts only the canonical lowercase spelling, so a parsed
    // derivation id is canonical.
    if identity
        .derivation
        .strip_prefix("drv:b3v1:")
        .and_then(hex::<32>)
        .is_none()
    {
        return Err("invalid derivation identity".into());
    }
    let output = ObjectId::parse(&identity.output).ok_or("invalid output object identity")?;
    if output.domain != Domain::Artifact {
        return Err("output must be an artifact-domain object".into());
    }
    if hex::<32>(&identity.builder_blake3).is_none() {
        return Err("builder_blake3 must be a lowercase BLAKE3 digest".into());
    }
    if !label(&identity.target) || !label(&identity.sandbox_contract) {
        return Err("invalid target or sandbox contract identity".into());
    }
    Ok(())
}

/// The bytes a builder signs for `statement`.
pub fn canonical_signing_bytes(statement: &Statement) -> Result<Vec<u8>, String> {
    if statement.schema_version != SCHEMA_VERSION {
        return Err("unsupported statement schema".into());
    }
    if !label(&statement.key_id) {
        return Err("invalid statement key ID".into());
    }
    identity_valid(&statement.identity)?;
    if statement.expires_at <= statement.issued_at {
        return Err("statement expiry must follow issuance".into());
    }
    let payload = serde_json::to_vec(statement).map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(SIGNING_DOMAIN.len() + 8 + payload.len());
    bytes.extend_from_slice(SIGNING_DOMAIN);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

fn emit(out: &mut Vec<Diagnostic>, code: &str, key: Option<&str>, field: &str, message: &str) {
    out.push(Diagnostic {
        code: code.into(),
        key_id: key.map(str::to_owned),
        field: field.into(),
        message: message.into(),
    });
}

fn finish(
    policy: &BuilderPolicy,
    keys: BTreeSet<String>,
    operators: BTreeSet<String>,
    mut diagnostics: Vec<Diagnostic>,
) -> VerificationReport {
    diagnostics.sort();
    diagnostics.dedup();
    VerificationReport {
        accepted: diagnostics.is_empty() && operators.len() >= policy.minimum_operators,
        verified_keys: keys,
        verified_operators: operators,
        diagnostics,
    }
}

/// Verify every supplied envelope against `expected`, `policy` and `now`;
/// an invalid extra envelope rejects the whole bundle. Key and statement
/// intervals are half-open; a statement's interval must lie inside its
/// key's, and `now` inside both.
pub fn verify_attestations(
    expected: &BuildIdentity,
    bundle: &AttestationBundle,
    policy: &BuilderPolicy,
    now: u64,
) -> VerificationReport {
    let mut diagnostics = vec![];
    if let Err(error) = identity_valid(expected) {
        emit(
            &mut diagnostics,
            "invalid_expected",
            None,
            "expected",
            &error,
        );
    }
    if policy.schema_version != SCHEMA_VERSION || bundle.schema_version != SCHEMA_VERSION {
        emit(
            &mut diagnostics,
            "unsupported_schema",
            None,
            "schema_version",
            "unsupported bundle or policy schema",
        );
    }
    if !(2..=MAX_POLICY_KEYS).contains(&policy.minimum_operators) || policy.max_statement_ttl == 0 {
        emit(
            &mut diagnostics,
            "invalid_policy",
            None,
            "quorum",
            "quorum must be at least two and max_statement_ttl must be positive",
        );
    }
    if policy.builders.is_empty()
        || policy.builders.len() > MAX_POLICY_KEYS
        || bundle.attestations.len() > MAX_ATTESTATIONS
    {
        emit(
            &mut diagnostics,
            "limit_exceeded",
            None,
            "collections",
            "invalid builder count or too many attestations",
        );
        return finish(policy, BTreeSet::new(), BTreeSet::new(), diagnostics);
    }
    let mut builders = BTreeMap::new();
    let mut public_keys = BTreeSet::new();
    for builder in &policy.builders {
        if !label(&builder.key_id) || !label(&builder.operator_id) {
            emit(
                &mut diagnostics,
                "invalid_policy",
                None,
                "builder",
                "builder/operator identifiers must be bounded ASCII labels",
            );
            continue;
        }
        let key = Some(builder.key_id.as_str());
        if builder.valid_from >= builder.valid_until {
            emit(
                &mut diagnostics,
                "invalid_policy",
                key,
                "validity",
                "key validity interval must be nonempty",
            );
        }
        let Some(parsed) =
            hex::<32>(&builder.public_key).and_then(|b| VerifyingKey::from_bytes(&b).ok())
        else {
            emit(
                &mut diagnostics,
                "invalid_public_key",
                key,
                "public_key",
                "invalid lowercase Ed25519 public key",
            );
            continue;
        };
        if parsed.is_weak() {
            emit(
                &mut diagnostics,
                "invalid_public_key",
                key,
                "public_key",
                "weak Ed25519 public keys are forbidden",
            );
        }
        if !public_keys.insert(parsed.to_bytes()) {
            emit(
                &mut diagnostics,
                "duplicate_public_key",
                key,
                "public_key",
                "one public key cannot represent multiple trusted identities",
            );
        }
        if builders
            .insert(builder.key_id.as_str(), (builder, parsed))
            .is_some()
        {
            emit(
                &mut diagnostics,
                "duplicate_key_id",
                key,
                "key_id",
                "duplicate trusted key identity",
            );
        }
    }
    if !diagnostics.is_empty() {
        return finish(policy, BTreeSet::new(), BTreeSet::new(), diagnostics);
    }
    let mut seen = BTreeSet::new();
    let mut verified_keys = BTreeSet::new();
    let mut operators = BTreeSet::new();
    for envelope in &bundle.attestations {
        let statement = &envelope.statement;
        if !label(&statement.key_id) {
            emit(
                &mut diagnostics,
                "invalid_statement",
                None,
                "key_id",
                "invalid statement key identity",
            );
            continue;
        }
        let key = Some(statement.key_id.as_str());
        if !seen.insert(statement.key_id.as_str()) {
            emit(
                &mut diagnostics,
                "duplicate_attestation",
                key,
                "key_id",
                "duplicate attestation for a key",
            );
            continue;
        }
        let Some((builder, public_key)) = builders.get(statement.key_id.as_str()) else {
            emit(
                &mut diagnostics,
                "unknown_key",
                key,
                "key_id",
                "attestation key is absent from the trusted policy",
            );
            continue;
        };
        let message = match canonical_signing_bytes(statement) {
            Ok(message) => message,
            Err(error) => {
                emit(
                    &mut diagnostics,
                    "invalid_statement",
                    key,
                    "statement",
                    &error,
                );
                continue;
            }
        };
        let before = diagnostics.len();
        if statement.identity != *expected {
            emit(
                &mut diagnostics,
                "identity_mismatch",
                key,
                "identity",
                "signed build identity differs from the expected package/source/derivation/target/builder/contract/output",
            );
        }
        if builder.revoked {
            emit(
                &mut diagnostics,
                "revoked_key",
                key,
                "key_id",
                "attestation key is revoked",
            );
        }
        if now < builder.valid_from
            || now >= builder.valid_until
            || statement.issued_at < builder.valid_from
            || statement.expires_at > builder.valid_until
        {
            emit(
                &mut diagnostics,
                "key_expired_or_not_yet_valid",
                key,
                "validity",
                "key must cover issuance, expiry and evaluation time",
            );
        }
        if statement.issued_at > now || now >= statement.expires_at {
            emit(
                &mut diagnostics,
                "statement_expired_or_future",
                key,
                "validity",
                "statement is expired or issued in the future",
            );
        }
        if statement
            .expires_at
            .checked_sub(statement.issued_at)
            .is_none_or(|ttl| ttl > policy.max_statement_ttl)
        {
            emit(
                &mut diagnostics,
                "statement_ttl_exceeded",
                key,
                "validity",
                "statement exceeds maximum permitted TTL",
            );
        }
        let signature = hex::<64>(&envelope.signature).map(|b| Signature::from_bytes(&b));
        if signature.is_none_or(|s| public_key.verify_strict(&message, &s).is_err()) {
            emit(
                &mut diagnostics,
                "invalid_signature",
                key,
                "signature",
                "strict Ed25519 signature verification failed",
            );
        }
        if diagnostics.len() == before {
            verified_keys.insert(statement.key_id.clone());
            operators.insert(builder.operator_id.clone());
        }
    }
    if operators.len() < policy.minimum_operators {
        emit(
            &mut diagnostics,
            "insufficient_quorum",
            None,
            "operators",
            "too few distinct configured operators supplied valid matching attestations",
        );
    }
    finish(policy, verified_keys, operators, diagnostics)
}
