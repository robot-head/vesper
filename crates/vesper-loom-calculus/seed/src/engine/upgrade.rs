//! The Rust upgrade guard, frozen for the compiler's closure (see
//! `super`).

use crate::*;
use std::collections::{BTreeMap, BTreeSet};

fn finish(
    mut diagnostics: Vec<UpgradeDiagnostic>,
    mut changes: Vec<AuthorityChange>,
) -> UpgradeReport {
    diagnostics.sort();
    diagnostics.dedup();
    changes.sort();
    changes.dedup();
    UpgradeReport {
        schema_version: UPGRADE_SCHEMA_VERSION,
        allowed: diagnostics.is_empty(),
        diagnostics,
        changes,
    }
}

fn validation_diagnostics(
    snapshot: UpgradeSnapshot,
    validation: &Validation,
    diagnostics: &mut Vec<UpgradeDiagnostic>,
) {
    for item in &validation.diagnostics {
        diagnostics.push(UpgradeDiagnostic {
            code: "invalid_snapshot".into(),
            package: item.package.clone(),
            dependency: None,
            snapshot: Some(snapshot),
            message: format!("{:?} at {}: {}", item.code, item.subject, item.message),
        });
    }
    if validation.graph.is_none() && validation.diagnostics.is_empty() {
        diagnostics.push(UpgradeDiagnostic {
            code: "invalid_snapshot".into(),
            package: None,
            dependency: None,
            snapshot: Some(snapshot),
            message: "snapshot validation produced no graph".into(),
        });
    }
}

fn sets(
    scope: AuthorityScope,
    package: &PackageName,
    dependency: Option<&PackageName>,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    diagnostics: &mut Vec<UpgradeDiagnostic>,
    changes: &mut Vec<AuthorityChange>,
) {
    if before == after {
        return;
    }
    let added = after.difference(before).cloned().collect::<BTreeSet<_>>();
    let removed = before.difference(after).cloned().collect();
    if !added.is_empty() {
        diagnostics.push(UpgradeDiagnostic {
            code: "capability_escalation".into(),
            package: Some(package.clone()),
            dependency: dependency.cloned(),
            snapshot: None,
            message: format!(
                "{scope:?} adds declared capabilities: {}",
                added.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        });
    }
    changes.push(AuthorityChange {
        scope,
        package: package.clone(),
        dependency: dependency.cloned(),
        before: before.clone(),
        after: after.clone(),
        added,
        removed,
        before_forbid_unsafe_ffi: None,
        after_forbid_unsafe_ffi: None,
        unsafe_ffi_relaxed: false,
    });
}

fn edge_ceilings(graph: &PackageGraph) -> BTreeMap<(PackageName, PackageName), &BTreeSet<String>> {
    graph
        .packages
        .iter()
        .flat_map(|(name, node)| {
            node.dependencies.iter().map(move |edge| {
                (
                    (name.clone(), edge.package.clone()),
                    &edge.allowed_capabilities,
                )
            })
        })
        .collect()
}

pub(crate) fn check_upgrade(baseline: &Solution, candidate: &Solution) -> UpgradeReport {
    let before = validate_solution(baseline);
    let after = validate_solution(candidate);
    let mut diagnostics = vec![];
    validation_diagnostics(UpgradeSnapshot::Baseline, &before, &mut diagnostics);
    validation_diagnostics(UpgradeSnapshot::Candidate, &after, &mut diagnostics);
    if !diagnostics.is_empty() {
        return finish(diagnostics, vec![]);
    }
    let (Some(before), Some(after)) = (before.graph, after.graph) else {
        diagnostics.push(UpgradeDiagnostic {
            code: "invalid_snapshot".into(),
            package: None,
            dependency: None,
            snapshot: None,
            message: "snapshot validation did not produce both comparison graphs".into(),
        });
        return finish(diagnostics, vec![]);
    };
    if before.roots.is_empty() || before.roots != after.roots {
        diagnostics.push(UpgradeDiagnostic {
            code: "root_mismatch".into(),
            package: None,
            dependency: None,
            snapshot: None,
            message: "baseline and candidate must have the same nonempty root identities".into(),
        });
        return finish(diagnostics, vec![]);
    }
    let mut changes = vec![];
    let empty = BTreeSet::new();
    let packages = before
        .packages
        .keys()
        .chain(after.packages.keys())
        .collect::<BTreeSet<_>>();
    for name in packages {
        let old = before.packages.get(name);
        let new = after.packages.get(name);
        sets(
            AuthorityScope::RequiredCapabilities,
            name,
            None,
            old.map(|p| &p.required_capabilities).unwrap_or(&empty),
            new.map(|p| &p.required_capabilities).unwrap_or(&empty),
            &mut diagnostics,
            &mut changes,
        );
        sets(
            AuthorityScope::CapabilityCeiling,
            name,
            None,
            old.map(|p| &p.capability_ceiling).unwrap_or(&empty),
            new.map(|p| &p.capability_ceiling).unwrap_or(&empty),
            &mut diagnostics,
            &mut changes,
        );
        if let Some(new) = new {
            let old_forbid = old.map(|p| p.forbid_unsafe_ffi).unwrap_or(true);
            if old_forbid != new.forbid_unsafe_ffi {
                let relaxed = !new.forbid_unsafe_ffi;
                changes.push(AuthorityChange {
                    scope: AuthorityScope::UnsafeFfiPolicy,
                    package: name.clone(),
                    dependency: None,
                    before: empty.clone(),
                    after: empty.clone(),
                    added: empty.clone(),
                    removed: empty.clone(),
                    before_forbid_unsafe_ffi: Some(old_forbid),
                    after_forbid_unsafe_ffi: Some(new.forbid_unsafe_ffi),
                    unsafe_ffi_relaxed: relaxed,
                });
                if relaxed {
                    diagnostics.push(UpgradeDiagnostic {
                        code: "unsafe_ffi_policy_relaxed".into(),
                        package: Some(name.clone()),
                        dependency: None,
                        snapshot: None,
                        message: "candidate no longer forbids unsafe FFI for this package".into(),
                    });
                }
            }
        }
    }
    let old_edges = edge_ceilings(&before);
    let new_edges = edge_ceilings(&after);
    for edge in old_edges
        .keys()
        .chain(new_edges.keys())
        .collect::<BTreeSet<_>>()
    {
        sets(
            AuthorityScope::DependencyCeiling,
            &edge.0,
            Some(&edge.1),
            old_edges.get(edge).copied().unwrap_or(&empty),
            new_edges.get(edge).copied().unwrap_or(&empty),
            &mut diagnostics,
            &mut changes,
        );
    }
    finish(diagnostics, changes)
}
