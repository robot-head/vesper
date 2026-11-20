# Drop glue on every exit: raise, field returns and early returns

> Self-hosting wave SH1, lane 2. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/exit-drops`. Claim issue: #1145.

The toolchain pilots found three leaks that every compiler module would hit: `Fail::raise` with a live local struct that owns a `String`, returning one field of a local struct, and an early `return` inside an `if` after a scratch struct is made. The ports worked around each by hand. P3b gaps lane 10 (#1095) fixed drops on several exits, so start by checking which shapes still leak.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-toolchain-pilots.md (gaps 1-3), docs/parallel-results/p3b-gaps-exits-and-handlers.md if present, and the drop elaboration added by #1018 and #1105.

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1145 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1145", or "Part of #1145" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1145. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Drop glue on every exit: raise, field returns and early returns.

Exclusive ownership:
- drop elaboration for function exits in src/ (the drop-glue modules #1018 and #1105 touched; list the files in the PR body)
- new tests/exit_drops*.rs
- the workaround hunks in crates/vesper-lean-syntax/vesper/** and crates/vesper-span/vesper/** (a separate follow-up commit after the fix, measured)
- docs/parallel-results/self-host-exit-drops.md

Implement:
1. Reproduce every gap this lane owns on current main first (the P3b gaps wave merged fixes for nearby shapes after some SH0 reports were written). Record which repros already pass and drop them from the scope; commit each repro that still fails as a test before the fix.
2. Make `Fail::raise`, `?`, handler aborts and early `return` drop every live owned local, including fields of partially moved structs, in the evaluator, the JIT (optimize on and off) and objects.
3. Make returning a field of a local struct drop the other fields.
4. Leak tests for each shape (the leak auditor counts, as tests/std_arena.rs does), in their own test binary.
5. Remove the pilots' workarounds (`p.out = String::new()` before raises, destructure-to-return) and show the ports still pass and do not leak; give the Ir change.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Every repro of the pilots' gaps 1-3 is a passing leak test on all three engines.
- No new leak in the port harness's leak tier.
- Report: docs/parallel-results/self-host-exit-drops.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
