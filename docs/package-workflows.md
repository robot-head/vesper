# Interface comparison and offline package resolution

These commands connect the checked compiler and the first Loom libraries. The seven external workstreams in `parallel-work.md` retain their assigned paths.

## Checked source interfaces

```sh
cargo run -- emit examples/interfaces/v1.ves --emit interface --package @example/receipts -o /tmp/receipts-v1.vhir
cargo run -- emit examples/interfaces/v2.ves --emit interface --package @example/receipts -o /tmp/receipts-v2.vhir
cargo run -- semver /tmp/receipts-v1.vhir /tmp/receipts-v2.vhir --json
```

The example strengthens `Receipt` from linear to copy and classifies as a compatible minor change. Export `v3-breaking.ves` with the same package namespace and compare against v2 to see an added `Fail[u32]` requirement classified as major. Breaking and unknown results exit nonzero; `--json` still writes the structured report to stdout. Caller witnesses are descriptions, not compiler-checked source snippets.

Export runs after parsing/checking and before optimization. Its deterministic JSON includes declared public contracts, not source spans, bodies or parameter/local names. All current source declarations are public because visibility syntax is not implemented. Builtins retain reserved `std/core` identities. `--package` supplies an identity namespace, not authenticated namespace ownership.

The `.vhir` suffix here denotes an experimental schema/semantics-versioned source-contract DTO, not a binary ABI, MIF hash, artifact-cache key, proof certificate, or finalized interchange format. Named/latent capability scopes, effectful arrows, captured generic arrows and named enum payloads remain explicit Unsupported contracts; even identical unsupported exports cannot certify compatibility. See `parallel-results/interface-export.md` for the full boundary.

### Release-version policy

```sh
cargo run -- semver /tmp/receipts-v1.vhir /tmp/receipts-v2.vhir --old-version 1.0.0 --new-version 1.1.0 --json
```

Supplying both version flags checks whether the proposed release bump covers the actual interface classification. Patch requires at least a patch increase, Minor at least a minor increase, and Major a numeric major increase. Larger bumps are allowed. Unknown interface comparisons always fail, including across a major bump. Without these flags, the existing source-compatibility check and JSON report are unchanged.

Release policy version 1 compares SemVer precedence, so changing build metadata alone is not an increase. It applies the same conservative numeric-component rules to `0.x`; there is no automatic exception for breaking pre-1.0 changes. Prerelease comparisons are explicitly unsupported by this policy. Invalid version strings and unpaired/duplicate flags are errors.

The versioned release report contains the comparison, required/actual bump, acceptance and policy issues. An accepted intentional major release may still require downstream migration: breaking witnesses and required reborrow adapters remain in the report. This does not make it source-compatible or publish the package, and it does not establish source identity, provenance or executable ABI compatibility.

## Offline resolution

```sh
cargo run -- resolve examples/loom/Loom.toml --catalog examples/loom/catalog.json -o /tmp/Loom.lock --json
```

The catalog supplies local manifest/version metadata; the command selects concrete versions and validates the returned solution with `vesper-loom-calculus`. No registry queries, downloads, builds, macros or package code execute. The root manifest defaults to `Loom.toml`; `--catalog` is required. `--features a,b` requests root features and `--max-decisions N` bounds search. Only `-o` writes a lockfile; otherwise the command reports its result without writing one.

Successful JSON output is `{"status":"resolved","solution":...}`. Solver errors use `{"status":"error","error":...}` and exit nonzero. Search-budget exhaustion is distinct from an unsatisfiable graph. File/argument parsing errors are reported on stderr. The output lockfile is a schema-versioned exact `Solution` snapshot containing manifests, selected features and system versions. It is not a signed registry lockfile or a trusted artifact derivation.

Lockfiles are written via a temporary file and atomic replacement after successful resolution and serialization. Staging names have a fixed short prefix independent of the destination filename, so a valid maximum-length output name remains usable. A resolution or temporary-write failure leaves an existing lockfile intact. As with the standalone calculus, capability checks validate declared metadata, not actual compiler-body authority. Package-qualified authority analysis and provenance verification remain separate integration gates. Unsupported optimization/attestation/vetting policies fail closed rather than being reported as enforced.

The first resolver uses bounded deterministic backtracking, not the design's planned CDCL/Max-SMT engines or its performance targets. The source ceiling, feature and version constraints are useful now while those engines and external trust services remain future work.

## Read-only locked resolution

After creating a solution snapshot with `resolve -o`, check it without resolving again:

```sh
cargo run -- resolve examples/loom/Loom.toml --catalog examples/loom/catalog.json --locked --lockfile /tmp/Loom.lock --json
```

`--locked` reads `Loom.lock` in the current working directory by default; `--lockfile` chooses another input path. It never writes or replaces the snapshot, and a missing snapshot is an error. It cannot be combined with `-o` or `--max-decisions`, and `--lockfile` requires `--locked`.

Validation checks the current root manifest and requested features, every selected manifest's exact metadata against the local catalog, native versions, and the complete dependency/capability graph. It recomputes the reachable feature closure from the request rather than trusting features embedded in the snapshot. Extra roots, unused packages/native selections and injected features are rejected. A valid older selection remains pinned when the catalog gains newer versions; no search is performed.

Successful JSON output is `{"status":"locked","solution":...}`. Invalid or stale snapshots return an error with kind `locked_mismatch`, distinct from a failed search's `unsatisfiable`. Invalid catalog/manifest inputs and unsupported policy remain `invalid_input` and `unsupported_policy`. Structured validation errors use the existing `{"status":"error","error":...}` envelope with zero search decisions. Argument, file and JSON parsing errors go to stderr. Failed validation leaves all files untouched.

Locked validation checks consistency with supplied local metadata. It does not authenticate that metadata, verify package source bytes, audit compiler authority, execute builds or establish provenance. The snapshot format remains the experimental exact `Solution` DTO described above.

## Guarding declared permissions during resolution

```sh
cargo run -- resolve examples/loom/Loom.toml --catalog examples/loom/catalog.json \
  --baseline /tmp/Loom.lock -o /tmp/Loom.lock --json
```

`--baseline` is an opt-in guard on a fresh resolution. The existing snapshot is read and validated first; the candidate is resolved normally, then compared before any output is written. Both graphs must have the same nonempty root identities. Successful guarded JSON includes `upgrade` alongside `status: "resolved"` and `solution`; ordinary resolution without the flag is unchanged. `--baseline` cannot be combined with `--locked`.

The guard rejects added required capabilities, widened package/dependency ceilings and relaxing `forbid_unsafe_ffi`, including grants on edges whose current target is pure. It compares each reachable package, so moving `Net` from one dependency to another is detected even if the root's total remains unchanged. Typed capability identities are compared exactly. Reductions and new pure dependencies are allowed; native-version changes alone do not count as authority changes. Unreachable selections do not grant authority, but both entire snapshots must still pass structural validation.

A rejected comparison exits nonzero, returns `capability_escalation` or `upgrade_mismatch` plus the structured upgrade report in JSON mode, and preserves existing output bytes. Invalid baseline graphs produce `invalid_baseline`; file/JSON parsing errors use stderr. The guard checks the proposed result rather than searching for an older alternative. It has no override flag in this milestone.

This checks declared local metadata, not compiler-body authority, interface SemVer, source provenance, version progression or binary ABI. A caller-supplied baseline is not authenticated. The command performs no builds, package code or tests; full automatic update/publish policy remains separate integration work.

## Inspecting the package graph

```sh
cargo run -- graph /tmp/Loom.lock
cargo run -- graph /tmp/Loom.lock --package @example/math --invert --json
cargo run -- graph /tmp/Loom.lock --dot > /tmp/loom.dot
```

`graph` reads `Loom.lock` by default and validates the entire snapshot before displaying it. No catalog, source compiler, search or build execution is involved. Use `resolve --locked` first when you need to check that the snapshot still agrees with a current manifest/catalog.

The default view includes all reachable packages and native requirements. `--package @org/name` follows that package's dependency closure; adding `--invert` follows its dependents instead. Cycles terminate and shared dependencies appear once. Native requirements of the displayed packages remain visible. Unknown or unreachable focus packages are errors. Unreachable package/native selections in an otherwise valid snapshot are reported separately and excluded from the graph.

Text output shows versions, features, declared ceilings, required capabilities and dependency constraints. Required capabilities are the calculus's conservative union of declared metadata across the complete reachable graph; filtering the view does not recalculate them over a smaller subgraph. They are not an audit of compiler-body authority.

`--json` emits a schema-versioned report with `status: "graph"`, `basis: "declared_metadata"`, the direction/focus, original snapshot roots, visible package/native nodes and edges, and unused selection lists. Dependency records retain their original `dependent`/`dependency` identities in both directions. `--dot` emits deterministic Graphviz DOT with distinct Vesper/native node identities; `--invert` reverses its arrows. The two output flags are mutually exclusive.

Invalid snapshots and unknown focus packages produce nonzero exits with structured errors in JSON mode. Arguments, file reads and JSON parsing errors use stderr. No output files are created or rewritten by this command; use shell redirection to save its output. Report schema 1 is an inspection format, not an interface fingerprint, authenticated SBOM or provenance certificate. Resolver trust/optimization policy is not enforced by graph inspection.

## CycloneDX inventory export

```sh
cargo run -- sbom /tmp/Loom.lock > /tmp/vesper-bom.json
```

`sbom` emits CycloneDX 1.6 JSON by default; its input defaults to `Loom.lock`. It validates the complete snapshot before producing any output and includes only reachable Vesper/native components and their direct dependencies. Shared dependencies appear once, cycles remain graph edges, and every dependency reference resolves to an emitted component. Valid unused selections are disclosed in metadata properties.

Deterministic generic Package URLs preserve Vesper namespaces, reserved `std/*` identities, native identity domains and SemVer build metadata. The document records active features, declared/required capability metadata, edge budgets, FFI policy and native requirements as namespaced properties. Free-form license text is preserved without claiming SPDX-expression validation. No random UUID, timestamp, fabricated source hash or installed-system inventory is added.

This is a standard-format inventory of supplied declarations, not an authenticated source SBOM, CVE scan, ABI check or compiler authority audit. Native identities still use the local catalog's SemVer model, not Debian/RPM versions. Generation performs no network calls, builds, code execution or file writes. Invalid snapshots, malformed JSON and option errors exit nonzero on stderr without a partial BOM. Use `resolve --locked` beforehand to check current manifest/catalog consistency.
