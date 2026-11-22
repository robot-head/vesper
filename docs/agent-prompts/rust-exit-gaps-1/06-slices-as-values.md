# Slices as values

> Rust-exit gap wave 1, lane 6. The [manuscript](../../manuscript/README.md)
> is the sole specification. Rules are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `rust-exit-gaps/slices-as-values`. Claim issue: #1503.

Item 1 of the Quarry/Flux report and the small-libs report: `let s = seq.as_slice();` is refused (E1521, slices are second-class), slice parameters stop the inliner, and `a[lo..hi].cmp(in b[..])` on bytes is an inline byte loop, not memcmp. Ports work around all three with copies or repeated borrows.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that keeps Vesper ports above the owner's flip bar, in https://github.com/robot-head/vesper. The owner's priority (2026-11-21): "Prioritize closing compiler gaps so we can expedite vesper transition."

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/rust-exit-gaps-1/README.md (this wave), docs/maintenance/rust-exit-plan.md, docs/maintenance/rust-census.md, docs/agent-prompts/self-host-sh6/README.md ("Rules for every lane", which apply), and the lane reports this prompt names (docs/parallel-results/rust-exit-*.md).

When to start: now (manuscript change, flagged).

Claim: GitHub issue #1503 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the native code size thread #1426, the try_get vectorizer #1474, the bounds-hoisting thread and the SH4 family lanes #1280-#1291, which own their packages). If something overlaps, stop and report instead of duplicating it. Link the claim from every PR ("Part of #1503", and "Closes #1503" on the last).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1503. Merging is authorized once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed.
- Report: docs/parallel-results/rust-exit-gaps-slices-as-values.md with the repro, the fix, the numbers before and after per port, the Stage 1 guard evidence, new gaps found (minimal repros) and what is left. Update the affected rows of scripts/rust-census.py (merge main first; that hunk is shared).

Assignment: Slices as values.

Item 1 of the Quarry/Flux report and the small-libs report: `let s = seq.as_slice();` is refused (E1521, slices are second-class), slice parameters stop the inliner, and `a[lo..hi].cmp(in b[..])` on bytes is an inline byte loop, not memcmp. Ports work around all three with copies or repeated borrows.

Exclusive ownership:
- the slice-binding hunks of the checker (`src/seq/check.rs`, the E1521 site) with the matching `packages/vesper-check-regions-seqs` hunks, the inliner's slice-parameter rule in `src/ir_inline.rs` and `packages/vesper-ir-inline`, byte-slice `cmp`/`eq` lowering, the smallest manuscript hunk (flagged in the PR), `tests/slice_values*.rs`, report

Implement:
1. Write the manuscript change: a `let` may bind a borrowed slice whose lifetime the checker bounds to the enclosing block (no escape, no store into an owner). Flag it for the owner in the PR.
2. Implement it in the Rust checker and its Vesper port, which must stay in step. Let the inliner inline callees with slice parameters, and lower byte-slice `cmp`/`eq` to memcmp or a vectorized compare.
3. Remove the workarounds in one port (Quarry or Semver) to show the gain, and re-measure it.

Acceptance:
- The flagged manuscript change; slice bindings, inlining and memcmp; one port re-measured.
- Tests pass on all four platforms; the Stage 1 guard holds; the hybrid differential stays at 0.
- The report above.
~~~
