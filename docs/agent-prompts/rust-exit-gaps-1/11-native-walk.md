# Port the native walk

> Rust-exit gap wave 1, lane 11. The [manuscript](../../manuscript/README.md)
> is the sole specification. Rules are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `rust-exit-gaps/native-walk`. Claim issue: #1509.

SH4 swapped native lowering family by family through hooks, but the walk that drives them (`src/native.rs`, 8.8k lines: function setup, the expression and statement walk, variables, homes, registers, edge infos) and the new CLIF clean-up pass (`src/native_clif_opt.rs`, from #1466) are still Rust with no owner. Without them, the retirement cannot delete native lowering.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that keeps Vesper ports above the owner's flip bar, in https://github.com/robot-head/vesper. The owner's priority (2026-11-21): "Prioritize closing compiler gaps so we can expedite vesper transition."

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/rust-exit-gaps-1/README.md (this wave), docs/maintenance/rust-exit-plan.md, docs/maintenance/rust-census.md, docs/agent-prompts/self-host-sh6/README.md ("Rules for every lane", which apply), and the lane reports this prompt names (docs/parallel-results/rust-exit-*.md).

When to start: now (after the SH4 family hooks it drives).

Claim: GitHub issue #1509 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all the native code size thread #1426, the try_get vectorizer #1474, the bounds-hoisting thread and the SH4 family lanes #1280-#1291, which own their packages). If something overlaps, stop and report instead of duplicating it. Link the claim from every PR ("Part of #1509", and "Closes #1509" on the last).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1509. Merging is authorized once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed.
- Report: docs/parallel-results/rust-exit-gaps-native-walk.md with the repro, the fix, the numbers before and after per port, the Stage 1 guard evidence, new gaps found (minimal repros) and what is left. Update the affected rows of scripts/rust-census.py (merge main first; that hunk is shared).

Assignment: Port the native walk.

SH4 swapped native lowering family by family through hooks, but the walk that drives them (`src/native.rs`, 8.8k lines: function setup, the expression and statement walk, variables, homes, registers, edge infos) and the new CLIF clean-up pass (`src/native_clif_opt.rs`, from #1466) are still Rust with no owner. Without them, the retirement cannot delete native lowering.

Exclusive ownership:
- new `packages/vesper-codegen-walk/**`, the hybrid hook that installs it (serialize with other hybrid PRs), `tests/self_host_native_walk*.rs`; `src/native.rs` and `src/native_clif_opt.rs` are read-only to this lane (the native code size thread #1426 and gap lanes 1-3 edit them; re-sync the port to their changes), report

Implement:
1. Map what the walk does outside the family hooks: entry and frame setup, variable and home maps, register locals, block and edge bookkeeping, re-entry into families.
2. Port it to Vesper on the lent Cranelift handles (SH4 lane 1, SH5 lane 1), with the CLIF/object differential at 0 over the corpus, then port `native_clif_opt` the same way once #1426 says it is stable.
3. Install it in the hybrid and measure the whole build against the Rust walk.

Acceptance:
- The walk swapped in the hybrid with 0 CLIF/object diffs; build-time ratio recorded.
- Tests pass on all four platforms; the Stage 1 guard holds; the hybrid differential stays at 0.
- The report above.
~~~
