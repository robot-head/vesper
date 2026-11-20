# `for` over arrays and sequences, and string views in bindings

> Self-hosting wave SH3, lane 4. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/iteration-string-views`. Claim issue: #1243.

`for` iterates only integer ranges (E0213), and a `Str` view from `as_bytes()` is a second-class borrow that cannot be bound (E1531). The checker ports replaced Rust's `for s in [..]`, `rsplit("::")` and `strip_prefix` with index loops that copy bytes into a `ChunkSeq[u8]` and intern them again.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-check-loops-atomics.md on #1223 (gaps 1, 3), docs/parallel-results/self-host-check-regions-seqs.md on #1226 (gap 3), src/loops/**, src/strings/**, the `for` and string-view sections of docs/manuscript, packages/vesper-std/bytes/**, packages/vesper-std/intern/**.

When to start: now, from main.

Claim: GitHub issue #1243 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1243", or "Part of #1243" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1243. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: `for` over arrays and sequences, and bound string views.

Exclusive ownership:
- `for`-iteration hunks of src/loops/** and its lowering
- string-view hunks of src/strings/** and the E1531 rule
- view helpers in packages/vesper-std/bytes/** and a lookup of a byte view in packages/vesper-std/intern/** (intern without copying)
- manuscript hunks (flagged)
- tests/for_sequences*.rs, tests/string_views*.rs

Implement:
1. Repros first: `for s in ["a", "b"] { .. }`; `for x in xs` over `ChunkSeq[u64]`, a slice and a fixed array; `let v = s.as_bytes(); v.len()`; the last `::` segment of a name as a view, looked up in an interner without a copy.
2. `for x in e` over fixed arrays, slices and sequences: copy elements bind by value, non-copy elements bind an `in` view (needs #1221's element views: until it merges, do copy elements only and add non-copy in a follow-up). Bounds checks hoisted as for ranges.
3. Bound `Str`/byte views with a lifetime of the enclosing block, the same rule as other bound views (#1112's bound views on `in` sources), with the manuscript change flagged.
4. `rsplit_once`, `split_once`, `strip_prefix`, `strip_suffix` returning views, and `Interner::lookup_view`.
5. Rewrite `last_segment` in `@vesper/check-regions-seqs` and the `Atomic::` stripping in `@vesper/check-loops-atomics` locally with the new forms and measure (the patch goes to the project files or the owning lane, not into their paths).

Acceptance:
- Repros pass; evaluator, JIT (optimize on and off) and executable agree; no copies in the rewritten helpers (callgrind).
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-iteration-string-views.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
