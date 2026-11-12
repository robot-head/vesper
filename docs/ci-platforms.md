# CI platform lanes

CI runs one reference lane and two platform lanes on every push and pull request
(`.github/workflows/ci.yml`).

| Lane | Runner | Runs |
| --- | --- | --- |
| `check` (Linux x86_64) | `ubuntu-latest` | fmt, Clippy, workspace tests, Lean, AArch64 objects under QEMU, host native executables |
| macOS (AArch64) | `macos-latest` (Apple silicon) | Clippy, workspace tests, Lean, host native executables on real AArch64 hardware |
| Windows (x86_64 MSVC) | `windows-latest` | Clippy, workspace tests, Lean |

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
| `vesper build` native executables | `cc` links ELF | `cc` links Mach-O; examples run natively | Not supported: the driver invokes `cc ... -lm`, which MSVC hosts lack. `native_executable` is ignored on Windows |
| AArch64 object validation | Cross ELF objects under QEMU | Native hardware via `vesper build` | n/a |
| JIT (`vesper jit`) | Tested | Tested | Tested |
| Lean extraction | Tested | Tested | Tested |

## Pending crates (open milestone PRs)

These crates carry a temporary standalone `[workspace]` and are not yet root
members, so CI does not see them. When the integration owner registers one
(rule 3), it joins every lane automatically through `cargo test --workspace`.
What each lane will then cover, and the gating it needs:

| PR | Crate | Expected platform coverage once registered |
| --- | --- | --- |
| #2 | `vesper-fiber-abi` | Stacks (`mmap`) and context switch (`global_asm!`) are `cfg(target_os = "linux")` with x86_64/AArch64 only, so macOS and Windows build only the portable ABI/data layer. A macOS AArch64 port (Mach-O symbol naming, `libc::mmap` on Darwin) is the next step to use Apple-silicon runners as real fiber hardware; Windows needs a separate fiber backend. `experiments/native-pulse` stays outside the workspace and needs its own Linux step if it should run in CI. |
| #4 | `vesper-pact` | Pure Rust; expected to pass on all three lanes. |
| #5 | Lean effects and places | Lean-only; covered by the Lean steps on all lanes. Add any new `.lean` files to every lane's Lean step list. |
| #6 | `vesper-loom-cas` | Tested on Linux only so far. macOS takes the `link`+`unlink` publish path; Windows has no `cfg(unix)` permissions and no executable bits. Expect platform failures to surface here first; gate with `cfg_attr(..., ignore = "...")` per the policy above rather than excluding the crate. |
| #7 | `vesper-loom-sandbox` | Wasmtime host is portable. Its guest fixtures need the `wasm32-wasip2` (and `wasm32-wasip1`) Rust targets: add `targets: wasm32-wasip2, wasm32-wasip1` to the toolchain step of every lane, or commit prebuilt fixtures. Symlink-escape tests are Unix-only. |
| #8 | `vesper-ignis` | GPU-independent planner, pure Rust; expected to pass everywhere. It is the planned target for later Windows (D3D12) acceptance, which will need a GPU-capable self-hosted runner, not `windows-latest`. |

Registering a crate also means adding it to `default-members` so plain
`cargo test` keeps the same coverage.
