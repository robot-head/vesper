# Lent callbacks keep their parameters in registers

> Self-hosting wave SH4, lane 4. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh4/callback-registers`. Claim issue: #1278.

Callbacks through `with_ref` and `with_mut` cost 1.3% to 4.1% more Ir than hand-written element-view lookups (#1271). When `with_ref` is inlined, `in_param_registers` keeps every parameter of a function that calls, and every function-typed parameter, in stack slots, so each call stores and reloads the closure tag, the key and the captured names. Compiler code is full of such callbacks.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh4/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and SH3's additions, which all apply), docs/parallel-results/self-host-cranelift-shim.md, docs/parallel-results/self-host-census.md (the lowering sample), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1271, docs/parallel-results/self-host-closure-modes.md (the `flat` driver), docs/parallel-results/p3b-gaps-aggregate-registers.md and #983, src/native.rs (`register_locals`, `in_param_registers`, the closure call path).

When to start: now, from main. If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1278 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1278", or "Part of #1278" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1278. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: callback parameters in registers.

Exclusive ownership:
- the `register_locals` and `in_param_registers` hunks of src/native.rs, and the closure-call hunks they need
- tests/callback_registers*.rs
- report docs/parallel-results/self-host-callback-registers.md

Implement:
1. Reproduce with `vesper emit --emit clif` on `flat_callbacks` (slots ss4 and ss5).
2. Keep parameters in registers across calls when no address escapes, including function-typed parameters whose closure is known after inlining.
3. Re-measure FlatTable, CacheBTree and IdxMap callbacks against the hand-written views, and the SH2 checker ports that use callbacks.

Acceptance:
- Callbacks cost no more Ir than the hand-written view match (within 0.5%). Closes #1271.
- No port gets slower (port harness numbers in the report).
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-callback-registers.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
