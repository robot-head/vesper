# .vsys imports from dependency packages, bool and C strings

> Self-hosting wave SH1, lane 7. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/vsys-packages`. Claim issue: #1150.

The std I/O lane found that `.vsys` imports are read only from the root module of a library build, so every consumer of `@vesper-std/fs` copies its import texts, and an executable cannot import at all. The Cranelift shim found no `bool` in `.vsys`; std I/O found no C string parameter and an impossible error arm on non-NULL handle results. The self-hosted driver is an executable that imports fs, process, env and Cranelift, so these block SH5 and every hybrid swap that touches the OS.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-std-io.md (gaps 1, 2, 5), docs/parallel-results/self-host-cranelift-shim.md (gaps 1, 4, 5).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1150 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1150", or "Part of #1150" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1150. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: .vsys imports from dependency packages, bool and C strings.

Exclusive ownership:
- src/ffi_lowering.rs and src/packages.rs hunks for package-level `.vsys` files
- the Loom manifest field (crates/vesper-loom-calculus/seed/** and its port if the schema lives there; say which in the PR)
- packages/vesper-std/{fs,process,env}/Loom.toml and the driver copies they replace (crates/vesper-std-driver/vesper/**)
- new tests/vsys_packages*.rs
- the manuscript §16.3 hunk (flagged)
- docs/parallel-results/self-host-vsys-packages.md

Implement:
1. Reproduce every gap this lane owns on current main first (the P3b gaps wave merged fixes for nearby shapes after some SH0 reports were written). Record which repros already pass and drop them from the scope; commit each repro that still fails as a test before the fix.
2. A Loom manifest field naming the package's `.vsys` files; `packages::load` reads it for every package in the graph, so `@vesper-std/fs` carries its own imports, for `vesper build --lib`, `vesper build` of executables and `vesper run`.
3. `bool` parameters and results in `.vsys` (C `_Bool`/`uint8_t`), so the shim drops its `u32` flags.
4. `nocapture in path: CStr` parameters: copied from a `Slice[u8]` into a NUL-terminated buffer, refusing interior NULs, plus an errno/GetLastError read after the call.
5. Non-NULL handle results (`ForeignVal[!H]` or the manuscript's chosen spelling) without the error arm.
6. Remove the driver's copied `.vsys` texts and the `driver_vsys_copies_match_the_packages` test once packages carry their own.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- `@vesper-std/fs` is used from a dependency package by an executable on all three OSes.
- The shim's flag conversions are gone in a follow-up commit and its family tests stay byte-identical.
- Report: docs/parallel-results/self-host-vsys-packages.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
