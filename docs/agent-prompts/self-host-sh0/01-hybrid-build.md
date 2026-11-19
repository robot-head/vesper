# Hybrid Stage 2 build

> Self-hosting wave SH0, lane 1. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/hybrid-build`. Claim issue: #1123.

The compiler links the frozen Rust seed copy of every ported crate it uses, because a Rust build of `vesper` cannot link a Vesper object that needs `vesper` to build (porting-a-crate.md, "Crates the compiler links"). Self-hosting needs the same trick the ports use, applied to the compiler itself: a second, non-default compiler build in which one module at a time is a Vesper object, compiled by the default Rust `vesper` executable. Every later wave (SH1-SH5) swaps its modules into this build.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1123 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1123", or "Part of #1123" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1123. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Hybrid Stage 2 build.

Exclusive ownership:
- new crates/vesper-hybrid/** (the hybrid compiler crate, its build.rs and swap-in glue)
- new tools/bazel/vesper_hybrid.bzl
- tests/hybrid_*.rs
- the root Cargo.toml workspace member line and Cargo.lock (root patch rule)
- docs/parallel-results/self-host-hybrid-build.md

Implement:
1. Design the build so Cargo sees no cycle and resolver 2 cannot unify features into the default compiler: `vesper-hybrid` is a separate binary crate that depends on the `vesper` library with a feature (or `cfg`) per swappable module, and whose build.rs runs the default `vesper` executable (as `vesper-port-harness` does) on Vesper packages. If feature unification makes this impossible in one Cargo workspace, use a separate Cargo invocation or Bazel-only target, and say why in the report.
2. Define a swap-in point per compiler module: a Rust trait or function table the module already presents, with the Rust implementation as the default and the Vesper export (`vesper build --lib`, export ABI v3) as the alternative. Write the recipe in docs/maintenance/porting-a-crate.md style in the report so SH1+ lanes can follow it.
3. Pilot: swap one small pure module, e.g. the word-layout functions of `src/layout.rs`, for a Vesper package (not committed under packages/vesper-compiler/**). Do not edit src/layout.rs beyond what the swap-in point needs; if it needs more, pick another pure module and say so.
4. Differential harness: run the default and hybrid compilers over the bootstrap corpus and `tests/` and require identical outputs (objects, diagnostics, exit codes); add a CI job that is non-required.
5. Measure the hybrid compiler's compile time on the corpus against the default.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- `vesper-hybrid` builds with Cargo and Bazel on all four platforms, and the default `vesper` is unchanged (same binary digest in the Stage 1 guard run).
- The pilot module runs from Vesper in the hybrid build and the differential harness is green.
- Report: the build design, the swap-in recipe, the pilot numbers and the compiler gaps found.
- Report: docs/parallel-results/self-host-hybrid-build.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
