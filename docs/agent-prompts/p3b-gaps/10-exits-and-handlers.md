# Drops on every exit and handler fixes

> P3b gaps wave, lane 10. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/exits-and-handlers`. Claim issue: #1095.

Correctness bugs, each with a repro. Merge each part as soon as it is green (four PRs are fine).

1. **Bit builtin on a reassigned `let mut`** (port-p3b-kinetic.md gap 1, port-p3b-prism-h3.md gap 1): `pub fn f(v: u32) -> u32 { let mut x = v; x = wrapping_shl_u32(x, 16); x }` under `vesper build --lib` fails `optimized IR failed ownership validation: E0410 IR access to '$slot1_x' overlaps active loan '$slot1_x'`; also `acc = wrapping_shl_u64(acc, 5)` in a loop. `wrapping_add` in the same position is fine. Likely `integer_widths::lower_bit_builtin_calls` (#988).
2. **A resuming handler around a loop hangs natively** (port-p3b-dyn-effects-closures.md): `handle Net in { for i in 0..3 { Net::emit(1) } }`, on `main` and without `dyn`.
3. **Box drops on `Fail` exits and the specialized `sink` leak** (port-p3b-loop-owned-boxes.md "Remaining gaps", dyn-effects-closures): a box owned at a failing call is not freed (sequences already drop on that path, `seq_drop_exit`); a handler-specialized (inlined) callee does not drop a `sink` parameter of another type it leaves unconsumed (a `ChunkSeq`), and leaks it natively when called statically too. Waits for #1076 (`src/fip.rs`).
4. **Affine extern-type drop imports** (port-p3b-bridge-handles.md, flagged decision): dropping an `affine extern type` handle calls nothing; Warp's frame pool and sessions want a declared drop import. Waits for #1077.

Ports to re-measure: Kinetic, Prism H3 (E0410), Chorus, Warp, Kinetic (boxes), Flux/Chorus (handlers), Warp (affine handles).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-kinetic.md (gap 1), port-p3b-prism-h3.md (gap 1), port-p3b-dyn-effects-closures.md ("Gaps found"), port-p3b-loop-owned-boxes.md ("Remaining gaps"), port-p3b-bridge-handles.md ("Flagged decision", "What the Warp port needs"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: parts 1-2 now; part 3 after #1076 merges; part 4 after #1077 merges. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1095 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1095", or "Part of #1095" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1095. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Drops on every exit and handler fixes.

Exclusive ownership:
- the bit-builtin rewrite hunk of src/integer_widths.rs (lower_bit_builtin_calls)
- the loop/resume hunks of src/native_handlers.rs, src/native_handlers/**
- the Fail-exit and specialized-callee sink hunks of src/fip.rs (after #1076)
- the affine drop-import hunks of src/ffi_lowering/**, src/fip.rs and the evaluator (after #1077)
- tests/bit_builtin_mut*.rs, tests/native_handler_loops*.rs, tests/native_fail_exit_drops*.rs, tests/native_specialized_sink*.rs, tests/ffi_affine_drop*.rs
- the §16.3 affine-handle bullet of the manuscript (part 4)
- docs/parallel-results/p3b-gaps-exits-and-handlers.md

Implement:
1. Part 1: fix the E0410 validation failure for every suffix bit builtin assigned to a mutable local, with tests over each builtin.
2. Part 2: fix the native hang; the evaluator, JIT and objects must agree on the handler-around-loop programs, including `break` out of the loop and nested handlers.
3. Part 3 (after #1076): Box drops on `Fail` exits in acyclic and loop functions; drop unconsumed `sink` parameters in handler-specialized callees; leak tests with the counting allocator.
4. Part 4 (after #1077): an `affine extern type` may declare a drop import that runs when the handle is dropped (native drop plan and evaluator); update the §16.3 bullet and flag it.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- Each repro passes on the evaluator, JIT (optimize on and off) and objects; leak tests show zero leaked boxes and sequences; the Kinetic `util::shr_u64` helpers and the Prism H3 `int.ves` shift helpers are removed in a local rewrite with identical output.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-exits-and-handlers.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
