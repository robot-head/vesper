# Compiler data across the export ABI and borrowed Box trees

> Self-hosting wave SH1, lane 6. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/tree-abi`. Claim issue: #1149.

The hybrid build can swap only arithmetic, because the real entry points take the checker's `World` and recursive `ast::Type` values, which export ABI v3 cannot carry. Ports flatten trees by hand (611 lines of Rust for lean-syntax). And Vesper code cannot walk a borrowed `Box` tree twice: matching `in e: Expr` with boxed children is E0305. This lane decides how compiler data crosses the boundary in both directions and makes borrowed trees walkable. Lane 12 (the parser swap) depends on it.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-hybrid-build.md (gaps 3-4), docs/parallel-results/self-host-toolchain-pilots.md (gap 8), docs/parallel-results/self-host-std-data.md (gap 3), docs/parallel-results/p3b-gaps-export-abi-widening.md if present (lent `in` handles, #1107) and the bridge handles work (#1077).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1149 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1149", or "Part of #1149" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh1/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front in the same PR, or tests/self_host_front_v2.rs fails.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1149. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Compiler data across the export ABI and borrowed Box trees.

Exclusive ownership:
- the export ABI version bump and its binding generator hunks in src/native_export.rs (merge after lane 4 if both change the binding text; merge main between)
- the checker hunks for matching through `in` and `inout` references to `Box` payloads
- the manuscript hunks for both (flagged as spec calls)
- new tests/tree_abi*.rs and tests/borrowed_box_match*.rs
- docs/parallel-results/self-host-tree-abi.md

Implement:
1. Reproduce every gap this lane owns on current main first (the P3b gaps wave merged fixes for nearby shapes after some SH0 reports were written). Record which repros already pass and drop them from the scope; commit each repro that still fails as a test before the fix.
2. Borrowed matching: `match e { Expr::Add(l, r) => ... }` on `in e: Expr` binds `l` and `r` as `in` borrows of the boxed children, so a tree can be walked any number of times; the same for `inout` with `inout` bindings. Evaluator, JIT and objects.
3. Vesper-to-Rust trees: export a Vesper enum/struct tree to Rust as a lent read-only view with generated accessors (no copy), and as an owned value the binding decodes once. Measure both on the lean-syntax encoder and delete nothing yet: show the encoder can go in a follow-up.
4. Rust-to-Vesper data: lent opaque Rust handles (`World`, `ast::Type`) that Vesper code reads through `.vsys` accessor imports the Rust side implements, as a design note plus a working pilot on `Layout::of(world, ty)` in crates/vesper-hybrid if it fits; otherwise the design and a minimal test.
5. Write the ABI version and layout rules into the manuscript and docs/maintenance/porting-a-crate.md, and flag them.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The lean-syntax port runs without its flat encoder in a test (owned tree path) and gives the Ir change.
- Borrowed matching passes the arena and tree tests in tests/std_arena.rs rewritten over `Box` trees.
- Report: docs/parallel-results/self-host-tree-abi.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
