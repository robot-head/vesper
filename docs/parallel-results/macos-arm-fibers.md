# macOS AArch64 pinned-fiber ABI port

Lane 6 of the [third-wave prompts](../agent-prompts/wave-three/06-macos-arm-fibers.md).
The branch is `claude/project-thread-4ufk2z` (the requested name was
`wave3/macos-arm-fibers`) and the PR is
[robot-head/vesper#25](https://github.com/robot-head/vesper/pull/25). The base
is `1ffd0aec27dfcc24c5564585ae9068255490bf61`, a descendant of the required
`02b3a1d`. The ABI contract is in [native-pulse-abi.md](../rfcs/native-pulse-abi.md),
Appendix A.

The work edited only `crates/vesper-fiber-abi/**`, this report and the RFC
appendix. It did not touch `src/native.rs`, the Pulse scheduler or driver, the
compiler, `experiments/native-pulse`, CI, or the root manifests and lockfile.
`ABI_VERSION` is still 2, and the v2 `repr(C)` layouts, offsets and Linux
public signatures are unchanged.

## What changed

1. **Platform gate.** A new `build.rs` sets `cfg(vesper_direct_fibers)` for
   Linux x86_64, Linux AArch64 and macOS AArch64. That cfg gates `fiber`,
   `stack`, `switch`, `fpenv`, the root re-exports and `JitPrototype`.
   `pulse` (the scheduler adapter and `NativeRuntime`) is gated on
   `all(vesper_direct_fibers, target_os = "linux")`. Direct fibers on macOS
   do not make the io_uring driver or the Pulse scheduler portable. Other
   targets, such as macOS x86_64 and Windows, keep only the portable
   evidence, data, ABI and object APIs.
2. **Switch and start trampolines.** The AArch64 switch and start bodies are
   now single `concat!` macros shared by the ELF and Mach-O wrappers, so the
   v2 frame cannot drift between platforms: 192 bytes, x19–x30 and d8–d15,
   then FPCR at +160 and FPSR at +168. The Linux wrapper keeps
   `.type`/`.size`/`.hidden`. Its disassembly of `vesper_fiber_switch` and
   `vesper_fiber_start` is byte-for-byte identical to the base, and the symbol
   table is identical too: switch is global, start is global hidden. The
   Darwin wrapper uses `_vesper_fiber_switch` (global),
   `_vesper_fiber_start` (`.private_extern`) and no ELF-only directives.
   - Darwin specifics: x18, the platform register, is never read or written.
     The callee-saved set and 16-byte SP alignment match AAPCS64. The initial
     x29 is 0, which terminates the frame-record chain.
   - macOS arm64 user code is not arm64e, so the saved x30 carries no PAC
     signature.
   - Unwinding across a switch is still unsupported, and panics still abort
     at the `extern "C"` boundaries.
3. **Pinned stacks.**
   - On macOS the mapping is `MAP_PRIVATE | MAP_ANON` with a `PROT_NONE`
     guard, then `mprotect`. XNU does not implement `MAP_NORESERVE`, so it is
     not passed.
   - Sizes and the guard round to the host page: 16 KiB on Apple silicon,
     4 KiB on typical Linux. The new `FiberStack::page_size()` exposes that
     page size.
   - `libc` is now a dependency on Linux and macOS.
4. **Floating-point state.** The ABI v2 contract is unchanged on Darwin:
   - Each context owns its FPCR (control) and FPSR (status).
   - A new fiber starts from `FpEnv::DEFAULT` with FPCR = 0 and FPSR = 0,
     whatever the creating thread is using.
   - Additive API: `fpenv::exception_flags()` (a portable bit encoding of
     the x86 MXCSR and AArch64 FPSR cumulative flags),
     `clear_exception_flags()`, `FLAG_INEXACT` and `FLAG_DIVIDE_BY_ZERO`.
5. **Object emission.**
   - `emit_object("aarch64-apple-darwin", _)` emits Mach-O with the
     `apple_aarch64` Cranelift calling convention and `_`-prefixed exported
     entries. All seven park sites import `_vesper_fiber_park` through
     `ARM64_RELOC_GOT_LOAD_PAGE21`.
   - Object generation is reported as object generation, not execution.
6. **Tests.**
   - `round_trip.rs` (Cranelift park/resume, repeated and stale tokens,
     handle moves, code lifetime, ABI violations) now runs on every
     direct-fiber target.
   - The fiber-level lifecycle tests moved unchanged from `lifecycle.rs` to
     the new `fiber_lifecycle.rs`, which is not Linux-gated. They cover
     cancel before start, cancellation re-park, `cancel_all` with stale
     tokens, and the counted drop fallback.
   - The scheduler and runtime tests (`lifecycle.rs`, `scheduler.rs`,
     `fp_env.rs`) stay Linux-only. The total test count went from 50 to 57,
     and none were removed.
   - The new `tests/platform.rs` has three checks:
     - Guard faults: the test re-executes itself as a child process. A
       write to the lowest usable byte succeeds. A direct touch of the guard
       page, and a running fiber recursing past its stack, both end with
       SIGSEGV or SIGBUS.
     - Register-precise callee-saved isolation: naked-asm probes load
       distinct seed patterns into x19–x28 and d8–d15 (rbx, rbp and r12–r15
       on x86_64) on the host side of `resume` and on the fiber side of
       `park`. After interleaved switches across two fibers, a check counts
       every register that lost its pattern. A negative control shows the
       probe detects a deliberate clobber.
     - FP ownership without a scheduler: the host runs toward-zero
       rounding, one fiber runs round-up with the inexact flag set, and the
       other runs round-down with the divide-by-zero flag set. Each context
       sees only its own mode and its own sticky flag, the arithmetic
       results match each mode, and the host's flags stay clear.
   - The new `objects.rs` Mach-O test checks the format, the symbols, the
     calling convention and the GOT relocations.

## Mutation checks (do the tests discriminate?)

On AArch64 Linux under QEMU (the switch body is shared with Darwin), each
mutation was applied to the switch, the platform tests were run, and the
mutation was reverted:

| Mutation | Failing test |
| --- | --- |
| `msr fpsr, x10` → `nop` | `fp_rounding_and_status_flags_are_owned_by_each_context` |
| restore of `d12, d13` → `nop` | `callee_saved_registers_survive_interleaved_switches` |
| restore of `x23, x24` → `nop` | `callee_saved_registers_survive_interleaved_switches` |

## Validation

These commands ran from the repository root with
`CARGO_TARGET_DIR=/home/claude/target-4ufk2z` (not shared), debug info off and
`CARGO_INCREMENTAL=0`, on an x86_64 Linux cloud container on 2026-11-12 with
rustc 1.97.0:

```sh
cargo fmt --all -- --check                                           # ok
cargo clippy -p vesper-fiber-abi --locked --all-targets --target x86_64-unknown-linux-gnu  -- -D warnings  # ok
cargo clippy -p vesper-fiber-abi --locked --all-targets --target aarch64-unknown-linux-gnu -- -D warnings  # ok
cargo clippy -p vesper-fiber-abi --locked --all-targets --target aarch64-apple-darwin      -- -D warnings  # ok
cargo clippy -p native-pulse-experiment --locked --all-targets --target aarch64-apple-darwin -- -D warnings # ok
cargo test -p vesper-fiber-abi --locked             # 57 passed
cargo test -p vesper-fiber-abi --locked --release   # 57 passed
cargo test -p native-pulse-experiment --locked      # ok
# AArch64 Linux execution under QEMU user-mode:
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu"
export VESPER_PROBE_RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu"   # child re-exec for guard probes
cargo test -p vesper-fiber-abi --locked --target aarch64-unknown-linux-gnu            # 57 passed
cargo test -p vesper-fiber-abi --locked --release --target aarch64-unknown-linux-gnu  # 57 passed
# Darwin cross-assembly (no linking): Mach-O rlib objects inspected with llvm-objdump
cargo build -p vesper-fiber-abi --lib --target aarch64-apple-darwin
```

The `aarch64-apple-darwin` rlib contains Mach-O 64-bit arm64 objects:
`_vesper_fiber_switch` is global, `_vesper_fiber_start` is private extern, and
`_vesper_fiber_park` is defined. Their disassembly matches the Linux body
exactly. Cross-checking proves assembly and type-checking only. It is not
Darwin execution, and neither is QEMU-Linux.

### Apple-silicon execution

**Validated in CI on real Apple silicon.** The evidence is
[run 43167667734, job "macOS (AArch64)"](https://github.com/robot-head/vesper/actions/runs/43167667734/job/131013234553)
on PR head `3602d47`. The GitHub `macos-26-arm64` image ran rustc 1.99.1 on
aarch64-apple-darwin.

- `cargo clippy --workspace --locked --all-targets -- -D warnings` passed.
- In `cargo test --workspace --locked`, every `vesper-fiber-abi` test ran
  natively and passed:

| Binary | Result on Darwin AArch64 |
| --- | --- |
| unit tests (incl. `fpenv` default env and flags, 16 KiB-page guard) | 9 passed |
| `fiber_lifecycle.rs` (cancel before start, cancellation re-park, `cancel_all` stale tokens, counted drop) | 4 passed |
| `objects.rs` (ELF + Mach-O) | 4 passed |
| `platform.rs` (guard faults, register isolation, FP mode/status ownership) | 5 passed |
| `round_trip.rs` (Cranelift JIT park/resume at both opt levels, stale/repeated tokens, code lifetime) | 14 passed |
| `fp_env.rs`, `lifecycle.rs`, `scheduler.rs` (Linux-only scheduler) | 0 (gated out, as intended) |

The acceptance items are covered by these tests:

- A real Cranelift/direct-fiber round trip: `round_trip.rs`.
- Cancellation re-park: `fiber_lifecycle.rs`.
- Independent host and fiber FP modes and status:
  `fp_rounding_and_status_flags_are_owned_by_each_context`.
- Default FPCR = 0 on the macOS test thread: `fpenv` unit tests.

The job as a whole is still red, and this PR did not cause it.
`vesper-loom-build`'s `tests/build.rs` fails two tests
(`a_prepared_build_runs_only_under_the_identity_that_keyed_it` and
`incompatible_engine_builder_or_host_identities_never_share_results`). The
same two tests fail identically on the base commit `1ffd0ae` on `main`
([run 43165628295](https://github.com/robot-head/vesper/actions/runs/43165628295/job/131006721273)).
That crate is outside this lane and untouched here. Because cargo stops at
the first failing test binary, the workspace packages after
`vesper-loom-build` and the "Validate host native executables" step did not
run on macOS in either run.

## Coordinator patch

None is required:

- No root `Cargo.toml` or `Cargo.lock` change. The crate is already
  registered, `libc 0.2` is already locked, and widening a target cfg or
  enabling `object`'s `macho` feature adds no lock entries. `cargo test
  --locked` passes.
- No CI change. The existing macOS (AArch64) platform job already runs
  `cargo clippy --workspace --all-targets` and `cargo test --workspace` on
  `macos-latest`, which now includes the Darwin fiber tests.

Optional follow-ups for the coordinator:

- In `docs/rfcs/native-pulse-abi.md`, the non-appendix §1 table row
  "Context switch … AAPCS64 Linux", §3's x18 sentence and §10's "Linux only"
  bullet now predate the port. Appendix A supersedes them for macOS AArch64.
  Rewording those global sections belongs to the coordinator under this
  lane's edit boundary.
- `docs/ci-platforms.md` could note that the macOS lane now executes direct
  fibers.

## Decisions

- **A build-script cfg instead of repeated `cfg(any(...))`.** It is one
  source of truth for "direct fibers exist here", and it keeps the gate
  separate from the Linux-only `pulse` gate.
- **A shared AArch64 body instead of a Darwin copy.** The frame offsets are
  ABI, and a single body keeps them in lockstep. The unchanged Linux
  disassembly is the regression evidence.
- **Naked functions only in tests.** The production trampolines keep
  `global_asm!` with explicit per-format directives, as the prompt asks
  ("symbol/assembler conventions"). The register probes use
  `#[unsafe(naked)]` plus `sym` so the test code needs no per-platform symbol
  spelling.
- **No new `experiments/native-pulse-macos` probe.** The CI macOS lane
  already executes the crate's tests on real hardware, and a new experiment
  would need coordinator workspace registration for no extra evidence.

## Remaining gaps and non-claims

- No kqueue or IOCP driver, no Windows fibers, no macOS x86_64 fibers, and
  no portable Pulse scheduler or `NativeRuntime` on macOS.
- No source suspension. The native source slice is still Linux (lane 2).
- No latency or performance claim for any platform.
- No nested fibers, work stealing, `Send` fibers or physical-stack
  relocation. The `!Send` compile-fail doctests run on every direct-fiber
  target.
- The guard page is not a stack-overflow guarantee. Generated code has no
  stack probes, and a frame larger than one 16 KiB page can skip the guard.
- The JIT relies on `cranelift-jit`'s RW→RX `mprotect` path, which macOS
  allows for processes without the hardened runtime. A hardened or sandboxed
  app would need a `MAP_JIT`/`pthread_jit_write_protect_np` memory provider,
  and that was not tested.
- SVE and SME state is still unsupported, as on Linux.
