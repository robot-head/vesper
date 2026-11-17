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
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the self-hosted corpus skip predicate in `fixtures()` in tests/fixtures/self_host/driver.rs) and, for a compiler-linked crate, the dependency switch lane 2 documents: seed package for every crate inside the compiler closure, port package outside it (own commit). Other P1 lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
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
- A narrow hunk in src/lib_commands.rs (lane 1 owns the file) so `vesper build --lib` accepts a package directory (own commit; merge after lane 1 or coordinate on #898)
- scripts/test-port-harness.sh
- the port-harness steps in .github/workflows/*.yml (own commit)
- docs/maintenance/porting-a-crate.md: the build-script and CI sections, and a new section "Crates the compiler links"
- docs/parallel-results/port-p1-cycle-free-port-builds.md
Dependencies: Merged: #879. No other lane.

Implement:
1. Reproduce the problem: on main, adding the `vesper-port` feature to `crates/vesper-beacon` fails `cargo check -p vesper-beacon --features vesper-port` with `error: cyclic package dependency` (vesper -> vesper-beacon -> [build] vesper-port-harness -> vesper). The ten affected crates are vesper-alloc, vesper-beacon, vesper-crucible, vesper-grammar, vesper-loom-calculus, vesper-loom-solver, vesper-semver, vesper-serde, vesper-simpulse and vesper-tensor (from `cargo tree -p vesper -e normal,build`).
2. Give the harness a build mode with no dependency on the `vesper` library: it runs a `vesper` binary named by `VESPER_PORT_COMPILER` (or found by a documented lookup) to compile the port, write the interface and the binding, and check the committed binding. Keep the current library mode for crates the compiler does not link, or replace it if one mode is simpler; the pilot must keep working.
3. CI: build `vesper` first in the macOS and Windows jobs and pass it to `scripts/test-port-harness.sh`. Bazel already passes `//:bin/vesper`; check that a port of a compiler-linked crate builds there without a cycle (Bazel targets: the port `rust_library` must not be a dependency of `//:vesper`).
4. Package inputs: every port is a multi-file Loom package with path dependencies (plan README, "Where the port lives"), but `vesper_port_object` takes one `.ves` file and declares only it as an action input, and the Cargo harness reads one file through `compile_source`. Make both accept a package directory: `vesper build --lib <dir>` through `packages::load`, the Bazel rule taking the manifest, all `src/**/*.ves` and the transitive path dependencies as inputs, and the harness emitting `cargo:rerun-if-changed` for each. Test with a multi-file package that imports a path-dependent package, through both Cargo and Bazel.
5. Seed-copy rule: a Cargo feature cannot keep the compiler on Rust, because resolver 2 unifies features per package across the whole build: if any crate in the compiler's dependency closure turned on `vesper-port` for a shared crate, the compiler would get the port too. Use separate package identities instead: the frozen Rust moves to a seed package (for example `crates/<crate>/seed/`, package `<crate>-seed`, same library name), and every crate inside `cargo tree -p vesper -e normal,build` depends on the seed through `package = "<crate>-seed"`. The original package becomes the port-backed build, used only by crates outside that closure. Demonstrate on `crates/vesper-beacon` with a stub port of one function: `//:vesper` links the seed, and `vesper-beacon-arrow` (outside the closure) links the port, in Cargo and Bazel. Write the rule into the recipe.
6. Add a CI guard that fails if any port-backed package appears in `cargo tree -p vesper -e normal,build` (or the Bazel deps of `//:vesper`), and that `vesper-codegen-verified` links no Vesper-built code at all.

Stays in Rust: Nothing to port in this lane.

Acceptance:
- The beacon stub port builds and runs its tests on Linux, macOS and Windows while `//:vesper` and `cargo build -p vesper` still link the seed, and the pilot still passes.
- The multi-file package test passes through Cargo and Bazel.
- The guard fails on a deliberate violation (show it in a test or the report).
- The recipe explains both build modes and the seed-copy rule.
- Report: docs/parallel-results/port-p1-cycle-free-port-builds.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
