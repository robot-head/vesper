//! The compiler's closure (SH6 lane 3, #1398): the workspace crates that the
//! `vesper` package (the compiler library and binary) links as normal
//! dependencies, directly or through another workspace crate.
//!
//! A crate the compiler links cannot lose its Rust before Stage 2, so tool
//! commands link their libraries from `crates/vesper-tools` instead. This test
//! fails when a workspace crate outside [`ALLOWED`] re-enters the closure, and
//! when a crate in [`ALLOWED`] has left it (then delete its row, so the list
//! only shrinks). It reads the manifests the way `cargo tree -p vesper -e
//! normal` resolves path dependencies: `[dependencies]` and every
//! `[target.*.dependencies]` table, through `workspace = true` entries, on all
//! targets at once. Optional dependencies are left out: no crate in the
//! closure turns one on (a feature that did would show in `cargo tree`, which
//! the report for #1398 records).
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Every workspace crate in the compiler's closure, by package name, with why
/// the compiler needs it. Shrink this list; never grow it without moving the
/// use behind `vesper <tool>` first.
const ALLOWED: &[(&str, &str)] = &[
    (
        "vesper-alloc-seed",
        "Strata regions evaluate on the host region allocator",
    ),
    (
        "vesper-beacon-seed",
        "trace effects run on the span pool and telemetry ring",
    ),
    (
        "vesper-chrono",
        "`vesper test` simulation capsules (anvil_sim)",
    ),
    ("vesper-core", "shared compiler data types"),
    (
        "vesper-crucible-seed",
        "`vesper test` simulation and native coverage probes",
    ),
    ("vesper-diagnostics-seed", "diagnostic rendering"),
    ("vesper-doc", "`vesper doc`, doctests and LSP documentation"),
    ("vesper-ebpf-seed", "the eBPF backend and stack bounds"),
    ("vesper-ffi", "FFI lowering and `.vsys` imports"),
    ("vesper-fiber-abi", "native effect handlers and sessions"),
    (
        "vesper-fmt-seed",
        "`vesper fmt`, LSP actions and canonical output",
    ),
    ("vesper-forge-seed", "the Forge front end"),
    (
        "vesper-highlight-seed",
        "`vesper highlight` and `vesper shot`",
    ),
    (
        "vesper-kinetic-seed",
        "the `detfloat` backend verifier (eff [DetFloat])",
    ),
    ("vesper-lean-syntax-seed", "Lean extraction"),
    ("vesper-lint-seed", "`vesper lint` and LSP diagnostics"),
    (
        "vesper-loom-calculus-seed",
        "package manifests and authority",
    ),
    (
        "vesper-loom-cas",
        "test verdict cache, doc certificates, `vesper verify`",
    ),
    ("vesper-lsp", "`vesper lsp`"),
    ("vesper-pact-binding", "the Pact front end"),
    ("vesper-pact-seed", "the Pact front end"),
    ("vesper-pulse", "atomics workers and native sessions"),
    (
        "vesper-resonance-seed",
        "the `realtime` backend verifier (eff [Realtime])",
    ),
    (
        "vesper-semver-seed",
        "interfaces and `vesper lint --semver`",
    ),
    ("vesper-serde-seed", "linked through vesper-lsp"),
    (
        "vesper-silicon-seed",
        "stack bounds and freestanding images",
    ),
    ("vesper-simpulse-seed", "linked through vesper-chrono"),
    ("vesper-span-seed", "source spans"),
    (
        "vesper-std-host",
        "the host shim of @vesper-std/fs, process and env",
    ),
    ("vesper-syntax", "the parser"),
    (
        "vesper-tensor-seed",
        "source tensor types evaluate on the host kernels",
    ),
    ("vesper-test", "`vesper test`"),
    (
        "vesper-vault",
        "the `const-time` backend verifier (eff [ConstTime])",
    ),
];

/// Crates that SH6 lane 3 moved out; named so a regression says which.
const MOVED_OUT: &[&str] = &[
    "vesper-loom-admission",
    "vesper-loom-trust",
    "vesper-migrate",
    "vesper-simd",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A manifest's dependency tables that Cargo resolves for a normal build:
/// entry key to the raw inline value.
fn normal_dependencies(manifest: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut in_table = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let header = line.trim_matches(|c| c == '[' || c == ']');
            in_table = header == "dependencies"
                || (header.starts_with("target.") && header.ends_with(".dependencies"));
            continue;
        }
        if !in_table || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    out
}

/// The `[workspace.dependencies]` table of the root manifest.
fn workspace_dependencies(manifest: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut in_table = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_table = line == "[workspace.dependencies]";
            continue;
        }
        if in_table
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            out.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    out
}

/// The string value of `field = "..."` inside an inline table.
fn field(value: &str, name: &str) -> Option<String> {
    let mut rest = value;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().last();
        let after = rest[at + name.len()..].trim_start();
        if matches!(before, None | Some('{' | ' ' | ',')) && after.starts_with('=') {
            let quoted = after[1..].trim_start().strip_prefix('"')?;
            return Some(quoted[..quoted.find('"')?].to_owned());
        }
        rest = &rest[at + name.len()..];
    }
    None
}

fn package_name(manifest: &str) -> String {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some(name) = line.strip_prefix("name")
            && let Some(name) = name.trim_start().strip_prefix('=')
        {
            return name.trim().trim_matches('"').to_owned();
        }
    }
    panic!("manifest without [package] name")
}

/// Workspace package names in the closure, or `None` when a manifest is not
/// readable (Bazel declares only the root package's files as test data).
fn closure() -> Option<BTreeSet<String>> {
    let root = root();
    let root_manifest = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let workspace = workspace_dependencies(&root_manifest);
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut stack: Vec<PathBuf> = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let dir = fs::canonicalize(&dir).unwrap_or(dir);
        if !seen.insert(dir.clone()) {
            continue;
        }
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        if dir != fs::canonicalize(&root).unwrap_or(root.clone()) {
            names.insert(package_name(&manifest));
        }
        for (key, value) in normal_dependencies(&manifest) {
            if value.replace(' ', "").contains("optional=true") {
                continue;
            }
            let (base, value) = if field(&value, "workspace").is_none()
                && value.replace(' ', "").contains("workspace=true")
            {
                let name = key.trim_end_matches(".workspace").to_owned();
                match workspace.get(&name) {
                    Some(v) => (root.clone(), v.clone()),
                    None => continue,
                }
            } else {
                (dir.clone(), value)
            };
            if let Some(path) = field(&value, "path") {
                stack.push(base.join(Path::new(&path)));
            }
        }
    }
    Some(names)
}

#[test]
fn the_compiler_links_only_the_allowed_crates() {
    let Some(names) = closure() else {
        assert!(
            std::env::var_os("TEST_SRCDIR").is_some(),
            "a workspace manifest is unreadable outside Bazel"
        );
        eprintln!("skipped: Bazel declares only the root package's manifest");
        return;
    };
    let allowed: BTreeSet<String> = ALLOWED.iter().map(|(n, _)| (*n).to_owned()).collect();
    for moved in MOVED_OUT {
        assert!(
            !names.contains(*moved),
            "{moved} re-entered the compiler's closure; link it from crates/vesper-tools"
        );
    }
    let entered: Vec<_> = names.difference(&allowed).collect();
    assert!(
        entered.is_empty(),
        "workspace crates entered the compiler's closure: {entered:?}; move the use behind \
         `vesper <tool>` (crates/vesper-tools) or, if checking or code generation needs it, add \
         it to ALLOWED in tests/compiler_closure.rs with the reason"
    );
    let left: Vec<_> = allowed.difference(&names).collect();
    assert!(
        left.is_empty(),
        "crates left the compiler's closure: {left:?}; delete their ALLOWED rows in \
         tests/compiler_closure.rs and mark them flippable in scripts/rust-census.py"
    );
}

#[test]
fn the_parser_reads_inline_tables() {
    assert_eq!(
        field(r#"{ path = "crates/x/seed", package = "x-seed" }"#, "path").as_deref(),
        Some("crates/x/seed")
    );
    assert_eq!(
        field(r#"{ version = "1", workspace = true }"#, "path"),
        None
    );
    let deps = normal_dependencies(
        "[dependencies]\na = { path = \"a\" }\n[dev-dependencies]\nb = { path = \"b\" }\n\
         [target.'cfg(unix)'.dependencies]\nc = { path = \"c\" }\n",
    );
    let keys: Vec<_> = deps.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["a", "c"]);
}
