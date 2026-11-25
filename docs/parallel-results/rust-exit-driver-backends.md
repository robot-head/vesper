# Rust exit: the driver's backend steps in Vesper (#1895)

Group 4 of removing `HybridDriver::hd_step` (#1367, split in #1896): the
steps `WasmBuild`, `EbpfBuild` and `VerifiedBuild` of
crates/vesper-hybrid/src/driver/backends.rs.

## Part 1: wasm and eBPF builds

The Vesper driver (crates/vesper-hybrid/modules/driver) now runs both builds
itself. Until the front end moves to Vesper (#1893), the checked world comes
from the Rust session as words, the same words the hybrid's `wasm` and `ebpf`
swaps already take.

- **`vesper build --target wasm32-wasip2`:** step `WasmInput` (60) returns
  the `wasm` swap's input. The driver compiles it with
  `@vesper/wasm-backend` (`wasm_backend_module`, new) and wraps it with
  `@vesper/wit-component` (`wit_component_command`, new). It writes the
  component with `@vesper-std/fs` `replace_path` (atomic) and prints the
  result line.
- **`vesper build --target bpf`:** step `EbpfInput` (61) parses, checks and
  encodes the source, as `ebpf_backend::compile_source` does. The driver
  calls `@vesper/ebpf-backend` `ebpf_backend_build` (new), which compiles,
  runs the `@vesper/ebpf` verifier (`verify_program`, after
  `map_validate`) and writes the ELF object. The writer is the new
  packages/vesper-ebpf-backend/src/object.ves: `btf_ext::build`/`maps_btf`
  and the `object` 0.37 ELF layout. The driver then writes the object with
  `@vesper-std/fs` and prints `build_command`'s summary.
- **Fallbacks:** a world that does not encode, a backend error or construct
  not ported yet, a verifier rejection, a WIT the wrapper declines and a
  failed write all go to the old Rust step. That keeps every message
  Rust's. When the wasm backend itself declined, the Rust step skips the
  `wasm` swap, which would only decline again.
- **Swap sets:** both paths need their swap installed, so under
  `VESPER_HYBRID_SWAPS=gated` or `none` the builds stay on Rust.
  `VESPER_HYBRID_DRIVER_WASM=0` and `VESPER_HYBRID_DRIVER_EBPF=0` keep the
  old Rust step (the differential baseline). `VESPER_HYBRID_STATS=1`
  reports `driver built N wasm components / eBPF objects in Vesper (M fell
  back to Rust)`.

### Evidence (Linux x86_64, release, `RUSTFLAGS` default)

Tests: crates/vesper-hybrid/tests/driver.rs,
`wasm_build_matches_the_rust_pass` and `ebpf_build_matches_the_rust_pass`.
`driver_matches_the_default_compiler` also gained three wasm option sets.

| Check | Result |
|---|---|
| wasm: 417 corpus programs x {opt, `--no-opt`} = 834 runs | 316 components, 176 built in the Vesper driver; **0 differ** from the Rust step (exit status, stdout, stderr, bytes) |
| wasm vs the default compiler | 0 unexplained; 12 runs are the wasm swap's known extensions (loops, u128; tests/self_host_wasm_backend.rs runs them) and 3 the optimizer's accepted equal-cost ties |
| eBPF: fixtures x every entry, default output path, truncated fixtures, every corpus `main` = 457 runs | 9 objects, all built in the Vesper driver; **0 differ** from the Rust step and from the default compiler |
| driver differential (22 inputs x 39 option sets) | 858 runs, 0 differ |

ABBA timing compares the hybrid on the Vesper driver path with the hybrid on
the Rust step. Each program ran 6 (wasm) or 10 (eBPF) times in alternating
order. The table gives the geometric mean of per-program median ratios, with
a 95% bootstrap CI.

| Build | Programs | Ratio | 95% CI |
|---|---|---|---|
| wasm, built in Vesper | 88 | 1.022x | 1.012..1.033 |
| wasm, fell back | 105 | 0.996x | 0.990..1.002 |
| wasm, all | 193 | **1.008x** | 1.002..1.014 |
| eBPF, built in Vesper | 3 | 0.987x | 0.969..1.008 |
| eBPF, rejected (falls back) | 22 | 1.261x | 1.232..1.292 |
| eBPF, all | 25 | **1.224x** | 1.178..1.269 |

The eBPF fallback costs more because it parses, checks and compiles twice:
once for the Vesper attempt and once in the Rust step that reports the
error. That only happens on programs that fail to build, and it goes away
once the front end is in Vesper.

- **Differences:** 0.
- **Rust lines deleted:** 0. The Rust steps stay as the fallback until
  the front end (#1893) and the backends' error reporting are in Vesper.
  The whole driver goes at the Stage 2 retirement (#1397).

## Next

- `VerifiedBuild`, after the verified-codegen port (#1886).
- Errors in Vesper's own words (wasm `WasmError`, eBPF `EbpfBackendError`
  and verifier messages), so the fallbacks can go.
