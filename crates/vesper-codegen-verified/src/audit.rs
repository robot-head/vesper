//! The §42.5 fixed-point and DDC gate over a validated lock and its evidence.

use crate::{BootstrapLock, BootstrapVerifyError};
use std::collections::BTreeSet;
use vesper_loom_calculus::PackageName;
use vesper_loom_cas::{Digest, Domain, ObjectId, ObjectKind, blob_id};
use vesper_loom_trust::{AttestationBundle, BuildIdentity, BuilderPolicy, verify_attestations};

/// TEE evidence for `aegis_tdx_snp_attested = true` locks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeeEvidence {
    /// No quote was supplied.
    None,
    /// Report data from a TDX or SEV-SNP quote that the caller already verified
    /// (for example with `vesper-aegis`). This crate does not verify quotes; it
    /// checks only that the first 32 bytes equal the fixed-point digest.
    VerifiedReportData([u8; 64]),
}

/// Equational rewrite certificates attached to the Stage 3 build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RewriteEvidence {
    /// The build carries no rewrite certificates.
    None,
    /// Encoded certificates. No checker is wired here, so these are rejected.
    Certificates(Vec<u8>),
}

/// Builder attestations, the trusted key policy and the derivation they sign.
#[derive(Clone, Debug)]
pub struct BuilderEvidence<'a> {
    pub bundle: &'a AttestationBundle,
    pub policy: &'a BuilderPolicy,
    /// `drv:b3v1:<hex>` derivation of the Stage 3 build.
    pub derivation: &'a str,
    pub target: &'a str,
    pub sandbox_contract: &'a str,
    /// Evaluation time for key and statement validity windows.
    pub now: u64,
}

/// Everything the gate inspects. Artifact bytes are hashed here, never trusted
/// from a caller-supplied digest.
#[derive(Clone, Debug)]
pub struct BootstrapEvidence<'a> {
    /// `(relative path, bytes)` of the compiler source tree.
    pub source_files: &'a [(&'a str, &'a [u8])],
    /// The Lean specification module interface bytes.
    pub lean_spec: &'a [u8],
    /// Axioms reported by `#print axioms` over the soundness certificates.
    pub lean_axioms_used: &'a [&'a str],
    pub stage2: &'a [u8],
    pub stage3: &'a [u8],
    pub path_b_ddc: &'a [u8],
    pub builders: BuilderEvidence<'a>,
    pub tee: TeeEvidence,
    pub rewrite: RewriteEvidence,
}

/// The accepted fixed point and who attested it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    pub fixed_point: blake3::Hash,
    pub verified_keys: BTreeSet<String>,
    pub verified_operators: BTreeSet<String>,
}

/// Stage 3 identity the builders must have signed: the locked package and
/// version, the source root, Stage 2 as the builder, and Stage 3 as the output.
pub fn expected_build_identity(
    lock: &BootstrapLock,
    source_root: blake3::Hash,
    stage2: blake3::Hash,
    stage3_bytes: &[u8],
    builders: &BuilderEvidence<'_>,
) -> Result<BuildIdentity, BootstrapVerifyError> {
    let package = PackageName::parse(lock.bootstrap.compiler_package.clone()).map_err(|_| {
        BootstrapVerifyError::InvalidLockField {
            field: "compiler_package",
            reason: "not a scoped package name",
        }
    })?;
    Ok(BuildIdentity {
        package,
        version: lock.bootstrap.version.clone(),
        source_tree: ObjectId::new(
            Domain::Source,
            ObjectKind::Tree,
            Digest(*source_root.as_bytes()),
        )
        .to_string(),
        derivation: builders.derivation.to_owned(),
        target: builders.target.to_owned(),
        builder_blake3: stage2.to_hex().to_string(),
        sandbox_contract: builders.sandbox_contract.to_owned(),
        output: blob_id(Domain::Artifact, stage3_bytes).to_string(),
    })
}

/// Placeholder for the Stage 4 rewrite-certificate check. The checker lives
/// in the root package's optimizer and is not reachable from a library crate.
pub fn verify_rewrite_certificate_chain(_certificates: &[u8]) -> Result<(), BootstrapVerifyError> {
    Err(BootstrapVerifyError::RewriteCheckerNotWired)
}

/// Run the whole gate. Checks run in a fixed order so each failure reports
/// its own error: source, Lean spec and axioms; Stage 2 = Stage 3; Path A =
/// Path B; artifacts against the lock; builder quorum; TEE binding; rewrite hook.
pub fn audit_ddc_bootstrap_quorum(
    lock: &BootstrapLock,
    evidence: &BootstrapEvidence<'_>,
) -> Result<AuditReport, BootstrapVerifyError> {
    let source_root = crate::source_merkle(evidence.source_files)?;
    if source_root != lock.bootstrap.source_merkle {
        return Err(BootstrapVerifyError::SourceMerkleMismatch {
            locked: lock.bootstrap.source_merkle,
            actual: source_root,
        });
    }
    let spec = blake3::hash(evidence.lean_spec);
    if spec != lock.bootstrap.lean_spec {
        return Err(BootstrapVerifyError::LeanSpecMismatch {
            locked: lock.bootstrap.lean_spec,
            actual: spec,
        });
    }
    if let Some(axiom) = evidence
        .lean_axioms_used
        .iter()
        .find(|a| !lock.bootstrap.allowed_lean_axioms.iter().any(|b| b == *a))
    {
        return Err(BootstrapVerifyError::DisallowedLeanAxiom {
            axiom: (*axiom).to_owned(),
        });
    }

    let stage2 = blake3::hash(evidence.stage2);
    let stage3 = blake3::hash(evidence.stage3);
    if stage2 != stage3 {
        return Err(BootstrapVerifyError::StageFixedPointDivergence { stage2, stage3 });
    }
    let ddc = blake3::hash(evidence.path_b_ddc);
    if stage3 != ddc {
        return Err(BootstrapVerifyError::DdcCrossCompilerMismatch {
            stage3_path_a: stage3,
            stage3_path_b_ddc: ddc,
        });
    }
    // The lock is a fixed point (checked at parse), so comparing one digest
    // covers all three locked fields.
    if stage3 != lock.quorum.path_a_stage3 {
        return Err(BootstrapVerifyError::TamperedDigest {
            field: "path_a_stage3_blake3",
            locked: lock.quorum.path_a_stage3,
            actual: stage3,
        });
    }

    let b = &evidence.builders;
    let expected = expected_build_identity(lock, source_root, stage2, evidence.stage3, b)?;
    let report = verify_attestations(&expected, b.bundle, b.policy, b.now);
    let required = lock
        .quorum
        .lila_builder_signatures
        .max(b.policy.minimum_operators);
    let only_quorum = report
        .diagnostics
        .iter()
        .all(|d| d.code == "insufficient_quorum");
    if !only_quorum {
        let mut codes: Vec<String> = report.diagnostics.iter().map(|d| d.code.clone()).collect();
        codes.dedup();
        return Err(BootstrapVerifyError::BuilderSignatureRejected { codes });
    }
    if !report.accepted || report.verified_operators.len() < required {
        return Err(BootstrapVerifyError::InsufficientLilaBuilderQuorum {
            actual: report.verified_operators.len(),
            min_required: required,
        });
    }

    if lock.quorum.aegis_tdx_snp_attested {
        match &evidence.tee {
            TeeEvidence::None => return Err(BootstrapVerifyError::MissingTeeAttestation),
            TeeEvidence::VerifiedReportData(data) if data[..32] != *stage3.as_bytes() => {
                return Err(BootstrapVerifyError::TeeReportDataMismatch);
            }
            TeeEvidence::VerifiedReportData(_) => {}
        }
    }
    if let RewriteEvidence::Certificates(certificates) = &evidence.rewrite {
        verify_rewrite_certificate_chain(certificates)?;
    }
    Ok(AuditReport {
        fixed_point: stage3,
        verified_keys: report.verified_keys,
        verified_operators: report.verified_operators,
    })
}
