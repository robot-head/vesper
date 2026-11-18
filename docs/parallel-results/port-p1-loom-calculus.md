# Port wave P1 lane 16: Loom calculus (#913)

Manifest validation, solution validation and the declared-authority upgrade
guard of `vesper-loom-calculus` run in Vesper, from the Loom package
`@vesper/loom-calculus` in `crates/vesper-loom-calculus/vesper/`. The crate
now follows the seed-copy rule (as Beacon does since #919):

- `vesper-loom-calculus` is the port-backed package. Its `build.rs` always
  links the Vesper port and sets `cfg(vesper_port)`.
- `vesper-loom-calculus-seed` (`seed/`) builds the same `src/` as frozen
  Rust, under the same library name. The compiler, and every crate that
  depends on the calculus, link the seed.

All 29 existing tests pass against both builds, and so do 5 new ones. The
port is 1.4x slower than Rust on the 256-package benchmark, 1.7x on 64
packages and 2.3x on 8 packages. **No Rust code was deleted.** Every
dependent stays on the Rust seed, under the owner's parity rule
(2026-11-17) and the seed-copy rule.

Baseline: `main` at 69c79b9.

## API inventory (re-measured before porting)

Public API, unchanged by the port. Both builds share these types in the crate
root:

- `src/lib.rs`: `SCHEMA_VERSION`, `PackageName` (`parse`, `as_str`, a
  validating `Deserialize`), `Package`, `Dependency`, `Feature`,
  `BuildSandbox`, `ResolverPolicy`, `Resolver`, `Verification`, `Manifest`,
  `Code`, `Diagnostic`, `validate_manifest`, `parse_manifest`,
  `RootRequirement`, `Selection`, `SystemSelection`, `Solution`,
  `DependencyEdge`, `SystemEdge`, `PackageNode`, `PackageGraph`
  (`canonical_json`), `Validation`, `validate_solution`.
- `src/upgrade.rs`: `UPGRADE_SCHEMA_VERSION`, `UpgradeSnapshot`,
  `UpgradeDiagnostic`, `AuthorityScope`, `AuthorityChange`, `UpgradeReport`,
  `check_upgrade`.

This lane adds the same API to both builds: `IMPLEMENTATION` (`"vesper"` or
`"rust"`) and `port_live_allocations()`, which is always 0 in the Rust build
and is used for the leak check.

In-crate `#[cfg(test)]` unit tests: none. Integration tests: 17 in
`tests/calculus.rs`, 12 in `tests/upgrade.rs`, and the README doctest.

## What was ported

| Rust | Vesper module | Notes |
| --- | --- | --- |
| `PackageName::parse` (the spelling check) | `check.ves` `name_valid` | Rust still formats the error message. |
| `validate_manifest`, `check_caps`, `capability_valid`, `feature_valid`, `system_name_valid`, sandbox path rules | `check.ves` | Diagnostics are built in a byte pool. |
| `validate_solution`: selection order and uniqueness, native selections, roots, feature fixed point (`expand_features`), active closure, native edges, backwards authority worklist, ceilings, `forbid_unsafe_ffi`, edge budgets, unused selections | `solve.ves` | Sets are rows of flags over dense capability and feature indices. Parents are kept in CSR form. |
| `check_upgrade` and its helpers (`finish`, `validation_diagnostics`, `sets`, `edge_ceilings`) | `upgrade.ves` | Merge walks over both snapshots. |
| `VersionReq::matches` (semver 1.0.28 `eval.rs`) | `semver.ves` | All eight operators and the prerelease rule. |
| `Ord` for `Version`, `Prerelease` and `BuildMetadata`; diagnostic, change and selection orders; a stable merge sort | `util.ves` | |
| Decoding the input words, encoding the results | `decode.ves`, `encode.ves` | Format documented in `src/port.rs`. |

Encoding: `src/port.rs` interns every string into a table of
`(offset, length)` ranges over one text buffer. Vesper sorts the table once,
and each string's rank becomes its id, so comparing two ids compares the
strings. The wrapper writes `BTreeSet`/`BTreeMap` contents in their own
order, which is ascending by id. Results come back as words and a byte pool,
and the wrapper rebuilds the crate's types from them. Versions, requirements
and dependencies are referenced by index, so the wrapper clones the original
`semver` values.

Vesper source: 3,594 lines in 8 modules. Wrapper: 528 lines (`src/port.rs`).

## What stays in Rust, and why

| Part | Reason | Unblocked by |
| --- | --- | --- |
| `parse_manifest`'s TOML decoding (`toml`) and serde `Deserialize`, including `deny_unknown_fields` | The lane prompt keeps decoding in Rust for this wave | #965 (Vesper TOML reader) |
| `PackageGraph::canonical_json` and the selection sort key (`serde_json`) | JSON encoding is decoding-side work | #965 |
| SemVer text: `Version`/`VersionReq` parsing and `Display` (in messages and `SystemEdge::requirement`), plus splitting native requirements at the first whitespace | Text decoding; Vesper gets the parsed comparators and the display strings | #965, or the vesper-semver port (lane 15, #912) |
| The `PackageName::parse` error message | `String` errors are not exportable (lane 1 ABI) | none needed: the check itself is ported |
| The frozen seed copy (`seed/`, all of `src/` without `cfg(vesper_port)`) | Seed-copy rule: the compiler links this crate | Self-hosting Stage 2 |

The crate's Rust bodies stay under `#[cfg(not(vesper_port))]`, renamed to
`rust_validate_manifest`, `rust_validate_solution` and `rust_check_upgrade`.
The public functions forward on `cfg(vesper_port)`. Unsafe code: none in the
crate. The generated binding (`port/binding.rs`) has the harness's usual
`unsafe` FFI calls, under `#[allow(unsafe_code)]`.

## Dependents

Every dependent is inside the compiler's dependency closure, so all of them
use the seed. That switch is its own commit:

- root `Cargo.toml` (`vesper-loom-calculus` → `crates/vesper-loom-calculus/seed`,
  `package = "vesper-loom-calculus-seed"`)
- `vesper-loom-solver`, `vesper-loom-audit`, `vesper-loom-trust`,
  `vesper-loom-admission` and `vesper-codegen-verified`

`cargo tree -p vesper -e normal,build` and
`cargo tree -p vesper-codegen-verified` reach only
`vesper-loom-calculus-seed`. The lane 2 Cargo guard
(`crates/vesper-port-harness/tests/closure_guard.rs`) passes locally. The
Bazel guard (`compiler_closure_has_no_ports`) runs in CI.

## Tests

- Unit tests: the crate had no `#[cfg(test)]` tests, so none needed
  translating.
- `tests/calculus.rs` and `tests/upgrade.rs` run unchanged against both
  packages (`cargo test -p vesper-loom-calculus -p vesper-loom-calculus-seed`).
  One test was added: `authority_reaches_ancestors_that_sort_before_their_dependencies`.
  It is the first test whose package names sort with the parent first, so a
  single pass of the authority worklist fails it.
- `tests/differential.rs` (new) adds deterministic corpora checked against
  the Rust build's output. Each one hashes the `Debug` text of every result
  (and `canonical_json` of every graph) with FNV-1a and compares the hash
  with the one the Rust seed produced:
  - Manifest corpus: 400 random manifests with valid and invalid
    capabilities, feature names, native requirements and sandbox paths,
    plus `PackageName::parse` cases (417 results).
  - SemVer matrix: 19 versions times 17 requirements, and every pair of
    duplicate selections and native selections, which tests the version
    order (684 results).
  - Solution corpus: 400 random solutions, with duplicates, missing roots,
    cycles, optional dependencies, features and native edges. It also has
    400 mostly valid layered graphs with upgrade candidates, of which about
    half have authority reduced or `forbid_unsafe_ffi` flipped, and 400
    self-upgrades (2,000 results). More than half of the graphs validate,
    and the upgrade guard both allows and rejects.
  - Leak check: `port_live_allocations()` matches before and after 20
    rounds of every entry point.
- All digests match: the Vesper build prints exactly what the Rust build
  prints on all 3,101 results.

## Divergence checks

The `vesper-port-divergent` feature builds the port from a copy of
`vesper/` with one deliberate bug per ported module (`DIVERGENT` in
`build.rs`). Bazel builds each edit as its own object
(`:loom_calculus_vesper_divergent_<module>`).
`scripts/test-port-harness.sh` asserts that each bug is caught by its own
test, and that an unchanged test still passes
(`documented_manifest_is_declarative_and_secure_by_default`):

| Module | Edit | Caught by |
| --- | --- | --- |
| `check.ves` | `:` no longer rejects a build input path | `manifest_corpus_matches_the_rust_build` |
| `semver.ves` | `^1.2` stops matching `1.2.x` | `semver_matrix_matches_the_rust_build` (and `additive_features_activate_optional_dependencies_and_native_edges`) |
| `solve.ves` | authority makes a single backwards pass | `authority_reaches_ancestors_that_sort_before_their_dependencies` |
| `upgrade.ves` | relaxing `forbid_unsafe_ffi` is allowed | `unsafe_ffi_policy_relaxation_is_rejected_for_existing_and_new_packages` |

I checked each edit on its own first: each designated test fails only
under its own edit.

## Performance

`examples/abba.rs` builds a layered graph of N packages. Package i depends on
i+1, i+3 and i+7, where the last edge is optional and a feature turns it on.
Each package has two features and a native requirement, and its ceilings
cover its whole closure, so the graph validates. One pass runs
`validate_solution` on the graph and `check_upgrade` against the same graph
with versions bumped. Both arms are release builds, the Rust seed
(`-p vesper-loom-calculus-seed --example abba_rust`) against the port (`-p vesper-loom-calculus`),
run as separate processes in A B B A order. There are 9 epochs of
2 × 20 passes per arm, analysed with `vesper_bench::analyze_pairs`. The host
was the 4-vCPU cloud container (Intel Xeon at 2.10 GHz), and the Vesper
objects were compiled by a debug `vesper` at 69c79b9 plus this branch.

| Packages per graph | Rust | Vesper | Median B/A | 2x gate |
| --- | --- | --- | --- | --- |
| 8 | 157 us/pass | 350 us/pass | 2.35x | Regression (+134%) |
| 64 | 2.10 ms/pass | 3.63 ms/pass | 1.69x | Pass (+69%) |
| 256 | 9.61 ms/pass | 13.8 ms/pass | 1.45x | Pass (+45%) |

(Times are from epoch 0. The ratios are medians over the 9 epochs.)

The ratio falls as graphs grow. That means the per-call overhead dominates
on small inputs: copying every string into one buffer, Vesper sorting the
string table, decoding the words, and rebuilding the Rust types. The
calculus itself is close to Rust. The first version of the port kept sets
as rows over all string ids, which made it quadratic, at 2.5x/2.7x/4.9x.
Dense capability and feature indices fixed that.

Rust deletion: not attempted. This is a compiler-linked crate, so the Rust
code must stay as the seed until Stage 2. The 8-package case is also over
the 2x stop rule, and the owner's parity rule keeps Rust the default for
every dependent in any case.

## Platforms run

- Linux x86_64 (cloud): `cargo test -p vesper-loom-calculus -p vesper-loom-calculus-seed`,
  the divergent build, the closure guard,
  `cargo +1.100.0 clippy --all-targets -- -D warnings` on both packages
  (and on the divergent build), and `vesper fmt --check` on `vesper/`.
- Bazel (BuildBuddy), macOS (AArch64) and Windows (x86_64 MSVC): in the
  PR's CI.

## Remaining gaps

- #965: TOML, JSON and SemVer text in Vesper, so decoding can move too.
- Fixed per-call overhead: the wrapper interns and copies every string, and
  Vesper copies the input into its own buffers. A borrowed input view
  (export ABI v3's in-place parameters) and caching the sorted string table
  across the two snapshots of `check_upgrade` would cut the small-graph
  ratio.
