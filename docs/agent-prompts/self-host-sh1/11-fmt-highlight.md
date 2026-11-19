# Formatter and lexical highlighter in Vesper

> Self-hosting wave SH1, lane 11. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/fmt-highlight`. Claim issue: #1154.

`vesper-fmt` (2.3k lines) formats the rowan CST, and the lexical part of `vesper-highlight` classifies tokens. Both need only a lossless tree, which `@vesper/front` has. They are the first compiler-linked tools that can move to Vesper whole.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-toolchain-pilots.md (seed pattern), docs/parallel-results/self-host-front-end-v2.md, crates/vesper-fmt and crates/vesper-highlight.

When to start: now. Lane 10 ports the editor-mode tree; if the formatter needs recovery nodes before lane 10 lands, format only inputs that parse cleanly and report the rest. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1154 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1154", or "Part of #1154" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1154. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Formatter and lexical highlighter in Vesper.

Exclusive ownership:
- crates/vesper-fmt/** (new vesper/, seed/, port/ dirs, build.rs, tests), except lane 5's DepthLimit fix (merge main first if it has landed)
- crates/vesper-highlight/** (lexical classes only; rasterizing stays Rust)
- their root Cargo.toml seed lines and Cargo.lock (root patch rule)
- the ports' entries in the Linux port shard 7 list
- docs/parallel-results/self-host-fmt-highlight.md

Implement:
1. Follow the seed pattern for compiler-linked crates: the compiler links `vesper-fmt-seed` and `vesper-highlight-seed`; each crate gains a non-default `vesper-port` build running its tests against the Vesper package.
2. Port the formatter over the @vesper/front tree, with byte-identical output to Rust on every .ves file in the repository, the fmt tests and the 131,072-token cap behaviour.
3. Port highlight's lexical token classification with identical class spans on the corpus.
4. Use @vesper-std/hash-sys for fmt's BLAKE3 digests.
5. ABBA and callgrind for formatting the largest repository file and the whole corpus.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each crate's tests pass against both builds on all four platforms; Rust stays the default.
- The byte-identical corpus test passes.
- Report: docs/parallel-results/self-host-fmt-highlight.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
