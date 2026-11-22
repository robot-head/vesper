# Vectors in registers and the SIMD paths

> Rust-exit gap wave 1, lane 3. The [manuscript](../../manuscript/README.md)
> is the sole specification. Rules are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `rust-exit-gaps/vectors-in-registers`. Claim issue: #1500.

Item 2 of the Kinetic/Resonance report: `Vec[T, N]` values (§14) live in stack slots and are copied through 8-byte words around each operation (`src/native_vector.rs`: "store the result into a fresh stack slot"), so a four-lane add loop costs about 55 instructions per four elements, more than scalar code. The ported SIMD paths run at 8-48x and the graph sums (AVX in Rust) are the Resonance gap. Lower vector values to Cranelift vector SSA values, with loads, stores, lane ops and bit casts in registers.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that keeps Vesper ports above the owner's flip bar, in https://github.com/robot-head/vesper. The owner's priority (2026-11-21): "Prioritize closing compiler gaps so we can expedite vesper transition."

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/rust-exit-gaps-1/README.md (this wave), docs/maintenance/rust-exit-plan.md, docs/maintenance/rust-census.md, docs/agent-prompts/self-host-sh6/README.md ("Rules for every lane", which apply), and the lane reports this prompt names (docs/parallel-results/rust-exit-*.md).

When to start: now.

Claim: GitHub issue #1500 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the native code size thread #1426, the try_get vectorizer #1474, the bounds-hoisting thread and the SH4 family lanes #1280-#1291, which own their packages). If something overlaps, stop and report instead of duplicating it. Link the claim from every PR ("Part of #1500", and "Closes #1500" on the last).

Delivery and coordination:
- Measure before and after on the ports that reported the gap (ABBA through `scripts/ci/flip_gate.py`, callgrind where the report used it), and tell each SH6 lane whose port now passes the gate (a comment on its claim issue).
- A compiler change must keep the Rust compiler and its Vesper ports in step: when you change a Rust pass that has a Vesper port in `packages/` (swapped in `crates/vesper-hybrid`), change the port in the same PR, or hand the patch to the lane that owns it and land after it. The hybrid differential must stay at 0.
- CI rule (owner, standing): no GitHub Actions runs for a branch that cannot merge yet. Right after such a push, cancel its runs (`gh api -X POST repos/robot-head/vesper/actions/runs/<id>/cancel`). Push rarely. Run targeted tests locally.
- Run the new `vesper fmt` on every `.ves` file you touch before pushing (#1313), or Native and acceptance fails.
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/stage1* or packages/vesper-compiler/**; show `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock when your change reaches the compiler.
- Edit only the exclusive paths below. Serialize merges that touch root Cargo.toml or crates/vesper-hybrid (the merge-captain thread orders them).
- Bazel: `cargo +1.100.0 clippy` before pushing; files a test reads from outside its crate go in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Licence `MIT OR Apache-2.0`.
- Use an isolated target directory, CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1500. Merging is authorized once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed.
- Report: docs/parallel-results/rust-exit-gaps-vectors-in-registers.md with the repro, the fix, the numbers before and after per port, the Stage 1 guard evidence, new gaps found (minimal repros) and what is left. Update the affected rows of scripts/rust-census.py (merge main first; that hunk is shared).

Assignment: Vectors in registers and the SIMD paths.

Item 2 of the Kinetic/Resonance report: `Vec[T, N]` values (§14) live in stack slots and are copied through 8-byte words around each operation (`src/native_vector.rs`: "store the result into a fresh stack slot"), so a four-lane add loop costs about 55 instructions per four elements, more than scalar code. The ported SIMD paths run at 8-48x and the graph sums (AVX in Rust) are the Resonance gap. Lower vector values to Cranelift vector SSA values, with loads, stores, lane ops and bit casts in registers.

Exclusive ownership:
- `src/native_vector.rs`, the vector hunks of `src/vector.rs` and `src/tiled/`, the vector hunks of `packages/vesper-codegen-numeric` (SH4 lane 11 owns that package: coordinate, or hand over the patch), `tests/native_vector_registers*.rs`, report

Implement:
1. Turn the `acc4` repro into a test that checks the CLIF has no stack traffic for vector temporaries.
2. Keep vectors in SSA values: `Vec::load`/`store` straight to memory, lane arithmetic and `to_bits`/`from_bits` as no-ops in registers, and spills only where Cranelift decides. Handle `Isa[Baseline128]` and the wider profiles, plus the scalar fallback on targets without them.
3. Re-measure Resonance's graph sums and Kinetic's and Resonance's SIMD paths (8-48x today), and the serde SIMD stage 1 if SH6 lane 11 has ported it.

Acceptance:
- No stack round trips for vector temporaries; the SIMD paths re-measured; ports that now pass flagged to their lanes.
- Tests pass on all four platforms; the Stage 1 guard holds; the hybrid differential stays at 0.
- The report above.
~~~
