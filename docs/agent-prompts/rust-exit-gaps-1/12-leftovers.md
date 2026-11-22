# Leftover modules and sys crates

> Rust-exit gap wave 1, lane 12. The [manuscript](../../manuscript/README.md)
> is the sole specification. Rules are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `rust-exit-gaps/leftovers`. Claim issue: #1510.

The census rows that no lane owns: `src/ir_opt_policy.rs` (671 lines; SH5 lane 9 closed), what is left of `src/database` (597, salsa; SH5 lane 16 closed), the tour playground (`src/browser_tour.rs`, `src/source_analysis.rs`), and `vesper-blake3-sys`, which wraps the BLAKE3 C library in Rust when Vesper can import it straight from `.vsys`.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that keeps Vesper ports above the owner's flip bar, in https://github.com/robot-head/vesper. The owner's priority (2026-11-21): "Prioritize closing compiler gaps so we can expedite vesper transition."

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/rust-exit-gaps-1/README.md (this wave), docs/maintenance/rust-exit-plan.md, docs/maintenance/rust-census.md, docs/agent-prompts/self-host-sh6/README.md ("Rules for every lane", which apply), and the lane reports this prompt names (docs/parallel-results/rust-exit-*.md).

When to start: now.

Claim: GitHub issue #1510 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the native code size thread #1426, the try_get vectorizer #1474, the bounds-hoisting thread and the SH4 family lanes #1280-#1291, which own their packages). If something overlaps, stop and report instead of duplicating it. Link the claim from every PR ("Part of #1510", and "Closes #1510" on the last).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1510. Merging is authorized once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed.
- Report: docs/parallel-results/rust-exit-gaps-leftovers.md with the repro, the fix, the numbers before and after per port, the Stage 1 guard evidence, new gaps found (minimal repros) and what is left. Update the affected rows of scripts/rust-census.py (merge main first; that hunk is shared).

Assignment: Leftover modules and sys crates.

The census rows that no lane owns: `src/ir_opt_policy.rs` (671 lines; SH5 lane 9 closed), what is left of `src/database` (597, salsa; SH5 lane 16 closed), the tour playground (`src/browser_tour.rs`, `src/source_analysis.rs`), and `vesper-blake3-sys`, which wraps the BLAKE3 C library in Rust when Vesper can import it straight from `.vsys`.

Exclusive ownership:
- `packages/vesper-ir-opt-policy/**` (new), the database rows of `packages/vesper-memo` (from SH5 lane 16), new tour package files, `crates/vesper-blake3-sys` and its users' `.vsys` hunks, tests named after each, report, report

Implement:
1. Port `ir_opt_policy` and swap it into the hybrid (differential 0).
2. Check what SH5 lane 16 left in `src/database` and port the rest on `@vesper/memo`.
3. Port the tour playground's analysis to Vesper (it already runs in browser wasm).
4. Replace `vesper-blake3-sys` with a `.vsys` import of the BLAKE3 C library shared by Loom CAS (SH6 lane 16) and Loom audit (lane 12), and delete the crate.

Acceptance:
- Each row ported or deleted, with its census row updated.
- Tests pass on all four platforms; the Stage 1 guard holds; the hybrid differential stays at 0.
- The report above.
~~~
