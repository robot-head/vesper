//! The seed-copy guard (port wave P1 lane 2, #899; "Crates the compiler
//! links" in docs/maintenance/porting-a-crate.md). No port-backed package,
//! that is no package whose build script runs this harness, may be in the
//! normal or build dependency closure of the compiler (`vesper`) or of the
//! verified backend (`vesper-codegen-verified`, which links no Vesper-built
//! code at all). Bazel checks the same on its own graph
//! (crates/vesper-port-harness/BUILD.bazel, `:compiler_closure_has_no_ports`).
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const ROOTS: [&str; 2] = ["vesper", "vesper-codegen-verified"];

fn metadata() -> Option<Value> {
    // Cargo sets CARGO for the tests it runs; Bazel does not, and runs the
    // query-based guard instead.
    let cargo = std::env::var_os("CARGO")?;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new(cargo)
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .expect("cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).expect("metadata JSON"))
}

/// Package id -> name.
fn names(meta: &Value) -> BTreeMap<String, String> {
    meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_owned(),
                p["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Resolved dependencies of each package: (dependency id, kinds).
fn edges(meta: &Value) -> BTreeMap<String, Vec<(String, Vec<String>)>> {
    meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let deps = n["deps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| {
                    let kinds = d["dep_kinds"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|k| k["kind"].as_str().unwrap_or("normal").to_owned())
                        .collect();
                    (d["pkg"].as_str().unwrap().to_owned(), kinds)
                })
                .collect();
            (n["id"].as_str().unwrap().to_owned(), deps)
        })
        .collect()
}

/// Port-backed packages in the closure of `root`, by name.
fn violations(meta: &Value, root: &str) -> Vec<String> {
    let names = names(meta);
    let edges = edges(meta);
    let id_of = |name: &str| {
        names
            .iter()
            .find(|(_, n)| n.as_str() == name)
            .map(|(id, _)| id.clone())
    };
    let harness = id_of("vesper-port-harness").expect("vesper-port-harness is a workspace member");
    let port_backed = |id: &String| {
        edges[id]
            .iter()
            .any(|(dep, kinds)| *dep == harness && kinds.iter().any(|k| k == "build"))
    };
    let mut seen = BTreeSet::new();
    let mut todo = vec![id_of(root).unwrap_or_else(|| panic!("no package {root}"))];
    let mut found = Vec::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if port_backed(&id) {
            found.push(names[&id].clone());
        }
        for (dep, kinds) in &edges[&id] {
            if kinds.iter().any(|k| k == "normal" || k == "build") {
                todo.push(dep.clone());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn compiler_closure_links_only_seed_packages() {
    let Some(meta) = metadata() else {
        eprintln!("skipped: not run by Cargo (Bazel runs :compiler_closure_has_no_ports)");
        return;
    };
    for root in ROOTS {
        assert_eq!(
            violations(&meta, root),
            Vec::<String>::new(),
            "port-backed packages in the dependency closure of {root}: depend on their \
             `<crate>-seed` package instead (docs/maintenance/porting-a-crate.md)"
        );
    }
    // The guard sees ports at all: vesper-beacon-arrow, outside the
    // closure, is port-backed and links the port-backed vesper-beacon.
    assert_eq!(
        violations(&meta, "vesper-beacon-arrow"),
        ["vesper-beacon", "vesper-beacon-arrow"]
    );
}

#[test]
fn guard_fails_when_the_compiler_links_a_port() {
    let Some(mut meta) = metadata() else {
        eprintln!("skipped: not run by Cargo");
        return;
    };
    // The deliberate violation: point the compiler's dependency on the
    // Beacon seed at the port-backed package instead.
    let names = names(&meta);
    let id = |name: &str| {
        names
            .iter()
            .find(|(_, n)| n.as_str() == name)
            .map(|(id, _)| id.clone())
            .unwrap()
    };
    let (vesper, seed, port) = (id("vesper"), id("vesper-beacon-seed"), id("vesper-beacon"));
    let nodes = meta["resolve"]["nodes"].as_array_mut().unwrap();
    let node = nodes
        .iter_mut()
        .find(|n| n["id"] == vesper.as_str())
        .unwrap();
    let dep = node["deps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["pkg"] == seed.as_str())
        .expect("vesper depends on vesper-beacon-seed");
    dep["pkg"] = Value::String(port);
    assert_eq!(violations(&meta, "vesper"), ["vesper-beacon"]);
}
