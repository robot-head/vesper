# Quarry moves scratch into locals and replaces the word-stream codec

> Port parity wave, lane 7. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-parity/quarry`. Claim issue: #955.

Quarry's planner port is 3.17-3.3x slower than Rust on `examples/planner_abba.rs` after #946. The #940 report found that 15-20% of the run is the Rust-side word-stream encode/decode in `src/port.rs`, a format choice of the port: its six entry points take `&[u64]`/`&[u8]` streams and return a handle, so ABI v3 changes nothing until the port passes mirrored node slices instead. The #945 report noted the `aux*` and `bits` scratch fields can move back into locals now that local sequences are freed.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. This lane changes no language, builtin or ABI; it uses what the manuscript already specifies (§13 byte builtins and scans, the export ABI v3 directive).

You are bringing a Vesper port of https://github.com/robot-head/vesper closer to its Rust build's speed, using compiler work that has already merged.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 69c79b9 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-parity/README.md (the wave plan and the rules for every lane), the port's P1 report docs/parallel-results/port-p1-quarry-planner.md, and the Stage 1 reports docs/parallel-results/stage1-perf-*.md (above all the sections that measured this port).

Claim: GitHub issue #955 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #955").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-parity/README.md.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Keep `vesper-port` non-default. Delete no Rust code.
- Port only: edit only the exclusive paths below. Never edit src/**, crates/vesper-port-abi/**, crates/vesper-port-harness/**, crates/vesper-port-build/**, packages/**, lean/** or bootstrap/**. A gap only the compiler can close goes in your report as a compiler gap with a minimal .ves repro and its measured share of the run.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #955. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the ratio before and after each lever, the profile of what is left, the compiler gaps and whether the port meets the parity bar.

Assignment: Quarry moves scratch into locals and replaces the word-stream codec.

Exclusive ownership:
- crates/vesper-quarry/vesper/**
- crates/vesper-quarry/src/port.rs and the port-only glue it uses
- crates/vesper-quarry/examples/planner_abba.rs, the port's tests, BUILD.bazel
- Not the Rust-only modules the P1 lane kept in Rust (store/, pool.rs, wal.rs, durable.rs, device.rs, page.rs, vector*)
- docs/parallel-results/port-parity-quarry.md

Implement:
1. Re-measure `examples/planner_abba.rs` on main.
2. Move the `aux*` and `bits` scratch fields back into locals.
3. Replace the word-stream codec: under `// @export abi 3`, pass the parsed query and catalog as mirrored structs and `in` slices of mirrored nodes, so the Vesper build reads them in place. Keep the same public API; the Rust build is unchanged.
4. Profile what is left and remove what a port change can remove.
5. Re-measure after each step.

Acceptance:
- Every port test, divergence test and leak test passes with and without `--features vesper-port` on all four platforms. The default build is unchanged.
- The report gives the Vesper/Rust ratio on `main` before the lane and after each lever, measured in one ABBA run per step on the same machine, with the 95% interval and the setup.
- The encode/decode share of the run is reported before and after.
- The report names the top remaining Vesper costs from a profile, says for each whether it is a port or a compiler gap, gives a repro for each compiler gap, and says whether the port meets the parity bar in the wave plan.
- Report: docs/parallel-results/port-parity-quarry.md.
~~~
