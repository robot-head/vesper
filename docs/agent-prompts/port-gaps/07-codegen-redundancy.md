# Variants built in place, one compare per fused read, sequence headers hoisted out of loops

> Port gaps wave, lane 7. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/codegen-redundancy`. Claim issue: #990.

With locals in registers (#983) the next native costs are: enum variants built in a zeroed slot and copied twice into the result (Ignis's `check` repro, 148 instructions and 32 zero stores for four returns), constant aggregates not folded (BBR's `outer_new`, 60 instructions after #983 for 9 zero words), the fused `try_get` comparing its bound twice (warp-wheel's `sum3`), and a sequence field read through `inout q` reloading `{len, data}` per element (Quarry's `lower_ok`, 290 instructions per identifier).

Ports to re-measure: all seven.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend gap that the port parity wave of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-gaps/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-parity.md (the scoreboard), the port reports it cites for this lane (port-parity-ignis.md gap 1, port-parity-bbr.md gap 2, port-parity-warp-wheel.md gap 3, port-parity-quarry.md gap 6), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: after PR #983 (#972) merges; it needs locals in registers. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #990 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all #972 and PR #983); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #990").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 9 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #990. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Variants built in place, one compare per fused read, sequence headers hoisted out of loops.

Exclusive ownership:
- the aggregate construction and variant hunks of src/native.rs (after PR #983 merges; coordinate with #972's follow-ups)
- the fused-read hunks of src/seq/native.rs
- src/bounds_hoist.rs (header-load hoisting for loops that do not write the sequence)
- tests/native_aggregates*.rs, tests/native_hoist*.rs
- docs/parallel-results/port-gaps-codegen-redundancy.md

Implement:
1. Re-measure the four repros on main with #983 in.
2. Build a variant directly in the result slot or destination; skip zeroing words that are written before they are read.
3. Fold constant aggregates into stores of constants (or a copy from read-only data).
4. Emit one bounds compare per fused `try_get`.
5. Hoist the `{len, data}` load of a sequence (local or through `inout`) out of loops that do not write or resize it, with the guard #942 uses.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each repro named above is re-measured (instructions in the emitted function, or `objdump`) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks, with local port rewrites where the lane adds a builtin; the report gives each ratio before and after with the setup. Report only what you measured.
- Report: docs/parallel-results/port-gaps-codegen-redundancy.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
