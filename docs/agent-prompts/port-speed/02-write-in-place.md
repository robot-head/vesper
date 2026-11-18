# Write destinations and block-parameter values built in place

> Port speed wave, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-speed/write-in-place`. Claim issue: #1022.

`q.f = T { .. }` is built in a temporary and copied, and a value that reaches its consumer through a block parameter (a callee inlined at IR level whose `return` becomes a jump to a join block, or an `if` that picks between two constructors) gets no home (port-gaps-codegen-redundancy.md, "What is left"). BBR's four hottest Vesper functions are 48% of its instructions (port-gaps-rescore.md).

Ports to re-measure: BBR, Ignis, Quarry.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the port gaps re-score (#992) of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-speed/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-gaps-rescore.md (the latest scoreboard), port-gaps-codegen-redundancy.md ("Repros", "What is left") and port-parity-bbr.md gap 2, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1022 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1022").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-speed/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port but Aura and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 8 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1022. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Write destinations and block-parameter values built in place.

Exclusive ownership:
- the `plan()` / home-assignment and `Write` hunks of src/native.rs (aggregate destinations)
- tests/native_write_in_place*.rs, tests/native_block_param_homes*.rs
- docs/parallel-results/port-speed-write-in-place.md

Implement:
1. Write repros: `q.f = T { a, b, c }` through `inout q`, an `if` between two variant constructors bound to a local, and an IR-inlined callee returning a struct. Count the instructions in each emitted function on main.
2. Build a `Write` destination in place when no operand reads the destination after the first in-place store; otherwise keep the temporary. Prove the rule in the code with a comment and test both sides.
3. Extend `plan()` to single-use block parameters so the value is built in its final home.
4. Keep a failing operand (`Fail` between stores) from leaving a half-written destination visible: a destination observable after the fault keeps the temporary.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- The repros lose their copies; the evaluator, the JIT with optimize on and off and an executable agree on every test.
- Each repro is measured (instructions in the emitted function, or callgrind per call) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks and callgrind, with the setup of port-gaps-rescore.md and local port rewrites where the lane adds a builtin; the report gives each ratio before and after. Report only what you measured.
- Report: docs/parallel-results/port-speed-write-in-place.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
