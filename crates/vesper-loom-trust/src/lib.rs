//! Real Ed25519 verification and configured-operator quorum for offline build attestations.
//!
//! Acceptance is conditional on the caller's expected build identity, trusted key
//! policy and clock. Operator independence is configured, not discovered. This
//! module does not implement Sigstore/OIDC/TUF, authenticate source namespaces,
//! execute hermetic builds, inspect artifact bytes, or enable a cache trust bit.
//!
//! The verifier and the admission screen are the Vesper port in `vesper/`
//! (`@vesper/loom-trust`, Rust-exit SH6 lane 17, #1412): every identity,
//! policy, interval, quorum and signature rule, the signing message and the
//! screen. Ed25519 is the crypto shim, `vesper-crypto-sys`. This crate keeps
//! the serde types and the JSON parsing, and converts its values to the
//! port's trees ([`port`]).
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;
use vesper_loom_calculus::PackageName;
// The Ed25519 calls of the Vesper object resolve to this crate's shim.
extern crate vesper_crypto_sys;

mod port;
pub mod slopsquat;

#[cfg(test)]
mod differential;
#[cfg(test)]
mod reference;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_POLICY_KEYS: usize = 64;
pub const MAX_ATTESTATIONS: usize = 128;
pub const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

/// Complete identity supplied by a trusted build orchestrator, never an interface MIF alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildIdentity {
    pub package: PackageName,
    pub version: String,
    pub source_tree: String,
    pub derivation: String,
    pub target: String,
    /// Exact builder/compiler implementation digest, not its package version label.
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
    /// Explicit independence group assigned by the trusted policy maintainer.
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: String,
    pub key_id: Option<String>,
    pub field: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    pub schema_version: u32,
    pub accepted: bool,
    pub evaluated_at: u64,
    pub minimum_operators: usize,
    pub verified_keys: BTreeSet<String>,
    pub verified_operators: BTreeSet<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Signing message: fixed domain, LE u64 payload length, compact serde JSON of
/// the validated Statement in its declared field order. JSON input ordering and
/// whitespace are not signed; unknown/duplicate fields are rejected on parsing.
/// Every identity field and the selected key ID is covered by the signature.
pub fn canonical_signing_bytes(statement: &Statement) -> Result<Vec<u8>, String> {
    port::signing_bytes(statement)
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err("attestation JSON exceeds 4 MiB".into());
    }
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}
pub fn parse_identity(bytes: &[u8]) -> Result<BuildIdentity, String> {
    parse(bytes)
}
pub fn parse_policy(bytes: &[u8]) -> Result<BuilderPolicy, String> {
    parse(bytes)
}
pub fn parse_bundle(bytes: &[u8]) -> Result<AttestationBundle, String> {
    parse(bytes)
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
    port::verify(expected, bundle, policy, now)
}
