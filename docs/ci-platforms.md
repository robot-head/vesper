# CI platform lanes

Linux runs the workspace, lint, proof, and general acceptance suites in the
BuildBuddy `Test all targets` and `Native and acceptance scripts` workflows.
The GitHub `Platforms` workflow checks the exact planned SHA on macOS AArch64
and Windows x86_64 MSVC. Both required check names remain unchanged.

The native lanes run only host-specific coverage:

- macOS runs direct-fiber ABI tests, Pulse's host-driver and pool tests against
  kqueue, the macOS temporary-directory symlink regression, and Mach-O
  executables built and run on Apple silicon.
- Windows runs its direct-fiber and IOCP suites, Pulse's host-driver and pool
  tests against IOCP, and COFF executables linked with MSVC and run on Windows.
- Both run the export ABI tier of `scripts/test-port-harness.sh`
  (`VESPER_PORT_HARNESS_SCOPE=abi`): the port pilot's export ABI v2 kit and
  v3 gauge against their Rust builds, their divergence and leak checks,
  `vesper-port-build` package linking, and the port closure guard. These
  link Vesper-compiled Mach-O or COFF objects into Rust, so they depend on the
  host's object format and calling convention.

The rest of the port harness, every ported crate's tests against both builds
and its divergence checks, is platform-neutral once the ABI tier passes. It
runs once, in the `Vesper ports (Linux n/7)` jobs of the same workflow, on
`ubuntu-latest` (#1010). Before that, macOS and Windows each ran the whole
harness, which took 27 and 39 minutes of 34 and 50 minute jobs on main
(November 2026). Some divergence assertions exist only in this Cargo run (the
Bazel targets of those crates build the divergent objects but do not run
them), so the Linux job is not a duplicate of BuildBuddy. `Platform
validation complete` requires it. A new port appends its lines after the ABI
tier, so it runs on Linux only.

The Linux job runs as seven parallel shards, `Vesper ports (Linux 1/7)` to
`(Linux 7/7)` (`VESPER_PORT_HARNESS_SHARD`, #1019, #1100). With one job, a run
took 52 minutes and others hit the 60 minute limit once Loom, Crucible and
serde landed; with three, the last shard ran past its 90 minute limit once
the P3a ports landed. Each shard is a run of consecutive blocks of the
script: 1 the ABI tier through Ignis, 2 Quarry through pact, 3 semver through
Crucible, 4 serde, 5 aegis through bench, 6 eBPF and silicon, and 7 prism-h3
and every port appended after it. Each takes about 10-30 minutes after a 2-4
minute compiler build; the 60 minute job timeout is a backstop. When a shard
passes about 35 minutes, move a shard boundary in the script, or add a shard
there and in the `ports` matrix.

The native executable cases are shared with `scripts/test-native-host.sh`;
`scripts/ci/test-native-executables.sh` runs them with the Cargo-built compiler.
Linux retains general source-macro, formatting, proof, and workspace tests.
The native lanes no longer compile or run the entire workspace, Lean proofs,
Clippy, generic acceptance scripts, or doctests. Tests for other host-sensitive
features must be added explicitly to these lanes when needed; platform-neutral
coverage on Linux does not prove those features work on other systems.

PR, scheduled, manual, and release-candidate platform runs use the same native
OS selection. The release workflow still requires the two Linux statuses and
successful native checks for the same immutable SHA; `full` identifies the
release workflow profile, not a full workspace rerun on each native OS. The
workflow publishes nothing. A planning failure, failed test, or wrong checkout
fails the corresponding required check.

## PR test tiers

`Test all targets` builds and lints every target on every run. Its tests come
in two tiers:

- **Every run:** all tests except the ones tagged `lean_replay` in
  `BUILD.bazel`.
- **`lean_replay`:** the Lean/Logos replay suite (`lean4lean_*42`,
  `logos42_acceptance`, `oracle42_acceptance`, `resource_oracle42`,
  `resource_model42`, `replay42_kernel`). Each reserves 5-9 GiB, so together
  they made up a 20-25 minute tail of the 8 CPU / 22 GB workflow VM (32-38
  minute runs, November 2026). They run on every push to `main`, and on a pull
  request only when it changes a path listed in
  `scripts/ci/lean_replay_scope.py` (Lean models, replay tooling, the
  Lean-facing compiler modules, those tests, the toolchain and CI config).
  Put `[ci full]` in a commit message of the PR to force them.

If the scope script cannot read the diff, it runs the full suite. A
regression in a skipped replay test shows up on the next `main` run; treat it
like any red main. When you add a test that reads new Lean-facing inputs, tag
it `lean_replay` only if those inputs are in the trigger list.
