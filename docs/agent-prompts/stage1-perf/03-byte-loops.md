# Block copy, wide reads and bounds-check hoisting for byte loops

> Stage 1 performance wave, lane 3. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `stage1-perf/byte-loops`. Claim issue: #933.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend performance gap found by port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 943d901 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/stage1-perf/README.md (the wave plan, why the work fits in Stage 1, and the rules for every lane), the port reports docs/parallel-results/port-p1-*.md (on main or on PRs #924-#930), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: the bounds-check hoisting and the 64x64->128 multiply now; the new sequence builtins after lane 2 (#932) merges, since both edit src/seq/native.rs. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #933 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #933").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/stage1-perf/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. Measure ports; do not push to port branches.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #933. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the port ratios before and after, decisions and remaining gaps.

Assignment: Block copy, wide reads and bounds-check hoisting for byte loops.

Exclusive ownership:
- src/seq/check.rs, src/seq/eval.rs, src/seq/lean.rs (typing and reference semantics of the new builtins)
- the new-builtin hunks in src/seq/native.rs after lane 2 merges
- NEW src/bounds_hoist.rs (a native-path pass run only under `optimize`)
- the wide-multiply builtin: its typing where the `wrapping_*` builtins are typed (src/check.rs, src/integer_widths/**; only that hunk) and its interpreter and native lowerings
- packages/vesper-std/bytes/** (switch to the new builtins, keep the API)
- tests/native_bytes*.rs
- docs/parallel-results/stage1-perf-byte-loops.md
Dependencies: Lane 2 (#932) for src/seq/native.rs. Lane 6 builds on this lane's builtins.

Implement:
1. Measure first: warp-tcp (#927, `examples/abba.rs`, codec and transfer) and Prism (#926): record the cost of payload copies, checksum loops and header reads.
2. Builtins (manuscript §13, smallest change): `copy_from(inout dst: Slice[T], src: in Slice[T])` and `ChunkSeq::extend_from_slice` for copy `T`, `fill`, and `read_u16_le/be`, `read_u32_le/be`, `read_u64_le/be` with matching `write_*` on `Slice[u8]`/`ChunkSeq[u8]`. One bounds check each, `Fail[RuntimeError]` on a short buffer, as today's per-byte code would raise. Lower copies to `memcpy`/`memmove` (overlap rules from the spec) and wide reads to unaligned loads plus a byte swap where needed. Interpreter semantics in src/seq/eval.rs; Lean reference semantics in src/seq/lean.rs if the module models sequences.
3. Bounds-check hoisting: in a `for i in a..b` loop whose body indexes a sequence or slice that the loop does not resize, replace the per-iteration checks with one check of `b <= len` before the loop (and keep the exact failure point: if the hoisted check fails, run the original checked loop so the fault happens at the same iteration with the same prior effects).
4. A `mul_wide(u64, u64) -> (u64, u64)` builtin (BBR's 64x64->128 multiply), lowered to Cranelift `umulhi`/`imul`.
5. Switch `@vesper-std/bytes` to the builtins; keep its API and tests.
6. Tests: differential interpreter/JIT/AOT tests, short-buffer faults at the exact element, overlap cases, the hoisting fallback path.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- warp-tcp, Prism and BBR are re-measured with their hot loops rewritten to the builtins in a local experiment (do not push to port branches); the report gives each ratio before and after.
- Report: docs/parallel-results/stage1-perf-byte-loops.md, with what changed, the Stage 1 guard evidence, the port ratios before and after with the setup, and what is left.
~~~
