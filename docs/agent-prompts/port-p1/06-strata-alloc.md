# Port vesper-alloc (Strata collections) to Vesper against the #847 traces

> Port wave P1, lane 6. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p1/strata-alloc`. Claim issue: #903.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline d523661 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md (the recipe), docs/maintenance/vesper-port-survey.md, crates/vesper-port-pilot (the worked example), and the crate's README, public API and tests.

When to start: after lanes 1, 2 and `@vesper-std` (#897) merge. #897 implements `CacheBTree` and replays the #847 traces in `@vesper-std`; this lane builds on it instead of writing a second one. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #903 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #903").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-p1/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, performance, CI, checks before pushing, merging and the report.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the tests/self_host_front_end.rs skip list) and, for a compiler-linked crate, the one-line feature switch in each non-compiler dependent's Cargo.toml (own commit). Other P1 lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/**, packages/vesper-std/** (P0 lane 8, #897) or the strings module (P0 lane 7, #896). Ask those lanes for what you need.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock; for a new crate, registration in root Cargo.toml `members` and `default-members` plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100. Files a test reads from outside its crate must be declared in the crate's BUILD.bazel. Bazel's rustfmt checks every file under src/ and ignores #[rustfmt::skip]: keep generated bindings in port/. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. "0 Alloc" claims need a counting-allocator test over the warmed-up steady state. Report only performance you measured, with the setup.
- Lean: any Lean file must compile with the pinned Lean 4.24.0 (use the GitHub release .zip), with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #903. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, decisions and remaining gaps.

Assignment: Port vesper-alloc (Strata collections) to Vesper against the #847 traces.

Exclusive ownership:
- crates/vesper-alloc/**
- lean/StrataCollections.lean (additive, if the model needs the port's invariants)
- docs/parallel-results/port-p1-strata-alloc.md
Dependencies: Lanes 1 and 2, #897. #588 (source RegionSlice/SlotArena) is separate compiler work; do not edit src/regions/**. Lane 18 waits for this lane.

Implement:
1. Port `cache_btree.rs` (forward to `@vesper-std/collections::CacheBTree`, adding any API the Rust crate has that std lacks, through #897 or a private wrapper), `tiled.rs`, `region/{arena,flat,tiled,vec,string}.rs` logic, `capability.rs` and `error.rs`.
2. The page and OS layer stays Rust behind a `.vsys` surface owned by this crate: `backing.rs`, `virtual_arena.rs` and `virtual_arena/os.rs` (mmap, VirtualAlloc, 2 MiB hints), `slab.rs` and `slab/sharded.rs` (lock-free remote queue, per-core shards: they need F12). Name each in the report.
3. `unsafe` in ported modules disappears; `unsafe` in the shim stays and is counted in the report.
4. Traits `Alloc`, `KeyOrder<K>`, `HotColumns`, `TiledRecord`: bounded generics by monomorphization where the instances are known; otherwise keep the trait on the Rust side and export the instances the tests and dependents use.
5. Compiler-linked (`src/regions*`, `src/freestanding/checks.rs`): seed-copy rule. vesper-kinetic and vesper-crucible move to the port build.

Stays in Rust: Backing pages, VirtualArena, ShardSlab and the remote queue (F12 and OS); the frozen seed copy.

Acceptance:
- The #847 conformance traces (`tests/collections_conformance.rs`, `tests/fixtures/strata/`) replay byte-for-byte against the port.
- All 223 tests run against both builds; in-crate tests translated or ported.
- ABBA benchmarks for CacheBTree insert/lookup/range and the region vec push path, with a 2x stop rule.
- Counting-allocator steady-state tests still hold or are reported with numbers.
- Report: docs/parallel-results/port-p1-strata-alloc.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
