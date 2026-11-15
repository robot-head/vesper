//! Multi-objective Pareto/lexicographic dependency optimization (§10.2–§10.4).

use crate::{
    ResolveDiagnostic, ResolveError, ResolveRequest, closure, diagnostic, error, from_calculus,
    inputs, solution,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use vesper_loom_calculus::{Manifest, PackageName, Solution, validate_solution};

pub const OPTIMIZE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizeObjective {
    ZeroCve,
    MinCapabilitySurface,
    MinDupMajors,
    MinStaleness,
    MinTrustCost,
}

impl OptimizeObjective {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "zero_cve" => Ok(Self::ZeroCve),
            "min_capability_surface" => Ok(Self::MinCapabilitySurface),
            "min_dup_majors" => Ok(Self::MinDupMajors),
            "min_staleness" => Ok(Self::MinStaleness),
            "min_trust_cost" => Ok(Self::MinTrustCost),
            _ => Err(format!("unknown optimization objective '{raw}'")),
        }
    }
}

/// Caller-supplied advisory, staleness, trust, and attestation metadata for a catalog candidate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateMetadata {
    #[serde(default)]
    pub cve_count: u64,
    #[serde(default)]
    pub staleness_secs: u64,
    #[serde(default)]
    pub trust_cost: u64,
    #[serde(default)]
    pub repro_attestations: u32,
    #[serde(default)]
    pub vetted_namespace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizeRequest {
    pub resolve: ResolveRequest,
    pub objectives: Vec<OptimizeObjective>,
    pub metadata: BTreeMap<(PackageName, Version), CandidateMetadata>,
    pub max_solutions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolutionMetrics {
    pub total_cves: u64,
    pub capability_surface: u64,
    pub duplicate_majors: u64,
    pub total_staleness_secs: u64,
    pub total_trust_cost: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizedResolution {
    pub schema_version: u32,
    pub solution: Solution,
    pub metrics: SolutionMetrics,
    pub evaluated_solutions: usize,
    pub objective_order: Vec<OptimizeObjective>,
}

fn active_objectives(request: &OptimizeRequest) -> Result<Vec<OptimizeObjective>, ResolveError> {
    if !request.objectives.is_empty() {
        return Ok(request.objectives.clone());
    }
    if let Some(resolver) = &request.resolve.root.resolver
        && !resolver.policy.optimize.is_empty()
    {
        let mut parsed = Vec::with_capacity(resolver.policy.optimize.len());
        for raw in &resolver.policy.optimize {
            let obj = OptimizeObjective::parse(raw).map_err(|msg| {
                error(
                    crate::ErrorKind::UnsupportedPolicy,
                    0,
                    vec![diagnostic(
                        "unknown_optimize_objective",
                        Some(&request.resolve.root.package.name),
                        "resolver.policy.optimize",
                        msg,
                    )],
                )
            })?;
            parsed.push(obj);
        }
        return Ok(parsed);
    }
    Ok(vec![
        OptimizeObjective::ZeroCve,
        OptimizeObjective::MinCapabilitySurface,
        OptimizeObjective::MinDupMajors,
        OptimizeObjective::MinStaleness,
    ])
}

fn candidate_satisfies_trust_policy(
    root: &Manifest,
    candidate: &Manifest,
    metadata: &BTreeMap<(PackageName, Version), CandidateMetadata>,
) -> Result<(), ResolveDiagnostic> {
    let Some(resolver) = &root.resolver else {
        return Ok(());
    };
    let policy = &resolver.policy;
    let id = (
        candidate.package.name.clone(),
        candidate.package.version.clone(),
    );
    let meta = metadata.get(&id);

    if let Some(min_att) = policy.min_repro_attestations
        && min_att > 0
    {
        let observed = meta.map_or(0, |m| m.repro_attestations);
        if observed < min_att {
            return Err(diagnostic(
                "insufficient_repro_attestations",
                Some(&candidate.package.name),
                candidate.package.name.as_str(),
                format!(
                    "version {} has {observed} reproducibility attestation(s), fewer than required {min_att}",
                    candidate.package.version
                ),
            ));
        }
    }
    if policy.block_unvetted_namespaces && !meta.is_some_and(|m| m.vetted_namespace) {
        return Err(diagnostic(
            "unvetted_namespace",
            Some(&candidate.package.name),
            candidate.package.name.as_str(),
            format!(
                "version {} is in an unvetted namespace blocked by resolver.policy",
                candidate.package.version
            ),
        ));
    }
    Ok(())
}

pub fn compute_metrics(
    sol: &Solution,
    root: &PackageName,
    metadata: &BTreeMap<(PackageName, Version), CandidateMetadata>,
) -> SolutionMetrics {
    let mut total_cves = 0u64;
    let mut total_staleness_secs = 0u64;
    let mut total_trust_cost = 0u64;
    let mut distinct_caps = BTreeSet::<String>::new();
    let mut short_name_majors = BTreeMap::<&str, BTreeSet<u64>>::new();

    for selected in &sol.packages {
        let name = &selected.manifest.package.name;
        let version = &selected.manifest.package.version;
        let short = name
            .as_str()
            .split_once('/')
            .map_or(name.as_str(), |(_, pkg)| pkg);
        short_name_majors
            .entry(short)
            .or_default()
            .insert(version.major);

        if name != root {
            for cap in &selected.manifest.package.capability_ceiling {
                distinct_caps.insert(cap.clone());
            }
            for feat in &selected.features {
                if let Some(def) = selected.manifest.features.get(feat) {
                    for cap in &def.enables_effects {
                        distinct_caps.insert(cap.clone());
                    }
                }
            }
            for dep in selected.manifest.dependencies.values() {
                if !dep.optional {
                    for cap in &dep.effects {
                        distinct_caps.insert(cap.clone());
                    }
                }
            }
        }

        if let Some(meta) = metadata.get(&(name.clone(), version.clone())) {
            total_cves = total_cves.saturating_add(meta.cve_count);
            total_staleness_secs = total_staleness_secs.saturating_add(meta.staleness_secs);
            total_trust_cost = total_trust_cost.saturating_add(meta.trust_cost);
        }
    }

    let duplicate_majors = short_name_majors
        .values()
        .filter(|majors| majors.len() > 1)
        .map(|majors| (majors.len() - 1) as u64)
        .sum();

    SolutionMetrics {
        total_cves,
        capability_surface: distinct_caps.len() as u64,
        duplicate_majors,
        total_staleness_secs,
        total_trust_cost,
    }
}

fn score_key(metrics: &SolutionMetrics, order: &[OptimizeObjective]) -> Vec<u64> {
    order
        .iter()
        .map(|obj| match obj {
            OptimizeObjective::ZeroCve => metrics.total_cves,
            OptimizeObjective::MinCapabilitySurface => metrics.capability_surface,
            OptimizeObjective::MinDupMajors => metrics.duplicate_majors,
            OptimizeObjective::MinStaleness => metrics.total_staleness_secs,
            OptimizeObjective::MinTrustCost => metrics.total_trust_cost,
        })
        .collect()
}

/// Searches valid dependency graphs and returns the lexicographically optimal solution.
pub fn resolve_optimized(request: &OptimizeRequest) -> Result<OptimizedResolution, ResolveError> {
    if request.max_solutions == 0 {
        return Err(error(
            crate::ErrorKind::InvalidInput,
            0,
            vec![diagnostic(
                "invalid_max_solutions",
                None,
                "max_solutions",
                "max_solutions must be positive",
            )],
        ));
    }
    let objectives = active_objectives(request)?;
    let inputs = inputs(&request.resolve)?;
    let root = &request.resolve.root.package.name;
    let mut pending = vec![BTreeMap::from([(root.clone(), &request.resolve.root)])];
    let mut decisions = 0usize;
    let mut conflicts = BTreeSet::new();
    let mut best: Option<(Vec<u64>, Solution, SolutionMetrics)> = None;
    let mut evaluated_solutions = 0usize;

    while let Some(assigned) = pending.pop() {
        if assigned.len() > 1 {
            if decisions == request.resolve.max_decisions {
                if let Some((_, sol, metrics)) = best {
                    return Ok(OptimizedResolution {
                        schema_version: OPTIMIZE_SCHEMA_VERSION,
                        solution: sol,
                        metrics,
                        evaluated_solutions,
                        objective_order: objectives,
                    });
                }
                return Err(error(
                    crate::ErrorKind::BudgetExhausted,
                    decisions,
                    vec![diagnostic(
                        "budget_exhausted",
                        None,
                        "max_decisions",
                        "search budget exhausted; satisfiability is unknown",
                    )],
                ));
            }
            decisions += 1;
        }

        let closure = match closure(&request.resolve, &assigned) {
            Ok(c) => c,
            Err(errors) => {
                conflicts.extend(errors);
                continue;
            }
        };

        let mut branch_conflicts = vec![];
        for (name, requirements) in &closure.requirements {
            if let Some(manifest) = assigned.get(name) {
                for req in requirements {
                    if !req.matches(&manifest.package.version) {
                        branch_conflicts.push(diagnostic(
                            "version_conflict",
                            Some(name),
                            name.as_str(),
                            format!(
                                "selected {} does not satisfy {req}",
                                manifest.package.version
                            ),
                        ));
                    }
                }
            }
        }
        let partial = validate_solution(&solution(&request.resolve, &assigned, &closure, vec![]));
        branch_conflicts.extend(
            partial
                .diagnostics
                .into_iter()
                .filter(|d| {
                    !matches!(
                        d.code,
                        vesper_loom_calculus::Code::MissingDependency
                            | vesper_loom_calculus::Code::MissingSystemPackage
                    )
                })
                .map(from_calculus),
        );
        if !branch_conflicts.is_empty() {
            conflicts.extend(branch_conflicts);
            continue;
        }

        if let Some((name, requirements)) = closure
            .requirements
            .iter()
            .find(|(name, _)| !assigned.contains_key(*name))
        {
            let candidates = inputs
                .packages
                .get(name)
                .into_iter()
                .flatten()
                .filter(|m| {
                    requirements
                        .iter()
                        .all(|req| req.matches(&m.package.version))
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                conflicts.insert(diagnostic(
                    "no_candidate",
                    Some(name),
                    name.as_str(),
                    format!(
                        "no local version satisfies {}",
                        requirements
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                ));
            }
            for manifest in candidates.into_iter().rev() {
                if let Err(diag) = candidate_satisfies_trust_policy(
                    &request.resolve.root,
                    manifest,
                    &request.metadata,
                ) {
                    conflicts.insert(diag);
                    continue;
                }
                let mut next = assigned.clone();
                next.insert(name.clone(), manifest);
                pending.push(next);
            }
            continue;
        }

        let mut systems = vec![];
        let mut native_failure = false;
        for (name, requirements) in &closure.systems {
            if let Some(selection) = inputs
                .systems
                .get(name)
                .into_iter()
                .flatten()
                .find(|s| requirements.iter().all(|req| req.matches(&s.version)))
            {
                systems.push((*selection).clone());
            } else {
                native_failure = true;
                conflicts.insert(diagnostic(
                    "no_system_candidate",
                    None,
                    name,
                    format!(
                        "no local native version satisfies {}",
                        requirements
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                ));
            }
        }
        if native_failure {
            continue;
        }

        let candidate_sol = solution(&request.resolve, &assigned, &closure, systems);
        let validation = validate_solution(&candidate_sol);
        if validation.graph.is_some()
            && validation.diagnostics.is_empty()
            && validation.unused_selections.is_empty()
        {
            evaluated_solutions += 1;
            let metrics = compute_metrics(&candidate_sol, root, &request.metadata);
            let key = score_key(&metrics, &objectives);
            let replace = best.as_ref().is_none_or(|(best_key, _, _)| key < *best_key);
            if replace {
                best = Some((key, candidate_sol, metrics));
            }
            if evaluated_solutions >= request.max_solutions {
                break;
            }
        } else {
            conflicts.extend(validation.diagnostics.into_iter().map(from_calculus));
        }
    }

    if let Some((_, sol, metrics)) = best {
        Ok(OptimizedResolution {
            schema_version: OPTIMIZE_SCHEMA_VERSION,
            solution: sol,
            metrics,
            evaluated_solutions,
            objective_order: objectives,
        })
    } else {
        Err(error(
            crate::ErrorKind::Unsatisfiable,
            decisions,
            conflicts.into_iter().collect(),
        ))
    }
}
