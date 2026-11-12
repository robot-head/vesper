//! Read-only inspection of the dependency graph declared by a solution snapshot.
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    fs,
};
use vesper_loom_calculus::{
    Diagnostic, PackageGraph, PackageName, Solution, Validation, validate_solution,
};

const HELP: &str = "Usage: vesper graph [Loom.lock] [--package @org/name] [--invert] [--json|--dot]\n\nInspect declared snapshot metadata without resolving, executing or modifying files.\n--package selects a reachable package and its dependency closure.\n--invert follows dependents instead, and reverses drawn arrows.\n--json emits the versioned inspection report; --dot emits Graphviz DOT.\n";

#[derive(Default)]
struct Options {
    input: Option<String>,
    focus: Option<PackageName>,
    invert: bool,
    json: bool,
    dot: bool,
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--package" => {
                if options.focus.is_some() {
                    return Err("--package may only be specified once".into());
                }
                let name = args.next().ok_or("--package requires a package name")?;
                options.focus = Some(PackageName::parse(name)?);
            }
            "--invert" => options.invert = true,
            "--json" => options.json = true,
            "--dot" => options.dot = true,
            _ if arg.starts_with('-') => return Err(format!("unknown graph option '{arg}'")),
            _ => {
                if options.input.replace(arg).is_some() {
                    return Err("graph accepts one solution snapshot".into());
                }
            }
        }
    }
    if options.json && options.dot {
        return Err("--json and --dot cannot be combined".into());
    }
    let input = options.input.as_deref().unwrap_or("Loom.lock");
    let bytes = fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    let solution: Solution =
        serde_json::from_slice(&bytes).map_err(|e| format!("{input}: invalid lockfile: {e}"))?;
    let Validation {
        graph,
        diagnostics,
        unused_selections,
    } = validate_solution(&solution);
    if solution.roots.is_empty() || !diagnostics.is_empty() || graph.is_none() {
        let message = if solution.roots.is_empty() {
            "snapshot must declare at least one root".into()
        } else {
            format!(
                "invalid snapshot: {} validation diagnostic(s)",
                diagnostics.len()
            )
        };
        return semantic_error(options.json, "invalid_snapshot", message, diagnostics);
    }
    let Some(graph) = graph else {
        return Err("snapshot validation did not produce a graph".into());
    };
    if let Some(focus) = &options.focus
        && !graph.packages.contains_key(focus)
    {
        return semantic_error(
            options.json,
            "unknown_package",
            format!(
                "package '{}' is not reachable from the snapshot roots",
                focus.as_str()
            ),
            vec![],
        );
    }
    let unused_systems = solution
        .system_packages
        .iter()
        .filter(|selection| !graph.system_packages.contains_key(&selection.name))
        .map(|selection| selection.name.clone())
        .collect();
    let report = project(
        &graph,
        options.focus,
        options.invert,
        unused_selections,
        unused_systems,
    )?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else if options.dot {
        print!("{}", render_dot(&report));
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}

fn semantic_error(
    json: bool,
    kind: &str,
    message: String,
    diagnostics: Vec<Diagnostic>,
) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "status": "error",
                "error": {"kind": kind, "message": &message, "diagnostics": diagnostics}
            }))
            .map_err(|e| e.to_string())?
        );
    }
    Err(message)
}

#[derive(Serialize)]
struct PackageSummary {
    version: String,
    features: BTreeSet<String>,
    capability_ceiling: BTreeSet<String>,
    required_capabilities: BTreeSet<String>,
    forbid_unsafe_ffi: bool,
    is_root: bool,
}

#[derive(Serialize)]
struct Dependency {
    dependent: PackageName,
    dependency: PackageName,
    requirement: String,
    allowed_capabilities: BTreeSet<String>,
    requested_features: BTreeSet<String>,
}

#[derive(Serialize)]
struct SystemDependency {
    package: PackageName,
    system: String,
    requirement: String,
}

#[derive(Serialize)]
struct GraphReport {
    schema_version: u32,
    status: &'static str,
    basis: &'static str,
    direction: &'static str,
    focus: Option<PackageName>,
    snapshot_roots: BTreeSet<PackageName>,
    packages: BTreeMap<PackageName, PackageSummary>,
    system_packages: BTreeMap<String, String>,
    dependencies: Vec<Dependency>,
    system_dependencies: Vec<SystemDependency>,
    unused_packages: BTreeSet<PackageName>,
    unused_system_packages: BTreeSet<String>,
}

fn project(
    graph: &PackageGraph,
    focus: Option<PackageName>,
    invert: bool,
    unused_packages: BTreeSet<PackageName>,
    unused_system_packages: BTreeSet<String>,
) -> Result<GraphReport, String> {
    let visible = match &focus {
        None => graph.packages.keys().cloned().collect(),
        Some(focus) => {
            let mut adjacent = BTreeMap::<PackageName, BTreeSet<PackageName>>::new();
            for (name, node) in &graph.packages {
                for edge in &node.dependencies {
                    let (from, to) = if invert {
                        (&edge.package, name)
                    } else {
                        (name, &edge.package)
                    };
                    adjacent.entry(from.clone()).or_default().insert(to.clone());
                }
            }
            let mut visible = BTreeSet::new();
            let mut queue = VecDeque::from([focus.clone()]);
            while let Some(name) = queue.pop_front() {
                if visible.insert(name.clone())
                    && let Some(neighbors) = adjacent.get(&name)
                {
                    queue.extend(neighbors.iter().cloned());
                }
            }
            visible
        }
    };
    let mut report = GraphReport {
        schema_version: 1,
        status: "graph",
        basis: "declared_metadata",
        direction: if invert { "dependents" } else { "dependencies" },
        focus,
        snapshot_roots: graph.roots.clone(),
        packages: BTreeMap::new(),
        system_packages: BTreeMap::new(),
        dependencies: vec![],
        system_dependencies: vec![],
        unused_packages,
        unused_system_packages,
    };
    for (name, node) in &graph.packages {
        if !visible.contains(name) {
            continue;
        }
        report.packages.insert(
            name.clone(),
            PackageSummary {
                version: node.version.to_string(),
                features: node.features.clone(),
                capability_ceiling: node.capability_ceiling.clone(),
                required_capabilities: node.required_capabilities.clone(),
                forbid_unsafe_ffi: node.forbid_unsafe_ffi,
                is_root: graph.roots.contains(name),
            },
        );
        for edge in &node.dependencies {
            if visible.contains(&edge.package) {
                report.dependencies.push(Dependency {
                    dependent: name.clone(),
                    dependency: edge.package.clone(),
                    requirement: edge.requirement.to_string(),
                    allowed_capabilities: edge.allowed_capabilities.clone(),
                    requested_features: edge.requested_features.clone(),
                });
            }
        }
        for edge in &node.system_dependencies {
            let version = graph.system_packages.get(&edge.name).ok_or_else(|| {
                format!("validated graph is missing native package '{}'", edge.name)
            })?;
            report
                .system_packages
                .insert(edge.name.clone(), version.to_string());
            report.system_dependencies.push(SystemDependency {
                package: name.clone(),
                system: edge.name.clone(),
                requirement: edge.requirement.clone(),
            });
        }
    }
    Ok(report)
}

fn list(values: &BTreeSet<String>) -> String {
    format!(
        "[{}]",
        values.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

fn render_text(report: &GraphReport) -> String {
    let mut out = format!("Package graph (declared metadata; {})\n", report.direction);
    if let Some(focus) = &report.focus {
        let _ = writeln!(out, "focus: {}", focus.as_str());
    }
    out.push_str(
        "Capability requirements describe the full validated snapshot, including when filtered.\n",
    );
    for (name, node) in &report.packages {
        let _ = writeln!(
            out,
            "{} {}{}",
            name.as_str(),
            node.version,
            if node.is_root { " [root]" } else { "" }
        );
        let _ = writeln!(out, "  features: {}", list(&node.features));
        let _ = writeln!(
            out,
            "  declared ceiling: {}",
            list(&node.capability_ceiling)
        );
        let _ = writeln!(
            out,
            "  required capabilities: {}",
            list(&node.required_capabilities)
        );
        let _ = writeln!(out, "  forbid unsafe FFI: {}", node.forbid_unsafe_ffi);
    }
    // Keep constraint ownership explicit, even when selection follows dependents.
    out.push_str("Dependency constraints (dependent -> dependency):\n");
    for edge in &report.dependencies {
        let _ = writeln!(
            out,
            "  {} -> {}: {}; ceiling {}; requested features {}",
            edge.dependent.as_str(),
            edge.dependency.as_str(),
            edge.requirement,
            list(&edge.allowed_capabilities),
            list(&edge.requested_features)
        );
    }
    for edge in &report.system_dependencies {
        let version = &report.system_packages[&edge.system];
        let _ = writeln!(
            out,
            "  {} -> native {} {}: {}",
            edge.package.as_str(),
            edge.system,
            version,
            edge.requirement
        );
    }
    if !report.unused_packages.is_empty() {
        let _ = writeln!(
            out,
            "Unreachable packages excluded: {}",
            report
                .unused_packages
                .iter()
                .map(PackageName::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !report.unused_system_packages.is_empty() {
        let _ = writeln!(
            out,
            "Unused native selections excluded: {}",
            list(&report.unused_system_packages)
        );
    }
    out
}

fn quote(text: &str) -> String {
    // JSON quoted strings also safely quote DOT labels and node IDs. No input
    // text can close the string and introduce Graphviz attributes/statements.
    serde_json::to_string(text).expect("serializing a string is infallible")
}

fn render_dot(report: &GraphReport) -> String {
    let mut out = String::from("digraph vesper {\n");
    let _ = writeln!(
        out,
        "  graph [label={}];",
        quote(&format!("Declared metadata: {}", report.direction))
    );
    for (name, node) in &report.packages {
        let label = format!(
            "{} {}{}\nfeatures: {}\ndeclared: {}\nrequired: {}",
            name.as_str(),
            node.version,
            if node.is_root { " [root]" } else { "" },
            list(&node.features),
            list(&node.capability_ceiling),
            list(&node.required_capabilities)
        );
        let _ = writeln!(
            out,
            "  {} [shape=box, label={}, peripheries={}];",
            quote(&format!("pkg:{}", name.as_str())),
            quote(&label),
            if node.is_root { 2 } else { 1 }
        );
    }
    for (name, version) in &report.system_packages {
        let _ = writeln!(
            out,
            "  {} [shape=ellipse, label={}];",
            quote(&format!("sys:{name}")),
            quote(&format!("native {name} {version}"))
        );
    }
    let invert = report.direction == "dependents";
    for edge in &report.dependencies {
        let dependent = format!("pkg:{}", edge.dependent.as_str());
        let dependency = format!("pkg:{}", edge.dependency.as_str());
        let (from, to) = if invert {
            (&dependency, &dependent)
        } else {
            (&dependent, &dependency)
        };
        let label = format!(
            "{} requires {}\nrequirement: {}\nceiling: {}\nrequested features: {}",
            edge.dependent.as_str(),
            edge.dependency.as_str(),
            edge.requirement,
            list(&edge.allowed_capabilities),
            list(&edge.requested_features)
        );
        let _ = writeln!(
            out,
            "  {} -> {} [label={}];",
            quote(from),
            quote(to),
            quote(&label)
        );
    }
    for edge in &report.system_dependencies {
        let package = format!("pkg:{}", edge.package.as_str());
        let system = format!("sys:{}", edge.system);
        let (from, to) = if invert {
            (&system, &package)
        } else {
            (&package, &system)
        };
        let label = format!(
            "{} requires native {}\nrequirement: {}",
            edge.package.as_str(),
            edge.system,
            edge.requirement
        );
        let _ = writeln!(
            out,
            "  {} -> {} [label={}];",
            quote(from),
            quote(to),
            quote(&label)
        );
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_strings_cannot_end_labels_or_add_statements() {
        assert_eq!(
            quote("a\"; b -> c; \\\n\r\t"),
            "\"a\\\"; b -> c; \\\\\\n\\r\\t\""
        );
    }
}
