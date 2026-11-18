# Disjoint field paths in E0301, `copy_within`, `swap`/`replace` through `inout`, and two-phase borrows

> Port gaps wave, lane 3. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/in-place-borrows`. Claim issue: #986.

`q.a.extend(in q.b[0..1])` is refused with E0301 although `a` and `b` are different fields; call arguments already treat distinct field paths as disjoint (Quarry gap 3). `s.copy_from(0, in s[1..n])` is refused, so inserts and removes shift element by element (Aura gap 3). A non-copy field cannot be moved, swapped or replaced through `inout` (E0305), so Aura's recorder copies its whole staged state on every commit (Aura gap 2). And a receiver loan blocks reading the same sequence in the call's arguments (E0410, Aura gap 5): `s.try_set(i, wrapping_add(get(in s, i), 1))` needs a `let` first.

Ports to re-measure: Aura (insert/remove shifts, state publish), Quarry (code only).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend gap that the port parity wave of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-gaps/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-parity.md (the scoreboard), the port reports it cites for this lane (port-parity-quarry.md gap 3, port-parity-aura.md gaps 2-3, port-parity-ignis.md gap 3), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #986 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all #972 and PR #983); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #986").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 9 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #986. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Disjoint field paths in E0301, `copy_within`, `swap`/`replace` through `inout`, and two-phase borrows.

Exclusive ownership:
- the E0301 checks for builtin receivers and their sources in src/seq/check.rs and src/strings/check.rs (the field-path disjointness test)
- the E0305 rule for `swap`/`replace` in src/check.rs (only that hunk)
- `copy_within`, `swap` and `replace`: typing in src/seq/check.rs, reference semantics in src/seq/eval.rs and src/runtime.rs, native lowering in src/seq/native_bytes.rs (a `memmove`)
- the manuscript §13 hunks for the new builtins
- the E0410 receiver-loan rule in src/ir_check.rs (two-phase reservation) and its manuscript hunk
- tests/borrow_paths*.rs, tests/two_phase_borrows*.rs, tests/native_copy_within*.rs
- docs/parallel-results/port-gaps-in-place-borrows.md

Implement:
1. Treat distinct field paths as disjoint for builtin receivers and sources, with the same rule call arguments use. This only accepts programs that were refused; the self-hosted Stage 1 checker (packages/vesper-compiler) may stay stricter, never more permissive, and this lane does not edit it.
2. Add `s.copy_within(from: Interval, to: u64)` (total `try_` form plus the checked form, `memmove` semantics) to §13.
3. Add `swap(inout a: T, inout b: T)` and `replace(inout a: T, v: T) -> T` for non-copy `T` (or the field forms the manuscript prefers), so a staged state can be published without a copy. Write the smallest manuscript change and flag it.
4. Differential tests through the interpreter, JIT and AOT native; leak tests for `swap`/`replace` of sequences.
5. Two-phase borrows (owner approved 2026-11-18): a mutable receiver loan of a method call or builtin is reserved while its arguments are evaluated and activated only at the call, so arguments may read (not write, move or borrow mutably) the receiver's place. `s.try_set(i, f(get(in s, i)))` is accepted; `s.push(s.pop())`-style writes in arguments stay refused. Write the rule into the manuscript's borrow section, flag it in the PR, and keep the interpreter, JIT, AOT native and Lean extraction in agreement (the argument reads see the value before the call). The self-hosted checker may stay stricter. Tests: accepted and refused shapes, and differential results through all paths.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each repro named above is re-measured (instructions in the emitted function, or `objdump`) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks, with local port rewrites where the lane adds a builtin; the report gives each ratio before and after with the setup. Report only what you measured.
- Report: docs/parallel-results/port-gaps-in-place-borrows.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
