# f32 vector lanes

> P3b gaps wave, lane 1. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/f32-vectors`. Claim issue: #1086.

Resonance's DSP kernel is 16 independent `f32` lanes. The Rust build uses `simd.rs` (AVX/NEON); the port is scalar because `Vec[T, N]` (§14.3, `src/vector.rs`) has `u8`, `u32`, `u64`, `i64` and `f64` lanes but no `f32` (f32 itself only arrived with #893), and the only vector load is `Vec::load_masked[u8, N]` from a string. Repro from port-p3b-resonance.md, gap 1: `for v in 0..16 { s.set(v, to_bits_f32(from_bits_f32(a[v]) * from_bits_f32(b[v]))) }` compiles to 16 `mulss`. Kinetic's narrow-phase and Tensor's kernels are the next users.

Ownership note: `src/vector.rs` and `src/native_vector.rs` are listed under the stale wave-nine claim #587 (source `VVec`/`VMask`), which has no open PR. Comment on #587 that this lane adds `f32` lanes and slice loads to the fixed-width `Vec[T, N]` only, and stop and report if #587 has an open PR when you start.

Ports to re-measure: Resonance (ABBA kernel 5.3x, graph 5.0x), Kinetic, Tensor.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-resonance.md ("Compiler gaps" 1), port-p3b-kinetic.md, bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1086 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1086", or "Part of #1086" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1086. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: f32 vector lanes.

Exclusive ownership:
- Elem::F32 and the f32 hunks of src/vector.rs, src/vector/** and src/native_vector.rs
- the slice load/store intrinsics added to the same files
- tests/vector_f32*.rs, tests/vector_slice_io*.rs
- the §14.3 hunk of docs/manuscript/content.typ that lists lane types
- docs/parallel-results/p3b-gaps-f32-vectors.md

Implement:
1. Add `f32` as a lane type of `Vec[T, N]` and `Mask[T, N]` (128, 256 and 512 bits: `Vec[f32, 4]`, `[f32, 8]`, `[f32, 16]`), with `F32X4` chunks in the native lowering, every existing lane operation that applies to `f64` (arithmetic, compare, select, min/max, reduce, extract/replace, shuffle, splat/from), and IEEE single semantics in the evaluator.
2. Add `Vec::load[T, N](in s: Slice[T], at: u64)` and `Vec::store(inout s: Slice[T], at: u64, v)` for every lane type whose packed stride is its lane size (`u8`, `u32`, `u64`, `i64`, `f32`, `f64`), checked once per call (`Fail[RuntimeError]` when `at + N > len`, like `copy_from`), plus `Vec::load_masked`/`store_masked` with a tail mask for the same types.
3. Add `Vec::mul_add(a, b, c)` (fused, `fma` where the ISA has it, a correctly rounded software fallback otherwise so every backend agrees), `Vec::sqrt`, `Vec::abs`, `Vec::neg` for float lanes, and the `f32`<->`u32`/`i32`-bits and `f32`<->`f64` lane conversions Resonance and Kinetic use.
4. Make sure the operations work in `vesper build --lib` packages (export ABI 3) under `Isa[Baseline128]`, and that a function holding them still inlines.
5. Write the smallest manuscript change in §14.3 listing `f32` lanes and the slice loads, and flag it.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- The Resonance repro kernel (16 lanes, multiply-add) is measured before and after in instructions per call, and a `Vec[f32, 16]` rewrite of `k_render` (local, not committed) is re-measured with the committed ABBA benchmark.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-f32-vectors.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
