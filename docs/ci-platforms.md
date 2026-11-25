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
- Both run the export ABI tier from `scripts/port-abi-plan.json`
  (`VESPER_PORT_HARNESS_SCOPE=abi`): the port pilot's export ABI v2 kit and
  v3 gauge against their Rust builds, their divergence and leak checks,
  `vesper-port-build` package linking, and the port closure guard. These
  link Vesper-compiled Mach-O or COFF objects into Rust, so they depend on the
  host's object format and calling convention.

Both lanes build the compiler and every test above it with the
`port-compiler` Cargo profile: release optimizations with debug assertions
and overflow checks kept on. The fiber, Pulse and Loom tests and the port
ABI tier's Rust tests build for debug.

The codegen swap point tests (`vesper-codegen-swap-test`) compare each
corpus world built by the Rust emitters with the same world built through a
Vesper hook: CLIF text and object bytes, and for sampled worlds a JIT run.
Only the hooks' calls and the JIT runs need the host. An object for another
target needs no machine of that kind, so the comparisons for the macOS and
Windows objects run on `ubuntu-latest`, in `Codegen swap point (Linux for
macOS)` and `(Linux for Windows)` (`CODEGEN_SWAP_TARGET`, which skips the JIT
runs). They keep the sweep the native lanes ran: every fourth world for the
pilot, control and walk-state swaps (`CODEGEN_SWAP_STEP=4`), every world for
the others. `Platform validation complete` requires both. The native lanes
then run the suite over every fourth corpus file (`CHECK_HARNESS_STRIDE=4`),
skipping the two tests whose corpus worlds a quarter may lack
(`a_divergent_hook_is_caught` and `box_drops_match_the_rust_emitter`, which
check the harness and the Linux jobs run). So the drops, calls, scalar and
sequence sweeps JIT-run about a quarter as many corpus worlds on macOS and
Windows as before; the pilot, control and walk-state sweeps run as many.

Checking the corpus, not comparing it, is most of each test binary's time.
Built for debug, the suite took about 55 minutes of a 60 minute macOS job
and 35-61 minutes on Windows (November 2026). On a 4-core Linux machine the
whole sweep took 13.7 minutes optimized, and `self_host_codegen_seq` alone
97 s against 319 s for debug; building the optimized test binaries from
scratch took 7 minutes against 4 for debug. Over a quarter of the corpus the
suite took 4.5 minutes. Bazel also runs the suite, unoptimized and for
Linux objects, in `Self-host codegen differentials`.

The rest of the port harness, every ported crate's tests against both builds
and its divergence checks, is platform-neutral once the ABI tier passes. It
runs once, in the `Vesper ports (Linux n/9)` jobs of the same workflow, on
`ubuntu-latest` (#1010). Before that, macOS and Windows each ran the whole
harness, which took 27 and 39 minutes of 34 and 50 minute jobs on main
(November 2026). Some divergence assertions exist only in this Cargo run (the
Bazel targets of those crates build the divergent objects but do not run
them), so the Linux job is not a duplicate of BuildBuddy. `Platform
validation complete` requires it. A new port appends its lines after the ABI
tier, so it runs on Linux only.

The Linux job runs as nine parallel shards, `Vesper ports (Linux 1/9)` to
`(Linux 9/9)` (`VESPER_PORT_HARNESS_SHARD`, #1019, #1100, #1924). With one job, a run
took 52 minutes and others hit the 60 minute limit once Loom, Crucible and
serde landed; with three, the last shard ran past its 90 minute limit once
the P3a ports landed. Each shard is a run of consecutive blocks of the
script: 1 the ABI tier through Ignis, 2 Quarry through pact, 3 semver through
Crucible, 4 serde, 5 aegis through bench, 6 eBPF and silicon, 7 Warp through
diagnostics, 8 fmt through lint, and 9 sim-session and every port appended
after it. Each takes about 10-30 minutes after a 2-4
minute compiler build; the 60 minute job timeout is a backstop. When a shard
passes about 35 minutes, move a shard boundary or add a shard in
`scripts/port-harness-plan.json`. The workflow gets the shard count from the
selected checkout so it can also run older revisions with seven shards.

Nearly all of the harness's time is the compiler compiling Vesper sources:
the Rust tests themselves finish in seconds. So one `Vesper ports compiler`
job builds `vesper` once with the `port-compiler` Cargo profile (release
optimizations without thin LTO, debug assertions and overflow checks kept on)
and every shard downloads that binary instead of building a debug compiler
of its own (#1103).

On pull requests the plan job also picks the ports to test
(`aspect ci port-harness-selection`, `VESPER_PORT_HARNESS_CRATES`). If
every changed file is documentation or sits inside workspace crates outside
the compiler's dependency closure, only those crates' blocks of the harness
and the blocks of the crates that depend on them run; a documentation-only
change runs none. Any other change (the compiler, std packages, a seed copy
the compiler links, the harness, CI, the root manifests or lockfile) runs
every port, and so do pushes to main, merge queue groups and the nightly.
Since #1479 the shards do not run for pull requests at all: the Platforms run
that `Native platform checks` dispatches for a pull request sets the plan's
ports to `none`. The selection still applies to manual `pr` runs.
Each block of the script names its crate directory with `if ported vesper-x;
then ... fi` or `skip_port vesper-x || ...`; a line without a guard always
runs.
When the plan names no port, the nine shard jobs are skipped as jobs, so
they take no runners, and `Platform validation complete` accepts that skip
(#1321).

## Keeping the queue short

GitHub runs a limited number of jobs at once per account (5 on macOS), and
every push to a pull request reruns its workflows. On 2026-11-21 that left 78
runs queued and every Platforms run cancelled by the next push before it
finished (#1321). To keep pull requests within the limit:

- `Hybrid compiler` and the `Vesper ports` jobs (the compiler build and its
  nine shards, about eleven Linux runners) run only on pushes to main, the
  nightly schedule and manual runs, not on pull requests (#1479). Neither is
  a required check, and Bazel builds and tests both crates on every pull
  request.
- The narrow integration lanes (Kafka, lavapipe GPU, Ignis Vulkan, Aura GTK,
  QEMU vectors) are not triggered on pull requests by `Cargo.toml` or
  `Cargo.lock` alone; pushes to main still run them on those changes.
- A push to main cancels the older main run of the same workflow, except
  Platforms and `Hybrid compiler`: pull requests skip the port shards and the
  hybrid job, so their main runs must finish. They run one at a time on main,
  and a burst of merges waits as a single newest run (#1479). A pull request
  that wants the port shards before merging can dispatch Platforms on its
  branch with profile `pr` and `pull_request` left empty; that runs every
  port shard.
- The merge gate (`.github/actions/merge-gate`, `scripts/ci/merge_gate.py`)
  enforces the project rule that pull requests which cannot merge yet get no
  CI: when a pull request's base is not main, its branch carries another open
  pull request's commits (a merged-in parent that has not landed), or it
  conflicts with main, the gate cancels every Actions run for that push and
  fails with the reason. Push again once the parent has merged. It is the
  first step of every job that runs on pull requests, so it takes no runner
  of its own; as a separate `Merge gate` job it waited over an hour for a
  Linux runner ahead of everything else (#1479). The gate also stops a pull
  request that changes the generated `docs/maintenance/rust-census.md` along
  with anything else: lanes edit the STATUS tables in
  `scripts/rust-census.py`, and `.github/workflows/rust-census.yml`
  regenerates the census after each push to main into one "Refresh the Rust
  census" pull request, which changes only that file.
- `macOS (AArch64)` and `Windows (x86_64 MSVC)` start only when a pull
  request is ready to merge. Platforms no longer runs on pull request pushes;
  `Native platform checks` (`.github/workflows/native-checks.yml`) runs on
  each BuildBuddy status and, once `Test all targets` and `Native and
  acceptance scripts` are both green on the head commit and the merge gate
  passes, dispatches Platforms on the branch with `pull_request` set. That
  run tests `refs/pull/<n>/merge`, as pull request runs did, and reports both
  required checks on the head commit; until then they show as expected and
  the pull request cannot merge. A new push starts over when its own
  BuildBuddy checks go green. To start them by hand, run Platforms from the
  Actions tab on the branch with profile `pr`, `checkout_ref`
  `refs/pull/<n>/merge` and `pull_request` `<n>`.

The native executable cases are shared with `aspect acceptance test-native-host`;
Use `aspect ci test-native-executables` on macOS. Windows CI runs
`scripts/windows/test-native-executables.ps1` with the Cargo-built compiler.
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
leave those files behind. Each BuildBuddy action runs
`aspect ci reclaim-disk`, which removes earlier runs' temporary files
and runs `bazel clean` when less than 20 GB is free. The Bazel test steps log
free space every two minutes. The replay step removes its temporary
checker when it exits.

## PR test tiers

`Test all targets` builds the workspace and runs the tests, Clippy and
rustfmt on every run, except the corpus differentials (`test/self_host_*`,
tagged `self_host` by `tools/bazel/defs.bzl`): since together they passed
BuildBuddy's one-hour limit (#1519), the codegen swap point and backend
differentials (also tagged `self_host_codegen`, by the name prefixes in
`_SELF_HOST_CODEGEN`) run as the `Self-host codegen differentials` check,
those tagged `self_host_part2` (by the prefixes in `_SELF_HOST_PART2`) as
`Self-host differentials 2` (#1882), and the rest as `Self-host
differentials`, each on its own runner. The self-host checks print every
target's time (`--test_summary=short`); rebalance the two non-codegen checks
by moving prefixes in `_SELF_HOST_PART2`. It downloads remote outputs as local actions need
them and excludes the Bazel test targets tagged `lean_replay` from its
build and test selection. `Native and acceptance
scripts` runs the acceptance scripts, the Lean model audit and, when
selected, the tagged replay tests on its own runner. Both status checks
remain required for merge and release.

The Lean/Logos replay tests include `lean4lean_*42`, `logos42_acceptance`,
`oracle42_acceptance`, `resource_oracle42` (those three in
`crates/vesper-oracles`), `resource_model42` and `replay42_kernel`. They run on every push to `main` and on pull requests
that touch paths in `.aspect/lean-replay-scope.axl`. Put `[ci full]` in a
PR commit message to force a replay. The replay step queries the tagged
test labels and builds only those targets and their dependencies.

If the scope script cannot read the diff, it runs the replay suite. A
regression in a skipped replay test shows up on the next `main` run; treat it
like any red main. When you add a test that reads new Lean-facing inputs, tag
it `lean_replay` only if those inputs are in the trigger list.
