# `effect`, `handle` and pass-mode words as names; literals past u64::MAX; fmt on long chains

> Self-hosting wave SH3, lane 5. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/names-and-literals`. Claim issue: #1244. Stacks on: #1239.

SH2 lane 3 (#1218) made `eff` contextual. The ports still rename `effect`, `handle`, `sink` and friends (`fn effect()`, `let effect`, `struct S { sink: u64 }` are E0002), the parser refuses integer literals above `u64::MAX` now that `u128` exists, and `vesper fmt` stops at DepthLimit on a 24-term `||` chain.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from the head of the pull request this lane stacks on (#1239) in a NEW cloud checkout or a dedicated git worktree (`git fetch origin pull/<n>/head`). Record that SHA and origin/main's SHA. Open your PR against `main`, and say in its first line which PR it stacks on. Never merge before it; when it merges, merge origin/main into your branch (no rebase of a pushed branch) and re-run your tests before merging. If it is closed without merging, stop and report. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: #1239 (value enums: it also edits the parser and both front-end packages), docs/parallel-results/self-host-contextual-eff.md (gaps 1-3), docs/parallel-results/self-host-wide-integers.md (gap 1), docs/parallel-results/self-host-check-effects.md on #1238 (gaps 1, 3), docs/parallel-results/self-host-check-names-packages.md on #1227 (the `effect` note), crates/vesper-syntax/src/{lexer,parser}.rs, packages/vesper-front, packages/vesper-ast, crates/vesper-fmt.

When to start: now, stacked on #1239.

Claim: GitHub issue #1244 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1244", or "Part of #1244" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1244. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Contextual `effect`/`handle`/pass-mode names, wide literals and fmt on long chains.

Exclusive ownership:
- keyword and integer-literal hunks of crates/vesper-syntax/src/{lexer,parser}.rs with their packages/vesper-front and packages/vesper-ast mirror
- the literal hunk of crates/vesper-core (`Literal::Int` widened to hold `u128::MAX`, or a sign and a `u128`)
- the binary-chain hunk of crates/vesper-fmt (and its `vesper-fmt` port seed only if the mirror requires it)
- crates/vesper-highlight tier 0 keyword hunks if the classes change
- manuscript hunks (flagged)
- tests/contextual_names*.rs, tests/wide_literals*.rs

Implement:
1. Repros first: `fn effect() {}`, `fn handle(x: u64) -> u64 { x }`, `let effect = 1;`, `struct S { sink: u64, inout: u8 }`, `fn f() -> u128 { 340282366920938463463374607431768211455 }`, a `-> bool` function of 24 `f(x)` terms joined by `||` through `vesper fmt`.
2. Make `effect`, `handle` and the pass-mode words contextual where the grammar allows (LL(2) at most; report any that cannot be), in the Rust parser and both packages.
3. Literals up to `u128::MAX` (and `i128::MIN`) in the lexer, parser, AST and both packages; the checker's literal typing (SH2 lane 7 owns its Vesper port; you change only the Rust and the front end).
4. fmt: format long homogeneous binary chains iteratively so DepthLimit does not apply to them.
5. Highlighting classes stay right for the new names (tier 0 mirrors the Rust highlighter).

Acceptance:
- Repros pass; tests/self_host_front_v2.rs and tests/self_host_ast.rs pass in full with new hand cases; fmt is idempotent on the corpus.
- Tests pass on all four platforms and the Stage 1 guard holds (the Stage 1 front end in packages/vesper-compiler is not touched).
- Report: docs/parallel-results/self-host-names-and-literals.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
