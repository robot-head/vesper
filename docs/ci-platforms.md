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

Nearly all of the harness's time is the compiler compiling Vesper sources:
the Rust tests themselves finish in seconds. So one `Vesper ports compiler`
job builds `vesper` once with the `port-compiler` Cargo profile (release
optimizations without thin LTO, debug assertions and overflow checks kept on)
and every shard downloads that binary instead of building a debug compiler
of its own (#1103).

On pull requests the plan job also picks the ports to test
(`scripts/ci/port_harness_selection.py`, `VESPER_PORT_HARNESS_CRATES`). If
every changed file is documentation or sits inside workspace crates outside
the compiler's dependency closure, only those crates' blocks of the harness
and the blocks of the crates that depend on them run; a documentation-only
change runs none. Any other change (the compiler, std packages, a seed copy
the compiler links, the harness, CI, the root manifests or lockfile) runs
every port, and so do pushes to main, merge queue groups and the nightly.
Each block of the script names its crate directory with `if ported vesper-x;
then ... fi` or `skip_port vesper-x || ...`; a line without a guard always
runs.

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

## Runner disk

BuildBuddy reuses workflow runners between runs, disk included. Tests write
their scratch roots under `/tmp` (Bazel sets no `TMPDIR` for tests), and the
replay job builds the Logos42 checker there. A killed or failed run can
leave those files behind. Both BuildBuddy actions start with
`scripts/ci/reclaim-disk.sh`, which removes earlier runs' temporary files
and runs `bazel clean` when less than 20 GB is free. The Bazel test steps log
free space every two minutes. The replay step removes its temporary
checker when it exits.

## PR test tiers

`Test all targets` builds the workspace and runs the tests, Clippy and
rustfmt on every run. It downloads remote outputs as local actions need
them and excludes the Bazel test targets tagged `lean_replay` from its
build and test selection. `Native and acceptance
scripts` runs the acceptance scripts, the Lean model audit and, when
selected, the tagged replay tests on its own runner. Both status checks
remain required for merge and release.

The Lean/Logos replay tests include `lean4lean_*42`, `logos42_acceptance`,
`oracle42_acceptance`, `resource_oracle42`, `resource_model42` and
`replay42_kernel`. They run on every push to `main` and on pull requests
that touch paths in `scripts/ci/lean_replay_scope.py`. Put `[ci full]` in a
PR commit message to force a replay. The replay step queries the tagged
test labels and builds only those targets and their dependencies.

If the scope script cannot read the diff, it runs the replay suite. A
regression in a skipped replay test shows up on the next `main` run; treat it
like any red main. When you add a test that reads new Lean-facing inputs, tag
it `lean_replay` only if those inputs are in the trigger list.
