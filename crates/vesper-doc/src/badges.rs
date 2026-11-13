//! Proof-backed badges (§40.1 item 4) computed only from `vesper proof-audit
//! --json` reports, never from a hand-written flag.
//!
//! A documented item names the theorems that back it with `@proof(Name)`
//! lines in its doc comment. The anchor alone grants nothing:
//!
//! - **proved**: every anchored theorem appears in an accepted, kernel-replayed
//!   audit with an empty axiom footprint;
//! - **audited**: every anchored theorem appears in an accepted, kernel-replayed
//!   audit, and at least one depends on axioms that audit's explicit policy
//!   allowed (never `sorryAx`);
//! - **unverified**: anything else, with the reason.
//!
//! A badge reports a Lean kernel audit of named theorems about a Lean model.
//! It is not a theorem about the Rust compiler or the documented Vesper code,
//! and the audit is local, not a provenance certificate.
use crate::Document;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_AUDIT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ANCHORS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditedTheorem {
    pub name: String,
    pub axioms: Vec<String>,
    pub type_repr: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleDigest {
    pub name: String,
    pub scope: String,
    pub files: Vec<FileDigest>,
}
/// The complete `vesper proof-audit --json` success schema (version 1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofAudit {
    pub schema_version: u32,
    pub status: String,
    pub accepted: bool,
    pub kernel_replayed: bool,
    pub cache_eligible: bool,
    pub module: String,
    pub lean_version: String,
    pub lean_binary_blake3: String,
    pub auditor_blake3: String,
    pub artifact_inventory_blake3: String,
    pub checked_constants: u64,
    pub allowed_axioms: BTreeSet<String>,
    pub forbidden_axioms: BTreeSet<String>,
    pub theorems: Vec<AuditedTheorem>,
    pub artifact_files: Vec<FileDigest>,
    pub imports: Vec<ModuleDigest>,
}

/// Parse one audit report. Error reports, other schemas and internally
/// inconsistent reports are rejected rather than downgraded.
pub fn parse_audit(text: &str) -> Result<ProofAudit, String> {
    if text.len() > MAX_AUDIT_BYTES {
        return Err("proof audit report exceeds 8 MiB".into());
    }
    let audit: ProofAudit =
        serde_json::from_str(text).map_err(|e| format!("invalid proof audit report: {e}"))?;
    if audit.schema_version != 1 || audit.status != "audited" {
        return Err("proof audit report must be a schema 1 'audited' result".into());
    }
    let mut names = BTreeSet::new();
    let mut forbidden = BTreeSet::new();
    for theorem in &audit.theorems {
        if !names.insert(&theorem.name) {
            return Err(format!("duplicate audited theorem {}", theorem.name));
        }
        for axiom in &theorem.axioms {
            if axiom == "sorryAx" || !audit.allowed_axioms.contains(axiom) {
                forbidden.insert(axiom.clone());
            }
        }
    }
    if audit.allowed_axioms.contains("sorryAx")
        || forbidden != audit.forbidden_axioms
        || audit.accepted != forbidden.is_empty()
    {
        return Err("proof audit report verdict is inconsistent with its theorems".into());
    }
    Ok(audit)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeLevel {
    Proved,
    Audited,
    Unverified,
}
impl BadgeLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Proved => "proved",
            Self::Audited => "audited",
            Self::Unverified => "unverified",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub theorem: String,
    pub axioms: Vec<String>,
    pub module: String,
    pub lean_version: String,
    pub artifact_inventory_blake3: String,
    pub auditor_blake3: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Badge {
    pub item: String,
    pub kind: String,
    pub anchors: Vec<String>,
    pub level: BadgeLevel,
    pub reason: String,
    pub evidence: Vec<Evidence>,
}

fn lean_name(name: &str) -> bool {
    name.len() <= 1024
        && name.split('.').all(|part| {
            !part.is_empty()
                && (part.as_bytes()[0].is_ascii_alphabetic() || part.starts_with('_'))
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'\'')
        })
}

/// `@proof(Qualified.name)` anchors on their own doc lines, in order.
pub fn proof_anchors(docs: &str) -> Result<Vec<String>, String> {
    let mut anchors = Vec::new();
    for line in docs.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("@proof(") else {
            if line.contains("@proof(") {
                return Err(format!("@proof anchor must be on its own line: '{line}'"));
            }
            continue;
        };
        let name = rest
            .strip_suffix(')')
            .ok_or_else(|| format!("unterminated @proof anchor: '{line}'"))?;
        if !lean_name(name) {
            return Err(format!("invalid @proof theorem name '{name}'"));
        }
        if !anchors.iter().any(|a| a == name) {
            anchors.push(name.to_string());
        }
        if anchors.len() > MAX_ANCHORS {
            return Err("too many @proof anchors on one item".into());
        }
    }
    Ok(anchors)
}

fn badge(item: &str, kind: &str, docs: &str, audits: &[ProofAudit]) -> Result<Badge, String> {
    let anchors = proof_anchors(docs).map_err(|e| format!("{item}: {e}"))?;
    let mut result = Badge {
        item: item.into(),
        kind: kind.into(),
        anchors: anchors.clone(),
        level: BadgeLevel::Unverified,
        reason: String::new(),
        evidence: Vec::new(),
    };
    if anchors.is_empty() {
        result.reason = "no @proof anchor".into();
        return Ok(result);
    }
    let mut any_axiom = false;
    for anchor in &anchors {
        // Accepted, kernel-replayed evidence wins regardless of report order;
        // a rejected report only explains an otherwise unverified anchor.
        let lookup = |eligible: bool| {
            audits
                .iter()
                .filter(|a| (a.accepted && a.kernel_replayed) == eligible)
                .find_map(|a| {
                    a.theorems
                        .iter()
                        .find(|t| t.name == *anchor)
                        .map(|t| (a, t))
                })
        };
        let Some((audit, theorem)) = lookup(true) else {
            result.reason = match lookup(false) {
                Some((audit, _)) => format!(
                    "theorem {anchor} audit was rejected: axioms {:?} exceed its policy",
                    audit.forbidden_axioms
                ),
                None => format!("theorem {anchor} is not in any supplied proof audit"),
            };
            result.evidence.clear();
            return Ok(result);
        };
        any_axiom |= !theorem.axioms.is_empty();
        result.evidence.push(Evidence {
            theorem: theorem.name.clone(),
            axioms: theorem.axioms.clone(),
            module: audit.module.clone(),
            lean_version: audit.lean_version.clone(),
            artifact_inventory_blake3: audit.artifact_inventory_blake3.clone(),
            auditor_blake3: audit.auditor_blake3.clone(),
        });
    }
    if any_axiom {
        result.level = BadgeLevel::Audited;
        result.reason =
            "kernel-replayed; depends on axioms the audit policy explicitly allowed".into();
    } else {
        result.level = BadgeLevel::Proved;
        result.reason = "kernel-replayed with no axioms".into();
    }
    Ok(result)
}

/// One badge per documented function and type, in document order.
pub fn badges(document: &Document, audits: &[ProofAudit]) -> Result<Vec<Badge>, String> {
    let mut out = Vec::new();
    for f in &document.functions {
        out.push(badge(&f.name, "function", &f.docs, audits)?);
    }
    for t in &document.types {
        out.push(badge(&t.declaration.name, "type", &t.docs, audits)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_exact_lines_with_lean_names() {
        assert_eq!(
            proof_anchors("Docs.\n@proof(A.b)\n @proof(A.c') \n@proof(A.b)").unwrap(),
            ["A.b", "A.c'"]
        );
        assert!(proof_anchors("see @proof(A.b) inline").is_err());
        assert!(proof_anchors("@proof(A..b)").is_err());
        assert!(proof_anchors("@proof(A.b").is_err());
        assert!(proof_anchors("no anchors").unwrap().is_empty());
    }
}
