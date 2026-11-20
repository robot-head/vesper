# Element borrows and collections that own sequences

> Self-hosting wave SH2, lane 1. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh2/element-borrows`. Claim issue: #1173.

The census IR check keeps one dataflow state per block, and each state owns sequences. Vesper cannot read such a struct out of a `ChunkSeq` (`E1522 cannot copy 'State' out of a sequence because it is not copy; use pop`), and `@vesper-std/collections` (`CacheBTree`, `OrderedSet`, `FlatTable`) and `@vesper-std/arena` (`IdxMap`) take only copy keys and values. The sample worked around both with a flat state pool of about 340 lines (census gaps C5 and C6). Every checker lane has tables of this shape.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh2/README.md (the wave plan and the rules for every lane), docs/parallel-results/self-host-census.md (the census this wave comes from: its samples, numbers and gaps C1-C12), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/p3b-gaps-export-abi-widening.md (it found the same E1522 gap), src/seq/check.rs, src/seq/ir.rs, packages/vesper-std/collections/** and packages/vesper-std/arena/**.

The census samples are not in the repository. A copy is in the project files at /mnt/project-files/census/ (sample/src/*.ves, sample/lower/lower.ves, harness/*.rs) when your session can see that folder; if not, the census report describes their design (hash-consed type arena, interned names, flat tables, sorted rows, a state pool) and you rebuild it.

When to start: now. This lane changes the checker and lowering of sequence element access, so merge origin/main right before merging and re-run the sequence and drop differential tests. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1173 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1173", or "Part of #1173" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front in the same PR, or tests/self_host_front_v2.rs fails.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1173. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Element borrows and collections that own sequences.

Exclusive ownership:
- the element-access hunks of src/seq/check.rs, src/seq/ir.rs and the native/JIT lowering they need (listed in the PR; never `src/ir.rs` lower_world or the Place IR types)
- packages/vesper-std/collections/** and packages/vesper-std/arena/** (non-copy values only; keep every existing API)
- manuscript hunks for element views (flagged)
- new tests/element_borrows*.rs, tests/owning_collections*.rs
- docs/parallel-results/self-host-element-borrows.md

Implement:
1. Commit the census repro as a failing test first (`struct State { slots: ChunkSeq[u32] }` read through `states.try_get(0)`).
2. Borrowed element views: `in` and `inout` access to an element of a `ChunkSeq[T]` with non-copy `T` in place (for example a `with_ref`/`with_mut`-style form or a borrowed `get`), checked with the existing two-phase borrow rules (#1094, #1004). Write the smallest manuscript change and flag it.
3. Evaluator, JIT (optimize on and off) and native agree; leak tests for elements that own sequences.
4. Let `CacheBTree`, `FlatTable` and `IdxMap` hold non-copy values (lookups return views, removal moves the value out). Keys stay copy.
5. Rewrite the census state pool's shape (blocks of states that own sorted rows) as a test on the new forms, and measure it against `Vec<State>` in Rust (ABBA, callgrind).

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The E1522 repro passes; element views are tested on all four platforms.
- Collections with owning values have leak tests.
- Report with numbers and a "Compiler gaps" section.
- Report: docs/parallel-results/self-host-element-borrows.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
