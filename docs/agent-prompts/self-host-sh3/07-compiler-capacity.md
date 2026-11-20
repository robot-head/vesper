# Compiler capacity: no native instance cap, combined checker compile time, entries with more than three words

> Self-hosting wave SH3, lane 7. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/compiler-capacity`. Claim issue: #1246. Stacks on: #1231.

Checker ports already hit the native monomorphization cap: `@vesper/check-tensor-types` came to 1,054 instances with `@vesper/ast` and had to be hand-inlined under 1,024; `@vesper/check-dyn` needed 1,031, so #1231 raised the cap to a named `MAX_NATIVE_INSTANCES = 4096`. The self-hosted checker links all of them into one program. Word entries also take at most three words, so ports pack options into a `flags` word.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1231) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/<n>/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1231 and docs/parallel-results/self-host-check-dyn.md (gaps 1, 4), docs/parallel-results/self-host-check-tensor-types.md on #1230 (gap 1), docs/parallel-results/self-host-large-functions.md (SH1 lane 8), src/native.rs (monomorphization, `WordFunction`), src/native_export.rs, src/test_runner.rs (test entries).

When to start: now, stacked on #1231.

Claim: GitHub issue #1246 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1246", or "Part of #1246" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md and the additions in docs/agent-prompts/self-host-sh3/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**; never route the verified backend's pipeline through a swap point; and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths. Depend on unmerged work only through the PR named in "Stacks on".
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front (and packages/vesper-ast where it lowers the construct) in the same PR, or tests/self_host_front_v2.rs and tests/self_host_ast.rs fail.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1246. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Remove the instance cap, measure and cut the compile time of the combined checker packages, and widen word entries.

Exclusive ownership:
- `MAX_NATIVE_INSTANCES` and its users in src/native.rs and src/native_export.rs
- the `WordFunction` entry hunks of src/native.rs and the test-entry hunks of src/test_runner.rs
- tests/compiler_capacity*.rs, and a generated large-package fixture under tests/fixtures/compiler_capacity/

Implement:
1. Repro first: a package whose entry reaches 5,000 instances (generated), and a word entry with six parameters.
2. Replace the fixed cap with a limit that scales with the program (or none, with a guard against runaway polymorphic recursion that reports the cycle); the checker's `pact_frontend::mono::MAX_INSTANCES` stays as it is unless the same reasoning applies (say which in the report).
3. Compile all SH2 checker packages that exist on main (and the open ones, from their branches, measured locally) into one JIT program and one `vesper build --lib` object; record instance count, compile wall time and peak memory; fix the first superlinear hot spot you find.
4. Word entries with up to eight words (JIT, export and test entries), so ports stop packing flags.
5. Report a table: package set, instances, compile time before and after.

Acceptance:
- Repros pass; compile time of the combined set is reported and no worse than the sum of its parts.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-compiler-capacity.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
