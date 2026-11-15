# CI platform lanes

CI runs one reference lane on BuildBuddy Workflows (`buildbuddy.yaml`) and two
platform lanes on GitHub Actions (`.github/workflows/platforms.yml`). Every
Linux and macOS build and test through Bazel; Windows uses Cargo because the
hermetic Bazel toolchain cannot link MSVC yet. See [bazel.md](bazel.md).

| Lane | Runner | Runs |
| --- | --- | --- |
| `Test all targets` (Linux x86_64) | BuildBuddy Workflows, remote execution for builds | rustfmt, Clippy, workspace tests, Lean |
| `Native and acceptance scripts` (Linux x86_64) | BuildBuddy Workflows | AArch64 objects under QEMU, host native executables, acceptance scripts |
| macOS (AArch64) | `macos-latest` (Apple silicon) | Native PR profile; full supported suite nightly and for release candidates |
| Windows (x86_64 MSVC) | `windows-latest` | Cargo Clippy, bounded test runner and doctests, native COFF executables and acceptance checks; full supported suite nightly and for release candidates |

The platform lanes check out sources with `core.autocrlf=false` so that span and
snapshot tests see the same bytes as Linux.

## PR, nightly and release profiles

Linux's two BuildBuddy actions remain complete on every PR. The native platform
jobs compile every workspace test but use an explicit execution catalog in
[`platform_test_profiles.json`](../scripts/ci/platform_test_profiles.json).
Tests absent from the catalog always run; adding a test does not silently
remove it from PR coverage.

| Group | Ordinary native PR | Full native profile |
| --- | --- | --- |
| Fixed logical proof packets and kernel replay matrices | Compiled; Linux executes the complete matrices | Execute every matrix supported by the host |
| Flux recovery and live rescaling | Ordinary cases, plus a bounded durable-store recovery smoke test | Original exhaustive fault-at-every-step cases too |
| Self-hosted frontend | Edge cases and typed errors | Complete repository differential corpus too |
| Filesystem, process, fibers, IOCP, ABI, JIT, native linking, endpoint controls, Quarry crash tests | Run | Run |

The new Flux smoke target uses a real durable CAS, an independent single-worker
reference, checkpoint-boundary failures, all three fault kinds, aligned and
unaligned checkpoints, live rescaling, and retired workers. It checks outputs,
exactly-once epochs and actual recovery metadata. It does not claim exhaustive
phase or scheduler-step coverage. The original exhaustive test bodies and
assertions are unchanged by CI tiering.

Changed paths restore complete coverage for the relevant catalog group. Flux,
storage and shared runtime changes restore the Flux fault matrices; parser,
evaluator, native compiler and Vesper-source changes restore the corpus; Lean,
proof tooling and cataloged proof-test changes restore proof matrices. Shared
dependency, toolchain and CI changes restore all groups. Missing diff information
also selects all groups. Invalid checkout identities or stale catalog targets
and case names fail rather than producing an empty successful selection.

Each job records its checked-out SHA, selected groups and deferred coverage in
its summary. Every nightly and explicitly requested full run removes all tier
exclusions. Existing source-level ignores and Bazel platform incompatibilities
still apply: full means the complete **supported** suite, not GPU access or a
new port of Linux-only tools. The existing test assertions, four corpus workers,
proof budgets and runtime deadlines are not changed.

Windows compiles test executables once, then runs whole test binaries with at
most two processes, one binary per package, and two libtest threads per binary.
Heavy root/Lean binaries reserve the runner's process capacity. This preserves
in-process test behavior while allowing independent packages to overlap.
Doctests still run through Cargo. macOS keeps ordinary Bazel tests separate
from the filtered Rust-only batch, so libtest flags never reach lint or doctest
runners. Both runners reject stale selectors and report failures and deferred
work explicitly.

The platform workflow's schedule runs full native validation nightly. Manual
full runs are also available. The release-candidate workflow requires an
immutable 40-hex commit SHA, successful Linux `Test all targets` and `Native and
acceptance scripts` statuses for that SHA, and full macOS/Windows validation of
the same checkout before producing its ready result and validation artifact.
It publishes nothing. There was no release publisher in this repository to
wire into; any automated publisher must depend on this successful full gate.
This workflow does not restrict manual release creation in GitHub's UI.

The required PR check names remain `macOS (AArch64)` and
`Windows (x86_64 MSVC)`. A planning or runner failure fails the corresponding
workflow; it must not be translated into a skipped green portability check.

## Platform gating policy

A test that cannot run on a platform is gated in source, never dropped from CI:

- Code that only exists on one platform is behind `cfg` (for example,
  `#[cfg(target_os = "linux")]` on Pulse's io_uring driver). The portable surface
  still builds and is tested everywhere.
- A test whose subject exists but whose environment is missing uses
  `#[cfg_attr(<platform>, ignore = "<reason>")]`, so `cargo test` lists it as
  ignored with the reason instead of passing silently.
- Kernel- or hardware-dependent tests follow integration rule 7 in
  [parallel-work.md](parallel-work.md): report unavailable validation explicitly.

## Known platform gaps (current workspace)

| Area | Linux | macOS AArch64 | Windows x86_64 |
| --- | --- | --- | --- |
| Pulse io_uring driver | Built and tested (GitHub runners may deny `io_uring_setup`) | Not built: Linux-only | Not built: Linux-only |
| `vesper build` native executables | `cc` links ELF | `cc` links Mach-O; CI runs examples natively | COFF objects linked with MSVC `link.exe`; CI executes the host examples |
| AArch64 object validation | Cross ELF objects under QEMU | Native hardware via `vesper build` | n/a |
| JIT (`vesper jit`) | Executed locally | Configured in CI | Configured in CI |
| Lean extraction | Executed locally | Configured in CI | Configured in CI |

## Integrated milestone crates

All handoff libraries and `experiments/native-pulse` are registered in both root member lists. They share the root lockfile and join every lane through workspace checks; there is no separate standalone-crate script. The sandbox fixture guest remains an independent wasm workspace, with committed fixture binaries used by the host tests. CI does not need wasm compilation targets unless rebuilding those fixtures.

| Crate | Platform boundary |
| --- | --- |
| `vesper-fiber-abi` | Native stacks and switching run only on Linux x86_64/AArch64. macOS and Windows check the portable ABI/data/codegen surface. The experiment compiles on every lane and reports an unsupported host outside Linux. |
| `vesper-pact` | Pure Rust solver and coherence checks. |
| Lean effects and places | The real extraction suite and `lean/EffectPlaces.lean` run on every lane. |
| `vesper-loom-cas` | Unix permissions/executable bits and atomic publish differ by host; platform tests must validate their actual behavior. |
| `vesper-loom-sandbox` | Wasmtime component host with prebuilt guest fixtures; Unix symlink tests remain gated to Unix. |
| `vesper-ignis` | GPU-independent planner only. D3D12 acceptance still needs Windows hardware and a real executor. |

The integration checkpoint was validated on Linux. These platform jobs configure additional acceptance checks; their presence does not establish a passing macOS/Windows run or hardware validation. Wasmtime 50 and the compiler currently use separate Cranelift versions, so clean workspace builds need more memory and disk than the earlier compiler-only workspace.

## Linux-only suites

macOS runners are the slowest and scarcest part of CI, so a few long suites
whose subject does not depend on the host OS run on Linux (BuildBuddy) only.
Each one is still built everywhere, and the tests that exercise the same code
paths on every OS (process spawning, file system, paths) keep running there.

| Suite | Why it is platform-neutral | macOS | Windows |
| --- | --- | --- | --- |
| `//:test/logos42_acceptance` | Logos rechecking of all three model import closures | Skipped (`BUILD.bazel`) | Ignored (`cfg_attr`) |
| `//:test/proof_audit_cli` | Six serial kernel replays of Lean's Std (>15 min on macOS) | Skipped (`BUILD.bazel`) | Runs |
| `scripts/test-format-proof.sh` | Serial Lean witness replay (~3.5 min on macOS) | Not run | Runs against the Cargo binary |
| `//:test/replay42_kernel`, `//:test/format_proof_cli`, `//:test/oracle42_acceptance` | Serial Lean kernel replays (390 s, 276 s and 135 s on macOS) | Skipped (`BUILD.bazel`) | Run: their `cfg(windows)` process and path code needs Windows |
| `//:test/resource_model42` | Twelve-module logical-model compilation, 285 source/alternate proof roots, and serial artifact/axiom controls; measured over 300 s on Linux, with a 900 s bound | Skipped (`BUILD.bazel`); `proof_audit_private` still runs its process/path regression | Runs the full target |
| Fixed `lean4lean_*42` proof packets | Complete logical projection, authored-root/alternate-proof audits and serial artifact/axiom controls; the foundation packet timed out after 300.3 s on macOS | Each packet uses the same macOS-only scheduling exclusion when introduced; `proof_audit_private` retains process/path coverage | Runs every full packet |

The fixed packets are `lean4lean_foundations42`, `lean4lean_construction42`,
`lean4lean_order42`, `lean4lean_map42`, `lean4lean_normalize42`, and
`lean4lean_normalization42`. Their dependent PRs add the same scheduling entry
as each target is introduced. Only the foundation packet's macOS timeout was
measured; the others share its platform-neutral replay design. Their internal
300-second limits and complete checks remain unchanged. See
[the scheduling evidence](rfcs/lean4lean-proof-packets-ci-scheduling.md).

Because nothing else uses it, the macOS and Windows lanes also no longer
prepare the pinned external Logos checker (`VESPER_LOGOS42_ROOT`),
which took about 6 minutes on macOS. Add a suite here only when it is long,
its result cannot differ by OS, and Linux CI runs it. Keep platform-sensitive
tests (fibers, MPK, ABI, codegen, file system, networking) on every lane.

Other CI time savers:

- The acceptance scripts look executables up in a file written by
  `scripts/ci/prebuild-acceptance.sh` (`VESPER_BAZEL_EXECUTABLES`, see
  `scripts/bazel-lib.sh`) instead of running `bazel build` and `bazel cquery`
  for each of about forty executables.
- Pushes to main share one concurrency group: a newer merge replaces a run
  that is still waiting, instead of each merge holding a macOS runner.
- The Windows Cargo cache is saved from main only; PRs restore it. Nightly
  runs on main warm that same cache.
- The source-macro matrix retains its distinct feature and dependency-graph
  configurations on all three operating systems. A standalone root Cargo test
  can have a narrower dependency graph than a unified workspace test, so those
  checks remain intentional. Native Stylus workspace and stdio checks have one
  owner per OS rather than a second complete Windows/macOS platform job.

## Required checks

The checks are named after the BuildBuddy actions (`Test all targets`,
`Native and acceptance scripts`) and the GitHub matrix entries
(`macOS (AArch64)`, `Windows (x86_64 MSVC)`). Renaming one renames its check,
so update branch protection in the same change.
