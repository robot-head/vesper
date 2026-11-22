# `@vesper/egraph`, an e-graph in Vesper for the optimizers

> Self-hosting wave SH3, lane 8. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh3/egraph`. Claim issue: #1247.

Gap S7 of the self-hosting plan: `src/optimize.rs` uses egg and `src/ir_opt.rs` uses egglog. The plan prefers a small e-graph in Vesper to keeping both behind an extern. Both rule sets are small (pure integer identities with totality guards; read-over-write forwarding with a disjointness relation), so a general egglog is not needed.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh3/README.md (the wave plan, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane", which all apply), docs/parallel-results/self-host-sh2-scoreboard.md (the SH2 numbers and gaps this wave comes from), docs/parallel-results/self-host-census.md, docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: src/optimize.rs (the egg language, rules, guards and extraction), src/optimize/certificate.rs (what an accepted rewrite must prove), src/ir_opt.rs (read only: the egglog program `RULES` and how results are read back), the egg 0.10 and egglog 3 sources in the Cargo registry for their rebuild and extraction order, packages/vesper-std/{hash,intern,arena}/**.

When to start: now, from main.

Claim: GitHub issue #1247 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1247", or "Part of #1247" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1247. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: `@vesper/egraph`.

Exclusive ownership:
- new packages/vesper-egraph/**
- tests/self_host_egraph*.rs
- its shard 7 entry

Implement:
1. Core: e-nodes hash-consed in flat tables, union-find with path compression, the rebuild (congruence closure) step, and a fixed-iteration saturation driver with node and iteration limits matching egg's runner defaults the compiler uses.
2. Patterns: e-matching of the rule shapes both optimizers use (constants, variables, guarded conditions on e-class contents), and a relation table for `disjoint`.
3. Extraction: the cost function `optimize` uses, with egg's tie-breaking where it can be reproduced.
4. Differential: run `optimize`'s rules on every corpus expression it optimizes and `ir_opt`'s rules on every memory-observation term it builds, in egg/egglog and in Vesper, and compare extracted terms. Where tie-breaking cannot match, compare cost and certificate acceptance, list the differences, and flag the choice for the owner (see the plan's spec calls).
5. Measure saturation time against egg on the same inputs (ABBA, callgrind).

Acceptance:
- 0 differences in extracted terms on the corpus, or a reported, owner-flagged list of equal-cost differences that all pass the certificate checker.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-egraph.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
