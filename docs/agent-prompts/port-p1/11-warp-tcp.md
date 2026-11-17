# Port vesper-warp-tcp to Vesper

> Port wave P1, lane 11. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p1/warp-tcp`. Claim issue: #908.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline d523661 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md (the recipe), docs/maintenance/vesper-port-survey.md, crates/vesper-port-pilot (the worked example), and the crate's README, public API and tests.

When to start: after lane 1 (#898) merges. Needs neither strings nor `@vesper-std`. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #908 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #908").

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #908. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, decisions and remaining gaps.

Assignment: Port vesper-warp-tcp to Vesper.

Exclusive ownership:
- crates/vesper-warp-tcp/**
- docs/parallel-results/port-p1-warp-tcp.md
Dependencies: Lane 1. Dependent vesper-warp keeps its API (do not edit crates/vesper-warp/**).

Implement:
1. Port the sans-IO RFC 9293 connection (`conn.rs`), wire codec (`wire.rs`), sequence arithmetic (`seq.rs`), RTT estimation (`rtt.rs`) and congestion control (`cc.rs`).
2. `CongestionController` trait: port the implementations the crate ships as a Vesper enum; a user-supplied Rust controller keeps working through a Rust-side adapter (report whether any exist outside tests).
3. `harness.rs` is a test simulator with a `Box<dyn FnMut>` drop filter: keep it Rust, driving the ported connection through the public API.
4. Delete the Rust implementation of the ported modules once both builds pass everywhere.

Stays in Rust: `harness.rs` (test driver) and any custom-controller adapter.

Acceptance:
- All 63 tests, including `tests/ffi_layout.rs` and `tests/malformed.rs`, pass against both builds.
- No panic on malformed segments.
- ABBA benchmark of segment encode/decode and the transfer loop.
- Report: docs/parallel-results/port-p1-warp-tcp.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
