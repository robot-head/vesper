# Loom manifest and package-calculus milestone

Implemented `crates/vesper-loom-calculus` against specification §8. The crate parses the supplied Loom.toml shape, validates scoped package identities and declarative configuration, expands additive features, and validates proposed exact-version package solutions. Its versioned JSON DTOs are independent of experimental compiler ASTs.

The public entry points are `parse_manifest`, `validate_manifest`, and `validate_solution`; a successful `Validation` includes a deterministic `PackageGraph`. Diagnostics are structured, sorted, and deduplicated. Errors never return a graph that could be mistaken for a validated solution. Unreachable but structurally valid selections are reported and omitted from the graph. System packages occupy a separate namespace and currently require SemVer-compatible versions.

Capability verification is conservative over declared package ceilings, active feature effects, and their transitive closure. Every active dependency edge must authorize its target's transitive required capabilities. Package ceilings must cover that closure, and `forbid_unsafe_ffi` applies transitively. This is a graph-level declaration check. A compiler authority audit must inspect privileged handler installation and FFI in package bodies: an outgoing public effect row can discharge local privileged handlers and is not such an audit. Package-qualified identities and kinded arguments must be resolved by that adapter; schema 1 rejects unsupported package-qualified spellings instead of collapsing names. Full opaque strings such as `Ffi[LibA]` and `Ffi[LibB]` are compared distinctly. No compiler authority adapter is connected yet.

Acceptance coverage includes the documented manifest, missing roots/dependencies, duplicate/exact-version failures, transitive escalation, optional dependencies, feature cycles and propagation, additive feature monotonicity, native requirements, unknown feature/manifest fields, FFI policy, sandbox path/network policy, and permutation-invariant diagnostics/serialization.

Integration should remove the crate's temporary `[workspace]` table, add it to the root workspace, and update the root lockfile. The local `.gitignore` excludes standalone target/lock files; workspace builds use the root lockfile. No root compiler files or manifests changed in this branch.

Remaining work: CDCL/Max-SMT solver, compiler interface/effect-ceiling adapter, native OS version schemes, platform conditions and sysroot closure, registry/CAS materialization, trust/attestation checks, Wasmtime execution, and CLI wiring. None is claimed implemented by this milestone.

Validation completed with 17 integration tests plus the compiled public README example, `cargo fmt --check`, and Clippy over all targets with warnings denied.
