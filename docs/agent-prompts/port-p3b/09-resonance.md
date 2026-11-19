# Port the vesper-resonance DSP core

> Port wave P3b, lane 9. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/resonance`. Claim issue: #1071.

`vesper-resonance` is realtime audio: DSP kernels, a latency-compensated processor graph (`pdc.rs`, a `Vec<Box<dyn DspProcessor>>` run every block), events, a slab, leases, a confinement checker for realtime effects, denormal control, lock-free SPSC rings and a Cranelift JIT. 4.1k lines and 80 tests, including realtime no-allocation tests. A loop over a sequence of boxed processors held by `inout self` already works on main (PR #1060).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P3b of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan and its rule changes), "Rules for every lane" in docs/agent-prompts/port-p1/README.md, docs/maintenance/porting-a-crate.md (the recipe, above all "Patterns for Rust APIs" and "Crates the compiler links"), docs/maintenance/vesper-port-survey.md, crates/vesper-beacon (the seed-pattern example), docs/parallel-results/port-p3-dyn-trait.md and docs/parallel-results/boxed-dyn-trait.md (what `dyn` supports today), the latest scoreboard docs/parallel-results/port-parity.md, and the crate's README, public API and tests.

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1071 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1071").

Delivery and coordination:
- Follow the P1 lane rules as changed by docs/agent-prompts/port-p3b/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, CI, checks before pushing, merging and the report.
- Standing owner rule: Rust stays the default build and is never deleted until the Vesper build matches it in both functionality and performance. Add the `vesper-port` build next to the Rust one, prove both pass, measure, and stop. Do not switch any dependent to the port and do not delete Rust code.
- Compiler-linked crate: use the seed pattern (crates/vesper-resonance/seed as `vesper-resonance-seed`; the root and every crate in `cargo tree -p vesper -e normal,build` that depends on it switch to the seed by rename), in its own commit. crates/vesper-beacon is the worked example.
- The lane may land as several PRs (part 1, part 2, ...), each green on its own and each closing nothing until the last one closes #1071.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the self-hosted corpus skip predicate) and, for seed lanes, the seed dependency switch. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/**, src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/** or packages/vesper-std/**. Put helpers you need in your own package and list them in the report as std candidates. A compiler gap is reported with a minimal repro, not fixed in this lane.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries, package registration in root Cargo.toml `members`, the regenerated Cargo.lock, BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Root patches merge one at a time: merge origin/main right before merging and regenerate the lockfile with cargo.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. Report only performance you measured, with the setup; give callgrind instruction counts next to wall clock.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1071. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, ABBA numbers, compiler gaps with repros, decisions and remaining gaps.

Assignment: Port the vesper-resonance DSP core.

Exclusive ownership:
- crates/vesper-resonance/**
- docs/parallel-results/port-p3b-resonance.md

Dependencies: None. Atomics (#1037, PR #1052) are merged.

Implement:
1. Port `kernels.rs`, `pdc.rs` (with `Box[dyn DspProcessor]` in a sequence), `events.rs`, `slab.rs`, `lease.rs` (source atomics) and `confine.rs`.
2. The realtime tests (`realtime.rs`, `alloc.rs`) must show zero allocation on the audio path for the port too; a compiler gap that allocates is a blocker reported with a repro. Realtime effects follow inlined bodies (#959).
3. Output samples compare bit for bit against the Rust build on `stability.rs` and `pdc.rs`.
4. Seed pattern: `vesper-resonance-seed` for the root and `vesper-fusion`.
5. ABBA: a 64-voice kernel block and a 32-node graph.

Stays in Rust: The Cranelift JIT (`clif.rs`), the `rtrb` SPSC rings (`spsc.rs`), denormal control registers (`denorm.rs`, behind `.vsys`), `simd.rs` intrinsics, and the frozen seed copy the compiler links.

Acceptance:
- Both builds pass `pdc.rs`, `stability.rs`, `slab.rs`, `confinement.rs`, `realtime.rs`, `alloc.rs` and the translated in-crate tests on the four required checks; `cranelift.rs`, `spsc_stress.rs`, `denorm.rs` and `simd.rs` keep passing with the Rust edges.
- Divergence check per ported module (samples bit for bit).
- Report: docs/parallel-results/port-p3b-resonance.md with the API inventory, what stays Rust, unit-test treatment, divergence checks, ABBA numbers and compiler gaps.
~~~
