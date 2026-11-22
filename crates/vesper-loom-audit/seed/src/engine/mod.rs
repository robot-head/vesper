//! The Rust audit, frozen for the compiler's closure: only the seed copy
//! vesper-loom-audit-seed builds it (bug fixes only until self-hosting Stage
//! 2); the package vesper-loom-audit runs the Vesper port in ../../vesper
//! instead (Rust-exit SH6 lane 12, #1407).

use crate::*;
use semver::Version;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vesper_loom_calculus::{PackageGraph, PackageName, Solution, validate_solution};

mod license;

/// `AffectedRange::contains`.
pub(crate) fn range_contains(range: &AffectedRange, version: &Version) -> bool {
    (range.include_prerelease || version.pre.is_empty())
        && version.cmp_precedence(&range.introduced) != Ordering::Less
        && range
            .fixed
            .as_ref()
            .is_none_or(|fixed| version.cmp_precedence(fixed) == Ordering::Less)
}

struct Work {
    remaining: usize,
}

impl Work {
    fn spend(&mut self, amount: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or_else(|| Error::Limit("logical work".into()))?;
        Ok(())
    }
}

fn native_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}

fn identity_valid(identity: &PackageIdentity) -> bool {
    match identity {
        PackageIdentity::Vesper { .. } => true,
        PackageIdentity::Native { name } => native_name(name),
    }
}

pub(crate) fn validate_catalog(catalog: &Catalog, limits: &Limits) -> Result<(), Error> {
    input::check_size(catalog, limits)?;
    let bad = |s: &str| Error::InvalidCatalog(s.into());
    if catalog.schema_version != SCHEMA_VERSION {
        return Err(bad("unsupported schema version"));
    }
    if catalog.catalog_id.is_empty() || catalog.catalog_id.len() > limits.max_string_bytes {
        return Err(bad("invalid catalog identity"));
    }
    let mut remaining = Work {
        remaining: limits.max_entries,
    };
    remaining.spend(catalog.advisories.len())?;
    remaining.spend(catalog.native_licenses.len())?;
    let mut ids = BTreeSet::new();
    for advisory in &catalog.advisories {
        if advisory.id.is_empty()
            || advisory.id.len() > limits.max_string_bytes
            || !advisory
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
            || !ids.insert(&advisory.id)
        {
            return Err(bad("empty, invalid or duplicate advisory ID"));
        }
        if !identity_valid(&advisory.package) {
            return Err(bad("invalid package identity"));
        }
        if advisory.summary.len() > limits.max_string_bytes {
            return Err(bad("advisory summary exceeds string limit"));
        }
        if advisory.affected.is_empty() {
            return Err(bad("advisory must have an affected range"));
        }
        remaining.spend(advisory.affected.len())?;
        let mut ranges = advisory.affected.iter().collect::<Vec<_>>();
        ranges.sort_by(|a, b| a.introduced.cmp_precedence(&b.introduced));
        let mut previous: Option<&AffectedRange> = None;
        for range in ranges {
            if range
                .fixed
                .as_ref()
                .is_some_and(|v| range.introduced.cmp_precedence(v) != Ordering::Less)
            {
                return Err(bad("affected interval must be nonempty and increasing"));
            }
            if previous.is_some_and(|prev| {
                prev.fixed.as_ref().is_none_or(|fixed| {
                    fixed.cmp_precedence(&range.introduced) == Ordering::Greater
                })
            }) {
                return Err(bad("overlapping or duplicate affected intervals"));
            }
            previous = Some(range);
        }
    }
    let mut native = BTreeSet::new();
    for record in &catalog.native_licenses {
        if !native_name(&record.name) || !native.insert((&record.name, &record.version)) {
            return Err(bad("invalid or duplicate native license identity/version"));
        }
        if record.expression.len() > limits.max_string_bytes {
            return Err(bad("native license exceeds string limit"));
        }
    }
    Ok(())
}

pub(crate) fn validate_policy(policy: &Policy, limits: &Limits) -> Result<BTreeSet<String>, Error> {
    input::check_size(policy, limits)?;
    if policy.schema_version != SCHEMA_VERSION {
        return Err(Error::InvalidPolicy("unsupported schema version".into()));
    }
    if policy.allowed_licenses.len() > limits.max_entries {
        return Err(Error::Limit("license grants".into()));
    }
    license::allowlist(&policy.allowed_licenses, limits)
}

/// Bound the existing calculus validator before invoking it: no audit result is
/// returned for an invalid solution. The complexity estimate is conservative
/// logical work, not wall-clock or instruction metering.
fn preflight(solution: &Solution, limits: &Limits, work: &mut Work) -> Result<(), Error> {
    if solution.roots.is_empty() {
        return Err(Error::InvalidInput(
            "audit requires at least one declared root package".into(),
        ));
    }
    input::check_size(solution, limits)?;
    if solution
        .packages
        .len()
        .saturating_add(solution.system_packages.len())
        > limits.max_packages
        || solution.roots.len() > limits.max_roots
    {
        return Err(Error::Limit("package/root count".into()));
    }
    let mut edges = 0usize;
    let mut declarations = solution.roots.len().saturating_add(1);
    for package in &solution.packages {
        let m = &package.manifest;
        edges = edges.saturating_add(m.dependencies.len());
        declarations = declarations
            .saturating_add(m.package.capability_ceiling.len())
            .saturating_add(package.features.len());
        for f in m.features.values() {
            edges = edges
                .saturating_add(f.sys_deps.len())
                .saturating_add(f.deps.len())
                .saturating_add(f.features.len());
            declarations = declarations
                .saturating_add(f.enables_effects.len())
                .saturating_add(1);
        }
        for d in m.dependencies.values() {
            declarations = declarations
                .saturating_add(d.effects.len())
                .saturating_add(d.features.len());
        }
    }
    if edges > limits.max_edges {
        return Err(Error::Limit("declared edge count".into()));
    }
    let complexity = solution
        .packages
        .len()
        .saturating_add(1)
        .saturating_mul(declarations.saturating_add(1))
        .saturating_mul(
            edges
                .saturating_add(declarations)
                .saturating_add(solution.packages.len())
                .saturating_add(1),
        );
    work.spend(complexity)
}

pub(crate) fn audit(
    solution: &Solution,
    catalog: &Catalog,
    policy: &Policy,
    limits: &Limits,
) -> Result<AuditReport, Error> {
    limits.validate()?;
    validate_catalog(catalog, limits)?;
    let grants = validate_policy(policy, limits)?;
    let mut work = Work {
        remaining: limits.max_work,
    };
    preflight(solution, limits, &mut work)?;
    let validated = validate_solution(solution);
    let Some(graph) = validated.graph else {
        return Err(Error::InvalidSolution(validated.diagnostics));
    };
    let paths = dependency_paths(&graph, limits, &mut work)?;
    let licenses: BTreeMap<_, _> = solution
        .packages
        .iter()
        .map(|s| {
            (
                s.manifest.package.name.clone(),
                s.manifest.package.license.as_deref(),
            )
        })
        .collect();
    let native_licenses: BTreeMap<_, _> = catalog
        .native_licenses
        .iter()
        .map(|l| ((l.name.as_str(), &l.version), l.expression.as_str()))
        .collect();
    let mut findings = Vec::new();
    let mut report_path_nodes = 0usize;
    let mut unknown = false;
    let mut denied = false;
    for (package, package_paths) in paths {
        let mut issues = Vec::new();
        for advisory in &catalog.advisories {
            work.spend(1)?;
            if advisory.package != package.identity {
                continue;
            }
            let mut affected = false;
            for range in &advisory.affected {
                work.spend(1)?;
                affected |= range.contains(&package.version);
            }
            if affected && advisory.severity >= policy.minimum_severity {
                denied = true;
                issues.push(FindingKind::Vulnerability {
                    advisory_id: advisory.id.clone(),
                    severity: advisory.severity,
                    summary: advisory.summary.clone(),
                });
            }
        }
        issues.sort_by(|a, b| match (a, b) {
            (
                FindingKind::Vulnerability { advisory_id: a, .. },
                FindingKind::Vulnerability { advisory_id: b, .. },
            ) => a.cmp(b),
            _ => Ordering::Equal,
        });
        let expression = match &package.identity {
            PackageIdentity::Vesper { name } => licenses.get(name).copied().flatten(),
            PackageIdentity::Native { name } => native_licenses
                .get(&(name.as_str(), &package.version))
                .copied(),
        };
        work.spend(expression.map_or(1, str::len))?;
        match license::assess(expression, &grants, limits) {
            Ok(true) => {}
            Ok(false) => {
                denied = true;
                issues.push(FindingKind::LicenseDenied {
                    expression: expression.unwrap().into(),
                });
            }
            Err(license::Failure::Limit(reason)) => return Err(Error::Limit(reason)),
            Err(license::Failure::Unknown(reason)) => {
                unknown = true;
                issues.push(FindingKind::LicenseUnknown {
                    expression: expression.map(str::to_string),
                    reason,
                });
            }
        }
        for issue in issues {
            let nodes = package_paths.iter().map(Vec::len).sum();
            work.spend(nodes)?;
            report_path_nodes = report_path_nodes
                .checked_add(nodes)
                .ok_or_else(|| Error::Limit("report path nodes".into()))?;
            if report_path_nodes > limits.max_path_nodes || findings.len() >= limits.max_entries {
                return Err(Error::Limit("report path nodes/findings".into()));
            }
            findings.push(Finding {
                package: package.clone(),
                issue,
                paths: package_paths.clone(),
            });
            if findings.len() > limits.max_entries {
                return Err(Error::Limit("finding count".into()));
            }
        }
    }
    Ok(AuditReport {
        schema_version: SCHEMA_VERSION,
        context: AuditContext {
            contract: "vesper.loom.offline-audit/1".into(),
            solution_digest: digest("vesper.loom.offline-audit/1 solution", solution)?,
            catalog_digest: digest("vesper.loom.offline-audit/1 catalog", catalog)?,
            policy_digest: digest("vesper.loom.offline-audit/1 policy", policy)?,
            limits_digest: digest("vesper.loom.offline-audit/1 limits", limits)?,
        },
        catalog_id: catalog.catalog_id.clone(),
        outcome: if unknown {
            Outcome::Unknown
        } else if denied {
            Outcome::Findings
        } else {
            Outcome::Clean
        },
        has_policy_violations: denied,
        reachable_vesper_packages: graph.packages.len(),
        reachable_native_packages: graph.system_packages.len(),
        findings,
    })
}

type Paths = BTreeMap<SelectedPackage, Vec<Vec<SelectedPackage>>>;

fn dependency_paths(
    graph: &PackageGraph,
    limits: &Limits,
    work: &mut Work,
) -> Result<Paths, Error> {
    let select = |name: &PackageName| SelectedPackage {
        identity: PackageIdentity::Vesper { name: name.clone() },
        version: graph.packages[name].version.clone(),
    };
    let mut result: Paths = BTreeMap::new();
    let mut path_nodes = 0usize;
    for root in &graph.roots {
        let mut queue = VecDeque::from([root.clone()]);
        let mut parents = BTreeMap::from([(root.clone(), None::<PackageName>)]);
        while let Some(name) = queue.pop_front() {
            work.spend(1)?;
            let mut reversed = Vec::new();
            let mut cursor = Some(&name);
            while let Some(node) = cursor {
                work.spend(1)?;
                if reversed.len() >= limits.max_path_depth {
                    return Err(Error::Limit("dependency path depth".into()));
                }
                reversed.push(select(node));
                cursor = parents[node].as_ref();
            }
            reversed.reverse();
            path_nodes = path_nodes
                .checked_add(reversed.len())
                .ok_or_else(|| Error::Limit("dependency path nodes".into()))?;
            if path_nodes > limits.max_path_nodes {
                return Err(Error::Limit("dependency path nodes".into()));
            }
            result
                .entry(select(&name))
                .or_default()
                .push(reversed.clone());
            for edge in &graph.packages[&name].system_dependencies {
                work.spend(1)?;
                if reversed.len() >= limits.max_path_depth {
                    return Err(Error::Limit("native dependency path depth".into()));
                }
                let native = SelectedPackage {
                    identity: PackageIdentity::Native {
                        name: edge.name.clone(),
                    },
                    version: graph.system_packages[&edge.name].clone(),
                };
                work.spend(reversed.len().saturating_add(1))?;
                let mut path = reversed.clone();
                path.push(native.clone());
                // Multiple parents of one native package: retain a shortest
                // path from this root, with lexicographic tie breaking.
                let paths = result.entry(native).or_default();
                if let Some(previous) = paths
                    .iter_mut()
                    .find(|p| p[0].identity == select(root).identity)
                {
                    if (path.len(), &path) < (previous.len(), &*previous) {
                        *previous = path;
                    }
                } else {
                    path_nodes = path_nodes
                        .checked_add(path.len())
                        .ok_or_else(|| Error::Limit("path nodes".into()))?;
                    if path_nodes > limits.max_path_nodes {
                        return Err(Error::Limit("path nodes".into()));
                    }
                    paths.push(path);
                }
            }
            let mut dependencies = graph.packages[&name]
                .dependencies
                .iter()
                .map(|d| d.package.clone())
                .collect::<Vec<_>>();
            dependencies.sort();
            for dependency in dependencies {
                work.spend(1)?;
                if !parents.contains_key(&dependency) {
                    parents.insert(dependency.clone(), Some(name.clone()));
                    queue.push_back(dependency);
                }
            }
        }
    }
    Ok(result)
}
