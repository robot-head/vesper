# Toolchain pilots: span, diagnostics, lean-syntax

> Self-hosting wave SH0, lane 6. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/toolchain-pilots`. Claim issue: #1128.

No toolchain crate that the compiler links has a Vesper port, so nobody has measured what compiler-style Rust needs from Vesper. Three small leaves are the pilots, as warp-wheel and beacon were for the libraries: `vesper-span` (631 lines, no deps), `vesper-diagnostics` (593 lines; ariadne rendering and JSON) and `vesper-lean-syntax` (1.5k lines, no deps).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1128 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1128", or "Part of #1128" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1128. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Toolchain pilots: span, diagnostics, lean-syntax.

Exclusive ownership:
- crates/vesper-span/**, crates/vesper-diagnostics/**, crates/vesper-lean-syntax/** (new vesper/, seed/, port/ dirs, build.rs, tests)
- their root Cargo.toml seed lines and Cargo.lock (root patch rule; after lane 2 merges, rebase its seed edits)
- docs/parallel-results/self-host-toolchain-pilots.md

Implement:
1. Follow docs/maintenance/porting-a-crate.md, "Crates the compiler links" (the Beacon seed pattern): the compiler and every compiler-linked dependent use the frozen `-seed` copy; the crate gains a non-default `vesper-port` build that runs the existing tests against the Vesper package.
2. `vesper-diagnostics`: render byte-identical output to ariadne for every case in the crate's tests (labels, multi-line spans, colors off and on), and the same JSON.
3. `vesper-lean-syntax`: byte-identical Lean text for every printer test.
4. ABBA and callgrind against Rust for each crate's hot function (span lookups, rendering a 200-label report, printing a large Lean term).
5. Write a "Compiler gaps" section with minimal repros: this is the main input to lane 8.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each crate's tests pass against both builds on all four platforms (the port tier of the port harness); Rust stays the default.
- Report: numbers, the seed edits, and the gaps.
- Report: docs/parallel-results/self-host-toolchain-pilots.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
