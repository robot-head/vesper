# Tool subcommands out of the compiler binary

> Self-hosting wave SH0, lane 2. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/tool-split`. Claim issue: #1124.

The compiler links 18 frozen Rust seed copies (about 113k lines). Eight are used only by tool subcommands: aegis (`tee_commands`), bench (`bench_commands`, via `vesper-bench-pmu`), crucible (`crucible_commands`), grammar (`grammar_commands`), serde (`grammar_commands`, `json_commands`; `vesper-lsp` also uses it), loom-audit (`audit_commands`, `publish_commands`), loom-solver (`package_commands`) and semver (`publish_commands`, `package_commands`, `tooling/lint`). See the seed table in docs/maintenance/self-hosting-plan.md. A tool does not need to be the compiler, so it can link the normal crate, and the compiler's closure, which Stage 2 must port, gets smaller.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1124 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1124", or "Part of #1124" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1124. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Tool subcommands out of the compiler binary.

Exclusive ownership:
- the moved src/*_commands.rs files and their src/main.rs and src/lib.rs hunks
- new crates/vesper-tools/** (one crate or one binary per tool family)
- the seed dependency lines of the root Cargo.toml and Cargo.lock (root patch rule)
- tests/tool_dispatch*.rs
- docs/parallel-results/self-host-tool-split.md

Implement:
1. Measure the compiler's normal closure first (`cargo tree -p vesper -e normal,build`, crate count, lines, release link time and binary size).
2. Add cargo-style dispatch: `vesper <tool> ...` runs `vesper-<tool>` next to the `vesper` executable (or on PATH) with the same arguments, exit code and output, and `vesper --help` still lists the tools. Bazel and Cargo both install the tool binaries beside `vesper`.
3. Move the tool-only command modules into the tool binaries. A tool that needs compiler services (checking a package, building an object) calls the compiler as a library or as a subprocess; prefer whichever keeps the tool off the compiler's closure. Tools link the normal crate (Rust default, per the owner rule), not the seed.
4. Remove each seed from the root package's dependencies once nothing in the compiler uses it. Do not delete any seed crate: other dependents and the parity rule still need them.
5. Keep every existing CLI test passing unchanged; add dispatch tests (unknown tool, missing binary message, exit codes, stdin/stdout passthrough).

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Every existing CLI test passes on all four platforms without edits to its expectations.
- The report gives the closure before and after (crates, lines, link time, binary size) and lists the seeds still in the closure with the compiler module that needs each.
- Report: docs/parallel-results/self-host-tool-split.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
