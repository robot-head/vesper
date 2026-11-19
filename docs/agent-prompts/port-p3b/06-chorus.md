# Port vesper-chorus

> Port wave P3b, lane 6. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/chorus`. Claim issue: #1068.

`vesper-chorus` is choreographic programming: a parser, checker and endpoint projection for choreographies, a replicated log, Raft, sessions over a `Transport` trait, a deterministic simulated network on SimPulse, a durable file log, a Warp/rustls network and RDMA. 9.9k lines and 83 tests. Sessions own a `Box<dyn Transport>` and loop on it, and real transports perform I/O, so that part needs lanes 1 and 2.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P3b of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan and its rule changes), "Rules for every lane" in docs/agent-prompts/port-p1/README.md, docs/maintenance/porting-a-crate.md (the recipe, above all "Patterns for Rust APIs" and "Crates the compiler links"), docs/maintenance/vesper-port-survey.md, crates/vesper-warp-tcp (a non-seed port with the `vesper-port` feature), docs/parallel-results/port-p3-dyn-trait.md and docs/parallel-results/boxed-dyn-trait.md (what `dyn` supports today), the latest scoreboard docs/parallel-results/port-parity.md, and the crate's README, public API and tests.

When to start: now for part 1; part 2 after lanes 1 and 2 (#1063, #1064) merge. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1068 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1068").

Delivery and coordination:
- Follow the P1 lane rules as changed by docs/agent-prompts/port-p3b/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, CI, checks before pushing, merging and the report.
- Standing owner rule: Rust stays the default build and is never deleted until the Vesper build matches it in both functionality and performance. Add the `vesper-port` build next to the Rust one, prove both pass, measure, and stop. Do not switch any dependent to the port and do not delete Rust code.
- Not compiler-linked (`cargo tree -i vesper-chorus -e normal,build` does not reach the root `vesper`), so no seed: the port is a `vesper-port` Cargo feature of the crate itself, as in crates/vesper-warp-tcp. Dependents keep the Rust build.
- The lane may land as several PRs (part 1, part 2, ...), each green on its own and each closing nothing until the last one closes #1068.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the self-hosted corpus skip predicate) and, for seed lanes, the seed dependency switch. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/**, src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/** or packages/vesper-std/**. Put helpers you need in your own package and list them in the report as std candidates. A compiler gap is reported with a minimal repro, not fixed in this lane.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries, package registration in root Cargo.toml `members`, the regenerated Cargo.lock, BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Root patches merge one at a time: merge origin/main right before merging and regenerate the lockfile with cargo.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. Report only performance you measured, with the setup; give callgrind instruction counts next to wall clock.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1068. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, ABBA numbers, compiler gaps with repros, decisions and remaining gaps.

Assignment: Port vesper-chorus.

Exclusive ownership:
- crates/vesper-chorus/**
- docs/parallel-results/port-p3b-chorus.md

Dependencies: Part 2 needs lane 1 (#1063, effectful `Box[dyn Transport]`) and lane 2 (#1064, owning the box in loops). The SimPulse port (#902) is merged; use it Vesper to Vesper.

Implement:
1. Part 1: port `parse.rs`, `check.rs`, `project.rs`, `chor.rs`, `replog.rs` and `raft.rs` (pure state machines; Raft takes its messages and timers as inputs).
2. Part 2: `session.rs` over `Box[dyn Transport]` and `simnet.rs` with `Box[dyn SimRole]` on the SimPulse port. The simulated transport is pure; the Warp and RDMA transports stay Rust and are lent to the port through a `.vsys` adapter that implements `Transport`.
3. `durable.rs` (file log), `warpnet.rs` (quinn, tokio, rustls, `dyn ClientCertVerifier`, the `Box<dyn FnOnce>` job queue) and `rdma.rs` stay Rust.
4. `raft_sim.rs` and `raft_depth.rs` run the same seeds against both builds and compare the logs byte for byte.
5. ABBA: projection of the fixtures and a seeded Raft simulation.

Stays in Rust: The durable file log, the Warp/rustls network and its job queue, and RDMA.

Acceptance:
- Both builds pass `projection.rs`, `raft_sim.rs`, `raft_depth.rs`, `session.rs` and the translated in-crate tests on the four required checks; the Warp, TLS and durable tests keep passing with the Rust edges over the port.
- Divergence check per ported module.
- Report: docs/parallel-results/port-p3b-chorus.md with the API inventory, what stays Rust, unit-test treatment, divergence checks, ABBA numbers and compiler gaps.
~~~
