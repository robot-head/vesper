# Own boxed objects in functions with loops

> Port wave P3b, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/loop-owned-boxes`. Claim issue: #1064.

The Box drop elaboration needs an acyclic control-flow graph, so a function with loops or early exits that owns a box is refused: a box-typed `let`, a `sink` box parameter, or a local struct owning a box (`fip::loop_boxes_reclaimed`, PR #1060). Ports work around it by moving each loop into a function that only borrows, which does not fit drivers that own their transport and loop on it (Chorus sessions and Raft nodes, Warp's endpoint with its CID source, Kinetic's world with a stored pair backend). PR #1060 also left a native refusal of a `sink` lend to a handler-specialized (inlined) callee.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are adding a compiler feature that the port wave P3b plan of https://github.com/robot-head/vesper needs for the next crates to port.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan), "Rules for every lane" in docs/agent-prompts/port-speed/README.md (the compiler-lane rules and the Stage 1 guard), docs/parallel-results/boxed-dyn-trait.md ("Loops" and "What the P3b ports still need"), the manuscript's Box and drop items in §7.3 and the linearity rules for loops (`E0302`, `E0312`), and src/fip.rs with src/fip/** (the Box drop elaboration).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1064 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1064").

Delivery and coordination:
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch). The verified backend and WebAssembly refuse the new construct with a typed diagnostic.
- Edit only the exclusive paths below. The port speed wave (#1021-#1028) owns the native backend's performance hunks and the port lanes of this wave own their crates; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port but Aura. Do not change any port's default build or delete Rust code. You may try the feature in a port locally; do not commit port changes.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1064. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the manuscript change, the Stage 1 guard evidence, tests, the benchmark, decisions and remaining gaps.

Assignment: Own boxed objects in functions with loops.

Exclusive ownership:
- the Box drop elaboration hunks of src/fip.rs and src/fip/** (`loop_boxes_reclaimed`) and the loop-ownership checks they feed in src/check.rs
- the `sink`-lend-to-specialized-callee refusal hunk of src/native.rs and src/native_handlers.rs
- tests/loop_boxes*.rs, tests/fixtures/loop_boxes/**
- the manuscript's Box ownership text for loops and early exits, and docs/parallel-results/port-p3b-loop-owned-boxes.md

Dependencies: None. Lane 1 (#1063) edits `dyn` dispatch nearby: whichever merges second merges main first.

Implement:
1. Specify the rule in the smallest manuscript change: a box-typed value owned across a loop back edge or an early exit is dropped exactly once on every path, with the existing `E0302`/`E0312` rules for values a loop moves.
2. Elaborate Box drops over cyclic graphs (drop flags or per-edge drops, your choice; justify it in the report) for box-typed locals, `sink` box parameters, boxes reassigned inside a loop, and local structs and enums owning boxes, including `Box[dyn Trait]`.
3. Lift the native refusal of a `sink` lend to a handler-specialized callee, or narrow it to the cases that still need it, with a fixture for each.
4. Plain `Box[T]` and `Box[dyn Trait]` take the same path; the old acyclic fast path may stay for functions without loops.
5. Tests: a loop that owns a `Box[dyn Trait]` and calls it every iteration, a reassignment inside the loop, `break`/`return` with a live box, a struct owning a box in a loop, the leak test (zero leaks under a counting allocator) and the evaluator agreeing with the JIT.
6. Measure the cost of the elaboration on a loop that owns a box against the borrow-only workaround.

Stays in Rust: Nothing; this is new compiler work.

Acceptance:
- Functions with loops and early exits own boxes in the native backend and the JIT on the four required checks, with zero leaks and no double drops.
- Every remaining refusal is a typed diagnostic with a fixture.
- Stage 1 lock reproduces unchanged.
- Report: docs/parallel-results/port-p3b-loop-owned-boxes.md with the manuscript change, the elaboration design, the measurement, and which P3b ports it unblocks.
~~~
