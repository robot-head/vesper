# Export ABI: f32, lent handles and handle collections

> P3b gaps wave, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/export-abi-widening`. Claim issue: #1087.

Four boundary gaps from the P3a/P3b reports, all in the export binding generator:

1. **No `f32`** (port-p3b-resonance.md gap 5, port-p3b-kinetic.md gap 4, known since Tensor): `pub fn f(x: f32) -> f32` in an exported package is refused; every `f32` crosses as `u32` bits and every `&[f32]` is lent through `bytemuck`.
2. **`in` handles are copied per call** (port-p3-silicon.md gap 1): the binding passes an `in` handle as a scratch copy of its words (`let mut arg_a = a.words;`), a 27-word `memcpy` for `Aplic`. Vesper never writes an `in` aggregate (E0307) and ABI v3 already lends `in` mirrors in place. Repro: `pub struct H { a: u64, s: ChunkSeq[u64] }` with `pub fn h_a(in h: H) -> u64 { h.a }` under `// @export abi 3`.
3. **No sequences of handles** (silicon gap 3): `pub struct F { files: ChunkSeq[Imsic] }` with an export taking `inout f: F` is refused ("only sequences and slices of copy elements cross the boundary"). The handle stays opaque to Rust; only its own layout has to be accepted.
4. **`Box[dyn Trait]` has no export layout** (resonance gap 4): `pub struct G { procs: ChunkSeq[Box[dyn P]] }` is refused ("type dyn P has no fixed word layout"), so a persistent graph cannot live in Vesper across calls. A box is one word inside an opaque handle; it needs a layout and drop glue, not a Rust mirror.

Ports to re-measure: silicon (23% memcpy in boundary calls), Resonance, Kinetic, Tensor, eBPF.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3-silicon.md ("Compiler and ABI gaps"), port-p3b-resonance.md (gaps 4-5), port-p3b-kinetic.md (gap 4), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1087 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1087", or "Part of #1087" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1087. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Export ABI: f32, lent handles and handle collections.

Exclusive ownership:
- src/native_export.rs, src/native_export/** (binding generators, glue, directives)
- the generated-binding runtime support crate(s) the bindings call, if any
- tests/native_export_f32*.rs, tests/native_export_lent_handles*.rs, tests/native_export_handle_seqs*.rs
- the export ABI hunk of the manuscript (§13)
- docs/parallel-results/p3b-gaps-export-abi-widening.md

Implement:
1. `f32` scalars as parameters and results (passed in float registers per the platform C ABI), and `Slice[f32]`/`ChunkSeq[f32]` lent and returned like the other copy element types, in ABI v2 and v3 bindings.
2. Lend `in` handle arguments in place in ABI v3 (no scratch copy), as `in` mirrors already are; keep `inout`/`sink` behaviour unchanged.
3. Accept handle types whose fields hold `ChunkSeq[H]` of another handle type or `Box[dyn P]` (and `ChunkSeq` of those): the handle stays opaque; dropping it from Rust runs the full drop glue. Element access from Rust stays out of scope.
4. Smallest manuscript change in the export ABI section, flagged.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- silicon's standalone AIA model calls (`aplic_mode`, `imsic_top`) are re-measured with a local rewrite that lends `in` handles and uses `ChunkSeq[Imsic]` instead of banks, and Resonance with a persistent `Box[dyn P]` graph handle, against their committed ABBA benchmarks.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-export-abi-widening.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
