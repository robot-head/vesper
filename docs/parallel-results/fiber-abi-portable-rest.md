# Fiber ABI: portable Pulse tests, kernel write tracking and deep unwinding (wave eight, lane 19)

Claim: #468. Design sections §12 (native fibers, Pulse) and §31 (Chrono).
Baseline: `1094e64` (origin/main, a descendant of the required `ea1ed29`;
the dependency PR #441 is merged). Exclusive paths:
`crates/vesper-fiber-abi/**`, `crates/vesper-chrono/**` (additive), this
report. No root patch: the new `gimli` use goes through Cranelift's own
re-export (`cranelift_codegen::gimli`, 0.32.3 with `read`/`write`), so
`Cargo.toml`, `Cargo.lock` and `BUILD.bazel` files are unchanged.

## What was built

### 1. The Pulse adapter and its tests on macOS and Windows

* `vesper_fiber_abi::pulse` is built on every direct-fiber target (Linux
  x86_64/AArch64, macOS AArch64, Windows x86_64), not only Linux. The
  adapter needed no logic change: it only uses the backend-neutral
  `vesper_pulse` protocol that PR #363 made portable.
* New helpers: `pulse::null_file()` (`/dev/null`, or `NUL` on Windows) and
  `pulse::cancelled_code()` (the host drivers' cancellation code: 125 on
  Linux, 89 on macOS, 995 = `ERROR_OPERATION_ABORTED` on Windows).
  `pulse::ECANCELED` stays the Linux value and says so.
* `tests/lifecycle.rs`, `tests/scheduler.rs` and `tests/fp_env.rs` are gated
  on `vesper_direct_fibers` instead of Linux. Fixture drivers complete
  cancelled requests with `vesper_pulse::driver::cancelled_error()`.
* The kernel tests run on **every driver the host starts**
  (`select::host_order()` + `AnyDriver::start`), and `select::open(Auto)`
  must succeed:
  * `kernel_round_trip_on_every_host_driver`: a JIT fiber reads a regular
    file and the null file (end of file) through the real driver.
  * `kernel_cancellation_during_io_on_every_host_driver`: a read parked on
    a pipe nobody writes stays owned by the driver, then `shutdown`
    delivers its one `DuringIo` cancellation. Windows uses
    `vesper_pulse::iocp::overlapped_pipe()` (IOCP completes synchronous
    handles inline, so an anonymous pipe would block `submit`).

### 2. Kernel-assisted write tracking (`write_watch`, Chrono `tracked_arena`)

`vesper_fiber_abi::write_watch` (new; Linux, macOS, Windows; unsafe code is
confined to it, as the assignment allows):

* `TrackedRegion::new(len)`: a private page-aligned anonymous mapping.
  `arm()`, `take_dirty() -> Vec<page index>` (ends the epoch and starts the
  next), `disarm()`, safe `read`/`write`, `as_mut_ptr()` for raw stores,
  `serviced_faults()`, `stats()`. `Mechanism::HOST`, `page_size()`,
  `MAX_REGIONS` (64 live regions per process on Unix).
* Linux and macOS: **`mprotect` faults**. Arming makes the region read-only;
  a process-wide `SIGSEGV`/`SIGBUS` handler (`SA_SIGINFO | SA_ONSTACK`,
  installed once) looks the fault address up in a lock-free slot table,
  sets the page's bit in an atomic bitmap and makes just that page writable,
  so the store restarts and succeeds: at most one fault per page per epoch.
  A fault outside every tracked region is handed to the previous handler
  (Rust's stack-overflow handler, for example) or, for the default action,
  re-raised with it. `take_dirty` re-protects only the pages that were
  written.
* Windows: **write watches**. The region is `VirtualAlloc`ed with
  `MEM_WRITE_WATCH`; `GetWriteWatch(WRITE_WATCH_FLAG_RESET)` reads and
  clears the written pages; `ResetWriteWatch` arms. No handler runs.

`vesper_chrono::tracked_arena::TrackedArena` (new, additive; still no unsafe
code in Chrono): checkpointable memory in a `TrackedRegion` with shards of
whole pages. `checkpoint()` turns the dirty pages into an `ArenaDelta` (the
existing versioned, BLAKE3-trailed format) holding just the written shards;
`keyframe()`, `apply()` (replay a delta or keyframe), `restore(keyframe)`
and `capture(recorder, time, keyframe_every)` (snapshot epochs, keyframe
every k-th epoch, as `capture_arena_epoch`). Its images replay onto a
`ShardArena` and through `rebuild_arena` unchanged. `probe_write_tracking`
now exercises the host mechanism on a one-page region and reports it as
`used`; userfaultfd and Mach `vm_copy` stay reported, not used.
`Unavailable::KernelWriteTracking`'s reason now names only what is left.

### 3. Register unwinding below the switch frame (`unwind`)

* **Park entry without a frame.** `vesper_fiber_park` is now a naked
  function (one per ABI: System V x86_64, Microsoft x64, AAPCS64/Apple
  arm64). It calls `park_prepare` (the former Rust body: null, magic,
  version, state and generation checks, then `Parked` at the next
  generation) and then tail-jumps into `vesper_fiber_switch` with the stack
  pointer it was called with. The saved switch frame's return address is
  therefore the park call site in the *parking* function, not a Rust frame
  inside the runtime. The resumer passes `&ctx.resume` as the switch
  argument, which the switch returns to the parked code; a fresh fiber's
  start trampoline ignores it. Signature, null-on-rejection contract,
  `repr(C)` layouts, offsets and switch frames are unchanged (ABI v2).
* **CFI for generated code.** The prototype JIT records, for every function
  it defines, Cranelift's System V unwind information
  (`create_unwind_info_of_kind(isa, SystemV)`, which Cranelift emits for any
  calling convention of the architecture, the Windows fastcall included)
  and builds a `unwind::CfiTable`: `.eh_frame` bytes written with gimli at
  the functions' final addresses, plus their names and ranges
  (`JitPrototype::cfi()`).
* **The walk.** `unwind::unwind(&StackImage, &RegisterFrame, &CfiTable,
  max_frames) -> Backtrace` starts from the switch frame (callee-saved
  registers, `sp = saved_sp + SWITCH_FRAME_BYTES`, `pc` = return address)
  and, per frame, finds the FDE for `pc - 1`, evaluates the CFA rule and the
  register rules (`Offset`, `ValOffset`, `Register`, `SameValue`,
  `Undefined`, `Constant`), reads saved slots from the stack image only, and
  moves to the caller. Each `UnwoundFrame` has `pc`, `sp`, `cfa`, the
  function name and the callee-saved GPRs it sees (rbx rbp r12–r15, plus
  rsi rdi on Windows; x19–x29). It works on a live `Parked` and on a decoded
  `FiberImage` identically. Stops: `Stop::NoCfi { pc }` (normally the host
  `fiber_main` that called the generated entry), `Stop::End`,
  `Stop::FrameLimit`. Errors: `Unreadable`, `BadCfa` (CFA not moving toward
  the stack top, or past it), `Unsupported` (DWARF expressions), `Cfi`,
  `WrongAbi`, `Truncated`.
* **Deep fixture.** `JitPrototype::spawn_deep(_with)`: `deep_entry` calls
  `deep_mid` calls `deep_leaf`, which parks on `Async::yield_now`. Each
  frame stores `get_stack_pointer`/`get_frame_pointer` at its call site
  (the JIT now sets `preserve_frame_pointers`), and `deep_entry`/`deep_mid`
  each load a creator-supplied secret before their call and add it after,
  so it must survive the call in a callee-saved register.

## Tests

| Test (crate) | What it checks |
| --- | --- |
| `fiber-abi tests/lifecycle.rs` (9) | as before, now on every direct-fiber host; kernel cancellation during I/O on every host driver |
| `fiber-abi tests/scheduler.rs` (8) | as before; kernel round trip (file and null file) on every host driver |
| `fiber-abi tests/fp_env.rs` (1) | per-fiber rounding modes, now on macOS and Windows |
| `fiber-abi tests/write_watch.rs` (4; 3 on Windows) | raw-pointer stores tracked per page and once per epoch; spanning writes; epochs; disarm; two regions and 200 sequential regions; on Unix a foreign fault still kills a child with `SIGSEGV`/`SIGBUS` |
| `fiber-abi tests/unwind.rs` (3) | deep fixture at both opt levels unwinds to `[leaf, mid, entry]` then `NoCfi`; every recovered `sp` and `rbp`/`x29` equals what the frame recorded; each caller's recovered registers hold its secret; frames nest (`cfa == caller sp`); the fiber still resumes to the right result; an encoded image unwinds identically; frame limit, truncated image and empty table give typed results |
| `fiber-abi tests/stack_image.rs` (6) | updated: the switch frame now returns into the generated function (`pulse_read`), found through the CFI table |
| `chrono tests/tracked_arena.rs` (3) | raw stores reach the delta (shards 2 and 4), deltas replay onto a `ShardArena` replica; snapshot epochs with keyframes rebuild; restore to a keyframe; a tracked arena replays; geometry, bounds, shape and epoch errors |
| `chrono tests/arena.rs` (6) | probe: exactly the host mechanism is used and was exercised |

## Checks run

Commands, all from the repository root with
`CARGO_TARGET_DIR=/home/user/target CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`:

* `cargo fmt --all -- --check`: clean.
* `cargo +1.100.0 clippy -p vesper-fiber-abi -p vesper-chrono --locked
  --all-targets -- -D warnings`: clean. Also clean with stable for
  `--target x86_64-pc-windows-gnu` (both crates) and
  `--target aarch64-apple-darwin` (`vesper-fiber-abi`; Chrono's blake3 C
  code cannot be cross-compiled for Darwin here).
* `cargo test -p vesper-fiber-abi -p vesper-chrono --locked` on Linux
  x86_64 (kernel 6.18; io_uring and epoll both validated): all pass.
* Windows x86_64 under Wine 9.0 (`x86_64-pc-windows-gnu`): `unwind` 3/3,
  `stack_image` 6/6, `windows_platform` 11/11, `round_trip` 14/14,
  `fiber_lifecycle` 4/4, `lifecycle` 9/9 (IOCP cancellation validated),
  `scheduler` 8/8 (IOCP round trip), `fp_env` 1/1, `write_watch` 3/3,
  Chrono `tracked_arena` 3/3 and `arena` 6/6.
* Linux AArch64 under QEMU 8.2 user mode: `unwind` 3/3, `stack_image` 6/6,
  `round_trip` 14/14, `fiber_lifecycle` 4/4, `lifecycle` 9/9, `scheduler`
  8/8, `fp_env` 1/1, `platform` 5/5. The `write_watch` tests that store
  *unaligned* bytes into an armed page abort QEMU itself ("QEMU internal
  SIGSEGV {code=ACCERR}"): QEMU's slow path for unaligned guest stores
  faults on the host instead of raising the guest signal. Aligned stores
  are tracked under QEMU; real AArch64 runs on the macOS CI job.
* `cargo test --workspace --locked`: see the PR description for the result
  on the final head.
* macOS AArch64 and Windows MSVC runs come from the required CI jobs on the
  PR (not available locally).

## Decisions

* **Naked park instead of host CFI.** Unwinding through the Rust
  `vesper_fiber_park` frame would have needed the host binary's CFI on
  every OS (`.eh_frame_hdr` on Linux, compact unwind on macOS, `.pdata` on
  Windows). Making park frameless leaves only generated frames between the
  switch frame and the entry, and those have DWARF CFI on every target.
  The cost is that `park_prepare`'s call from the naked entry has no unwind
  information on Windows; nothing raises an exception there (the helper
  only reads and writes the context).
* **System V CFI on Windows.** Cranelift's Windows unwind codes describe the
  same prologue; generating the System V kind for the JIT keeps one
  unwinder for all targets. Windows' own `RtlVirtualUnwind` still sees no
  table for JIT code (as before).
* **mprotect, not userfaultfd.** userfaultfd write-protect needs
  `CAP_SYS_PTRACE` or `vm.unprivileged_userfaultfd=1` (CI runners usually
  have neither) and a fault-servicing thread or the 6.7+ asynchronous mode;
  `mprotect` faults need no privilege and are the same mechanism on macOS.
* **No new dependency.** gimli comes from `cranelift_codegen::gimli`.
* Rust ownership of `TrackedRegion`/`TrackedArena` is not evidence that the
  Vesper source typing rules for region writes or linear obligations are
  implemented.

## Remaining gaps

* Host Rust frames are not unwound: the walk stops at `fiber_main`
  (`Stop::NoCfi`). Loading the host's CFI (Linux `.eh_frame_hdr`, macOS
  compact unwind, Windows `.pdata`) would extend it to the scheduler side.
* Vector callee-saved registers (d8–d15, xmm6–xmm15) are reported for frame
  0 only; DWARF expressions in CFI are rejected as `Unsupported` (Cranelift
  emits none).
* Write tracking: userfaultfd write-protect and Mach `vm_copy` are not used;
  on Unix a `read(2)` straight into an armed page fails with `EFAULT`
  rather than being tracked (the kernel does not fault for its own
  accesses); at most 64 regions per process; the `ShardArena`
  copy-on-write path still tracks writes at its API.
* `TrackedArena` copies dirty shards into each delta; it does not share
  them copy-on-write with checkpoints as `ShardArena` does.
