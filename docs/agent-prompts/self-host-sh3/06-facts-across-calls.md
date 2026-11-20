# Facts across calls: callee result ranges, u128 facts, widened multiplies

> Self-hosting wave SH3, lane 6. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/facts-across-calls`. Claim issue: #1245.

Bounds facts (#1088) do not cover a callee's result, so `128 / bits(x)` where `bits` returns only 8 or 64 is an unhandled `Fail` (E0401) and ports write a branch per value. `u128` constant facts stop at `i128::MAX`, and a checked multiply of a widened `u64` always runs the full word-split check.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-check-regions-seqs.md on #1226 (gap 1), docs/parallel-results/self-host-wide-integers.md (gaps 2 and 4), docs/parallel-results/ on bounds facts (#1088, p3b-gaps lane 3), the facts code in src/check.rs, src/integer_widths/**, the checked-multiply lowering in src/native*.rs.

When to start: now, from main.

Claim: GitHub issue #1245 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1245", or "Part of #1245" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1245. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Result-range facts across calls, `u128` facts and widened multiplies.

Exclusive ownership:
- result-range hunks of the bounds facts in src/check.rs
- the `u128` hunks of the facts in src/integer_widths/**
- the checked-multiply hunk of native lowering for widened operands
- manuscript hunks if the facts rule is specified there (flagged)
- tests/call_facts*.rs

Implement:
1. Repros first: `fn bits(x: u8) -> u64 { if x == 1 { 8 } else { 64 } } fn lanes(x: u8) -> u64 { 128 / bits(x) }` (no `Fail`); `let a: u128 = 18446744073709551615; a * a` (no `Fail`); `a * widen_u128(b)` with `b: u64` (fewer instructions).
2. A result-range summary per function (the set or interval of values its `return`s can take, when every path returns a constant or a fact-bounded value), used at call sites for divisors and bounds. Keep it modular: a summary depends only on the callee's body, and recursion gives no fact.
3. `u128` constant facts over the whole `u128` range.
4. Widened-operand multiply: when one operand's high word is known zero, emit the cheaper overflow check (native and JIT; the evaluator is unchanged).
5. Measure the checker's time on the corpus before and after (the facts must not slow `vesper check` by more than noise) and the Ir of the repros.

Acceptance:
- Repros pass; evaluator, JIT and executable agree; no new `Fail` is admitted where a divisor can be zero (negative tests).
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-facts-across-calls.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
