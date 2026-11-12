# CI platform lanes

CI runs one reference lane and two platform lanes on every push and pull request
(`.github/workflows/ci.yml`).

| Lane | Runner | Runs |
| --- | --- | --- |
| `check` (Linux x86_64) | `ubuntu-latest` | fmt, Clippy, workspace tests, Lean, AArch64 objects under QEMU, host native executables |
| macOS (AArch64) | `macos-latest` (Apple silicon) | Clippy, workspace tests, Lean, host native executables on real AArch64 hardware |
| Windows (x86_64 MSVC) | `windows-latest` | Clippy, workspace tests, Lean; native executable linking skipped |

Formatting runs only on Linux because it does not depend on the host. The
platform lanes check out sources with `core.autocrlf=false` so that span and
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

## Merge queue

`main` is protected by a GitHub merge queue. The CI workflow listens for `merge_group` as well as `pull_request` and pushes to `main`, so every queued group runs the same three jobs on the exact commit that will land. The repository ruleset for `main` requires these status checks, named as the jobs report them:

| Required check | Job |
| --- | --- |
| `check` | Linux reference lane |
| `macOS (AArch64)` | `platform` matrix entry |
| `Windows (x86_64 MSVC)` | `platform` matrix entry |

Renaming a job or matrix entry renames its check, so update the ruleset in the same change, or the queue waits forever on a check that never reports. A lane that is known to be red on `main` cannot be required until it is fixed, because every group would fail.
