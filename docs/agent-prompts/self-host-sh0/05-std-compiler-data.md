# std graph, interning, arenas and hashing

> Self-hosting wave SH0, lane 5. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/std-compiler-data`. Claim issue: #1127.

Compiler data is graphs (the Place Capability Graph and package graph use petgraph: SCCs, dominators, topological order), interned names, index arenas and content digests (BLAKE3 everywhere in Loom, fmt and the syntax cache). Vesper has `ChunkSeq`, `CacheBTree` and `FlatTable` but none of these.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1127 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1127", or "Part of #1127" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1127. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: std graph, interning, arenas and hashing.

Exclusive ownership:
- new packages/vesper-std/graph/**, packages/vesper-std/intern/**, packages/vesper-std/arena/**, packages/vesper-std/hash/**
- tests/std_graph*.rs, tests/std_intern*.rs, tests/std_arena*.rs, tests/std_hash*.rs
- the std catalog hunk of docs/manuscript/content.typ for these modules (after lane 3's hunk; merge main between)
- docs/parallel-results/self-host-std-data.md

Implement:
1. `@vesper-std/arena`: typed indices `Idx[T]` over a `ChunkSeq[T]` arena with checked access, so ASTs and IR can be index graphs instead of `Box` trees where that is faster. Measure both shapes on a 100k-node expression tree.
2. `@vesper-std/graph`: a compact index graph with deterministic iteration, Tarjan SCC, topological order with cycle report, Cooper-Harvey-Kennedy dominators, reachability. Differential tests against petgraph on random graphs.
3. `@vesper-std/intern`: `Str` to stable `Symbol` ids with O(1) lookup both ways, deterministic ids for a given insertion order.
4. `@vesper-std/hash`: BLAKE3 (one-shot and incremental) through the vetted Rust implementation (`blake3` crate) behind `.vsys`; FNV/xxhash-style fast hashes for tables if FlatTable needs them.
5. ABBA and callgrind against the Rust crates for each.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Tests pass on all four platforms; the evaluator, JIT and executable agree; BLAKE3 matches the official test vectors.
- Report: APIs, numbers against petgraph/blake3/HashMap interning, and gaps.
- Report: docs/parallel-results/self-host-std-data.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
