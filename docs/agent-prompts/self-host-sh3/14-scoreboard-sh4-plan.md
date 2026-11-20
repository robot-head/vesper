# SH2 and SH3 swap scoreboard and SH4 plan

> Self-hosting wave SH3, lane 14. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/scoreboard-sh4-plan`. Claim issue: #1253.

SH2's scoreboard (docs/parallel-results/self-host-sh2-scoreboard.md) was written from the lanes' reports while their swaps were still open, at the owner's request. This lane re-measures every merged SH2 and SH3 swap in one hybrid build and plans SH4 (native lowering through the Cranelift shim, export ABI, FFI lowering, freestanding, wasm, eBPF, backend verifiers).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: every SH2 and SH3 report, crates/vesper-hybrid/**, src/native*.rs, src/ffi_lowering/**, src/wasm_backend.rs, src/ebpf_backend.rs, src/native_verifiers.rs (read only), docs/parallel-results/self-host-cranelift-shim.md, the census's lowering sample in /mnt/project-files/census/lower/ when your session can see it.

When to start: last: when the SH2 swaps and lanes 9-13 have each merged a first swap. If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1253 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1253", or "Part of #1253" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1253. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: SH2 and SH3 scoreboard and SH4 plan.

Exclusive ownership:
- docs/maintenance/self-hosting-plan.md (SH3 results, SH4 sections, gap table)
- new docs/agent-prompts/self-host-sh4/**
- docs/parallel-results/self-host-sh3-scoreboard.md

Implement:
1. Build the hybrid compiler with every merged swap; run the full suite and the corpus differential; record compile time against the Rust compiler (ABBA, callgrind) per module, for the whole checker and for the IR pipeline.
2. Translate one SH4 sample locally (a native lowering family beyond the census's, through the Cranelift shim) to find SH4's gaps before planning it. Never edit the Stage 1 guarded files.
3. Write the SH4 plan (README with lanes, claims, exclusive boundaries and merge order, one prompt per lane), open the claim issues, update self-hosting-plan.md.

Acceptance:
- Scoreboard with function and speed per module; SH4 plan PR that claims its lanes.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-sh3-scoreboard.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
