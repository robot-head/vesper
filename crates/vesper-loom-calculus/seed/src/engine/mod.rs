//! The Rust calculus, frozen for the compiler's closure: only the seed copy
//! vesper-loom-calculus-seed builds it (bug fixes only until self-hosting
//! Stage 2); the package vesper-loom-calculus runs the Vesper port in
//! ../../vesper instead (Rust-exit SH6 lane 12, #1407).

use crate::*;
use semver::VersionReq;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) mod upgrade;

/// `PackageName::parse`'s spelling check.
pub(crate) fn package_name_valid(name: &str) -> bool {
    let parts = name
        .strip_prefix('@')
        .unwrap_or(name)
        .split('/')
        .collect::<Vec<_>>();
    let component = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
            && s.as_bytes()[0].is_ascii_lowercase()
    };
    !(parts.len() != 2
        || !component(parts[0])
        || !component(parts[1])
        || (!name.starts_with('@') && parts[0] != "std"))
}

fn emit(
    out: &mut Vec<Diagnostic>,
    code: Code,
    package: Option<&PackageName>,
    subject: impl Into<String>,
    message: impl Into<String>,
) {
    out.push(Diagnostic {
        code,
        package: package.cloned(),
        subject: subject.into(),
        message: message.into(),
    });
}

fn capability_valid(s: &str) -> bool {
    if s.is_empty() || !s.as_bytes()[0].is_ascii_uppercase() {
        return false;
    }
    let mut depth = 0usize;
    for c in s.bytes() {
        match c {
            b'[' => depth += 1,
            b']' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ if c.is_ascii_alphanumeric()
                || matches!(c, b'_' | b',' | b':' | b'-' | b'/' | b'@' | b'.') => {}
            _ => return false,
        }
    }
    depth == 0 && !s.contains("[]")
}

fn feature_valid(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

fn check_caps(
    caps: &BTreeSet<String>,
    owner: &PackageName,
    subject: &str,
    out: &mut Vec<Diagnostic>,
) {
    for cap in caps {
        if !capability_valid(cap) {
            emit(
                out,
                Code::InvalidCapability,
                Some(owner),
                subject,
                format!("non-canonical capability '{cap}'"),
            );
        }
    }
}

pub(crate) fn validate_manifest(manifest: &Manifest) -> Vec<Diagnostic> {
    let mut out = vec![];
    let owner = &manifest.package.name;
    check_caps(
        &manifest.package.capability_ceiling,
        owner,
        "package.capability_ceiling",
        &mut out,
    );
    if let Some(sandbox) = &manifest.build_sandbox {
        let relative = |path: &str| {
            !path.is_empty()
                && !path.starts_with('/')
                && !path.contains(['\\', ':', '\0'])
                && !path.split('/').any(|part| part == "..")
        };
        if sandbox.allow_network {
            emit(
                &mut out,
                Code::UnsupportedBuildPolicy,
                Some(owner),
                "build-sandbox.allow_network",
                "Loom build components have no network authority",
            );
        }
        for path in sandbox.script.iter().chain(&sandbox.wasi_read_paths) {
            if !relative(path) {
                emit(
                    &mut out,
                    Code::UnsupportedBuildPolicy,
                    Some(owner),
                    "build-sandbox",
                    format!("build input path must be relative and traversal-free: '{path}'"),
                );
            }
        }
    }
    for (name, dep) in &manifest.dependencies {
        check_caps(&dep.effects, owner, name.as_str(), &mut out);
    }
    for (name, feature) in &manifest.features {
        if !feature_valid(name) {
            emit(
                &mut out,
                Code::InvalidFeature,
                Some(owner),
                name,
                "invalid feature name",
            );
        }
        check_caps(&feature.enables_effects, owner, name, &mut out);
        for dep in &feature.deps {
            if !manifest.dependencies.contains_key(dep) {
                emit(
                    &mut out,
                    Code::UnknownDependency,
                    Some(owner),
                    name,
                    format!("feature refers to undeclared dependency {}", dep.as_str()),
                );
            }
        }
        for nested in &feature.features {
            if !manifest.features.contains_key(nested) {
                emit(
                    &mut out,
                    Code::UnknownFeature,
                    Some(owner),
                    name,
                    format!("feature refers to unknown feature '{nested}'"),
                );
            }
        }
        for sys in &feature.sys_deps {
            if parse_system_requirement(sys).is_err() {
                emit(
                    &mut out,
                    Code::InvalidSystemRequirement,
                    Some(owner),
                    name,
                    format!("expected native name followed by a SemVer requirement: '{sys}'"),
                );
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn system_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+'))
}

fn parse_system_requirement(source: &str) -> Result<(String, VersionReq), ()> {
    let (name, requirement) = source.split_once(char::is_whitespace).ok_or(())?;
    if !system_name_valid(name) {
        return Err(());
    }
    Ok((
        name.into(),
        VersionReq::parse(requirement.trim()).map_err(|_| ())?,
    ))
}

fn expand_features(
    manifest: &Manifest,
    initial: &BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) -> BTreeSet<String> {
    let mut expanded = BTreeSet::new();
    let mut queue = initial.iter().cloned().collect::<VecDeque<_>>();
    while let Some(name) = queue.pop_front() {
        if !expanded.insert(name.clone()) {
            continue;
        }
        if let Some(feature) = manifest.features.get(&name) {
            queue.extend(feature.features.iter().cloned());
        } else {
            emit(
                out,
                Code::UnknownFeature,
                Some(&manifest.package.name),
                &name,
                "requested feature is not declared",
            );
        }
    }
    expanded
}

pub(crate) fn validate_solution(solution: &Solution) -> Validation {
    let mut out = vec![];
    if solution.schema_version != SCHEMA_VERSION {
        emit(
            &mut out,
            Code::ManifestSyntax,
            None,
            "schema_version",
            "unsupported solution schema version",
        );
    }
    let mut selected = BTreeMap::new();
    let mut selections = solution.packages.iter().collect::<Vec<_>>();
    selections.sort_by_cached_key(|s| {
        (
            s.manifest.package.name.clone(),
            s.manifest.package.version.clone(),
            serde_json::to_string(s).expect("manifest DTOs contain no fallible JSON values"),
        )
    });
    for selection in selections {
        out.extend(validate_manifest(&selection.manifest));
        let name = &selection.manifest.package.name;
        if selected.insert(name.clone(), selection).is_some() {
            emit(
                &mut out,
                Code::DuplicatePackage,
                Some(name),
                name.as_str(),
                "version uniqueness requires one selection per package name",
            );
        }
    }
    let mut systems = BTreeMap::new();
    let mut native_selections = solution.system_packages.iter().collect::<Vec<_>>();
    native_selections.sort_by_key(|s| (s.name.clone(), s.version.clone()));
    for selection in native_selections {
        if !system_name_valid(&selection.name) {
            emit(
                &mut out,
                Code::InvalidSystemRequirement,
                None,
                &selection.name,
                "invalid native package name",
            );
        }
        if systems
            .insert(selection.name.clone(), selection.version.clone())
            .is_some()
        {
            emit(
                &mut out,
                Code::DuplicateSystemPackage,
                None,
                &selection.name,
                "native version uniqueness requires one selection per name",
            );
        }
    }
    let mut features = BTreeMap::<PackageName, BTreeSet<String>>::new();
    let mut queue = VecDeque::new();
    let mut roots = BTreeSet::new();
    for root in &solution.roots {
        roots.insert(root.name.clone());
        if let Some(selection) = selected.get(&root.name) {
            if !root.version.matches(&selection.manifest.package.version) {
                emit(
                    &mut out,
                    Code::VersionMismatch,
                    Some(&root.name),
                    "root",
                    format!(
                        "{} does not satisfy {}",
                        selection.manifest.package.version, root.version
                    ),
                );
            }
            features
                .entry(root.name.clone())
                .or_default()
                .extend(root.features.iter().cloned());
            queue.push_back(root.name.clone());
        } else {
            emit(
                &mut out,
                Code::MissingRoot,
                Some(&root.name),
                "root",
                "root inclusion requires a selected package",
            );
        }
    }
    let mut reachable = BTreeSet::new();
    let mut nodes = BTreeMap::<PackageName, PackageNode>::new();
    while let Some(name) = queue.pop_front() {
        let Some(selection) = selected.get(&name) else {
            continue;
        };
        let manifest = &selection.manifest;
        let first = reachable.insert(name.clone());
        let initial = features.entry(name.clone()).or_default();
        initial.extend(selection.features.iter().cloned());
        let expanded = expand_features(manifest, initial, &mut out);
        if !first && nodes.get(&name).is_some_and(|n| n.features == expanded) {
            continue;
        }
        *initial = expanded.clone();
        let mut enabled = manifest
            .dependencies
            .iter()
            .filter(|(_, d)| !d.optional)
            .map(|(n, _)| n.clone())
            .collect::<BTreeSet<_>>();
        let mut required = manifest.package.capability_ceiling.clone();
        let mut sys = BTreeSet::new();
        for feature in expanded.iter().filter_map(|f| manifest.features.get(f)) {
            enabled.extend(feature.deps.iter().cloned());
            required.extend(feature.enables_effects.iter().cloned());
            for source in &feature.sys_deps {
                if let Ok((sys_name, requirement)) = parse_system_requirement(source) {
                    sys.insert(SystemEdge {
                        name: sys_name,
                        requirement: requirement.to_string(),
                    });
                }
            }
        }
        let mut edges = vec![];
        for dependency in enabled {
            let Some(edge) = manifest.dependencies.get(&dependency) else {
                continue;
            };
            edges.push(DependencyEdge {
                package: dependency.clone(),
                requirement: edge.version.clone(),
                allowed_capabilities: edge.effects.clone(),
                requested_features: edge.features.clone(),
            });
            if let Some(target) = selected.get(&dependency) {
                if !edge.version.matches(&target.manifest.package.version) {
                    emit(
                        &mut out,
                        Code::VersionMismatch,
                        Some(&name),
                        dependency.as_str(),
                        format!(
                            "{} does not satisfy {}",
                            target.manifest.package.version, edge.version
                        ),
                    );
                }
                let enabled_features = features.entry(dependency.clone()).or_default();
                let before = enabled_features.len();
                enabled_features.extend(edge.features.iter().cloned());
                if !reachable.contains(&dependency) || before != enabled_features.len() {
                    queue.push_back(dependency);
                }
            } else {
                emit(
                    &mut out,
                    Code::MissingDependency,
                    Some(&name),
                    dependency.as_str(),
                    "dependency closure requires a selected package",
                );
            }
        }
        for edge in &sys {
            if let Some(version) = systems.get(&edge.name) {
                if !VersionReq::parse(&edge.requirement)
                    .unwrap()
                    .matches(version)
                {
                    emit(
                        &mut out,
                        Code::VersionMismatch,
                        Some(&name),
                        &edge.name,
                        format!(
                            "native version {version} does not satisfy {}",
                            edge.requirement
                        ),
                    );
                }
            } else {
                emit(
                    &mut out,
                    Code::MissingSystemPackage,
                    Some(&name),
                    &edge.name,
                    "active feature requires a native package selection",
                );
            }
        }
        nodes.insert(
            name,
            PackageNode {
                version: manifest.package.version.clone(),
                features: expanded,
                capability_ceiling: manifest.package.capability_ceiling.clone(),
                required_capabilities: required,
                forbid_unsafe_ffi: manifest.package.forbid_unsafe_ffi,
                dependencies: edges,
                system_dependencies: sys.into_iter().collect(),
            },
        );
    }
    // Propagate authority backwards over active edges. Each new capability
    // moves monotonically through the finite graph, including dependency cycles.
    let mut parents = BTreeMap::<PackageName, BTreeSet<PackageName>>::new();
    for (name, node) in &nodes {
        for edge in &node.dependencies {
            if nodes.contains_key(&edge.package) {
                parents
                    .entry(edge.package.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }
    }
    let mut pending = nodes.keys().cloned().collect::<VecDeque<_>>();
    let mut queued = nodes.keys().cloned().collect::<BTreeSet<_>>();
    while let Some(child) = pending.pop_front() {
        queued.remove(&child);
        let required = nodes[&child].required_capabilities.clone();
        for parent in parents.get(&child).into_iter().flatten() {
            let caps = &mut nodes.get_mut(parent).unwrap().required_capabilities;
            let before = caps.len();
            caps.extend(required.iter().cloned());
            if caps.len() != before && queued.insert(parent.clone()) {
                pending.push_back(parent.clone());
            }
        }
    }
    for (name, node) in &nodes {
        let excess = node
            .required_capabilities
            .difference(&node.capability_ceiling)
            .cloned()
            .collect::<Vec<_>>();
        if !excess.is_empty() {
            emit(
                &mut out,
                Code::CapabilityEscalation,
                Some(name),
                "package.capability_ceiling",
                format!(
                    "required capabilities exceed package ceiling: {}",
                    excess.join(", ")
                ),
            );
        }
        if node.forbid_unsafe_ffi
            && node
                .required_capabilities
                .iter()
                .any(|c| matches!(c.split('[').next(), Some("UnsafeFfi" | "Ffi")))
        {
            emit(
                &mut out,
                Code::UnsafeFfiForbidden,
                Some(name),
                "package.forbid_unsafe_ffi",
                "reachable package requires foreign authority",
            );
        }
        for edge in &node.dependencies {
            if let Some(target) = nodes.get(&edge.package) {
                let excess = target
                    .required_capabilities
                    .difference(&edge.allowed_capabilities)
                    .cloned()
                    .collect::<Vec<_>>();
                if !excess.is_empty() {
                    emit(
                        &mut out,
                        Code::EdgeCapabilityEscalation,
                        Some(name),
                        edge.package.as_str(),
                        format!(
                            "dependency exceeds edge capability ceiling: {}",
                            excess.join(", ")
                        ),
                    );
                }
            }
        }
    }
    out.sort();
    out.dedup();
    let unused = selected
        .keys()
        .filter(|n| !reachable.contains(*n))
        .cloned()
        .collect();
    let used_systems = nodes
        .values()
        .flat_map(|n| n.system_dependencies.iter().map(|e| e.name.clone()))
        .collect::<BTreeSet<_>>();
    Validation {
        graph: out.is_empty().then(|| PackageGraph {
            schema_version: SCHEMA_VERSION,
            roots,
            packages: nodes,
            system_packages: systems
                .into_iter()
                .filter(|(n, _)| used_systems.contains(n))
                .collect(),
        }),
        diagnostics: out,
        unused_selections: unused,
    }
}
