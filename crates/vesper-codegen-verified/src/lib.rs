//! §42.5 bootstrap fixed-point and diverse double-compiling (DDC) auditor.
//!
//! This crate validates `bootstrap/stage3_ddc.lock` (§42.2), compares the
//! Path A Stage 2 / Stage 3 artifacts and the Path B (Lean reference) artifact,
//! and enforces the clean-room builder quorum through the configured-key
//! Ed25519 verifier in `vesper-loom-trust`. Every divergence is a typed
//! [`BootstrapVerifyError`].
//!
//! What this crate does not do:
//! - It does not check equational rewrite certificates. That checker belongs to
//!   the optimizer work (`src/optimize*`, `lean/RewriteWitness*`) in the root
//!   package, which a library crate may not depend on. Supplying certificates
//!   returns [`BootstrapVerifyError::RewriteCheckerNotWired`].
//! - It does not verify TEE quotes. A caller that verified a quote with
//!   `vesper-aegis` passes the quote's report data; this crate checks only that
//!   it binds the fixed-point digest.
//! - There is no self-hosted compiler yet, so no real Stage 2 / Stage 3 exists.
//!   The Stage 0 seed reproducibility evidence is recorded separately in
//!   [`seed`] (`bootstrap/stage0_seed.lock`).
#![forbid(unsafe_code)]

mod audit;
mod digest;
mod lock;
pub mod seed;

pub use audit::{
    AuditReport, BootstrapEvidence, BuilderEvidence, RewriteEvidence, TeeEvidence,
    audit_ddc_bootstrap_quorum, expected_build_identity, verify_rewrite_certificate_chain,
};
pub use digest::{Blake3Digest, source_merkle};
pub use lock::{
    ALLOWED_AXIOM_UNIVERSE, BootstrapLock, BootstrapSection, FixedPointQuorum, MAX_LOCK_BYTES,
    parse_lock,
};

/// Every way a bootstrap audit can fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootstrapVerifyError {
    /// The lock file exceeds [`MAX_LOCK_BYTES`].
    LockTooLarge { len: usize, max: usize },
    /// The lock is not UTF-8 TOML of the §42.2 shape (unknown or missing keys included).
    LockSyntax { message: String },
    /// A lock field is present but malformed.
    InvalidLockField {
        field: &'static str,
        reason: &'static str,
    },
    /// The lock records digests that are not one fixed point.
    LockNotAtFixedPoint,
    /// The compiler source tree does not hash to the locked Merkle root.
    SourceMerkleMismatch {
        locked: blake3::Hash,
        actual: blake3::Hash,
    },
    /// The Lean specification bytes do not hash to the locked MIF digest.
    LeanSpecMismatch {
        locked: blake3::Hash,
        actual: blake3::Hash,
    },
    /// The Lean certificates used an axiom the lock does not allow.
    DisallowedLeanAxiom { axiom: String },
    /// An artifact hashes to something other than its locked digest.
    TamperedDigest {
        field: &'static str,
        locked: blake3::Hash,
        actual: blake3::Hash,
    },
    /// Stage 2 and Stage 3 (Path A) are not bit-identical.
    StageFixedPointDivergence {
        stage2: blake3::Hash,
        stage3: blake3::Hash,
    },
    /// Path A Stage 3 and the Path B (Lean, non-Rust C compiler) build differ.
    DdcCrossCompilerMismatch {
        stage3_path_a: blake3::Hash,
        stage3_path_b_ddc: blake3::Hash,
    },
    /// Too few distinct configured builder operators attested the fixed point.
    InsufficientLilaBuilderQuorum { actual: usize, min_required: usize },
    /// The attestation bundle or policy was rejected; codes from `vesper-loom-trust`.
    BuilderSignatureRejected { codes: Vec<String> },
    /// The lock requires a TEE attestation and none was supplied.
    MissingTeeAttestation,
    /// The TEE report data does not bind the fixed-point digest.
    TeeReportDataMismatch,
    /// Rewrite certificates were supplied but no checker is wired into this crate.
    RewriteCheckerNotWired,
}

impl std::fmt::Display for BootstrapVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use BootstrapVerifyError::*;
        match self {
            LockTooLarge { len, max } => write!(f, "lock is {len} bytes, limit {max}"),
            LockSyntax { message } => write!(f, "lock syntax: {message}"),
            InvalidLockField { field, reason } => write!(f, "lock field {field}: {reason}"),
            LockNotAtFixedPoint => f.write_str("lock digests are not one fixed point"),
            SourceMerkleMismatch { locked, actual } => {
                write!(
                    f,
                    "source Merkle root {actual} differs from locked {locked}"
                )
            }
            LeanSpecMismatch { locked, actual } => {
                write!(f, "Lean spec digest {actual} differs from locked {locked}")
            }
            DisallowedLeanAxiom { axiom } => write!(f, "Lean axiom {axiom} is not allowed"),
            TamperedDigest {
                field,
                locked,
                actual,
            } => write!(f, "{field}: artifact {actual} differs from locked {locked}"),
            StageFixedPointDivergence { stage2, stage3 } => {
                write!(f, "Stage 2 {stage2} differs from Stage 3 {stage3}")
            }
            DdcCrossCompilerMismatch {
                stage3_path_a,
                stage3_path_b_ddc,
            } => write!(
                f,
                "Path A {stage3_path_a} differs from Path B {stage3_path_b_ddc}"
            ),
            InsufficientLilaBuilderQuorum {
                actual,
                min_required,
            } => write!(
                f,
                "{actual} builder operators attested, {min_required} required"
            ),
            BuilderSignatureRejected { codes } => {
                write!(f, "builder attestations rejected: {}", codes.join(", "))
            }
            MissingTeeAttestation => f.write_str("lock requires a TEE attestation"),
            TeeReportDataMismatch => f.write_str("TEE report data does not bind Stage 3"),
            RewriteCheckerNotWired => f.write_str("rewrite-certificate checker is not wired"),
        }
    }
}

impl std::error::Error for BootstrapVerifyError {}
