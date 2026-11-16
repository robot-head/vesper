//! Anti-slopsquatting, identifier-conflation, namespace age, and trust-cost screening (§10.1–10.4).

use serde::{Deserialize, Serialize};
use vesper_loom_calculus::PackageName;

pub const SLOPSQUAT_SCHEMA_VERSION: u32 = 1;
pub const MAX_PACKAGE_NAME_LEN: usize = 256;

/// Catalog metadata evaluated when admitting a dependency into `Loom.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub package: PackageName,
    /// Unix epoch seconds when the publisher namespace was created.
    pub org_created_at: u64,
    /// Caller-supplied maintainer trust-cost score (lower is more trusted).
    pub trust_cost: u32,
    /// Whether the catalog entry carries a verified keyless OIDC/Rekor provenance record.
    pub attested_oidc: bool,
}

/// Configurable anti-slopsquatting and namespace vetting thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantinePolicy {
    pub schema_version: u32,
    pub max_edit_distance: usize,
    pub min_namespace_age_secs: u64,
    pub max_trust_cost: u32,
    pub require_oidc_provenance: bool,
    /// Mirrors `VESPER_AGENT_MODE=1`: hard-rejects quarantined candidates.
    pub agent_mode: bool,
}

impl Default for QuarantinePolicy {
    fn default() -> Self {
        Self {
            schema_version: SLOPSQUAT_SCHEMA_VERSION,
            max_edit_distance: 2,
            min_namespace_age_secs: 30 * 24 * 60 * 60,
            max_trust_cost: 50,
            require_oidc_provenance: true,
            agent_mode: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    MissingFromCatalog,
    OversizedPackageName,
    EditDistanceTyposquat,
    IdentifierConflation,
    StdNamespaceImpersonation,
    ImmatureNamespace,
    ExcessiveTrustCost,
    MissingOidcProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineDecision {
    Admit,
    Quarantined,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineFinding {
    pub reason: QuarantineReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similar_to: Option<PackageName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_distance: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineReport {
    pub schema_version: u32,
    pub candidate: PackageName,
    pub decision: QuarantineDecision,
    pub findings: Vec<QuarantineFinding>,
}

/// Splits `@org/name` or `std/name` into `(namespace, short_name)`.
pub fn split_package(name: &PackageName) -> (&str, &str) {
    let raw = name.as_str().strip_prefix('@').unwrap_or(name.as_str());
    raw.split_once('/').unwrap_or((raw, ""))
}

/// Normalizes delimiter swaps (`-`, `_`) and common ASCII lookalikes (`0/o`, `1/l`, `3/e`, `4/a`, `5/s`, `7/t`).
pub fn canonical_conflation_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        let mapped = match b.to_ascii_lowercase() {
            b'-' | b'_' => continue,
            b'0' => b'o',
            b'1' => b'l',
            b'3' => b'e',
            b'4' => b'a',
            b'5' => b's',
            b'7' => b't',
            other => other,
        };
        out.push(char::from(mapped));
    }
    out
}

/// Computes optimal string alignment (restricted Damerau-Levenshtein) distance over ASCII package strings
/// using O(min(|a|, |b|)) memory.
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    damerau_levenshtein_inner(a, b, None).expect("unbounded distance always returns Some")
}

fn bounded_damerau_levenshtein(a: &str, b: &str, max_dist: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max_dist {
        return None;
    }
    damerau_levenshtein_inner(a, b, Some(max_dist))
}

fn damerau_levenshtein_inner(a: &str, b: &str, max_dist: Option<usize>) -> Option<usize> {
    let mut a = a.as_bytes();
    let mut b = b.as_bytes();
    if a.len() < b.len() {
        std::mem::swap(&mut a, &mut b);
    }
    let n = a.len();
    let m = b.len();
    if m == 0 {
        return max_dist.map_or(Some(n), |limit| (n <= limit).then_some(n));
    }
    let width = m + 1;
    let mut prev2 = vec![0usize; width];
    let mut prev = (0..width).collect::<Vec<_>>();
    let mut curr = vec![0usize; width];
    let mut prev_row_min = 0usize;

    for i in 1..=n {
        curr[0] = i;
        let mut row_min = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let del = prev[j] + 1;
            let ins = curr[j - 1] + 1;
            let sub = prev[j - 1] + cost;
            let mut best = del.min(ins).min(sub);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                let trans = prev2[j - 2] + 1;
                best = best.min(trans);
            }
            curr[j] = best;
            row_min = row_min.min(best);
        }
        if let Some(limit) = max_dist
            && row_min > limit
            && prev_row_min > limit
        {
            return None;
        }
        prev_row_min = row_min;
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut curr);
    }
    let dist = prev[m];
    max_dist.map_or(Some(dist), |limit| (dist <= limit).then_some(dist))
}

fn is_std_impersonation(org: &str, candidate: &PackageName) -> bool {
    if candidate.as_str().starts_with("std/") {
        return false;
    }
    let key = canonical_conflation_key(org);
    key == "std"
        || key.starts_with("std") && key.len() <= 8
        || key == "vesperstd"
        || key == "officialstd"
}

/// Evaluates whether a package candidate may be added directly, must be quarantined, or must be rejected.
pub fn screen_package_admission(
    candidate: &PackageName,
    catalog: &[CatalogEntry],
    policy: &QuarantinePolicy,
    now_secs: u64,
) -> QuarantineReport {
    let mut findings = Vec::new();
    let (cand_org, cand_pkg) = split_package(candidate);
    let candidate_oversized = candidate.as_str().len() > MAX_PACKAGE_NAME_LEN;
    if candidate_oversized {
        findings.push(QuarantineFinding {
            reason: QuarantineReason::OversizedPackageName,
            similar_to: None,
            edit_distance: None,
            message: format!(
                "package name '{}' ({} bytes) exceeds maximum supported length {}",
                candidate.as_str(),
                candidate.as_str().len(),
                MAX_PACKAGE_NAME_LEN
            ),
        });
    }

    let exact_entry = catalog.iter().find(|e| &e.package == candidate);
    if exact_entry.is_none() {
        findings.push(QuarantineFinding {
            reason: QuarantineReason::MissingFromCatalog,
            similar_to: None,
            edit_distance: None,
            message: format!(
                "package '{}' is absent from the verified catalog",
                candidate.as_str()
            ),
        });
    }

    if is_std_impersonation(cand_org, candidate) {
        findings.push(QuarantineFinding {
            reason: QuarantineReason::StdNamespaceImpersonation,
            similar_to: None,
            edit_distance: None,
            message: format!(
                "namespace '@{cand_org}' resembles the reserved 'std/' platform namespace"
            ),
        });
    }

    if !candidate_oversized {
        let cand_full = format!("{cand_org}/{cand_pkg}");
        let cand_full_key = canonical_conflation_key(&cand_full);
        let cand_org_key = canonical_conflation_key(cand_org);
        let cand_pkg_key = canonical_conflation_key(cand_pkg);

        for other in catalog {
            if &other.package == candidate || other.package.as_str().len() > MAX_PACKAGE_NAME_LEN {
                continue;
            }
            let (other_org, other_pkg) = split_package(&other.package);
            let other_full = format!("{other_org}/{other_pkg}");
            let other_full_key = canonical_conflation_key(&other_full);
            let other_org_key = canonical_conflation_key(other_org);
            let other_pkg_key = canonical_conflation_key(other_pkg);

            let conflated = (cand_full_key == other_full_key && cand_full != other_full)
                || (cand_pkg_key == other_pkg_key
                    && cand_org_key == other_org_key
                    && cand_full != other_full)
                || (cand_pkg == other_pkg
                    && cand_org_key == other_org_key
                    && cand_org != other_org);
            if conflated {
                findings.push(QuarantineFinding {
                    reason: QuarantineReason::IdentifierConflation,
                    similar_to: Some(other.package.clone()),
                    edit_distance: Some(damerau_levenshtein(&cand_full, &other_full)),
                    message: format!(
                        "candidate '{}' conflates delimiter or lookalike characters with '{}'",
                        candidate.as_str(),
                        other.package.as_str()
                    ),
                });
                continue;
            }

            let pkg_dist = if cand_org == other_org {
                bounded_damerau_levenshtein(cand_pkg, other_pkg, policy.max_edit_distance)
            } else {
                None
            };
            let org_dist = if cand_pkg == other_pkg {
                bounded_damerau_levenshtein(cand_org, other_org, policy.max_edit_distance)
            } else {
                None
            };
            let full_dist =
                bounded_damerau_levenshtein(&cand_full, &other_full, policy.max_edit_distance);
            let near_match = pkg_dist.is_some_and(|d| d > 0)
                || org_dist.is_some_and(|d| d > 0)
                || full_dist.is_some_and(|d| d > 0);
            if near_match {
                let reported = if cand_org == other_org {
                    pkg_dist
                        .filter(|&d| d > 0)
                        .or(full_dist)
                        .unwrap_or_default()
                } else {
                    full_dist
                        .filter(|&d| d > 0)
                        .or(org_dist)
                        .unwrap_or_default()
                };
                findings.push(QuarantineFinding {
                    reason: QuarantineReason::EditDistanceTyposquat,
                    similar_to: Some(other.package.clone()),
                    edit_distance: Some(reported),
                    message: format!(
                        "candidate '{}' is within edit distance {reported} of existing package '{}'",
                        candidate.as_str(),
                        other.package.as_str()
                    ),
                });
            }
        }
    }

    if let Some(entry) = exact_entry {
        let immature = now_secs
            .checked_sub(entry.org_created_at)
            .is_none_or(|age| age < policy.min_namespace_age_secs);
        if immature {
            findings.push(QuarantineFinding {
                reason: QuarantineReason::ImmatureNamespace,
                similar_to: None,
                edit_distance: None,
                message: format!(
                    "namespace for '{}' has not reached minimum age {}s",
                    candidate.as_str(),
                    policy.min_namespace_age_secs
                ),
            });
        }
        if entry.trust_cost > policy.max_trust_cost {
            findings.push(QuarantineFinding {
                reason: QuarantineReason::ExcessiveTrustCost,
                similar_to: None,
                edit_distance: None,
                message: format!(
                    "maintainer trust cost {} exceeds policy maximum {}",
                    entry.trust_cost, policy.max_trust_cost
                ),
            });
        }
        if policy.require_oidc_provenance && !entry.attested_oidc {
            findings.push(QuarantineFinding {
                reason: QuarantineReason::MissingOidcProvenance,
                similar_to: None,
                edit_distance: None,
                message: format!(
                    "package '{}' lacks verified keyless OIDC provenance",
                    candidate.as_str()
                ),
            });
        }
    }

    let hard_reject = findings.iter().any(|f| {
        matches!(
            f.reason,
            QuarantineReason::MissingFromCatalog
                | QuarantineReason::OversizedPackageName
                | QuarantineReason::MissingOidcProvenance
        )
    });
    let decision = if findings.is_empty() {
        QuarantineDecision::Admit
    } else if policy.agent_mode || hard_reject {
        QuarantineDecision::Rejected
    } else {
        QuarantineDecision::Quarantined
    };

    QuarantineReport {
        schema_version: SLOPSQUAT_SCHEMA_VERSION,
        candidate: candidate.clone(),
        decision,
        findings,
    }
}
