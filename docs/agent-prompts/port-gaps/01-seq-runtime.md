# Cheap sequence creation, growth and free on the native path

> Port gaps wave, lane 1. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/seq-runtime`. Claim issue: #984.

Every native `ChunkSeq` allocates a header and a data buffer separately and registers the header in a thread-local `HashMap` (`HEADERS` in `src/seq/native_runtime.rs`); every grow looks it up and every free removes it. Quarry measured about 330 instructions to create and free an empty sequence and 290 per grow, 21.1% of its run. It is most of Prism's 23.8% allocator share and part of Aura's 10.5%.

Ports to re-measure: Quarry, Prism, Aura, Ignis.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend gap that the port parity wave of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-gaps/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-parity.md (the scoreboard), the port reports it cites for this lane (port-parity-quarry.md gap 1, port-parity-prism.md gap 6, port-parity-aura.md gap 4), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #984 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all #972 and PR #983); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #984").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 9 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #984. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Cheap sequence creation, growth and free on the native path.

Exclusive ownership:
- src/seq/native_runtime.rs, src/seq/aot_runtime.rs (sequence allocation, growth, free and the live-allocation counter)
- the `reserve` hunks of src/seq/check.rs and src/seq/eval.rs, if the lane adds `reserve`
- tests/native_seq_runtime*.rs
- docs/parallel-results/port-gaps-seq-runtime.md

Implement:
1. Measure the Quarry repro (`make()` with three empty sequences, exported and called in a loop) and Quarry's `planner_abba` share first.
2. Remove the per-sequence map: one allocation for header and data, or the header inline in its owner, or a size-class arena for headers. Keep the leak counter that `port_live_allocations()` reads exact, but make it an atomic count, not a map, outside leak-check builds.
3. Grow geometrically without a lookup. Keep `vesper_seq_*` symbol names and the AOT runtime in agreement.
4. If the manuscript has no `reserve`, add the smallest §13 change for `s.reserve(n)` (total, a capacity hint only) and its reference semantics.
5. Leak and double-free tests with the live-allocation counter for create, grow, free, move into a handle and drop.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each repro named above is re-measured (instructions in the emitted function, or `objdump`) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks, with local port rewrites where the lane adds a builtin; the report gives each ratio before and after with the setup. Report only what you measured.
- Report: docs/parallel-results/port-gaps-seq-runtime.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
