//! `bootstrap/stage3_ddc.lock` (§42.2): parsing, validation and the canonical writer.

use crate::BootstrapVerifyError;
use crate::digest::Blake3Digest;
use serde::Deserialize;

/// Largest accepted lock file.
pub const MAX_LOCK_BYTES: usize = 64 * 1024;
/// Axioms a lock may allow; anything else is rejected while parsing.
pub const ALLOWED_AXIOM_UNIVERSE: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];
/// Upper bound on `lila_builder_signatures` (the trust policy's key limit).
const MAX_QUORUM: usize = vesper_loom_trust::MAX_POLICY_KEYS;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLock {
    bootstrap: RawBootstrap,
    fixed_point_quorum: RawQuorum,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBootstrap {
    compiler_package: String,
    version: String,
    source_merkle_blake3: String,
    lean_spec_mif: String,
    allowed_lean_axioms: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuorum {
    path_a_stage2_blake3: String,
    path_a_stage3_blake3: String,
    path_b_lean_compcert_ddc: String,
    lila_builder_signatures: i64,
    aegis_tdx_snp_attested: bool,
}

/// The validated `[bootstrap]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapSection {
    pub compiler_package: String,
    pub version: String,
    pub source_merkle: blake3::Hash,
    pub lean_spec: blake3::Hash,
    pub allowed_lean_axioms: Vec<String>,
}

/// The validated `[fixed_point_quorum]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedPointQuorum {
    pub path_a_stage2: blake3::Hash,
    pub path_a_stage3: blake3::Hash,
    pub path_b_lean_ddc: blake3::Hash,
    pub lila_builder_signatures: usize,
    pub aegis_tdx_snp_attested: bool,
}

/// A validated §42.2 bootstrap lock. Construction goes through [`parse_lock`]
/// or [`BootstrapLock::new`], both of which enforce the same invariants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapLock {
    pub bootstrap: BootstrapSection,
    pub quorum: FixedPointQuorum,
}

fn invalid(field: &'static str, reason: &'static str) -> BootstrapVerifyError {
    BootstrapVerifyError::InvalidLockField { field, reason }
}

impl BootstrapLock {
    /// Validate an in-memory lock with the same rules as [`parse_lock`].
    pub fn new(
        bootstrap: BootstrapSection,
        quorum: FixedPointQuorum,
    ) -> Result<Self, BootstrapVerifyError> {
        let lock = Self { bootstrap, quorum };
        lock.validate()?;
        Ok(lock)
    }

    pub(crate) fn validate(&self) -> Result<(), BootstrapVerifyError> {
        let b = &self.bootstrap;
        vesper_loom_calculus::PackageName::parse(b.compiler_package.clone())
            .map_err(|_| invalid("compiler_package", "not a scoped package name"))?;
        if b.version.is_empty() || b.version.len() > 64 || !canonical_semver(&b.version) {
            return Err(invalid(
                "version",
                "not a canonical MAJOR.MINOR.PATCH version",
            ));
        }
        if b.allowed_lean_axioms.len() > ALLOWED_AXIOM_UNIVERSE.len() {
            return Err(invalid("allowed_lean_axioms", "too many axioms"));
        }
        for (i, axiom) in b.allowed_lean_axioms.iter().enumerate() {
            if !ALLOWED_AXIOM_UNIVERSE.contains(&axiom.as_str()) {
                return Err(invalid(
                    "allowed_lean_axioms",
                    "axiom outside the standard set",
                ));
            }
            if b.allowed_lean_axioms[..i].contains(axiom) {
                return Err(invalid("allowed_lean_axioms", "duplicate axiom"));
            }
        }
        let q = &self.quorum;
        if !(2..=MAX_QUORUM).contains(&q.lila_builder_signatures) {
            return Err(invalid(
                "lila_builder_signatures",
                "quorum must be between 2 and 64 independent builders",
            ));
        }
        if q.path_a_stage2 != q.path_a_stage3 || q.path_a_stage3 != q.path_b_lean_ddc {
            return Err(BootstrapVerifyError::LockNotAtFixedPoint);
        }
        Ok(())
    }

    /// Serialize in the §42.2 layout. `parse_lock(lock.to_toml())` round-trips.
    pub fn to_toml(&self) -> String {
        let b = &self.bootstrap;
        let q = &self.quorum;
        let axioms = b
            .allowed_lean_axioms
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "# bootstrap/stage3_ddc.lock — Pinned Hermetic Fixed-Point & Diverse Double-Compile Anchor\n\
             [bootstrap]\n\
             compiler_package     = \"{}\"\n\
             version              = \"{}\"\n\
             source_merkle_blake3 = \"blake3:{}\"\n\
             lean_spec_mif        = \"mif:blake3:{}\"\n\
             allowed_lean_axioms  = [{axioms}]\n\
             [fixed_point_quorum]\n\
             path_a_stage2_blake3      = \"blake3:{}\"\n\
             path_a_stage3_blake3      = \"blake3:{}\"\n\
             path_b_lean_compcert_ddc  = \"blake3:{}\"\n\
             lila_builder_signatures   = {}\n\
             aegis_tdx_snp_attested    = {}\n",
            b.compiler_package,
            b.version,
            b.source_merkle.to_hex(),
            b.lean_spec.to_hex(),
            q.path_a_stage2.to_hex(),
            q.path_a_stage3.to_hex(),
            q.path_b_lean_ddc.to_hex(),
            q.lila_builder_signatures,
            q.aegis_tdx_snp_attested,
        )
    }
}

fn canonical_semver(text: &str) -> bool {
    let parts: Vec<_> = text.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 20
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
                && p.parse::<u64>().is_ok()
        })
}

/// Parse and validate a §42.2 lock. Unknown keys, missing keys, malformed
/// digests, non-standard axioms, an out-of-range quorum and digests that are
/// not one fixed point are all typed errors.
pub fn parse_lock(bytes: &[u8]) -> Result<BootstrapLock, BootstrapVerifyError> {
    if bytes.len() > MAX_LOCK_BYTES {
        return Err(BootstrapVerifyError::LockTooLarge {
            len: bytes.len(),
            max: MAX_LOCK_BYTES,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| BootstrapVerifyError::LockSyntax {
        message: "lock is not UTF-8".into(),
    })?;
    let raw: RawLock = toml::from_str(text).map_err(|e| BootstrapVerifyError::LockSyntax {
        message: e.message().to_owned(),
    })?;
    let digest = |text: &str, prefix, field| Blake3Digest::parse_prefixed(text, prefix, field);
    let b = raw.bootstrap;
    let q = raw.fixed_point_quorum;
    let quorum = usize::try_from(q.lila_builder_signatures)
        .map_err(|_| invalid("lila_builder_signatures", "must be non-negative"))?;
    BootstrapLock::new(
        BootstrapSection {
            source_merkle: digest(&b.source_merkle_blake3, "blake3:", "source_merkle_blake3")?.0,
            lean_spec: digest(&b.lean_spec_mif, "mif:blake3:", "lean_spec_mif")?.0,
            compiler_package: b.compiler_package,
            version: b.version,
            allowed_lean_axioms: b.allowed_lean_axioms,
        },
        FixedPointQuorum {
            path_a_stage2: digest(&q.path_a_stage2_blake3, "blake3:", "path_a_stage2_blake3")?.0,
            path_a_stage3: digest(&q.path_a_stage3_blake3, "blake3:", "path_a_stage3_blake3")?.0,
            path_b_lean_ddc: digest(
                &q.path_b_lean_compcert_ddc,
                "blake3:",
                "path_b_lean_compcert_ddc",
            )?
            .0,
            lila_builder_signatures: quorum,
            aegis_tdx_snp_attested: q.aegis_tdx_snp_attested,
        },
    )
}
