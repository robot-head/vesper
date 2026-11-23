//! The Rust attestation verifier the port replaced (test-only reference of
//! `src/differential.rs`), unchanged but for its imports: the types are
//! the crate's own.
use crate::{
    AttestationBundle, BuildIdentity, BuilderPolicy, Diagnostic, MAX_ATTESTATIONS, MAX_POLICY_KEYS,
    SCHEMA_VERSION, Statement, VerificationReport,
};
use ed25519_dalek::{Signature, VerifyingKey};
use std::collections::{BTreeMap, BTreeSet};
use vesper_loom_cas::{DerivationKey, Digest, Domain, ObjectId, ObjectKind};

const SIGNING_DOMAIN: &[u8] = b"vesper.loom.builder-attestation.v1\0";

fn label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 256
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/@".contains(&b))
}
fn hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let digit = |b| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut bytes = [0; N];
    for (out, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *out = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(bytes)
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
    if version.to_string() != identity.version || identity.version.len() > 256 {
        return Err("version must use canonical exact SemVer".into());
    }
    let source = identity
        .source_tree
        .parse::<ObjectId>()
        .map_err(|_| "invalid source object identity")?;
    if source.domain != Domain::Source || source.kind != ObjectKind::Tree {
        return Err("source_tree must be a source-domain tree object".into());
    }
    let derivation = identity
        .derivation
        .strip_prefix("drv:b3v1:")
        .and_then(Digest::from_hex)
        .map(DerivationKey)
        .ok_or("invalid derivation identity")?;
    if derivation.to_string() != identity.derivation {
        return Err("noncanonical derivation identity".into());
    }
    let output = identity
        .output
        .parse::<ObjectId>()
        .map_err(|_| "invalid output object identity")?;
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

/// Signing message: fixed domain, LE u64 payload length, compact serde JSON of
/// the validated Statement in its declared field order. JSON input ordering and
/// whitespace are not signed; unknown/duplicate fields are rejected on parsing.
/// Every identity field and the selected key ID is covered by the signature.
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
    now: u64,
    keys: BTreeSet<String>,
    operators: BTreeSet<String>,
    mut diagnostics: Vec<Diagnostic>,
) -> VerificationReport {
    diagnostics.sort();
    diagnostics.dedup();
    VerificationReport {
        schema_version: SCHEMA_VERSION,
        accepted: diagnostics.is_empty() && operators.len() >= policy.minimum_operators,
        evaluated_at: now,
        minimum_operators: policy.minimum_operators,
        verified_keys: keys,
        verified_operators: operators,
        diagnostics,
    }
}

/// Verify every supplied envelope; invalid extras reject the whole bundle.
/// Key and statement intervals are half-open. A statement's complete interval
/// must be inside its key's interval, and `now` must be inside both. No implicit
/// host clock, network, key discovery or independence inference is performed.
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
        return finish(policy, now, BTreeSet::new(), BTreeSet::new(), diagnostics);
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
        let parsed =
            hex::<32>(&builder.public_key).and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok());
        let Some(parsed) = parsed else {
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
        return finish(policy, now, BTreeSet::new(), BTreeSet::new(), diagnostics);
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
        let signature = hex::<64>(&envelope.signature).map(|bytes| Signature::from_bytes(&bytes));
        if signature.is_none_or(|signature| public_key.verify_strict(&message, &signature).is_err())
        {
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
    finish(policy, now, verified_keys, operators, diagnostics)
}
