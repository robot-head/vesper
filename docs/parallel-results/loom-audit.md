# Bounded local Loom advisory and license audit

Base: `1ffd0ae` (`main`, merged PR23). Branch: `parallel-next/loom-audit`.
All edits are under the new `crates/vesper-loom-audit` and this report; no shared
schema, calculus, compiler, Lean, SIMD, existing CLI or workflow edits.

## Delivered behavior

A standalone, runnable crate audits an exact public calculus `Solution`, always
validating that solution before obtaining its `PackageGraph`. It does not accept
caller-created graph DTOs that claim prior validation. Both reachable Vesper
packages and reachable declarative native selections are checked; identity
domains remain explicitly tagged. Inactive optional dependencies, inactive native
feature edges and unused selections are outside the audited closure.

Findings carry the selected version and one shortest deterministic dependency
path from each reaching root. Breadth-first traversal terminates on cycles,
resolves diamonds consistently, and handles a shared native dependency with one
shortest root path. Report ordering is stable for the same input. These are
**declared dependency** paths, not compiler call-graph or exploitability evidence.

Versioned catalog schema 1 uses advisory IDs, tagged package identities,
severity, summary and one or more disjoint SemVer intervals. Introduced is
inclusive and fixed is exclusive. `fixed` is required (version or explicit null),
as is each interval's `include_prerelease` boolean. Build metadata is ignored for
range precedence; prerelease ordering follows SemVer numeric/text identifiers.
Unsupported ecosystems/range dialects, missing fields, unknown DTO fields,
unsupported schema versions, duplicate advisory IDs, overlapping/duplicate/empty
intervals and ambiguous native license claims fail. Adjacent disjoint intervals
are supported. This is an explicit local catalog schema, not an OSV decoder.

The documented finite SPDX subset parses AND, OR, WITH and parentheses with
WITH > AND > OR precedence. AND requires every grant; OR needs one complete
granted branch. Every branch is parsed first. A known allowed license OR an
unknown identifier still yields Unknown, rather than silently treating the
unknown as a valid SPDX identifier. WITH requires an explicit base/exception
pair in policy; allowing a base does not authorize arbitrary exceptions.
Missing/freeform/malformed/unsupported license expressions are Unknown.
Native licenses need caller-supplied catalog claims for the exact selected
version including build metadata; no claim is inferred from a package name.

Reports distinguish Clean, Findings and Unknown. Unknown dominates mixed reports;
`has_policy_violations` preserves known denials separately. Vulnerabilities are
reported at or above the explicit policy severity threshold. Domain-separated
BLAKE3 digests bind the complete solution, catalog, policy and limits DTO
contents under `vesper.loom.offline-audit/1`. A reused catalog label with changed
contents has a different context. These are input bindings, not authentication.
Vector order is retained in input commitments; equivalent reordered inputs may
have different context digests while producing identical findings.

Bounds cover input bytes, JSON node/depth/string size, graph selections/roots/
edges, catalog records/ranges, SPDX token/depth, retained graph/report paths,
finding count and logical work. Duplicate JSON keys are rejected recursively,
including keys in maps serde would otherwise overwrite. Programmatic DTOs are
bounded by capped serialization before graph validation. The existing calculus
validator is preceded by a conservative declaration-size work estimate. This is
not instruction or wall-clock metering. License parser budget exhaustion is an
error, distinct from unknown license data. No exhausted budget returns clean.

## Public API and root wiring

```rust
let limits = vesper_loom_audit::Limits::default();
let solution = vesper_loom_audit::parse_solution(solution_bytes, &limits)?;
let catalog = vesper_loom_audit::parse_catalog(catalog_bytes, &limits)?;
let policy = vesper_loom_audit::parse_policy(policy_bytes, &limits)?;
let report = vesper_loom_audit::audit(&solution, &catalog, &policy, &limits)?;
```

The three byte inputs are `&[u8]`. `AuditReport` and all catalog/policy DTOs are
serializable. `Error::InvalidSolution` retains the calculus diagnostics. Root CLI
can serialize the report and use example exit codes: Clean0, Findings1, Unknown2,
invalid input/budget/I/O3. The intended coordinator command is:

```sh
vesper audit Loom.lock --catalog advisories.json --policy policy.json
```

Exact coordinator registration/dependency proposal:

1. Remove the standalone `[workspace]` section and child `Cargo.lock` in
   `crates/vesper-loom-audit`. The child lock was seeded from the baseline root
   lock; integration should retain root validated dependencies.
2. Add `"crates/vesper-loom-audit"` to root workspace `members` and
   `default-members`.
3. Add root dependency
   `vesper-loom-audit = { path = "crates/vesper-loom-audit" }`.
4. Register root CLI arguments/read bounds and call the public API above; no root
   `src/lib.rs` registration is required. Default per-input maximum is1MiB.
5. Refresh the single root lock. Leaf dependencies are calculus(path), semver1
   with serde, serde1 derive, serde_json1 and blake3 1; no new ecosystem or native
   runtime dependency is introduced.

Standalone example and complete sample JSON inputs are in `examples/`.

## Actual checks

Using an isolated target in this worktree with build jobs1, DEV/TEST debug0 and
incremental disabled:

```sh
cargo test --manifest-path crates/vesper-loom-audit/Cargo.toml
cargo clippy --manifest-path crates/vesper-loom-audit/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/vesper-loom-audit/Cargo.toml -- --check
```

15 focused integration tests passed, no ignored tests; strict Clippy and format
passed. Tests cover transitive/native paths, cycles/diamonds/multiple roots,
shared native paths, feature activation/inactive selections, real SemVer
prerelease/build/fixed boundaries through audit, malformed/duplicate catalogs and
JSON keys, SPDX precedence/exception decisions/unknowns, invalid solutions,
byte/node/depth/edge/path/token/work bounds, complete context binding and report
serialization. No external service or network feed was used.

The actual runnable example was invoked with the checked-in solution/policy and
temporary catalog variants. Verified exit1+findings, exit0+clean, exit2+unknown,
and exit3+invalid-schema with no partial report. For example:

```sh
cargo run --manifest-path crates/vesper-loom-audit/Cargo.toml --example audit -- \
  crates/vesper-loom-audit/examples/solution.json \
  crates/vesper-loom-audit/examples/catalog.json \
  crates/vesper-loom-audit/examples/policy.json
```

## Scope remaining

No authenticated/live OSV/CVE feed, feed freshness guarantee, peer audit or
maintainer trust scoring, call-graph exploitation analysis, installed native
binary inspection, source/builder provenance, solver objective, SPDX registry
fetch, source license-file inspection, or CycloneDX/SLSA export is claimed.
The catalog and native license declarations are caller-selected local evidence;
a clean report is relative to those exact inputs, not a universal CVE/license
absence statement. No package is downloaded or executed by this audit.
