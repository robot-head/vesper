# Swap inputs without re-encoding

> Self-hosting wave SH4, lane 2. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh4/swap-inputs`. Claim issue: #1276. Stacks on: #1259.

Every swap so far encodes the checked `World` and the IR into words and decodes them on the Vesper side. #1259 measured encoding at 0.80x of `ir::lower_world` itself, and decoding is 27-42% of some SH2 checker ports' time (SH2 scoreboard). SH4's hooks are per family and per function, so encoding the world per call would sink every SH4 swap. This lane makes each input cross once per compilation.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1259) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/1259/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh4/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and SH3's additions, which all apply), docs/parallel-results/self-host-cranelift-shim.md, docs/parallel-results/self-host-census.md (the lowering sample), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1259 (tests/common/ir_words.rs, @vesper/place-ir), #1232 (tests/common/check_words.rs), packages/vesper-check-data/**, docs/parallel-results/self-host-tree-abi.md (ABI v4: owned trees, `Seq` views, lent Rust handles), docs/parallel-results/self-host-sh2-scoreboard.md (decode costs), src/native_export/binding_v4.rs, crates/vesper-hybrid/**.

When to start: now, stacked on #1259. If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1276 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1276", or "Part of #1276" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md and the additions in docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh4/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**; never route the verified backend's pipeline through a swap point; and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths. Depend on unmerged work only through the PR named in "Stacks on" or the lane named in "When to start".
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave. Port lowering emits through @vesper/cranelift, never by reimplementing Cranelift.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front (and packages/vesper-ast where it lowers the construct) in the same PR, or tests/self_host_front_v2.rs and tests/self_host_ast.rs fail.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo. Serialize merges that touch root Cargo.toml (no merge queue on this account).
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1276. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: encode-once swap inputs.

Exclusive ownership:
- new src/swap_input.rs (the per-compilation input cache) and its hunk in src/ir_swap.rs
- the encode-once hunks of the encoders #1232 and #1259 add (merge main between this lane and theirs once they merge)
- the input plumbing of crates/vesper-hybrid/** (not other lanes' modules)
- tests/self_host_swap_inputs*.rs
- report docs/parallel-results/self-host-swap-inputs.md

Implement:
1. Measure encode and decode per swap in the hybrid build: every merged SH2 and SH3 swap, and lane 1's pilot.
2. One shared encoding of the `World` (check-data sections) and of the `Program` per compilation, built once, lent `in` to every installed hook, and updated in place when a pass rewrites a function (not re-encoded whole).
3. Lazy sections: encode only what a hook reads. Where reading a lent Rust table through an accessor import (ABI v4 lent handles) beats copying it, use that instead. Decide by measurement and report both.
4. Re-measure the SH2 and SH3 swaps with the cache; give each swap's input cost before and after.

Acceptance:
- Every swap gets identical inputs (the existing differentials pass unchanged).
- Input cost per swap is reported before and after; target: under 10% of the Rust pass it replaces.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-swap-inputs.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
