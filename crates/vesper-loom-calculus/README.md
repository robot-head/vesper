# vesper-loom-calculus

The first Loom package-calculus milestone: declarative manifests and deterministic validation of proposed exact-version solutions. It does not resolve versions or execute package/build code.

```rust
use vesper_loom_calculus::{parse_manifest, validate_solution, PackageName,
    RootRequirement, Selection, Solution, SCHEMA_VERSION};
let manifest = parse_manifest(r#"
[package]
name = "@example/app"
version = "1.0.0"
edition = "2026"
capability_ceiling = []
"#).unwrap();
let solution = Solution {
    schema_version: SCHEMA_VERSION,
    roots: vec![RootRequirement {
        name: PackageName::parse("@example/app").unwrap(),
        version: "^1".parse().unwrap(),
        features: Default::default(),
    }],
    packages: vec![Selection { manifest, features: Default::default() }],
    system_packages: vec![],
};
let validated = validate_solution(&solution);
assert!(validated.diagnostics.is_empty());
let graph = validated.graph.unwrap();
assert!(graph.canonical_json().unwrap().contains("@example/app"));
```

`Solution` and `PackageGraph` carry schema version 1. Ordered maps/sets and sorted diagnostics make graph serialization and diagnostics independent of input selection order, including duplicate-selection errors. `canonical_json` means stable DTO ordering, not semantic normalization of all equivalent SemVer ranges, a MIF hash, or a provenance certificate.

Validation enforces root inclusion, active dependency closure, one exact version per Vesper/native package name, version requirements, package and dependency-edge capability ceilings, and transitive `forbid_unsafe_ffi`. An additive feature fixed point expands local feature references, optional dependencies, dependency-requested features, and separately named native requirements. Dependency/feature cycles terminate; transitive authority uses a backwards worklist. Unreachable selections are reported separately and excluded from the returned graph. Every selection is still structurally validated.

Capabilities are canonical identity strings compared exactly. Required capabilities conservatively include every reachable package's declared ceiling and active feature effects. No compiler authority analysis is performed. A compiler adapter must audit privileged handler installation and FFI throughout package bodies; a public outgoing effect row is insufficient because local privileged handlers can discharge it. The adapter must resolve package-qualified effect identities and kind-check arguments before using a graph result as a confinement gate. Schema 1 rejects package-qualified capability spellings beginning `@`; it does not silently reduce them to basenames. Opaque argument strings such as `Ffi[LibA]` and `Ffi[LibB]` remain distinct, but alias equivalence and argument kinds are not certified here. Active feature effects must stay inside the package ceiling. Monotonic features cannot disable effects/dependencies, and unknown fields/references fail closed. Network-enabled or path-escaping build sandbox configurations are rejected, but no Wasmtime execution occurs here.

Native requirements currently use `name SemVerRequirement` (for example `liburing ^2.6.0`). They never alias Vesper package names. Debian/RPM-specific versions, platform predicates, native dependency closure, and real sysroot resolution require later schema/solver work.

This crate does not provide CDCL/Max-SMT resolution, namespace/OIDC ownership verification, registry downloads, Sigstore/TUF checks, CVE policy optimization, cryptographic hashes, compiler SemVer checks, SBOMs, or hermetic build execution. Resolver/trust/verification configuration is retained as declarative data, not treated as verified evidence.
