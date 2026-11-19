# Front end v2 on sequences and strings

> Self-hosting wave SH0, lane 7. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/front-end-v2`. Claim issue: #1129.

The Stage 1 front end in packages/vesper-compiler predates sequences and strings: its input is `source_byte(i)` and its CST is folded into a digest. It is frozen by bootstrap/stage1.lock and must not change. A self-hosted compiler needs a front end that builds a real lossless CST, matching `crates/vesper-syntax` (logos lexer, rowan tree), which is what the rest of the compiler, fmt and the LSP read.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1129 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1129", or "Part of #1129" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1129. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Front end v2 on sequences and strings.

Exclusive ownership:
- new packages/vesper-front/**
- tests/self_host_front_v2*.rs
- docs/parallel-results/self-host-front-end-v2.md

Implement:
1. Lexer over `Str` into a token table (`ChunkSeq` of kind, start, len) with the trivia `vesper-syntax` keeps; the same token kinds as the logos lexer.
2. Lossless CST parser into node and child tables (or an `@vesper-std/arena` index tree once lane 5 lands; start with plain sequences), with the same node kinds, ranges and error recovery as `crates/vesper-syntax/src/parser.rs`, including `use` items, `..` slice intervals, `continue`, qualified paths, fixed arrays and `const` items as they land on main (merge main often; P3b gaps lanes 6 and 8 are editing the Rust parser).
3. Differential test: every `.ves` file in the repository, the parser test corpus and a fuzzed corpus give identical CSTs (kinds, ranges, diagnostics with codes and spans).
4. ABBA and callgrind: tokens and nodes per second against the Rust parser on the whole repository's `.ves` files.
5. Comment on #880 and #887 that the new package covers `use` and `..`, and leave those issues to their owners for the frozen package.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The differential test passes on all four platforms; the Stage 1 guard holds.
- Report: speed against Rust, memory per node, recovery differences (none expected) and gaps.
- Report: docs/parallel-results/self-host-front-end-v2.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
