# Cranelift through a C-ABI shim

> Self-hosting wave SH0, lane 4. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh0/cranelift-shim`. Claim issue: #1126.

About 38k lines of the compiler (`src/native*.rs`, `src/seq/native*.rs`, `src/vector.rs`, `src/tiled*`, `src/ffi_lowering*`, `src/freestanding*`, `src/native_export*`) lower Place IR to Cranelift through its Rust API. The owner decided on 2026-11-19 that Stage 2 keeps Cranelift as a vetted external library, like the crypto crates, and moves only the lowering to Vesper. That needs a C-ABI shim Vesper can import.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are laying groundwork for a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh0/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1126 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1126", or "Part of #1126" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh0/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1126. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Cranelift through a C-ABI shim.

Exclusive ownership:
- new crates/vesper-cranelift-sys/** (the shim: a staticlib/cdylib with a C header)
- new packages/vesper-cranelift/** (the `.vsys` import file and typed Vesper wrappers)
- tests/cranelift_shim*.rs
- the root Cargo.toml workspace member line and Cargo.lock (root patch rule)
- docs/parallel-results/self-host-cranelift-shim.md

Implement:
1. Inventory the Cranelift API surface the compiler uses (types, instructions, `FunctionBuilder`, `Variable`s, blocks, signatures and call conventions, stack slots, data objects, relocations, `ObjectModule`, `JITModule`, `isa::lookup` and flags). Put the list in the report; SH4 lanes will rely on it.
2. Write the shim with the same Cranelift version as the root package (0.125). Expose opaque handles (context, module, function builder, data description) as `linear extern type`s, and values, blocks, types, stack slots and function refs as plain integers. Keep each call coarse enough that the per-call overhead stays small (batch instruction emission if needed).
3. `@vesper/cranelift`: typed wrappers so Vesper code reads like the Rust lowering (`ins_iadd(b, x, y)`, `switch_to_block`, `declare_function`, `define_function`, `finish` to object bytes).
4. Pilot: build, from Vesper, an object for three functions (arithmetic, a loop with block params, a call and a stack slot), link it with the system linker and run it on all platforms. Then emit one real compiler function family (for example the checked-arithmetic helpers of `src/native.rs`) from Vesper and compare the object bytes with the Rust lowering.
5. Measure per-instruction overhead of emission through the shim against direct Rust calls.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The shim builds with Cargo and Bazel on all four platforms; the pilot objects link and run; handles have leak tests.
- The compared function family gives byte-identical Cranelift output (or every difference is explained).
- Report: API inventory, design, overhead numbers and gaps.
- Report: docs/parallel-results/self-host-cranelift-shim.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
