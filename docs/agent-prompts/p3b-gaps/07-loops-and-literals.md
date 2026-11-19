# continue and literal typing

> P3b gaps wave, lane 7. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/loops-and-literals`. Claim issue: #1092.

The most-repeated ergonomic gaps in the P3 reports; each port worked around them by hand:

- **No `while`** (port-p3b-kinetic.md gap 3, port-p3-ebpf.md gap 8): ports write `loop { if !(cond) { break; } .. }`. **Out of scope, owner decision 2026-11-18:** the manuscript keeps `while` out of the core (§7.6: the library defines it with `sugar`), and the owner chose to wait for `sugar` rather than add a core form. Do not add `while`.
- **No `continue`** (port-p3-pact.md gap 2, ebpf gap 8, loom audit): `E0201 unknown name 'continue'`.
- **Integer literals default to `i64`** (kinetic gap 2, prism-h3 gap 3, chorus gap 4, pact gap 4, bench gap 4): `for a in 0..3 { s.try_get(a) }` with `s: Slice[u64]` fails `E0200 expected u64, found i64`; so does `let x = if c { 1 } else { 2 }; x` returning `u64`. Literals should take their type from the use (the range's use as an index or argument, the other branch, the annotated result), defaulting to `i64` only when nothing constrains them.
- **`?` works on `Result` only** (port-p3-aegis.md gap 5): `num(in s, 0, 4)?` in an `Option`-returning function is E0200.
- **No `wrapping_neg`** (ebpf gap 8): ports write `wrapping_sub(0, x)`.

Ports to re-measure: every P3 port (Kinetic, Prism H3, Chorus, Pact, Bench, eBPF, Loom audit, Aegis).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-kinetic.md (gaps 2-3), port-p3b-prism-h3.md (gap 3), port-p3b-chorus.md (gap 4), port-p3-pact.md (gaps 2, 4), port-p3-bench.md (gap 4), port-p3-ebpf.md (gap 8), port-p3-aegis.md (gap 5), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1092 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1092", or "Part of #1092" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1092. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: continue and literal typing.

Exclusive ownership:
- the loop and continue hunks of crates/vesper-syntax/src/parser.rs, their HIR lowering and vesper fmt/highlighting support
- the integer-literal inference and ? hunks of src/check.rs and src/integer_widths/check.rs
- the wrapping_neg builtin wherever the other wrapping builtins live
- tests/continue*.rs, tests/literal_typing*.rs, tests/try_option*.rs
- the manuscript hunks for these forms (§2.2 grammar)
- docs/parallel-results/p3b-gaps-loops-and-literals.md

Implement:
1. `continue` in `loop` and `for`, with labels if `break` has them, with `vesper fmt` and highlighting support.
2. Integer literal typing from the use: range bounds unify with the loop variable's uses, `if`/`match` branches unify, a `let` without annotation takes the type of its later uses within the function; keep `i64` as the default when unconstrained. Programs that check today must keep their meaning (differential tests over the existing test corpus).
3. `?` on `Option[T]` in a function returning `Option[U]`.
4. `wrapping_neg` for every integer width (suffix names like the other bit builtins).
5. Write the manuscript changes and flag them. The self-hosted Stage 1 checker (`packages/vesper-compiler`) is not changed; note that it stays stricter.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- Each repro above checks and runs on the evaluator, JIT and objects; the Kinetic `0..u(3)` and Prism H3 `static_count()` workarounds are removed in a local rewrite and produce identical output.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-loops-and-literals.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
