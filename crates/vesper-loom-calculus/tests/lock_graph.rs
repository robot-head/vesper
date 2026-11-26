//! The Vesper `vesper graph` (`lock_graph.ves`, through `lock_graph`)
//! against the Rust one (src/package_graph.rs, copied below as the oracle
//! over the crate's own `validate_solution`): the same stdout, status and
//! error on generated lockfiles (mostly valid graphs), the invalid ones of
//! the lockfile reader's corpus, and every focus, direction and format.
#![cfg(vesper_port)]

use semver::{Version, VersionReq};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use vesper_loom_calculus::*;

/// `lock_graph`'s statuses.
const OK: u64 = 0;
const BAD_LOCKFILE: u64 = 1;
const SEMANTIC: u64 = 2;

type Run = (u64, String, String);

/// src/package_graph.rs `run` after the option checks and the file read.
fn rust(bytes: &[u8], focus: Option<&str>, invert: bool, json: bool, dot: bool) -> Run {
    let solution: Solution = match serde_json::from_slice(bytes) {
        Ok(s) => s,
        Err(e) => return (BAD_LOCKFILE, String::new(), e.to_string()),
    };
    let focus = focus.map(|f| PackageName::parse(f).unwrap());
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
        return semantic_error(json, "invalid_snapshot", message, diagnostics);
    }
    let graph = graph.unwrap();
    if let Some(focus) = &focus
        && !graph.packages.contains_key(focus)
    {
        return semantic_error(
            json,
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
    let report = project(&graph, focus, invert, unused_selections, unused_systems).unwrap();
    let out = if json {
        format!("{}\n", serde_json::to_string_pretty(&report).unwrap())
    } else if dot {
        render_dot(&report)
    } else {
        render_text(&report)
    };
    (OK, out, String::new())
}

fn semantic_error(json: bool, kind: &str, message: String, diagnostics: Vec<Diagnostic>) -> Run {
    let out = if json {
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "status": "error",
                "error": {"kind": kind, "message": &message, "diagnostics": diagnostics}
            }))
            .unwrap()
        )
    } else {
        String::new()
    };
    (SEMANTIC, out, message)
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

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn some(&mut self, xs: &[&str], max: usize) -> BTreeSet<String> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.pick(xs).to_owned()).collect()
    }
}

const NAMES: &[&str] = &[
    "@org/app",
    "@org/core",
    "@org/net",
    "std/bytes",
    "@b/x",
    "@b/y",
    "@org/q-9",
];
const CAPS: &[&str] = &["Net", "Fail", "Io", "Map[K,V]"];
const FEATURES: &[&str] = &["default", "tls", "x_y", "fast"];
const VERSIONS: &[&str] = &["1.0.0", "1.2.3-alpha.1", "0.9.2", "2.0.0+build.01"];
const SYSTEMS: &[(&str, &str, &str)] = &[
    ("zlib", "1.2.13", "zlib ^1.2"),
    ("liburing", "2.6.0", "liburing\u{3000}>=2.6.0, <3"),
    ("libc", "0.2.150", "libc  0.2"),
];

fn name(s: &str) -> PackageName {
    PackageName::parse(s).unwrap()
}

/// A snapshot that is usually valid: packages depend only on later ones,
/// with requirements their selection meets and ceilings that cover them.
fn solution(rng: &mut Rng) -> Solution {
    let n = 1 + rng.below(NAMES.len());
    let mut names: Vec<&str> = NAMES.to_vec();
    for i in (1..names.len()).rev() {
        names.swap(i, rng.below(i + 1));
    }
    names.truncate(n);
    let versions: Vec<&str> = (0..n).map(|_| rng.pick(VERSIONS)).collect();
    let mut packages = vec![];
    let mut used_systems = BTreeSet::new();
    for i in 0..n {
        let mut dependencies = BTreeMap::new();
        for j in i + 1..n {
            if rng.chance(45) {
                let req = match rng.below(3) {
                    0 if !versions[j].contains('-') => "*".to_owned(),
                    1 => format!("={}", versions[j]),
                    _ => format!(">={}", versions[j]),
                };
                dependencies.insert(
                    name(names[j]),
                    vesper_loom_calculus::Dependency {
                        version: VersionReq::parse(&req).unwrap(),
                        effects: if rng.chance(96) {
                            CAPS.iter().map(|c| c.to_string()).collect()
                        } else {
                            rng.some(CAPS, 2)
                        },
                        optional: rng.chance(20),
                        features: BTreeSet::new(),
                        path: None,
                    },
                );
            }
        }
        let mut features = BTreeMap::new();
        for f in rng.some(FEATURES, 2) {
            let optional: Vec<PackageName> = dependencies
                .iter()
                .filter(|(_, d)| d.optional)
                .map(|(n, _)| n.clone())
                .collect();
            let mut sys_deps = BTreeSet::new();
            if rng.chance(30) {
                let (sys, _, req) = SYSTEMS[rng.below(SYSTEMS.len())];
                used_systems.insert(sys);
                sys_deps.insert(req.to_owned());
            }
            features.insert(
                f,
                Feature {
                    enables_effects: rng.some(CAPS, 1),
                    deps: optional.into_iter().filter(|_| rng.chance(60)).collect(),
                    features: BTreeSet::new(),
                    sys_deps,
                },
            );
        }
        let feature_names: Vec<String> = features.keys().cloned().collect();
        packages.push(Selection {
            manifest: Manifest {
                package: Package {
                    name: name(names[i]),
                    version: Version::parse(versions[i]).unwrap(),
                    edition: "2026".into(),
                    license: None,
                    capability_ceiling: if rng.chance(97) {
                        CAPS.iter().map(|c| c.to_string()).collect()
                    } else {
                        rng.some(CAPS, 2)
                    },
                    forbid_unsafe_ffi: rng.chance(70),
                    vsys: vec![],
                    objects: vec![],
                },
                features,
                dependencies,
                build_sandbox: None,
                resolver: None,
                verification: None,
            },
            features: feature_names
                .into_iter()
                .filter(|_| rng.chance(50))
                .collect(),
        });
    }
    for i in (1..packages.len()).rev() {
        packages.swap(i, rng.below(i + 1));
    }
    let mut system_packages: Vec<SystemSelection> = SYSTEMS
        .iter()
        .filter(|(s, _, _)| used_systems.contains(s) || rng.chance(30))
        .map(|(s, v, _)| SystemSelection {
            name: s.to_string(),
            version: Version::parse(v).unwrap(),
        })
        .collect();
    if rng.chance(5) {
        system_packages.push(SystemSelection {
            name: "zlib".into(),
            version: Version::parse("1.2.13").unwrap(),
        });
    }
    let roots = if rng.chance(4) {
        vec![]
    } else {
        let root = names[0];
        vec![RootRequirement {
            name: name(root),
            version: VersionReq::parse(if rng.chance(95) { ">=0.0.0-0" } else { "^9" }).unwrap(),
            features: BTreeSet::new(),
        }]
    };
    Solution {
        schema_version: if rng.chance(3) { 2 } else { SCHEMA_VERSION },
        roots,
        packages,
        system_packages,
    }
}

/// Lockfile bytes: generated snapshots (compact and pretty) and a few
/// that do not parse.
fn lockfiles(n: usize) -> Vec<Vec<u8>> {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut out = vec![];
    for _ in 0..n {
        let s = solution(&mut rng);
        out.push(serde_json::to_vec(&s).unwrap());
        out.push(serde_json::to_vec_pretty(&s).unwrap());
    }
    out.push(b"{".to_vec());
    out.push(br#"{"schema_version":1,"roots":[],"packages":[],"extra":1}"#.to_vec());
    out.push(br#"{"schema_version":1,"roots":[],"packages":[]}"#.to_vec());
    out
}

fn options(lock: &[u8]) -> Vec<(Option<String>, bool, bool, bool)> {
    let mut focus = vec![None, Some("@none/missing".to_owned())];
    if let Ok(s) = serde_json::from_slice::<Solution>(lock) {
        focus.extend(
            s.packages
                .iter()
                .map(|p| Some(p.manifest.package.name.as_str().to_owned())),
        );
    }
    let mut out = vec![];
    for f in focus {
        for invert in [false, true] {
            for (json, dot) in [(false, false), (true, false), (false, true)] {
                out.push((f.clone(), invert, json, dot));
            }
        }
    }
    out
}

#[test]
fn graphs_match_the_rust_graph_command() {
    let mut diffs = vec![];
    let mut runs = 0;
    let mut graphs = 0;
    for lock in lockfiles(150) {
        for (focus, invert, json, dot) in options(&lock) {
            let want = rust(&lock, focus.as_deref(), invert, json, dot);
            let got = lock_graph(&lock, focus.as_deref(), invert, json, dot);
            runs += 1;
            if want.0 == OK {
                graphs += 1;
            }
            if got != want {
                diffs.push(format!(
                    "on {} (focus {focus:?}, invert {invert}, json {json}, dot {dot})\n  vesper {got:?}\n  rust   {want:?}",
                    String::from_utf8_lossy(&lock)
                ));
            }
        }
    }
    eprintln!("{graphs} of {runs} runs render a graph");
    assert!(
        diffs.is_empty(),
        "{} of {runs} differ:\n{}",
        diffs.len(),
        diffs
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        graphs * 4 > runs,
        "too few valid graphs: {graphs} of {runs}"
    );
}

#[test]
fn focus_names_are_checked_like_package_name_parse() {
    for f in [
        "@org/app",
        "std/bytes",
        "org/app",
        "@org",
        "",
        "@Org/app",
        "@org/app/x",
    ] {
        let want = PackageName::parse(f).err().unwrap_or_default();
        assert_eq!(graph_focus_error(f), want, "{f:?}");
    }
}
