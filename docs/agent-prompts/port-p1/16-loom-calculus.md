# Port vesper-loom-calculus to Vesper

> Port wave P1, lane 16. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p1/loom-calculus`. Claim issue: #913.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline d523661 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md (the recipe), docs/maintenance/vesper-port-survey.md, crates/vesper-port-pilot (the worked example), and the crate's README, public API and tests.

When to start: after lanes 1, 2, strings (#896) and `@vesper-std` (#897) merge. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #913 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #913").

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #913. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, decisions and remaining gaps.

Assignment: Port vesper-loom-calculus to Vesper.

Exclusive ownership:
- crates/vesper-loom-calculus/**
- docs/parallel-results/port-p1-loom-calculus.md
Dependencies: Lanes 1 and 2, #896, #897. Lane 17 waits for this lane. Dependents: vesper-loom-solver, vesper-loom-audit, vesper-loom-trust, vesper-loom-admission, vesper-codegen-verified.

Implement:
1. Port the manifest calculus: validation, capability ceilings, dependency rules and `upgrade.rs`.
2. Parsing: the crate reads TOML (`toml`) and JSON (`serde_json`). Keep decoding in the Rust wrapper for this wave, producing the existing typed manifest structs, and port everything after decoding. A Vesper TOML reader is a follow-up; file an issue for it.
3. Compiler-linked (package graph, publish, doc, sbom, LSP manifests): seed-copy rule. Every dependent (loom-solver, loom-audit, loom-trust, loom-admission, codegen-verified) is inside the compiler closure, so all stay on the seed; `vesper-codegen-verified` must never link Vesper-built code (the lane 2 guard checks it).

Stays in Rust: TOML/JSON decoding; the frozen seed copy.

Acceptance:
- All 29 tests pass against both builds.
- The lane 2 guard passes.
- Report: docs/parallel-results/port-p1-loom-calculus.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
