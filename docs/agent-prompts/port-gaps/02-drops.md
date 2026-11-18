# Free the sequences that native drops still leak

> Port gaps wave, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/drops`. Claim issue: #985.

Ports had to restructure code to avoid native leaks their leak tests caught. A dropped local of a non-copy struct with `ChunkSeq` fields, and a sequence field replaced through `inout` (`h.s = ChunkSeq::with_capacity[u64](8)`), never free the old sequence (Aura gap 4). The elements of a dropped `ChunkSeq` of sequences, strings or boxes, and temporaries never bound (`f().len()`), also leak. The `let`-bound local struct case (Quarry gap 2) is the #972 follow-up; coordinate with it and do not duplicate it.

Ports to re-measure: Aura, Quarry (leak tests; timing only if a workaround is undone).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend gap that the port parity wave of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-gaps/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-parity.md (the scoreboard), the port reports it cites for this lane (port-parity-aura.md gap 4, port-parity-quarry.md gap 2, stage1-perf-seq-access-drops.md "What is left"), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now for the drop elaboration in src/fip.rs; native drop-hook hunks in src/native.rs after PR #983 merges. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #985 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all #972 and PR #983); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #985").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 9 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #985. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Free the sequences that native drops still leak.

Exclusive ownership:
- the drop elaboration in src/fip.rs
- the native drop hooks in src/native.rs (only the drop hunks, after PR #983 merges) and src/seq/native.rs drop hunks
- tests/native_drop*.rs
- docs/parallel-results/port-gaps-drops.md

Implement:
1. Reproduce each shape with a leak test against the live-allocation counter: dropped non-copy local handle, field replacement through `inout`, `ChunkSeq[ChunkSeq[u64]]` and `ChunkSeq[String]` dropped, unbound temporaries, destructured bindings.
2. Fix each in the drop elaboration so the interpreter, the JIT and AOT native agree. A drop is a correctness fix, so it runs with `optimize` off too; show that the Stage 1 lock corpus does not change (none of its programs has sequences).
3. With the fixes in, check that Aura's and Quarry's workarounds (`free_d`, building exactly the handle returned) can be undone in a local experiment, and report it; lane 9 commits port changes.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each repro named above is re-measured (instructions in the emitted function, or `objdump`) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks, with local port rewrites where the lane adds a builtin; the report gives each ratio before and after with the setup. Report only what you measured.
- Report: docs/parallel-results/port-gaps-drops.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
