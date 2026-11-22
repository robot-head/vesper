# Codegen swap point, lent Cranelift handles and the lowering differential

> Self-hosting wave SH4, lane 1. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh4/codegen-swap`. Claim issue: #1275. Stacks on: #1259.

SH0 lane 4's shim (#1135) owns its own module and builder, wraps only `ObjectModule`, and lacks the builder extras the compiler uses. The Rust codegen cannot hand its live `FunctionBuilder` to Vesper, and nothing compares Vesper emission with Rust's over the corpus. This lane builds the pieces every SH4 port swaps through, so native lowering moves to Vesper one family at a time instead of all 38k lines at once.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1259) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/1259/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh4/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and SH3's additions, which all apply), docs/parallel-results/self-host-cranelift-shim.md, docs/parallel-results/self-host-census.md (the lowering sample), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: crates/vesper-cranelift-sys/**, packages/vesper-cranelift/**, docs/parallel-results/self-host-cranelift-shim.md ("API inventory" and "What is left"), the census lowering sample (/mnt/project-files/census/lower/ and census/harness/census_lower.rs when your session can see them; otherwise docs/parallel-results/self-host-census.md), src/native.rs (`Codegen`, `define_instances*`, `compile_entry`), #1259 (src/ir_swap.rs, packages/vesper-place-ir, tests/common/ir_words.rs), crates/vesper-hybrid/**.

When to start: now, stacked on #1259. If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1275 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1275", or "Part of #1275" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1275. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: the codegen swap point, lent Cranelift handles, `@vesper/codegen-base` and the lowering differential harness.

Exclusive ownership:
- crates/vesper-cranelift-sys/** and packages/vesper-cranelift/** (the shim and its wrappers)
- new packages/vesper-codegen-base/** (shared lowering state: value and variable maps, types, layout queries, the pilot family)
- new src/codegen_swap.rs and its call-site hunks: one hook call at each family's dispatch site in src/native.rs and the family files it hooks (the dispatch lines only), native path only
- new tests/common/clif_diff.rs and tests/self_host_codegen_swap*.rs
- the hybrid module `codegen` and its hook installs
- report docs/parallel-results/self-host-codegen-swap.md

Implement:
1. Shim: lent (borrowed) builder and module handles, so Rust code can lend its live `FunctionBuilder`, its `Variable`s and `Value`s to a Vesper hook for one family's emission and take them back; `JITModule` as a second backend of the module handle; the builder extras from the shim report's "What is left" (`set_srcloc`, `set_cold_block`, `current_block`, `UserFuncName::user`, colocated `ExtFuncData`, `declarations()`, `cranelift_frontend::Switch`, `ControlPlane`, dynamic stack slots, Windows unwind info).
2. `src/codegen_swap.rs`: one optional hook per SH4 family (`scalar`, `call`, `aggregate`, `drops`, `seq`, `control`, `numeric`, `export`, `ffi`, `freestanding`) called where Rust dispatches that family, and whole-backend hooks (`wasm`, `ebpf`, `verifiers`). Hooks receive lent handles plus lane 2's shared inputs. The default compiler installs nothing, and its objects are byte-identical to `main`'s on the corpus.
3. Harness: for each corpus world and each installed hook, per-function CLIF text (`ctx.func.display`) equal to Rust's, object bytes equal, and JIT run outputs equal with optimize on and off. Name the families and counts that differ.
4. Pilot: the census lowering family (`alloc`, `data_into`, `offset`, `load`, `store`, `copy_bytes`, `pattern_condition`) as the first swap through the `aggregate` hook, so lane 8 starts from a working swap. Measure the cost of one hook crossing and of emission per instruction against Rust (callgrind Ir).

Acceptance:
- The default compiler's objects are byte-identical to `main`'s over the corpus; the pilot swap has 0 CLIF differences and byte-identical objects.
- Every hook named above exists and is documented, with its lent inputs, so lanes 6-17 add only Vesper code and a hybrid install.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-codegen-swap.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
