# Port P1 lane 17: vesper-loom-solver in Vesper

Claim: #914. Base: `main` at 153f264, stacked on lane 16 (PR #978, branch
`claude/project-thread-yopgky` at 90cfe8b), because the port imports
`@vesper/loom-calculus`.

## Summary

- `@vesper/loom-solver` (`crates/vesper-loom-solver/vesper/`, 9 modules, about
  3,000 lines) runs all of the crate's logic:
  - input normalization and validation;
  - the bounded newest-first depth-first search with its feature fixed point;
  - locked-snapshot validation;
  - the lexicographic optimizer with its trust policies;
  - `compute_metrics`.
- It imports `@vesper/loom-calculus` for decoding, `validate_manifest`, SemVer
  matching and ordering, and `validate_solution`. The calculus runs on the
  calculus state that the solver holds.
- Beacon's seed pattern (#919) is used:
  - `vesper-loom-solver` is the port-backed package and depends on the
    port-backed `vesper-loom-calculus`.
  - `vesper-loom-solver-seed` (`seed/`) is the frozen Rust build. The root
    `vesper` package depends on it by rename.
  - Rust stays the default for the only dependent (the compiler), and no Rust
    code is deleted, by the owner's parity rule.
- All 36 lane tests pass on both builds. A new differential corpus of 1,083
  results gives byte-identical `Debug` output on both builds.

## API inventory

| Item | Port-backed build |
| --- | --- |
| `Catalog`, `ResolveRequest`, `ErrorKind`, `ResolveDiagnostic`, `ResolveError` (+ `Display`, `Error`) | Shared Rust types (unchanged) |
| `resolve` | Forwards to `@vesper/loom-solver::resolve` |
| `validate_locked` | Forwards to `validate_locked` |
| `optimize::resolve_optimized` | Forwards to `resolve_optimized` |
| `optimize::compute_metrics` | Forwards to `compute_metrics` |
| `optimize::OptimizeObjective::parse` | Forwards to `objective_code` (the error text is built in Rust) |
| `optimize::{OPTIMIZE_SCHEMA_VERSION, OptimizeObjective, CandidateMetadata, OptimizeRequest, SolutionMetrics, OptimizedResolution}` | Shared Rust types |
| New: `IMPLEMENTATION`, `port_live_allocations` | `"vesper"`/`"rust"`; leak counter |

The crate has no `#[cfg(test)]` unit tests, so there is nothing to translate.

## What was ported (module by module)

| Module | Contents |
| --- | --- |
| `input` | Request decoding. `inputs`: schema check, duplicate and conflicting-root candidates, `validate_manifest` of the root and every candidate, native name and duplicate checks, and candidate lists sorted newest first. |
| `closure` | `closure`: the additive feature worklist through cycles, then version and native requirements derived once from the fixed point, in owner-name order (so the messages join requirements in the Rust order). |
| `search` | The shared search loop of `resolve` and `resolve_optimized`: the budget, blocked policies, trust policies over reachable targets, version conflicts, branching (reverse push for newest-first LIFO), native selection, scoring and the best solution. |
| `snap` | `solution(...)` handed to the calculus `solve` on the current assignment, and the calculus state put back afterwards. |
| `locked` | `validate_locked`, with the Rust order of early returns (input, root policy, selected policies, mismatches). |
| `metrics` | `compute_metrics` (the graph path and the fallback path) and `score_key`. |
| `diag` | Diagnostic text, the diagnostic list and its periodic deduplication, and the merge sort. |
| `output` | Result words and objective names. |

Design notes:

- Assignments are chains of search nodes, so pushing a branch is O(1) where
  the Rust build clones a `BTreeMap`. Nodes are cut back on pop, because a
  node above the popped one is dead.
- Closure state is keyed by string id with generation stamps, so nothing
  k-sized is cleared per branch.
- Diagnostics are deduplicated in Vesper whenever the list doubles, so a long
  search does not grow without bound. Rust's `error()` sorts the final list.

## What stays in Rust and why

| File | What | Why / unblocks |
| --- | --- | --- |
| `src/port.rs` | Encoding the DTOs (the calculus word stream plus the solver fields) and rebuilding `Solution`/`ResolveError`/`OptimizedResolution` | Owned-tree export ABI (no record/string-tree ABI yet) |
| `src/port.rs` | SemVer text parsing and printing, the `=version` root requirement, splitting native requirements | Same rule as lane 16; Vesper SemVer text (#965, lane 15 #974) |
| `src/port.rs` | `serde_json` text of whole manifests, compared by string id in Vesper (`manifest != root`, `locked_manifest_mismatch`) | Structural equality over the full DTO needs the same tree ABI. The wrapper writes JSON only for catalog manifests with the root's identity in `resolve`/`resolve_optimized` (the only ones `inputs` compares), and for all of them in `validate_locked`. |
| `src/lib.rs`, `src/optimize.rs` | `ResolveError` display, `OptimizeObjective::parse`'s error string | Trivial formatting of Rust types |
| `seed/` | The whole frozen Rust build | Seed-copy rule: the compiler links it until Stage 2 |

The port has no unsafe code. The Rust shim's only unsafe code is in the
generated binding (`port/binding.rs`).

## Tests

- `cargo test -p vesper-loom-solver -p vesper-loom-solver-seed` runs all 36
  lane tests (resolve 19, locked 11, optimize 6) on both builds.
- `tests/differential.rs` (new) has two corpora:
  - **Resolve:** 300 random requests, 20% of them "wild" with invalid inputs.
    Each one runs `resolve`, then `validate_locked` on the result and on six
    edited snapshots. That makes 669 results, with every `ErrorKind`
    reached.
  - **Optimize:** 300 random requests with random objectives, root policies,
    metadata and `max_solutions` (0 included), plus `compute_metrics` on the
    result and on a broken snapshot (the fallback path). That makes 414
    results.
  - FNV-1a digests of the `Debug` text match constants recorded from the Rust
    build. Set `LOOM_SOLVER_DIGEST=1` to print them.
  - A leak check confirms the port's live allocations are unchanged after 20
    rounds.
- Divergence checks: there is one edit per module, in `build.rs` DIVERGENT
  (Cargo feature `vesper-port-divergent`) and in BUILD.bazel. Each edit was
  first run alone to find the tests that catch it. `port/test-port.sh`
  (one line in `scripts/test-port-harness.sh`) asserts that each guard test
  fails with all eight edits applied, and that
  `backtracks_latest_parent_after_transitive_conflict` still passes:

| Module | Edit | Guard test |
| --- | --- | --- |
| input | schema 2 accepted | `schema_unknown_fields_and_root_collisions_are_errors` |
| closure | late features never revisit | `late_feature_requests_revisit_nodes_through_cycles` |
| search | budget off by one | `valid_older_selection_is_preserved_without_a_search_budget` |
| snap | partial validation rejects missing natives | `native_constraints_unify_and_pick_latest_satisfying_version` |
| locked | metadata never compared | `manifest_metadata_and_build_metadata_must_match` |
| metrics | CVE counts dropped | `distinguishes_missing_candidate_metadata_from_zero_cost` |
| diag | messages print the prerelease | `resolve_corpus_matches_the_rust_build` (differential) |
| output | errors report zero decisions | `distinguishes_exhaustive_failure_from_search_budget` |

## Benchmark (ABBA)

`examples/solver_abba.rs` builds a layered catalog: n packages with 3 versions
each, three dependency edges per package (one of them optional, enabled by a
feature that also needs a native package), and backtracking at every eighth
package, whose newest version needs an absent `^2`. A `resolve` pass runs
`resolve` and then `validate_locked`. An `optimize` pass runs
`resolve_optimized` over at most 16 solutions.

Setup: release builds, Intel Xeon @ 2.10GHz, 4 vCPUs on a shared cloud VM,
9 epochs of A B B A, 10 passes per window, `vesper-bench` paired analysis.
The port was compiled with the release `vesper` from this branch.

| Workload | Rust (median per pass) | Vesper | Median port/Rust | 2x gate |
| --- | --- | --- | --- | --- |
| resolve, 8 packages | 654 us | 797 us | 1.27x | Pass |
| resolve, 64 packages | 35.1 ms | 16.6 ms | 0.49x | Pass |
| resolve, 256 packages | 1.01 s | 211 ms | 0.21x | Pass |
| optimize, 8 packages | 3.43 ms | 1.35 ms | 0.40x | Pass |
| optimize, 64 packages | 79.9 ms | 23.6 ms | 0.31x | Pass |

Above 8 packages the port is faster. That comes from its representation,
not from compiler parity:

- The Rust build clones a `BTreeMap` of manifests per branch and clones every
  selected `Manifest` into a `Solution` before each `validate_solution`.
- The port shares manifests by index and builds the calculus input in place.

At 8 packages the per-call cost dominates: encoding, the k-sized tables, and
sorting the string table. The largest graph in the lane tests has fewer than
10 packages, which is the 8-package row. Even so, the Rust code stays, by
the owner's parity rule and because the compiler needs the seed until
Stage 2.

## Other edits

- `crates/vesper-loom-calculus/BUILD.bazel` (lane 16's file): one
  `visibility` line on `:port_package`, so that this package's
  `vesper_package` can depend on it.
- `tests/fixtures/self_host/driver.rs`: the self-host skip line (`use`, #880).
- Root patch (its own commit): `crates/vesper-loom-solver/seed` in `members`
  and `default-members`, the root dependency renamed to the seed, and
  `Cargo.lock`.

## Checks run

- `cargo test` on both packages: all pass.
- `port/test-port.sh`: the divergent build is detected.
- `cargo +1.100.0 clippy --all-targets -D warnings` on both packages and on
  the divergent build: clean.
- `vesper fmt --check`: clean.
- `sync_build_files.py --check`: no changes.
- `cargo test -p vesper-port-harness` (closure guard): passes.
- `cargo check -p vesper`: passes.

Platforms run: Linux x86_64 locally. Bazel, macOS and Windows run in CI.

## Rust code deleted?

No. Rust stays the default and the compiler's seed copy.
