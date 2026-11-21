# The driver: `vesper build` from arguments through linking

> Self-hosting wave SH5, lane 12. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh5/driver`. Claim issue: #1335.

The CLI's compile path (`src/main.rs`, 901 lines, plus `src/lib_commands.rs` and `src/package_commands.rs` and the build helpers they call) parses arguments, finds the package, runs the pipeline, writes objects and calls the linker. SH6 starts when this lane lands: it is what makes the compiler a Vesper program.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh5/README.md (the wave plan, its CI rule, its stacking rule and its additions to the rules), docs/agent-prompts/self-host-sh4/README.md, docs/agent-prompts/self-host-sh3/README.md and docs/agent-prompts/self-host-sh2/README.md ("Rules for every lane" and the SH3 and SH4 additions, which all apply), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: src/main.rs, src/lib.rs, src/lib_commands.rs, src/package_commands.rs, src/packages.rs (read only), crates/vesper-hybrid (its include!'d CLI and fn tables), packages/vesper-std (fs, process, env), crates/vesper-std-driver, the SH0 lane 1 and lane 3 reports.

When to start: now, from main. Swaps of passes into the driver go through the hybrid's existing tables; this lane does not wait for SH4. If a PR this lane waits on is still open, work only on the parts that do not depend on it.

Claim: GitHub issue #1335 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the open SH4 lanes, #1275-#1291, which still own their own remaining parts); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1335", or "Part of #1335" for a partial PR).

Delivery and coordination:
- CI rule (owner, 2026-11-21, standing project rule): no GitHub Actions runs for a branch that cannot merge yet. While the PR you stack on is open, cancel every Actions run your push starts right after the push (list the branch's runs, then `gh api -X POST repos/robot-head/vesper/actions/runs/<id>/cancel` for each, or the GitHub MCP actions tools), including the bootstrap-stage1 workflow. Push rarely. Run targeted tests locally while stacked; let CI run (and dispatch bootstrap-stage1) only once your base has merged and your branch has merged origin/main, right before landing.
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh2/README.md and the additions in docs/agent-prompts/self-host-sh3/README.md, docs/agent-prompts/self-host-sh4/README.md and docs/agent-prompts/self-host-sh5/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**; never route the verified backend's pipeline through a swap point; and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch once it can merge).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths. Depend on unmerged work only through the PR named in "Stacks on" or the lane named in "When to start".
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave. Port lowering emits through @vesper/cranelift, never by reimplementing Cranelift.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front (and packages/vesper-ast where it lowers the construct) in the same PR, or tests/self_host_front_v2.rs and tests/self_host_ast.rs fail.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo. Serialize merges that touch root Cargo.toml (no merge queue on this account).
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1335. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: `@vesper/driver`.

Exclusive ownership:
- new `packages/vesper-driver/**`
- the hybrid's driver entry (a new hybrid module `driver` and its `main` hunk in `crates/vesper-hybrid/src/**`)
- `tests/self_host_driver*.rs`
- report docs/parallel-results/self-host-driver.md

Implement:
1. Port argument parsing and validation for `check`, `run`, `jit`, `build` and `emit`, with every error message byte-identical.
2. Port the build orchestration: package discovery and the manifest, the pass sequence (calling the Rust passes through the hybrid's tables), object output, and the linker invocation through `@vesper-std/process`.
3. Differential: for every corpus program and every option set the CLI tests use, the same exit code, stdout, stderr and output bytes as the Rust CLI.
4. A hybrid binary whose `main` is the Vesper driver; the full CLI test suite on it. Measure the driver's own time against Rust (it should be noise).

Acceptance:
- 0 differences in CLI behaviour over the corpus and tests/cli*.rs.
- The hybrid with the Vesper driver passes the CLI suites on all four platforms.
- Tests pass on all four platforms and the Stage 1 guard holds.
- Report: docs/parallel-results/self-host-driver.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
