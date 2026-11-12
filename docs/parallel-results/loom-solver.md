# Offline Loom resolution

Implemented `crates/vesper-loom-solver` with a versioned serde local catalog and
the agreed CLI-facing API:

```rust,ignore
Catalog {
    schema_version: u32,
    packages: Vec<vesper_loom_calculus::Manifest>,
    system_packages: Vec<vesper_loom_calculus::SystemSelection>,
}
ResolveRequest {
    root: Manifest,
    features: BTreeSet<String>,
    catalog: Catalog,
    max_decisions: usize,
}
pub fn resolve(request: &ResolveRequest) -> Result<Solution, ResolveError>;
```

`ResolveError` is serializable and has public `kind: ErrorKind`,
`decisions: usize`, and `diagnostics: Vec<ResolveDiagnostic>` fields.
`ErrorKind` distinguishes `InvalidInput`, `UnsupportedPolicy`, `Unsatisfiable`,
and `BudgetExhausted`. Diagnostic fields are `code`, `package`, `subject`, and
`message`. Error kinds serialize as snake_case.

The supplied root manifest is fixed exactly, including version build metadata.
Other versions are tried newest first, with lexical package ordering independent
of catalog input order. An explicit DFS stack performs actual backtracking over
transitive version conflicts, unavailable features, native package constraints,
and declaration capability failures. Feature expansion reaches a fixed point
again after each assignment, including late requests on previously visited
nodes and cycles. Native selections are chosen after all active requirements
are known; native candidates have no dependency metadata in this schema.

Partial calculus validation prunes monotonic declaration failures. A successful
result must pass `validate_solution`, have a graph, have no diagnostics, and
have no unused package selections. Duplicate candidate identities, invalid
manifests/native names, conflicting root identities, unknown catalog schemas,
and unknown serde fields fail closed. Native versions use SemVer only; native
platform constraints, distro versions, registry identities, and ABI closure
are not represented.

Selected resolver policies requesting namespace vetting, positive
reproducibility attestations, or optimization are unsupported. A different
candidate without those requirements may be selected. If no supported solution
is found and a branch needed an unsupported policy, the result is
`UnsupportedPolicy`, not a claim of unsatisfiability. Merely present unselected
policies do not block resolution. A deserialized policy table defaults
`block_unvetted_namespaces` to true, as required by the calculus DTO; callers
must explicitly disable that requirement or omit the table for unverified
offline resolution. No namespace authenticity is inferred.

This is a bounded backtracking milestone, not Resolvo CDCL or Max-SMT. It makes
no 50 ms / 1,000-node performance claim and produces encountered conflict
diagnostics rather than minimal incompatibility derivations. `max_decisions`
counts visited non-root package assignments, not pure feature propagation or
native version filtering. Root-only graphs may succeed with a zero budget;
budget exhaustion reports unknown satisfiability. Search is iterative, but
copies assignments and recomputes graph validation per candidate; large catalogs
still require memory and CPU proportional to the explored search.

The returned `Solution` is an offline exact-version snapshot, not the complete
cryptographic Loom.lock: source/interface/provenance/builder/proof anchors are
absent. Supplied capability declarations are checked; actual compiler body,
privileged handler, FFI, and macro authority audits remain separate work. Public
outgoing effect rows cannot establish package confinement.

Validation in the isolated crate:

- `cargo fmt --manifest-path crates/vesper-loom-solver/Cargo.toml --check`
- `cargo test --manifest-path crates/vesper-loom-solver/Cargo.toml` — 19 tests
- `cargo clippy --manifest-path crates/vesper-loom-solver/Cargo.toml --all-targets -- -D warnings`

Tests cover real fallback from latest candidates, combined constraints,
optional/nested/transitive features, feature/package/root cycles, exact root
preservation, source and edge ceilings, FFI identities and unsafe bans, native
version intersections and fallback, invalid/duplicate identities, unsupported
policies, catalog permutation determinism, error serde roundtrip, exhaustive
failure versus budget exhaustion, and a 256-package chain on a 128 KiB thread
stack. There is no registry, download, code execution, trust, or external-lane
integration in this crate.

Integration owner should remove the temporary `[workspace]` from the crate
manifest and register the crate in the root workspace. No root manifest,
lockfile, or CLI changes are included in this branch.
