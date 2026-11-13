# Pulse fibers on Windows x86_64

Lane 22 of the [sixth-wave prompts](../agent-prompts/wave-six/22-windows-fibers.md),
claim issue [#264](https://github.com/robot-head/vesper/issues/264). The branch
is `claude/project-thread-ywo2mo` (the suggested name was
`wave6/windows-fibers`). The base is
`07322e3ba47890e939a48967a1ea8f62fdd7aac1`, a descendant of the required
`cda092f`.

The work edited only `crates/vesper-fiber-abi/**` (new Windows files plus
narrow `cfg` dispatch lines) and this report, plus the one-line `Cargo.lock`
root patch described below. `vesper-pulse` was not changed. `ABI_VERSION` is
still 2. The `repr(C)` context, evidence and data layouts, and the Linux and
macOS public signatures and switch frames, are unchanged.

## What was built

1. **Platform gate.** `build.rs` now sets `cfg(vesper_direct_fibers)` for
   `windows`/`x86_64` too (MSVC and GNU environments). `fiber`, `stack`,
   `switch`, `fpenv`, the root re-exports and `JitPrototype` therefore build
   and run on Windows. `pulse` (the scheduler adapter and `NativeRuntime`)
   stays Linux-only; see the gaps below.
2. **Pinned stacks** (`src/stack_windows.rs`, selected with
   `#[cfg_attr(windows, path = ...)]` in `lib.rs`).
   - One `VirtualAlloc(MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)` per stack.
     From low to high it holds a 20 KiB committed *overflow reserve*, one
     `PAGE_READWRITE | PAGE_GUARD` page, and the usable stack `[base, top)`.
     `guard_len()` is the reserve plus the guard page (24 KiB with 4 KiB
     pages). It is not one page as on Linux and macOS.
   - The overflow reserve is where Windows dispatches `STATUS_STACK_OVERFLOW`
     for a fiber, so vectored handlers and the unhandled-exception path have
     room to run. Its size matches the `SetThreadStackGuarantee(0x5000)` that
     Rust's standard library asks for on its own threads.
   - Commit is eager. Each fiber is charged `size + guard_len` bytes of
     commit, about 280 KiB at the default size. Stacks do not grow through
     guard pages on demand. This is deterministic: a fiber can never fail to
     commit stack in the middle of a run.
   - The new API, additive and Windows-only: `TebLimits`,
     `FiberStack::teb_limits()`, `FiberStack::allocation_base()` and
     `OVERFLOW_RESERVE`.
3. **Context switch** (`src/switch_windows.rs`). It has the same
   `vesper_fiber_switch(save_sp, load_sp, arg) -> arg` contract as on the
   other targets, with arguments in rcx, rdx and r8.
   - It saves exactly the state that the Microsoft x64 convention makes
     non-volatile: rbx, rbp, rdi, rsi, r12–r15, all 128 bits of xmm6–xmm15,
     the MXCSR (control bits plus sticky status, as on Linux) and the x87
     control word.
   - It also switches four TEB fields with each context, as `SwitchToFiber`
     does: `NT_TIB::StackBase` (`gs:[0x08]`), `NT_TIB::StackLimit`
     (`gs:[0x10]`), `DeallocationStack` (`gs:[0x1478]`) and
     `GuaranteedStackBytes` (`gs:[0x1748]`). Windows uses these to tell a
     stack overflow apart from a stray guard-page hit, and the exception
     dispatcher uses them to validate frames.
   - The frame is 288 bytes and private to the target; generated code never
     reads it. Its layout is tabulated in the module docs. The first word
     keeps the ABI v2 x86_64 layout (MXCSR, then the x87 control word).
4. **Fiber start and unwinding.** `vesper_fiber_start` is the outermost
   frame of every fiber. It reserves the callee's 32-byte shadow space and
   carries real x64 unwind information (`.seh_proc` / `.seh_stackalloc 32`).
   Its "return address" slot is zero, so `RtlVirtualUnwind`, exception
   dispatch and backtraces stop cleanly at the base of the fiber stack
   instead of walking into the scheduler's frames.
   - `vesper_fiber_switch` has no unwind information. This is the documented
     alternative to unwind info: a resumed context has already returned from
     the switch, so no exception is ever dispatched through it.
   - New fibers start from `FpEnv::DEFAULT` with xmm6–xmm15 zeroed.
   - On Windows, `FpEnv::DEFAULT` is MXCSR `0x1F80` with x87 control word
     `0x027F` (53-bit precision, the Windows x64 default), not `0x037F`.
5. **Structured exceptions** (`src/fault.rs`). The policy is report, then
   fail.
   - Creating a fiber stack installs a first-in-line vectored exception
     handler (once per process).
   - The switch writes a header at `[StackBase - 16]` (a magic value, then
     the context pointer). From that header, the handler can tell from the
     TEB alone whether a fault happened on a fiber stack, and on which
     fiber.
   - For hardware faults (access violation, stack overflow, guard page,
     illegal or privileged instruction, integer and FP divide by zero, and
     similar), the handler:
     - marks the context `FiberState::Poisoned`;
     - records the fault for `fault::last_fault()`;
     - prints one line to stderr naming the fiber, the NTSTATUS code and the
       faulting address;
     - then continues the search.
   - With no other handler, the process ends with the exception code as its
     exit status, as it would for a fault on an ordinary thread stack.
   - Because the TEB limits describe the fiber stack, dispatch is
     well-formed: it runs on the fiber stack and never touches the scheduler
     stack.
   - Software exceptions are not reported: Rust panics (which still abort at
     the `extern "C"` entry boundary), C++ exceptions and breakpoints.
   - A fault is never resumed, skipped or turned into a value.
6. **Tests.**
   - `tests/platform.rs` is now `#![cfg(all(vesper_direct_fibers, unix))]`.
     Its probes are written for the System V and AAPCS64 registers and use
     Unix signals.
   - The new `tests/windows_platform.rs` has 11 tests:
     - **Guard page from the host.** Touching the guard page from the host
       thread fails with `STATUS_GUARD_PAGE_VIOLATION` (0x80000001) and is
       not attributed to a fiber.
     - **Fiber overflow.** A fiber that recurses past its stack fails with
       `STATUS_STACK_OVERFLOW` (0xC00000FD), and the handler reports it. The
       kernel only raises that code when the TEB limits describe the running
       fiber's stack, so this test discriminates the TEB switch.
     - **Access violation after a resume.** A fault after one park/resume
       ends with 0xC0000005, reported under that fiber's id.
     - **TEB ownership.** Two interleaved fibers each see their own TEB
       stack fields across parks, the host's fields are restored after every
       switch, and the stack header names the running fiber.
     - **Register isolation.** Assembly probes on both sides of the switch
       load and check 28 register halves: 8 GPRs plus both 64-bit halves of
       xmm6–xmm15. Three negative controls must count exactly 4, 1 and 28
       mismatches: r13, rdi and xmm9; the upper half of xmm15 only; and
       everything.
     - **FP ownership.** Each context owns its rounding mode and sticky
       flags.
     - Stack layout, fault classification, and the poison-free state of a
       normally parked fiber.
   - On Windows, `round_trip.rs` (Cranelift JIT park/resume at both
     optimisation levels, stale and repeated tokens, code lifetime),
     `fiber_lifecycle.rs`, `objects.rs` and the unit tests now run too.
     Cranelift picks `WindowsFastcall` through `default_call_conv()`.

## Mutation checks

Each mutation was applied to `switch_windows.rs`, `windows_platform.rs` was
built for `x86_64-pc-windows-gnu` and run under Wine 9.0, and then the
mutation was reverted:

| Mutation | Failing tests |
| --- | --- |
| drop the four TEB stores on restore | `fiber_overflow_is_a_stack_overflow_reported_for_that_fiber`, `access_violation_in_a_resumed_fiber_is_reported_with_its_identity`, `teb_stack_limits_are_owned_by_each_context` |
| drop `movaps xmm12, [rsp + 144]` | `callee_saved_registers_survive_interleaved_switches` |
| `pop rsi` → `add rsp, 8` | `callee_saved_registers_survive_interleaved_switches` |
| drop `ldmxcsr [rsp]` | `fp_rounding_and_status_flags_are_owned_by_each_context` |

## Validation

### Local (x86_64 Linux cloud container, rustc 1.97.0, 2026-11-13)

```sh
cargo fmt --all -- --check                                                          # ok
cargo clippy -p vesper-fiber-abi --all-targets --target x86_64-pc-windows-msvc -- -D warnings   # ok
cargo clippy -p vesper-fiber-abi --all-targets --target x86_64-pc-windows-gnu  -- -D warnings   # ok
cargo clippy --workspace --locked --all-targets -- -D warnings                      # ok (Linux)
cargo test -p vesper-fiber-abi --locked                                             # Linux, unchanged counts
cargo test --workspace --locked                                                     # Linux, see PR
# Windows ABI execution under Wine 9.0 (mingw-w64 GNU target, debug and release):
cargo build -p vesper-fiber-abi --target x86_64-pc-windows-gnu --tests [--release]
wine <each test exe>
#   windows_platform 11 passed, round_trip 14 passed, fiber_lifecycle 4 passed,
#   objects 4 passed, unit tests 9 passed (both profiles)
```

Wine checks the code generation, the Microsoft x64 calling convention, the
COFF assembly and Wine's model of the TEB and of exception dispatch. It is
not Windows. One assertion is skipped under Wine and runs only on Windows:
the host guard-touch exit code. Wine's debugger resumes the faulting thread
after an unhandled guard-page violation, so the write then completes. The
test detects Wine through `ntdll!wine_get_version`.

### Windows CI

The `Windows (x86_64 MSVC)` job runs `cargo clippy --workspace --locked
--all-targets -- -D warnings` and `cargo test --workspace --locked` on
`windows-latest`. Those commands now include every direct-fiber test above.
The workflow did not need to change. The job URL and its per-binary results
are recorded in the PR.

## Root patch

`Cargo.lock` has one new line: `vesper-fiber-abi` now depends on
`windows-sys 0.61.2`. That version was already locked for
`vesper-loom-cas`. The crate declares it as a `cfg(windows)` target
dependency with the features `Win32_Foundation`,
`Win32_System_Diagnostics_Debug`, `Win32_System_Kernel`,
`Win32_System_LibraryLoader`, `Win32_System_Memory` and
`Win32_System_SystemInformation`. Root `Cargo.toml`, CI and Bazel files are
unchanged. Bazel derives target dependencies from Cargo metadata, as it
already does for `vesper-loom-cas`.

## Decisions

- **Hand-written switch, not `CreateFiber`/`SwitchToFiber`.** The ABI keeps
  raw `saved_sp` and `scheduler_sp` in `FiberContext`, and the other targets
  switch on them. OS fibers would make those fields meaningless and would
  also convert the creating thread (`ConvertThreadToFiber`). The cost is
  switching the four TEB fields by hand.
- **Eager commit and a fixed guarantee.** See item 2. A lazily committed
  stack would need the kernel's guard-page growth path, and its exact
  thresholds are undocumented.
- **Report, then fail** for faults, rather than recovering a poisoned fiber
  (item 5). Abandoning a faulted fiber's frames without unwinding could leave
  Rust state half-updated. The Vesper source rules give no meaning to a
  hardware fault inside a linear context.
- **Separate Windows test file** rather than `cfg` blocks spread through
  `platform.rs`. The calling convention, probe registers and fault reporting
  all differ.

## Remaining gaps

- **The Pulse scheduler adapter is not on Windows.** `vesper-pulse`'s
  scheduler is written against the io_uring `Request`/`Completion` types in
  `io_driver.rs`, and both modules are `cfg(target_os = "linux")`. A Windows
  completion driver (an IOCP backend, for example) and the refactor that
  makes the scheduler independent of io_uring types are outside this lane's
  boundary (Windows-specific files only). So `fp_env.rs`, `lifecycle.rs` and
  `scheduler.rs` stay Linux-only. `vesper-pulse`'s portable `frames` tests
  already ran on Windows.
- **Fiber-local storage is not switched.** `FlsAlloc` data and
  `NT_TIB::FiberData` are untouched.
- **AVX upper halves are not preserved** (they are volatile in the Microsoft
  convention), and x87 data registers must be empty across a switch.
- **CET shadow stacks are unsupported.** The switch returns onto a
  different stack, so binaries must not be built `/CETCOMPAT`. Rust does not
  build them that way by default.
- **The native executable driver still does not link with MSVC.** It is
  unchanged by this lane, and the CI workflow still skips the host-native
  step on Windows. Object emission for `x86_64-pc-windows-msvc` was not
  added.
- **Some RFC text predates this port.** `docs/rfcs/native-pulse-abi.md`'s
  platform table and the "Linux only" limitation do not mention Windows yet.
  That file is outside this lane's paths. This report is the Windows
  appendix until the coordinator folds it in.
- **No Vesper typing claim.** Rust ownership of `Fiber` and `Parked` is not
  evidence that the Vesper source typing rules are implemented. This lane
  changes no source typing.
