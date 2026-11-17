# Cycle-free port builds for crates the compiler links, and the seed-copy rule

> Port wave P1, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p1/cycle-free-port-builds`. Claim issue: #899.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are building part of the porting kit for port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline d523661 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md (the recipe), docs/maintenance/vesper-port-survey.md, crates/vesper-port-pilot (the worked example).

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #899 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #899").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-p1/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, performance, CI, checks before pushing, merging and the report.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the tests/self_host_front_end.rs skip list) and, for a compiler-linked crate, the one-line feature switch in each non-compiler dependent's Cargo.toml (own commit). Other P1 lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/**, packages/vesper-std/** (P0 lane 8, #897) or the strings module (P0 lane 7, #896). Ask those lanes for what you need.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock; for a new crate, registration in root Cargo.toml `members` and `default-members` plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100. Files a test reads from outside its crate must be declared in the crate's BUILD.bazel. Bazel's rustfmt checks every file under src/ and ignores #[rustfmt::skip]: keep generated bindings in port/. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. "0 Alloc" claims need a counting-allocator test over the warmed-up steady state. Report only performance you measured, with the setup.
- Lean: any Lean file must compile with the pinned Lean 4.24.0 (use the GitHub release .zip), with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #899. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, decisions and remaining gaps.

Assignment: Cycle-free port builds for crates the compiler links, and the seed-copy rule.

Exclusive ownership:
- crates/vesper-port-harness/**
- NEW crates/vesper-port-build/** if you choose a new crate (registration in root Cargo.toml in its own commit, BUILD.bazel from tools/bazel/sync_build_files.py)
- tools/bazel/vesper_port.bzl
- scripts/test-port-harness.sh
- the port-harness steps in .github/workflows/*.yml (own commit)
- docs/maintenance/porting-a-crate.md: the build-script and CI sections, and a new section "Crates the compiler links"
- docs/parallel-results/port-p1-cycle-free-port-builds.md
Dependencies: Merged: #879. No other lane.

Implement:
1. Reproduce the problem: on main, adding the `vesper-port` feature to `crates/vesper-beacon` fails `cargo check -p vesper-beacon --features vesper-port` with `error: cyclic package dependency` (vesper -> vesper-beacon -> [build] vesper-port-harness -> vesper). The ten affected crates are vesper-alloc, vesper-beacon, vesper-crucible, vesper-grammar, vesper-loom-calculus, vesper-loom-solver, vesper-semver, vesper-serde, vesper-simpulse and vesper-tensor (from `cargo tree -p vesper -e normal,build`).
2. Give the harness a build mode with no dependency on the `vesper` library: it runs a `vesper` binary named by `VESPER_PORT_COMPILER` (or found by a documented lookup) to compile the port, write the interface and the binding, and check the committed binding. Keep the current library mode for crates the compiler does not link, or replace it if one mode is simpler; the pilot must keep working.
3. CI: build `vesper` first in the macOS and Windows jobs and pass it to `scripts/test-port-harness.sh`. Bazel already passes `//:bin/vesper`; check that a port of a compiler-linked crate builds there without a cycle (Bazel targets: the port `rust_library` must not be a dependency of `//:vesper`).
4. Seed-copy rule: write down, and demonstrate on `crates/vesper-beacon` with a stub port of one function, how a compiler-linked crate keeps two builds: the default (Rust) build that the compiler links, and the `vesper-port` build that tests and every other dependent use. Show how a non-compiler dependent (for example `vesper-beacon-arrow`) opts into the port build in Cargo and Bazel without pulling the port into `//:vesper`.
5. Add a CI guard that fails if a compiler-linked crate's port build ever becomes a dependency of the root `vesper` package.

Stays in Rust: Nothing to port in this lane.

Acceptance:
- `cargo test -p vesper-beacon --features vesper-port` builds and runs the stub port on Linux, macOS and Windows, and the pilot still passes.
- The guard fails on a deliberate violation (show it in a test or the report).
- The recipe explains both build modes and the seed-copy rule.
- Report: docs/parallel-results/port-p1-cycle-free-port-builds.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
