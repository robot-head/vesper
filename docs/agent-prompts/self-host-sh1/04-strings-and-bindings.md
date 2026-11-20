# Strings and export-binding costs

> Self-hosting wave SH1, lane 4. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/strings-and-bindings`. Claim issue: #1147.

The pilots and the hybrid build found costs at the string and binding level that every swapped compiler module would pay: `push_str` copies byte by byte, bindings re-validate returned `String`s, aggregate results allocate a `Vec` per call, and a host cannot call an exported function with `Str` arguments and a sequence result without hand-written word plumbing.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-toolchain-pilots.md (gaps 4-5), docs/parallel-results/self-host-hybrid-build.md (gap 1), docs/parallel-results/self-host-front-end-v2.md (gap 7).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1147 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1147", or "Part of #1147" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1147. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Strings and export-binding costs.

Exclusive ownership:
- the `push_str`/`extend` lowering hunks in src/strings.rs and the sequence lowering
- the Rust binding generator in src/native_export.rs (binding text only) and crates/vesper-port-abi/**
- the regenerated committed bindings of ports whose binding text changes (regeneration only, no hand edits)
- new tests/string_copy*.rs and tests/binding_*.rs
- docs/parallel-results/self-host-strings-and-bindings.md

Implement:
1. Reproduce every gap this lane owns on current main first (the P3b gaps wave merged fixes for nearby shapes after some SH0 reports were written). Record which repros already pass and drop them from the scope; commit each repro that still fails as a test before the fix.
2. Lower `String::push_str`, `push_str` of a `Str` view and byte `extend` to the copy builtin (one reserve, one memcpy).
3. Bindings take a returned `String` as UTF-8 without `from_utf8` (Vesper strings are UTF-8 by construction; keep a debug assertion).
4. Aggregate results use a stack array of `Abi::WORDS` words, not a `Vec`.
5. Add a host helper in vesper-port-abi to call an exported function with `Str`/`Slice[u8]` arguments and a `ChunkSeq`/`String` result, and use it in tests/self_host_front_v2.rs in place of the hand-written word plumbing.
6. Measure span edits, lean print and one P3 port before and after.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Every committed binding check passes after regeneration on all four platforms.
- Span edits and lean print Ir in the report.
- Report: docs/parallel-results/self-host-strings-and-bindings.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
