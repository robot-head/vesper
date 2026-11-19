# Port the vesper-flux dataflow core

> Port wave P3b, lane 7. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/flux`. Claim issue: #1069.

`vesper-flux` is incremental dataflow (DBSP): Z-sets, frontiers, circuits, CDC, a log, checkpoints, a Kafka wire codec, consumer-group protocol and client, and a cluster driver that reads from an `EpochSource` and commits to a `CommitSink`. 12k lines and 91 tests. The cluster driver already takes `&dyn EpochSource` and `&mut dyn CommitSink`, which second-class `in dyn`/`inout dyn` covers today.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are porting one Rust crate to Vesper source in port wave P3b of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan and its rule changes), "Rules for every lane" in docs/agent-prompts/port-p1/README.md, docs/maintenance/porting-a-crate.md (the recipe, above all "Patterns for Rust APIs" and "Crates the compiler links"), docs/maintenance/vesper-port-survey.md, crates/vesper-warp-tcp (a non-seed port with the `vesper-port` feature), docs/parallel-results/port-p3-dyn-trait.md and docs/parallel-results/boxed-dyn-trait.md (what `dyn` supports today), the latest scoreboard docs/parallel-results/port-parity.md, and the crate's README, public API and tests.

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1069 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1069").

Delivery and coordination:
- Follow the P1 lane rules as changed by docs/agent-prompts/port-p3b/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, CI, checks before pushing, merging and the report.
- Standing owner rule: Rust stays the default build and is never deleted until the Vesper build matches it in both functionality and performance. Add the `vesper-port` build next to the Rust one, prove both pass, measure, and stop. Do not switch any dependent to the port and do not delete Rust code.
- Not compiler-linked (`cargo tree -i vesper-flux -e normal,build` does not reach the root `vesper`), so no seed: the port is a `vesper-port` Cargo feature of the crate itself, as in crates/vesper-warp-tcp. Dependents keep the Rust build.
- The lane may land as several PRs (part 1, part 2, ...), each green on its own and each closing nothing until the last one closes #1069.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the self-hosted corpus skip predicate) and, for seed lanes, the seed dependency switch. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/**, src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/** or packages/vesper-std/**. Put helpers you need in your own package and list them in the report as std candidates. A compiler gap is reported with a minimal repro, not fixed in this lane.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries, package registration in root Cargo.toml `members`, the regenerated Cargo.lock, BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Root patches merge one at a time: merge origin/main right before merging and regenerate the lockfile with cargo.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. Report only performance you measured, with the setup; give callgrind instruction counts next to wall clock.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1069. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, ABBA numbers, compiler gaps with repros, decisions and remaining gaps.

Assignment: Port the vesper-flux dataflow core.

Exclusive ownership:
- crates/vesper-flux/**
- docs/parallel-results/port-p3b-flux.md

Dependencies: None for the pure core. File-backed sources and sinks that perform effects through the object need lane 1 (#1063); until then they stay Rust and are lent through a `.vsys` adapter.

Implement:
1. Port `zset.rs`, `frontier.rs`, `circuit.rs`, `cdc.rs`, `log.rs`, `checkpoint.rs`, `kafka_wire.rs` (port CRC32C as a table or use a `.vsys` call, measure both), `kafka_group.rs` and `cluster.rs`.
2. `cluster.rs` takes `in dyn EpochSource` and `inout dyn CommitSink`. `Rc<dyn GroupOffsetSource>` in `kafka.rs` has no Vesper form: replace it with a borrowed object or an owned `Box[dyn ..]` and say which.
3. Compression (`flate2`, `zstd`, `lz4_flex`, `snap`) stays Rust behind stateless `.vsys` calls over byte slices; `blake3` likewise.
4. `kafka_client.rs` (rskafka, tokio, rustls, rsasl), `durable.rs` and the file parts of `sink.rs` stay Rust.
5. ABBA: the `incremental_join` and `checkpoint_resume` examples and the Kafka wire codec.

Stays in Rust: Compression codecs, blake3, TLS and SASL, the rskafka/tokio client, and the file-backed durable store.

Acceptance:
- Both builds pass `dbsp.rs`, `cluster.rs`, `checkpoints.rs`, `recovery.rs`, `rescale.rs`, `live_rescale.rs`, `sources_sinks.rs` and the translated in-crate tests on the four required checks; `kafka.rs`, `durable.rs` and `platform_recovery.rs` keep passing with the Rust edges over the port.
- Divergence check per ported module (checkpoint bytes and Z-set outputs compared byte for byte).
- Report: docs/parallel-results/port-p3b-flux.md with the API inventory, what stays Rust, unit-test treatment, divergence checks, ABBA numbers and compiler gaps.
~~~
