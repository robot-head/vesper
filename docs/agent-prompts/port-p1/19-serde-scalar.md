# Port the vesper-serde scalar codecs to Vesper

> Port wave P1, lane 19. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p1/serde-scalar`. Claim issue: #916.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline d523661 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md (the recipe), docs/maintenance/vesper-port-survey.md, crates/vesper-port-pilot (the worked example), and the crate's README, public API and tests.

When to start: after lanes 1, 2 and strings (#896) merge, and after wave-nine lane 31 (#608), which holds crates/vesper-serde/**, closes or releases the crate. Ask the owner before starting if #608 is still open. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #916 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #916").

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #916. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, decisions and remaining gaps.

Assignment: Port the vesper-serde scalar codecs to Vesper.

Exclusive ownership:
- crates/vesper-serde/** except src/simd/**
- docs/parallel-results/port-p1-serde-scalar.md
Dependencies: Lanes 1 and 2, #896, #608. Dependent vesper-lsp keeps its API.

Implement:
1. Port the scalar paths: JSON stage 2 (`stage2.rs`), index (`index.rs`), encoding (`encode.rs`, `helpers.rs`), floats (`float.rs`, `lemire.rs`, `pow5.rs`), NDJSON, the binary format, archives, event columns, tiles and StreamVByte's scalar codec.
2. Float decoding and encoding must round-trip and match the Rust build bit for bit (the existing float tests are the bar).
3. `src/simd/**` (stage 1 structural scanning, scalable kernels) stays Rust until source `VVec` (F11, #587): the scalar stage 1 fallback ports, and the wrapper picks the SIMD Rust path where it is used today.
4. Compiler-linked (`src/json_commands.rs`, `src/grammar_commands.rs`): seed-copy rule.

Stays in Rust: `src/simd/**` (F11); the frozen seed copy.

Acceptance:
- All 142 tests, including the JSON test suite (`tests/json_test_suite.rs`), pass against both builds.
- ABBA benchmarks for JSON parse and float decode against the scalar Rust path, with the 2x stop rule.
- Report: docs/parallel-results/port-p1-serde-scalar.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
