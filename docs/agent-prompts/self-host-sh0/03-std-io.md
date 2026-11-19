# std fs, process and env

> Self-hosting wave SH0, lane 3. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/std-io`. Claim issue: #1125.

`@vesper-std` has core, bytes, collections, fmt and sort. A Vesper compiler driver must read sources and `Loom.toml`, walk `src/**` in a deterministic order, write objects atomically, run the system linker and read its arguments and environment. None of that exists in Vesper source today; ports keep their OS edge in Rust.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1125 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1125", or "Part of #1125" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1125. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: std fs, process and env.

Exclusive ownership:
- new packages/vesper-std/fs/**, packages/vesper-std/process/**, packages/vesper-std/env/** and their .vsys files
- new crates/vesper-std-host/** if a Rust host shim is needed
- tests/std_fs*.rs, tests/std_process*.rs, tests/std_env*.rs
- the std catalog hunk of docs/manuscript/content.typ for these modules
- docs/parallel-results/self-host-std-io.md

Implement:
1. `@vesper-std/fs`: read a whole file to `ChunkSeq[u8]`/`String`, write a whole file, atomic replace (write to a temp file in the same directory, fsync, rename), create directories, directory walk returning sorted entries, metadata (size, mtime, kind), with typed `Fail[IoError]` errors.
2. `@vesper-std/process`: spawn with argv, env, cwd; wait; capture stdout/stderr; exit with a status.
3. `@vesper-std/env`: argv, environment variables, current directory.
4. Implement through `.vsys` imports with linear/affine handles (#1077, #1095 part 4) on Linux, macOS and Windows; prefer libc/Win32 calls directly over a Rust shim where the import ABI allows it.
5. Decide which effect these perform (today `.vsys` imports perform `Ffi[Lib]`; a named capability may fit `capability_ceiling` better). Write the smallest manuscript change and flag it.
6. A sample driver: a Vesper program that walks a package's `src/**`, concatenates file sizes, spawns `cc --version` and writes a result atomically, run in CI on all platforms.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Tests pass on all four platforms; handles have leak tests; the evaluator, JIT and executable agree where the evaluator supports imports.
- Report: API, effect decision, platform notes, throughput of reading the repo's `.ves` files against Rust `std::fs`, and gaps.
- Report: docs/parallel-results/self-host-std-io.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
