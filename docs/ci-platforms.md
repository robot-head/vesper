# CI platform lanes

CI runs one reference lane on BuildBuddy Workflows (`buildbuddy.yaml`) and two
platform lanes on GitHub Actions (`.github/workflows/platforms.yml`). Every
lane builds and tests through Bazel; see [bazel.md](bazel.md).

| Lane | Runner | Runs |
| --- | --- | --- |
| `Test all targets` (Linux x86_64) | BuildBuddy Workflows, remote execution for builds | rustfmt, Clippy, workspace tests, Lean |
| `Native and acceptance scripts` (Linux x86_64) | BuildBuddy Workflows | AArch64 objects under QEMU, host native executables, acceptance scripts |
| macOS (AArch64) | `macos-latest` (Apple silicon) | rustfmt, Clippy, workspace tests, Lean, host native executables, acceptance scripts (minus the [Linux-only suites](#linux-only-suites)) |
| Windows (x86_64 MSVC) | `windows-latest` | Cargo (Bazel cannot link MSVC on Windows yet): Clippy, workspace tests, Lean models; native executable linking skipped |

The platform lanes check out sources with `core.autocrlf=false` so that span and
snapshot tests see the same bytes as Linux.

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
| `vesper build` native executables | `cc` links ELF | `cc` links Mach-O; CI runs examples natively | Not supported: the driver invokes `cc ... -lm`, which MSVC hosts lack. `native_executable` is ignored on Windows |
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
| `//:test/lean4lean42_acceptance` | Lean kernel replay of both model import closures through the external checker (~15 min on macOS) | Skipped (`BUILD.bazel`) | Ignored (`cfg_attr`) |
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
prepare the pinned external Lean4Lean checker (`VESPER_LEAN4LEAN42_ROOT`),
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
- The Windows Cargo cache is saved from main only; PRs restore it.

## Required checks

The checks are named after the BuildBuddy actions (`Test all targets`,
`Native and acceptance scripts`) and the GitHub matrix entries
(`macOS (AArch64)`, `Windows (x86_64 MSVC)`). Renaming one renames its check,
so update branch protection in the same change.
