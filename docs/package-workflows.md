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

## Offline resolution

```sh
cargo run -- resolve examples/loom/Loom.toml --catalog examples/loom/catalog.json -o /tmp/Loom.lock --json
```

The catalog supplies local manifest/version metadata; the command selects concrete versions and validates the returned solution with `vesper-loom-calculus`. No registry queries, downloads, builds, macros or package code execute. The root manifest defaults to `Loom.toml`; `--catalog` is required. `--features a,b` requests root features and `--max-decisions N` bounds search. Only `-o` writes a lockfile; otherwise the command reports its result without writing one.

Successful JSON output is `{"status":"resolved","solution":...}`. Solver errors use `{"status":"error","error":...}` and exit nonzero. Search-budget exhaustion is distinct from an unsatisfiable graph. File/argument parsing errors are reported on stderr. The output lockfile is a schema-versioned exact `Solution` snapshot containing manifests, selected features and system versions. It is not a signed registry lockfile or a trusted artifact derivation.

Lockfiles are written via a temporary file and atomic replacement after successful resolution and serialization. A resolution or temporary-write failure leaves an existing lockfile intact. As with the standalone calculus, capability checks validate declared metadata, not actual compiler-body authority. Package-qualified authority analysis and provenance verification remain separate integration gates. Unsupported optimization/attestation/vetting policies fail closed rather than being reported as enforced.

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
