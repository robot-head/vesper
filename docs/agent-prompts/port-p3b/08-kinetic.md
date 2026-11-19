# Port vesper-kinetic

> Port wave P3b, lane 8. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/kinetic`. Claim issue: #1070.

`vesper-kinetic` is deterministic physics: correctly rounded and fast math, GJK, sweep and prune, Morton codes and an LBVH, graph coloring, rigid bodies with joints and CCD, soft bodies, articulations, coupling, differentiable steps, rollback rings and snapshots. 15.9k lines and 170 tests, including golden traces. Bit-identical results against the Rust build are the acceptance bar. The rigid and soft solvers fan out with `std::thread::scope`, which maps to `parallel_for` (#1037, PR #1062).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P3b of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan and its rule changes), "Rules for every lane" in docs/agent-prompts/port-p1/README.md, docs/maintenance/porting-a-crate.md (the recipe, above all "Patterns for Rust APIs" and "Crates the compiler links"), docs/maintenance/vesper-port-survey.md, crates/vesper-beacon (the seed-pattern example), docs/parallel-results/port-p3-dyn-trait.md and docs/parallel-results/boxed-dyn-trait.md (what `dyn` supports today), the latest scoreboard docs/parallel-results/port-parity.md, and the crate's README, public API and tests.

When to start: now for the single-threaded port; the threaded paths after PR #1062 merges. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1070 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1070").

Delivery and coordination:
- Follow the P1 lane rules as changed by docs/agent-prompts/port-p3b/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, CI, checks before pushing, merging and the report.
- Standing owner rule: Rust stays the default build and is never deleted until the Vesper build matches it in both functionality and performance. Add the `vesper-port` build next to the Rust one, prove both pass, measure, and stop. Do not switch any dependent to the port and do not delete Rust code.
- Compiler-linked crate: use the seed pattern (crates/vesper-kinetic/seed as `vesper-kinetic-seed`; the root and every crate in `cargo tree -p vesper -e normal,build` that depends on it switch to the seed by rename), in its own commit. crates/vesper-beacon is the worked example.
- The lane may land as several PRs (part 1, part 2, ...), each green on its own and each closing nothing until the last one closes #1070.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the self-hosted corpus skip predicate) and, for seed lanes, the seed dependency switch. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/**, src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/** or packages/vesper-std/**. Put helpers you need in your own package and list them in the report as std candidates. A compiler gap is reported with a minimal repro, not fixed in this lane.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries, package registration in root Cargo.toml `members`, the regenerated Cargo.lock, BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Root patches merge one at a time: merge origin/main right before merging and regenerate the lockfile with cargo.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. Report only performance you measured, with the setup; give callgrind instruction counts next to wall clock.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1070. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, ABBA numbers, compiler gaps with repros, decisions and remaining gaps.

Assignment: Port vesper-kinetic.

Exclusive ownership:
- crates/vesper-kinetic/** (not crates/vesper-kinetic/gpu/**)
- docs/parallel-results/port-p3b-kinetic.md

Dependencies: PR #1062 (`parallel_for`) for the threaded paths. `Box[dyn PairBackend]` works on main today; owning it in the world's loops needs lane 2 (#1064) or the borrow-only workaround. The `dyn FnMut` contact pass needs lane 1 (#1063) or a generic parameter.

Implement:
1. Port in dependency order: `crmath.rs`, `fastmath.rs`, `morton.rs`, `gjk.rs`, `sap.rs`, `lbvh.rs`, `coloring.rs`, `tile.rs`, `rigid.rs` with `rigid/**`, `soft.rs`, `articulation.rs`, `couple.rs`, `diff.rs`, `rigid_diff.rs`, `rollback.rs`, `snapshot.rs` and `world.rs`. Split into PRs where that keeps each reviewable.
2. Bit determinism: every golden trace (`tests/golden.rs`, `tests/data/**`) must match byte for byte; any `f32`/`f64` divergence is a blocker reported with a repro.
3. The `std::thread::scope` fan-out in `rigid.rs` and `soft.rs` becomes `parallel_for` once PR #1062 merges, with results still bit-identical at 1, 2 and 4 workers.
4. `rollback.rs` atomics use the source atomics from #1037 (PR #1052).
5. Local `&dyn Fn` closures in `gjk.rs` and `rigid.rs` become generic parameters.
6. Seed pattern: `vesper-kinetic-seed` for the root, `vesper-gpu-compute` and `vesper-kinetic-gpu`.
7. ABBA: the `bench_batches.rs` workloads and a golden rigid-body scene, single- and multi-worker.

Stays in Rust: The Cranelift IR scanner in `detfloat.rs` (it inspects generated code), `simd.rs` intrinsics (or port them to source `Vector` shapes and measure), `vesper-kinetic-gpu`, and the frozen seed copy the compiler links.

Acceptance:
- Both builds pass every `tests/*.rs` file and the translated in-crate tests on the four required checks, golden traces byte for byte.
- Divergence check per ported module.
- Report: docs/parallel-results/port-p3b-kinetic.md with the API inventory, what stays Rust, unit-test treatment, divergence checks, determinism evidence, ABBA numbers and compiler gaps.
~~~
