# Export ABI v3: inout aggregates and copy-struct slices without boundary copies

> Stage 1 performance wave, lane 4. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `stage1-perf/export-abi-v3`. Claim issue: #934.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend performance gap found by port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 943d901 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/stage1-perf/README.md (the wave plan, why the work fits in Stage 1, and the rules for every lane), the port reports docs/parallel-results/port-p1-*.md (on main or on PRs #924-#930), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #934 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #934").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/stage1-perf/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. Measure ports; do not push to port branches.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #934. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the port ratios before and after, decisions and remaining gaps.

Assignment: Export ABI v3: inout aggregates and copy-struct slices without boundary copies.

Exclusive ownership:
- src/native_export.rs, src/native_export/** (lane 2 adds one hunk for by-value sequences; lane 5 later adds Fail glue)
- src/lib_commands.rs
- crates/vesper-port-abi/**, crates/vesper-port-pilot/**
- tests/native_export*.rs, tests/fixtures/native_export/**
- docs/maintenance/porting-a-crate.md (ABI sections)
- docs/parallel-results/stage1-perf-export-abi-v3.md
Dependencies: None. Lane 5 waits for this lane.

Implement:
1. Measure first: BBR (`ack_path_abba` on main) spends about 25% of its instructions copying `Bbrv3` (about 50 fields) and `RateSample` in and out around each `inout` call; Quarry spends 15-20% in encode/decode. Confirm with callgrind and record it.
2. ABI v3 (`vesper.native.export-abi/3`), additive: a v1/v2 library keeps its interface and binding byte for byte.
   - A copy struct or tuple used as `inout` or `in` gets a `#[repr(C)]` Rust mirror whose layout the generator derives from the Vesper layout (field order, widths, padding) and checks with compile-time `size_of`/`offset_of` asserts in the binding. The binding passes `&mut`/`&` to Rust storage directly; no word conversion and no scratch copy.
   - Borrowed slices of copy structs (`in Slice[T]`, `inout Slice[T]`) cross as pointer + length over Rust `&[T]`/`&mut [T]` with the same mirror, with no heap copy.
   - Where a layout cannot match (enums with payloads, bool/char validity), keep v2's conversion and say so in the interface.
3. Update the binding generator, the interface file, `vesper-port-abi`, the pilot and the recipe. Move BBR's binding to v3 in a local experiment to measure it (do not push to the port branch); a follow-up on each port adopts it.
4. Tests: layout asserts fail on a deliberate mismatch; round trips of every field width; aliasing rules (an `inout` aggregate and a borrowed slice of the same storage are refused at the binding).

Acceptance:
- Tests pass on all four platforms, v1/v2 fixtures are unchanged byte for byte, and the Stage 1 guard holds (the verified backend has no export ABI, so its digests must not move).
- BBR and Quarry are re-measured on v3; the report gives each ratio before and after.
- Report: docs/parallel-results/stage1-perf-export-abi-v3.md, with what changed, the Stage 1 guard evidence, the port ratios before and after with the setup, and what is left.
~~~
