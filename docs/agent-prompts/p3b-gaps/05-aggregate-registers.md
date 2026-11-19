# Copy-struct locals in registers

> P3b gaps wave, lane 5. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/aggregate-registers`. Claim issue: #1090.

The port gaps wave put scalar locals in registers (#972, `register_locals`) and wrote struct literals in place (#1022). Three aggregate costs remain:

1. **Copy-struct locals in stack slots** (port-p3-ebpf.md gap 1): `scalar_normalize` keeps its `mut s: Scalar` (six words with the nested `Tnum`) in memory and reloads each field per use: about 450 instructions per call where Rust spends about 150, 64% of the port's scalar instructions. Repro: `vesper/src/scalar.ves` `scalar_normalize` in `crates/vesper-ebpf`, `objdump -d` of its `vesper_function_*`. The loop-owned-boxes report notes the same for `Box` locals.
2. **Aggregate results built in a temporary and copied** (ebpf gap 2): every `Option[Scalar]` return writes seven words to a stack temporary and then copies them to the result pointer.
3. **Field reads through a sequence element copy the element** (port-p3-silicon.md gap 4): `match a.hdr.try_get(d)` copies all 21 words of an `AplicHdr` to read `sources`; a field read through the sequence should be one load.

Ports to re-measure: eBPF (`scalar_normalize` 64% of scalar workload), silicon, BBR, Quarry, Kinetic.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3-ebpf.md ("Compiler gaps" 1-2), port-p3-silicon.md (gap 4), port-p3b-loop-owned-boxes.md, bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1090 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1090", or "Part of #1090" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1090. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Copy-struct locals in registers.

Exclusive ownership:
- the aggregate-local home and result hunks of src/native.rs (not the slice-header hunk, which lane 3 owns)
- src/native_aggregates.rs
- the element field-read hunk of src/seq/native.rs
- tests/native_sroa*.rs, tests/native_result_in_place*.rs, tests/native_seq_field_reads*.rs
- docs/parallel-results/p3b-gaps-aggregate-registers.md

Implement:
1. Scalar replacement for unaddressed copy-struct locals (including nested copy structs and `Option`/enum payloads of them) on the native optimize path: each field word becomes a Cranelift variable; spill only when the local is borrowed as a whole or passed by address.
2. Write aggregate results (`Option[T]`, `Result[T, E]`, structs) straight into the caller's result pointer when the returned value is built in the returning block.
3. Lower `s.try_get(i)` / `s[i]` followed only by field reads (or a match that binds only some fields) to per-field loads from the element.
4. Native optimize path only; no manuscript change.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- The eBPF `scalar_normalize` instructions per call and the silicon header read are measured before and after; eBPF (scalar workload), silicon (`aia-intc`) and BBR are re-measured with their committed ABBA benchmarks.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-aggregate-registers.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
