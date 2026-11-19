# Fixed-size arrays and constant tables

> P3b gaps wave, lane 6. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/fixed-arrays`. Claim issue: #1091.

Four reports ask for the same feature:

- port-p3-bench.md gap 2: `fn f() -> [u64; 4] { [0; 4] }` gives `E0002 expected RBracket, found Semi`. Every scratch table (rank sums up to 528, breakpoints, Walsh averages) is a heap `ChunkSeq` read through `try_get`/`try_set`.
- port-p3b-warp.md gap 3: ACK ranges live in Rust (`[AckRange; N]` lent as a slice) and warp-tcp's four SACK slots are `b0..b3` fields with `if` ladders.
- port-p3b-prism-h3.md gap 6: the Huffman, trie and static tables are `ChunkSeq`s built at run time, every read a checked `try_get`.
- port-p3-aegis.md gap 3: returning `[u8; N]` fields in a v3 mirror would let the port write the report structs in place (a quarter of the remaining instructions are boundary malloc/free/memcpy).
- port-p3b-chorus.md gap 3: no module constants.

Ports to re-measure: Bench, Warp (ACK ranges), Prism H3 (Huffman/static tables), Aegis (`[u8; N]` fields), Chorus (module constants).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3-bench.md (gap 2), port-p3b-warp.md (gap 3), port-p3b-prism-h3.md (gap 6), port-p3-aegis.md (gap 3), port-p3b-chorus.md (gap 3), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now (part 1); the export-mirror part after lane 2; merge after lanes 7 and 8 if both touch the parser. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1091 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1091", or "Part of #1091" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1091. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Fixed-size arrays and constant tables.

Exclusive ownership:
- new src/arrays.rs, src/arrays/**
- the array type, literal and const item hunks of crates/vesper-syntax/src/parser.rs (and the lexer only if needed)
- the array and const hunks of src/check.rs, src/layout.rs, the evaluator, and native lowering in a new src/native_arrays.rs
- the [u8; N] mirror-field hunk of src/native_export/** (part 2, after lane 2 merges)
- tests/arrays*.rs, tests/const_items*.rs
- the manuscript hunks for array types and const items
- docs/parallel-results/p3b-gaps-fixed-arrays.md

Implement:
1. Part 1: a `[T; N]` value type for copy `T` and literal `N` (copy, inline layout of `N` packed elements), literals `[x; N]` and `[a, b, c]`, indexing `a[i]` (checked with `Fail[RuntimeError]` like sequences, total when `i` is a constant below `N` or under a dominating guard), `a.set(i, v)`/`a[i] = v` on mutable places, `a.len()`, `in a[..]`/`inout a[..]` as `Slice[T]` views, arrays as struct fields and in copy structs, in the evaluator, JIT and objects.
2. Part 1: module-level `const NAME: T = expr;` for scalar, copy-struct and array types with constant initializers, placed in read-only data natively.
3. Part 2 (after lane 2): `[u8; N]` (and other copy element) fields in ABI v3 mirror structs.
4. Write the manuscript change for array types and `const` items (grammar §2.2 and the type chapter), and flag it.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- The bench `add_rank` scratch table on a stack array, a Prism H3 constant Huffman table, and a Warp ACK-range array are measured in local rewrites against their committed ABBA benchmarks.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-fixed-arrays.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
