# Bit-count builtins and total shifts

> Port gaps wave, lane 5. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/bit-builtins`. Claim issue: #988.

The manuscript has no `leading_zeros`, `trailing_zeros` or `count_ones`, so warp-wheel needs a de Bruijn table or a compare loop where Rust emits one `lzcnt`/`tzcnt`, and Beacon asks for `leading_zeros`. A variable shift always needs `Fail[ArithOverflow]` even behind `if s < 64`, a function with a handler never inlines, so ports write `x << s` as a rotation and a mask.

Ports to re-measure: warp-wheel, BBR.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend gap that the port parity wave of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-gaps/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-parity.md (the scoreboard), the port reports it cites for this lane (port-parity-warp-wheel.md gaps 4-5, PR #966 (Beacon) report), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. The native lowering hunks in src/native.rs wait for PR #983 if they touch its code; the typing, reference semantics and spec do not. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #988 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all #972 and PR #983); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #988").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 9 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #988. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Bit-count builtins and total shifts.

Exclusive ownership:
- the integer builtin typing in src/integer_widths/check.rs and src/check.rs (only the new builtins' hunks)
- their reference semantics in src/runtime.rs
- their native lowering hunks in src/native.rs (after PR #983 if they collide)
- the manuscript hunks for integer kinds
- tests/bit_builtins*.rs
- docs/parallel-results/port-gaps-bit-builtins.md

Implement:
1. Add `leading_zeros`, `trailing_zeros` and `count_ones` for every integer width (results `u32` or the manuscript's choice, defined at 0), and total `wrapping_shl`/`wrapping_shr` (shift amount taken modulo the width, as Rust's). Write the smallest manuscript change and flag it.
2. Lower them to single instructions where the target has them (`lzcnt`/`tzcnt`/`popcnt` with a fallback, `clz`/`rbit`/`cnt` on AArch64).
3. Optional, only if small: let a dominating `if s < width` guard make `x << s` total in the checker, so it needs no handler. If it is not small, list it as a gap.
4. Differential tests through the interpreter, JIT and AOT native for every width and edge value.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each repro named above is re-measured (instructions in the emitted function, or `objdump`) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks, with local port rewrites where the lane adds a builtin; the report gives each ratio before and after with the setup. Report only what you measured.
- Report: docs/parallel-results/port-gaps-bit-builtins.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
