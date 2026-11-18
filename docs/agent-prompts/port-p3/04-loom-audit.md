# Port vesper-loom-audit

> Port wave P3a, lane 4. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3/loom-audit`. Claim issue: #1039.

`vesper-loom-audit` audits a package catalog: license expressions, input records and the audit report (973 lines, 16 tests in `tests/audit.rs`). It imports `vesper-loom-calculus`, which P1 lane 16 ported (`@vesper/loom-calculus`), uses `semver` and `serde_json` for input, and BLAKE3 for digests. It is the smallest lane of the wave and the first to merge.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P3a of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 3c075a4 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3/README.md (the wave plan and its rule changes), "Rules for every lane" in docs/agent-prompts/port-p1/README.md, docs/maintenance/porting-a-crate.md (the recipe, above all "Patterns for Rust APIs" and "Crates the compiler links"), docs/maintenance/vesper-port-survey.md, crates/vesper-beacon (the seed-pattern example), the latest scoreboard docs/parallel-results/port-parity.md, and the crate's README, public API and tests.

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1039 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1039").

Delivery and coordination:
- Follow the P1 lane rules as changed by docs/agent-prompts/port-p3/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, CI, checks before pushing, merging and the report.
- Standing owner rule: Rust stays the default build and is never deleted until the Vesper build matches it in both functionality and performance. Add the `vesper-port` build next to the Rust one, prove both pass, measure, and stop. Do not switch any dependent to the port and do not delete Rust code.
- Compiler-linked crate: use the seed pattern (crates/<crate>/seed as `<crate>-seed`, root and every crate in `cargo tree -p vesper -e normal,build` depend on it by rename), in its own commit.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the self-hosted corpus skip predicate in `fixtures()` in tests/fixtures/self_host/driver.rs) and the seed dependency switch. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/** or packages/vesper-std/**. Put helpers you need in your own package and list them in the report as std candidates.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries, seed package registration in root Cargo.toml `members`, the regenerated Cargo.lock, BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Root patches merge one at a time: merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -p <crate> -- -D warnings` before pushing). Files a test reads from outside its crate must be declared in the crate's BUILD.bazel. Bazel's rustfmt checks every file under src/: keep generated bindings in port/. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file. Use --no-opt for quick local runs of @vesper-std code.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. Report only performance you measured, with the setup; give callgrind instruction counts next to wall clock.
- Lean: any Lean file must compile with the pinned Lean 4.24.0 (use the GitHub release .zip), with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only the crate's own tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1039. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, ABBA numbers, compiler gaps with repros, decisions and remaining gaps.

Assignment: Port vesper-loom-audit.

Exclusive ownership:
- crates/vesper-loom-audit/**
- docs/parallel-results/port-p3-loom-audit.md

Dependencies: None. Uses the merged ports `@vesper/loom-calculus` (P1 lane 16) and `@vesper/semver` (P1 lane 15) as path dependencies; the JSON reader can use the merged serde scalar port or stay in the wrapper. #965 (TOML reader) is related but not required.

Implement:
1. Port `license.rs` (SPDX-style expression parsing and evaluation) and the audit logic in `lib.rs`.
2. `input.rs` parses JSON with `serde_json`: keep the parse in the Rust wrapper unless `@vesper/serde` covers it, and pass the port typed records.
3. BLAKE3 digests stay behind a `.vsys` extern (vetted crypto stays Rust).
4. Import `@vesper/loom-calculus` and `@vesper/semver` instead of re-porting their types.
5. Seed pattern: `vesper-loom-audit-seed` for the root.
6. ABBA: the audit over the test catalog, scaled up.

Stays in Rust: JSON input parsing (unless the serde port covers it), BLAKE3, and the frozen seed copy.

Acceptance:
- Both builds pass `tests/audit.rs` and the translated in-crate tests on the four required checks.
- Divergence check per ported module.
- Report: docs/parallel-results/port-p3-loom-audit.md.
~~~
