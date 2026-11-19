# std sort and bulk sequence operations

> P3b gaps wave, lane 11. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/std-sort-and-bulk`. Claim issue: #1096.

Library and builtin gaps:

- **No sort in `@vesper-std`** (port-p3b-kinetic.md gap 4): Kinetic carries heap sorts in `util.ves` (`sort_by_key`, `sort_pairs`, `sort_u64`). Pact's and Loom audit's tie-break orders need the same.
- **Std candidates** the ports keep privately: `saturating_add_u64`/`sat_mul` (Warp `varint.ves`, Chorus), `all_zero` and constant-time `ct_eq` (Aegis), total slice getters (Kinetic).
- **No bulk fill or `resize` for `ChunkSeq`, and `extend` cannot read from the sequence it writes** (port-p3-ebpf.md part 4): the interpreter's per-run buffers took 8,704 pushes before a workaround.
- **Byte-at-a-time reversed pushes** (port-p3-aegis.md gap 2): `for i in 0..48 { d.push(..) }` reversing a signature component; an `extend_reversed` or byte-swap builtin removes it.
- **`find_not_in` is slow without SSSE3** (aegis gap 1): over the SNP report's ~600 reserved bytes it cost more than the rest of the parse (69,076 vs 35,063 instructions with a `try_read_u64_le` OR-fold). The fallback should be word-at-a-time.

Ports to re-measure: Kinetic (sorts), eBPF (interpreter setup), Aegis (2x on SNP), Warp, Chorus, Prism H3.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-kinetic.md (gap 4), port-p3-ebpf.md (part 4 gaps), port-p3-aegis.md (gaps 1-2), port-p3b-warp.md (gap 4), port-p3b-chorus.md, bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1096 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1096", or "Part of #1096" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1096. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: std sort and bulk sequence operations.

Exclusive ownership:
- packages/vesper-std/** (new sort module and helpers)
- the new ChunkSeq builtins in src/seq/check.rs, src/seq/eval.rs and their native lowering in src/seq/native*.rs (not the element field-read hunk, which lane 5 owns)
- the find_not_in fallback hunk of src/seq/native_bytes.rs
- tests/std_sort*.rs, tests/seq_bulk*.rs, tests/native_find_not_in*.rs
- the §13 builtin list and the std catalog hunks of the manuscript
- docs/parallel-results/p3b-gaps-std-sort-and-bulk.md

Implement:
1. `@vesper-std` sort module: in-place unstable sort (pattern-defeating quicksort or introsort) and stable merge sort over `inout Slice[T]` for integer and float element types, `sort_by_key` over a key function or a parallel key slice, and `is_sorted`; all total and allocation-free except the stable sort's scratch.
2. std helpers: `saturating_add/sub/mul` for each width (builtins if the bit-builtin pattern fits better), `all_zero(in Slice[u8])`, constant-time `ct_eq` under `ConstTime`.
3. `ChunkSeq` builtins: `resize(n, v)`, `fill(lo, hi, v)` on sequences and slices, `extend_from_within(lo, hi)`, and `extend_reversed(in Slice[T])`; one growth per call.
4. A word-at-a-time `find_not_in`/`find_in` fallback when SSSE3 is missing, with the same results.
5. Manuscript entries for the new builtins and std items, flagged.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- Sorts are tested against Rust's `sort_unstable`/`sort` on random inputs (equal results, stable order for the stable sort) and benchmarked against them; Kinetic's heap sorts, the eBPF buffer setup and the Aegis reversed copy are replaced in local rewrites and re-measured with their committed ABBA benchmarks.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-std-sort-and-bulk.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
