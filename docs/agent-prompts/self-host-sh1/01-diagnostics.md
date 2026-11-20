# Port vesper-diagnostics to Vesper

> Self-hosting wave SH1, lane 1. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/diagnostics`. Claim issue: #1128.

SH0 lane 6 (#1128) ported `vesper-span` and `vesper-lean-syntax` in part 1 (#1138) and left its part 2, the `vesper-diagnostics` port, unstarted. Every compiler error goes through this crate: it renders ariadne 0.5 terminal reports and LSP JSON. This lane finishes #1128.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-toolchain-pilots.md (part 1, its seed edits and its gaps) and crates/vesper-span (the finished seed pattern to copy).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1128 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1128", or "Part of #1128" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1128. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Port vesper-diagnostics to Vesper.

Exclusive ownership:
- crates/vesper-diagnostics/** (new vesper/, seed/, port/ dirs, build.rs, tests)
- its root Cargo.toml seed line and Cargo.lock (root patch rule)
- the port's entry in the Linux port shard 7 list
- docs/parallel-results/self-host-diagnostics.md

Implement:
1. Follow docs/maintenance/porting-a-crate.md, "Crates the compiler links", exactly as #1138 did for vesper-span: the compiler and every compiler-linked dependent use the frozen `vesper-diagnostics-seed` copy; the crate gains a non-default `vesper-port` build that runs the existing tests against the Vesper package. Rename the seed dev-dependency to avoid E0464, as span did.
2. Port the prepare checks (span validation, label ordering) and the diagnostic model in Vesper.
3. Reimplement the ariadne 0.5.1 output the crate uses byte for byte, colors off and on: headers, labels, multi-line spans, notes and help, and the unicode-width 0.1.14 column table (generate the table from the crate's data with a committed script; do not hand-type it).
4. Produce the LSP JSON in Vesper and check it by parsing both outputs with serde_json in the tests.
5. ABBA and callgrind against Rust for rendering a 200-label report and for the JSON path.
6. Write a "Compiler gaps" section with minimal repros.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Every test of the crate passes against both builds on all four platforms (the port tier of the port harness); Rust stays the default.
- Byte-identical rendering for every case in the crate's tests and for a generated corpus of at least 500 random reports (labels, overlaps, wide characters, tabs).
- Report: docs/parallel-results/self-host-diagnostics.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
