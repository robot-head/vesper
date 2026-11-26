# Rust exit SH6 lane 13: runtime glue on libc and system calls

Claim #1408, prompt `docs/agent-prompts/self-host-sh6/13-runtime-glue.md`
(on the plan branch of #1415 until it merges). Base: `main` at `ab965f2a`
(2026-11-21).

This is **part 1, the audit**. It lists, for each of the six crates, what it
needs from the OS or a C library, what the compiler cannot express yet, and
the order the ports can land in. Nothing is flipped or deleted yet. The
gaps have minimal repros, checked with `vesper check` / `vesper build` at the
base commit on Linux x86_64.

## Summary

| Crate | Non-test Rust | Compiler-linked | What it is | Can it go Vesper now? |
|---|--:|---|---|---|
| `vesper-std-host` | 1,073 | yes (`vesper jit`/`run` bind its symbols) | 39 `extern "C"` functions over `std::fs`, `std::process`, `std::env` behind `@vesper-std/{fs,process,env}` | File I/O yes (proved below). Directory listing, environment, arguments and spawning need G1, G2 and G4; the JIT needs G5; Windows needs G7 and G11. |
| `vesper-std-driver` | 70 (+123 test) | no | The sample driver is already Vesper (`vesper/src/lib.ves`); the Rust is a 16-line re-export, a generated binding and the test harness | Goes with `vesper-std-host`: once the driver is a `vesper build` executable over libc, the Rust re-export and binding go and the test runs the executable. |
| `vesper-ffi` | 4,972 (+2,939 test) | yes (the compiler parses every `.vsys` through it) | The `.vsys` parser, C layout and wire-layout validation, assumption checks (pure logic, `unsafe` denied), plus `isolation::hw` (PKU/POE protection keys, contained children) | The pure part (`sys/**`, `lib.rs` wire views, about 4.1k lines) can be ported now as a compiler package with the seed pattern: it needs only bytes, maps and BLAKE3, which `@vesper-std/hash` has. `isolation::hw` needs G3, G8 and G10. |
| `vesper-pulse` | 6,712 (+2,752 test) | yes (the evaluator's scheduler, `atomics::workers`, native sessions) | Frame storage, completion scheduler, the work-stealing pool (2.3k lines, portable), and the io_uring, epoll, kqueue, IOCP/AFD drivers | The portable half (frames, scheduler, driver protocol, pool) maps onto Vesper's `Atomic[T]` and structured workers; core pinning needs `sched_setaffinity`. The drivers need G9 (buffers that outlive a call) and, for io_uring, reads and atomics on mapped memory (G1). |
| `vesper-fiber-abi` | 7,129 (+4,503 test) | yes (native handlers, native sessions) | Pinned stacks, context switching, one-shot continuations, stack images, DWARF unwinding, FP environment, MMU write tracking, CLIF emission | Needs G10 (assembly objects) for the switch and FP environment, G8 (callbacks) and G1 for the fault handlers, and the Cranelift shim for CLIF. The ABI tables (`abi`, `evidence`, `image` decoding, `unwind` over `.eh_frame` bytes) are portable logic. |
| `vesper-simd` | 2,315 (+741 test) | yes (`vesper simd`) | Cranelift JIT scanner for the §14 delimiter example, `VVec`/`VMask` with SVE and RVV kernels in `asm!` (24 blocks) | The JIT goes through `vesper-cranelift-sys` (already the irreducible shim); the SVE and RVV kernels need G10; CPU feature detection needs `getauxval` (fine) or `cpuid` (G10). |

All six crates but `vesper-std-driver` are in the Rust compiler's closure
(`cargo tree -p vesper -e normal`). Under the wave rules their Rust cannot
be deleted before Stage 2 unless lane 3 moves them out: they flip with the
seed pattern, and lane 2's retirement PR deletes the seed.

## Proof: file I/O straight to libc works today

An executable whose package imports libc itself (`link = "dynamic", name =
"c"`), with no shim, writes a file, reads it back and unlinks it twice, and
gets `ENOENT` from the second `unlink` through `errno`:

```text
sys library LibC (target = "@TARGET@", abi = "@ABI@", link = "dynamic", name = "c",
    artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {
    fn open(nocapture in path: CStr, flags: i32, mode: u32) -> ForeignVal[i32] errno;
    fn write(fd: i64, nocapture in buf: CSlice[u8]) -> ForeignVal[i64] errno;
    fn read(fd: i64, nocapture inout buf: CSlice[u8]) -> ForeignVal[i64] errno;
    fn close(fd: i32) -> ForeignVal[i32];
    fn unlink(nocapture in path: CStr) -> ForeignVal[i32] errno;
}
```

`vesper build` of a `main` that calls these (Linux flag values
`O_WRONLY|O_CREAT|O_TRUNC` = 577) exits 42 = 40 + `ENOENT`, as expected.
`CStr` and `errno` (SH1 lane 7, #1198) already cover `open`, `stat`-family
paths, `rename`, `unlink`, `mkdir`, `rmdir`, `fsync`, `getcwd`, `access`.
`CSlice[u8]` matches `read(fd, buf, n)` and `write(fd, buf, n)` exactly.

## Compiler gaps

Each gap names the lane that owns the hunk a fix would touch. "This lane"
means `crates/vesper-ffi` (the `.vsys` parser), which this lane owns; the
matching lowering is in `src/ffi_lowering*`, which SH4 lane 13 (#1287, PR
#1296) is porting, so a lowering change has to land in both.

| # | Gap | Repro (`.vsys` line or source) | Result at base | Needed by | Owner of the fix |
|---|---|---|---|---|---|
| G1 | **No reads through a foreign pointer.** *C strings fixed in part 4*: `-> ForeignVal[Option[CStr]]` copies a returned C string into a fresh `ChunkSeq[u8]` (`NULL` is `None`) in native code and the evaluator. Reading structs behind a pointer (`struct dirent *`) and ring memory is still open; mappings (#1515) cover rings the program maps itself. | `fn getenv(nocapture in key: CStr) -> ForeignVal[Option[CStr]];` | was: `unknown foreign type 'CStr'; aggregates/handles are unsupported` | `getenv`, `strerror`, `environ`/`_NSGetEnviron` entries; `readdir`'s `struct dirent *` still needs a struct read | C strings done (`tests/vsys_cstr_result.rs`); struct reads: this lane |
| G2 | **No arrays of C strings.** *Fixed in part 3*: `nocapture in argv: CStrList` takes one `Slice[u8]` of NUL-ended strings and passes a NULL-terminated `char *const argv[]` (native lowering, evaluator host; the Vesper ffi hook declines such calls until SH4 lane 13 mirrors them). | `fn execv(nocapture in path: CStr, nocapture in argv: CSlice[CStr]) -> ForeignVal[i32] errno;` | was: `unknown slice element 'CStr'` | `@vesper-std/process` spawn (argv, envp) | done (`tests/vsys_cstr_list.rs`: JIT, evaluator, and a `posix_spawnp` executable) |
| G3 | **No variadic functions.** *Fixed in this PR* (asked by SH6 lane 14, #1422): `fn fcntl(fd: i32, cmd: i32, ..., arg: i64)`; see "Variadic functions" below. | `fn fcntl(fd: i32, cmd: i32, ...) -> ForeignVal[i32] errno;` | was: `unsupported token (variadics, attributes and imperative code are forbidden)` | `fcntl`, `ioctl`, `syscall` (pkeys, `close_range`), `open` with a mode, `snprintf` | done: parser in `crates/vesper-ffi`, two small lowering hunks in `src/ffi_lowering/` (SH4 lane 13's port must mirror them) |
| G4 | **No target-conditional imports or constants.** `O_CREAT` is `0o100` on Linux and `0x200` on macOS; `struct stat`, `struct dirent` and `kevent` layouts differ per target; the language has no `cfg`. | `[package] vsys = ["fs.vsys"]` takes one file for every target; the `.vsys` text only substitutes `@TARGET@` and `@ABI@`. | one layout and one set of constants for every target | `@vesper-std/fs` (flags, `stat`), Pulse (`epoll_event` is packed on x86_64 only) | `src/packages.rs` (`[package] vsys` per target, e.g. an `@OS@` token in the path): SH2 lane 6 follow-ups (#1178) |
| G5 | **`vesper jit`, `run` and `test` bound only `vesper_std_host`.** *Fixed in part 2*: on Unix a library with `link = "dynamic"` and name `c` or `m` binds from the process (`dlsym(RTLD_DEFAULT, …)`, `src/vsys_commands.rs`); others are still refused. Windows stays refused until G11. | a package whose `.vsys` says `name = "c"`, run with `vesper jit` | was: `library LibC links 'c', which this vesper does not host` | every std test once the std packages import libc | done (`tests/vsys_libc_host.rs`) |
| G6 | **Foreign symbols collide with the AOT runtime's libc imports.** The sequence runtime imports `write`, `exit`, `malloc` and `free` with all-`i64` signatures; a `.vsys` that declares `write(fd: i32, …)` fails to build. | `fn write(fd: i32, nocapture in buf: CSlice[u8]) -> ForeignVal[i64] errno;` | `Codegen: foreign symbol 'write': … signature … is incompatible with previous declaration` (works with `fd: i64`) | `exit(i32)`, `write`, `free` in the std packages | `src/seq/aot_runtime.rs` (SH4 lane 9, #1283) or the foreign-import declaration in `src/ffi_lowering/native.rs` (SH4 lane 13) |
| G7 | **No UTF-16 C strings.** *Fixed (#1610)*: `nocapture in s: CWStr` (from `Slice[u16]`, refusing embedded `0u16` with `ForeignInvalid`) and `-> ForeignVal[Option[CWStr]]` (copies UTF-16 code units before `0u16` into `Option[ChunkSeq[u16]]`) in `crates/vesper-ffi`, native lowering, `EvalHost`, and `@vesper/ffi-lowering` (`tests/vsys_cstr_result.rs`, `crates/vesper-ffi/tests/sys.rs`). | `fn DeleteFileW(nocapture in path: CWStr) -> ForeignVal[i32] errno;` | was: `unknown pointee 'CWStr'` | every Win32 path, argument and variable | done |
| G8 | **No callbacks.** C cannot call a Vesper function (signal handlers, `pthread_create`, `qsort`, GLib sources). | `fn signal(sig: i32, handler: extern fn(i32)) -> ForeignVal[u64];` | `expected ')'` | fiber-abi stack-overflow and write-watch fault handlers, `isolation::hw` containment | parser: this lane; lowering and an exported-function form: SH4 lane 13 and SH4 lane 12 (export ABI, #1286) |
| G9 | **No buffers that outlive a call.** Every pointer parameter must be `nocapture`. | `fn io_uring_submit(in ring: CSlice[u8]) -> ForeignVal[i32];` | `borrowed 'in' pointer parameters must be declared 'nocapture'; capturing foreign pointers are unsupported` | io_uring SQEs, IOCP `OVERLAPPED` and AFD poll buffers, `kevent` udata | parser: this lane; a pinned-buffer type in the language (manuscript change) |
| G10 | **No assembly in a build.** Vesper has no inline assembly, and a Loom package cannot carry an assembly file for the build to assemble and link. | context switch (`switch.rs`: 4 `global_asm!`, 3 `naked_asm!`), FP environment, `wrpkru`/`rdpkru`, `cpuid`, the SVE and RVV kernels (24 `asm!`) | none can be written | fiber-abi, `isolation::hw`, `vesper-simd` | a manifest field (`[package] objects` or `asm`) read by `src/packages.rs` and passed to the linker by `vesper build`: no active owner; manuscript change |
| G11 | **`vesper build` of an executable with `.vsys` imports refuses Windows.** | `vesper build --target x86_64-pc-windows-msvc` of the libc probe | `executables with .vsys imports link with cc only; Windows targets are not supported yet` | the std packages on the Windows required check | `src/vsys_commands.rs` `build`: no active owner |

## Variadic functions (G3, fixed here)

- **Syntax.** `fn f(fixed.., ..., variadic..)`: the parameters after `...`
  are the variadic arguments every call passes. At least one fixed
  parameter comes first and at least one variadic one follows. Each
  variadic parameter must be word-sized (a 64-bit integer, a pointer, a
  `CStr`, a handle, a slice); an `int` is declared `i64`, which C's
  `va_arg(ap, int)` reads correctly on every little-endian target.
- **ABI.** `FunctionDescriptor::abi()` reports `variadic: Some(Variadic {
  first, padding })`. SysV x86_64, AAPCS64 Linux and Win64 pass variadic
  words like fixed ones (`padding = 0`). Apple arm64 reads every variadic
  argument from the stack, so `padding` zero words fill `x0`-`x7` after the
  fixed words and the variadic words land at `[sp]`, in 8-byte slots.
- **Lowering.** The native call inserts the zero words
  (`src/ffi_lowering/native.rs`, 7 lines) and the evaluator's host does the
  same, with its word limit raised from 8 to 10
  (`src/ffi_lowering/eval_host.rs`). The descriptor's canonical text gains
  `variadic/from <n>` only for variadic functions, so every existing
  library keeps its identity.
- **Tests.** `crates/vesper-ffi/tests/sys_variadic.rs` (6: syntax,
  word-size rule, Apple padding, identity). `tests/vsys_variadic.rs` (2,
  Unix): libc's `snprintf(buf, n, "%ld:%ld", a, b)` in the JIT (optimizer on
  and off) and the evaluator, and an executable that sets `O_NONBLOCK` on a
  pipe with `fcntl(F_SETFL)` and reads it back. Both pass on Linux x86_64;
  the macOS (AArch64) required check runs the Apple path.
- **Manuscript.** One bullet, "Variadic Functions", after "C Strings and
  OS Error Codes" in §16.3.
- **Limit.** No floating-point variadic arguments (`.vsys` has no float
  parameters). On SysV x86_64 the call leaves `%al` (the vector-register
  count a variadic callee may read) unset; callees use it only to decide
  whether to spill `xmm` registers, so integer-only calls are correct.

Smaller notes found on the way (no fix needed by this lane):

- `ChunkSeq` has `try_get`, not `at`/`get`; a byte read in a hot loop goes
  through `Option` (gap 7 of `self-host-std-io.md` still stands).
- Passing a single pointer where C takes one pointer works with
  `CSlice[u8]` (the length is an extra argument C ignores). It is the same
  reliance as G7's workaround, so the port does not use it.

## What each crate needs

### `vesper-std-host` (and `vesper-std-driver`)

| Host function group | libc / Win32 call | Expressible now? |
|---|---|---|
| files, metadata, directory listings, trees, renames, temporary names | `open` (variadic mode), `read`, `write`, `fsync` (macOS `fcntl(F_FULLFSYNC)`), `close`, `fstat`, `stat`, `lstat`, `rename`, `unlink`, `rmdir`, `mkdir`, `getpid`, `opendir`/`readdir`/`closedir`, with `memcpy`, `strchr` and `bcopy` to read `struct dirent` | **done**: `@vesper-std/fs` imports libc on Linux and macOS (`fs-@OS@-@ARCH@.vsys`: `open` flags and the `struct stat`/`struct dirent` offsets are per-target constants; src/sys.ves mirrors `std::fs`, `remove_dir_all` and `create_dir_all` included). Windows keeps a libc-shaped shim over `std::fs` in `vesper-std-host/src/fs.rs` until G7; WASI keeps the `vstd_fs_*` interface (src-wasip2, `fs-wasip2-wasm32.vsys`). Reading the repository's sources runs at about the old shim's speed (1.2-1.6x of `std::fs` on a noisy 4-core runner, both). |
| arguments, variables, current directory, OS | `getenv`, `getcwd`; `argv` and `environ` through `dlsym` (glibc `__libc_stack_end` and `environ`, macOS `_NSGetArgv`, `_NSGetArgc`, `_NSGetEnviron`), read with `memcpy`, `strchr` and `strstr` | **done**: `@vesper-std/env` imports libc on Linux and macOS (`env-@OS@.vsys`) and kernel32 on Windows (#1633); `vesper-std-host/src/env.rs` is deleted (its `memcpy` and `strchr` moved to `fs.rs` for the Windows fs shim) |
| spawn, pipes, wait, kill, exit, pid | `posix_spawn`/`posix_spawnp` with file actions (`adddup2`, `addopen`, `addchdir_np`) and attributes (signal mask cleared, `SIGPIPE` default), `pipe2` (macOS `pipe` + `fcntl(FD_CLOEXEC)`), `poll`, `read`, `waitpid`, `kill`, `access`, `exit`, `getpid`; `environ` through `dlsym` (macOS `_NSGetEnviron`) | **done**: `@vesper-std/process` imports libc on Linux and macOS (`process-@OS@.vsys`); commands and children are `linear struct`s in Vesper, argv and envp are `CStrList`s, a bare name under a changed `PATH` is searched along the child's `PATH` as Rust's `Command` does. Windows keeps the `vstd_proc_*` shim (`src-windows`, `process-windows.vsys`, `vesper-std-host/src/process.rs`) until it calls kernel32. |
| Windows: every call above | `CreateFileW`, `ReadFile`, `FindFirstFileW`, `CreateProcessW`, `GetEnvironmentStringsW`, `MoveFileExW` | needs G7, G1 and G11 |
| `vesper jit`/`run`/the std tests | the JIT symbol table | needs G5 |

Order: G5 + G6 first (small), then G1 and G2 (the two `.vsys` types the
port really needs), then G4. With those, Linux and macOS go direct; Windows
follows with G7 and G11. Until Windows goes direct the crate keeps a
Windows-only Rust module, with a gap row.

### `vesper-ffi`

- `sys/parser.rs`, `sys/mod.rs`, `sys/clayout.rs`, `sys/assume.rs`,
  `sys/foreign.rs` and the wire views in `lib.rs`: a tokenizer, a recursive
  descent parser, C layout arithmetic, canonical text and a BLAKE3 identity.
  No OS calls. This is compiler code, so it ports like an SH3/SH4 module: a
  Vesper package, a differential over every `.vsys` file in the repository
  and the crate's tests, and a hybrid swap point; the Rust moves to `seed/`.
- `isolation.rs` (`hw`): `syscall(SYS_pkey_alloc/…)` (G3), `mmap`,
  `munmap`, `fork`, `pipe2`, `poll`, `waitpid`, `kill`, `sigaction` (G8),
  `getauxval`, `sysconf`, and `wrpkru`/`rdpkru`/POR_EL0 access plus `cpuid`
  (G10).

### `vesper-pulse`

- Portable: `frames.rs`, `scheduler.rs`, `driver.rs`, `io_driver.rs`, most of
  `pool.rs` (lock-free deques from `crossbeam-deque`, which become
  `Atomic[T]` Chase-Lev deques, and structured worker threads).
  `core_affinity` becomes `sched_setaffinity` on Linux (a `CSlice[u8]` mask)
  and `thread_policy_set` on macOS.
- Drivers: epoll (`epoll_create1`, `epoll_ctl`, `epoll_wait`, `eventfd`,
  `fcntl` (G3)); `epoll_event` is packed on x86_64 and natural on AArch64
  (G4). kqueue (`kqueue`, `kevent` with a pointer-sized `udata`). io_uring
  (the `io-uring` crate maps the SQ/CQ rings; ring heads are atomics in
  mapped memory: G1 and G9). IOCP and AFD (`NtDeviceIoControlFile` with an
  `OVERLAPPED` that lives until completion: G9, G7).

### `vesper-fiber-abi`

- Assembly (G10): the switch routines for SysV x86_64, AAPCS64 (Linux and
  Apple) and Win64, and the FP environment save/restore (MXCSR/x87 CW,
  FPCR).
- OS: `mmap` with guard pages, `mprotect`, `sigaction` with `SA_ONSTACK`
  and a `ucontext_t` read for faults on a fiber stack (G8, G1, G4);
  Windows `VirtualAlloc`, vectored exception handlers (G8), write watches.
- Cranelift: `clif.rs` and `prototype.rs` emit CLIF; they go through
  `vesper-cranelift-sys` (`packages/vesper-cranelift`), the irreducible shim.
- Portable: `abi.rs` (layouts and offsets), `evidence.rs`, `image.rs`
  decoding, `unwind.rs` (CFI interpretation over `.eh_frame` bytes; gimli
  becomes a small Vesper CFI reader).

### `vesper-simd`

- The Baseline128 scanner is Cranelift JIT and object emission: it moves to
  `packages/vesper-cranelift`'s `vcl_*` calls (`vesper-cranelift-sys`).
- `vvec/sve.rs` and `vvec/rvv.rs` are hand-written kernels in `asm!` (G10).
  The portable scalar profile (`vvec/mod.rs`) is plain logic.
- Feature detection: `is_aarch64_feature_detected!` becomes `getauxval(AT_HWCAP)`.

## Plan for the next parts

1. **`vesper-ffi` `sys` in Vesper** (no gap blocks it): a `@vesper/vsys`
   package with a differential over every `.vsys` in the repository, the
   seed pattern, and the hybrid swap at `Imports::add`. This lane owns the
   crate; it touches `crates/vesper-hybrid` (serialize the merge).
2. **G1, G2, G3, G7 in the `.vsys` parser** (this lane), with the lowering
   halves handed to SH4 lane 13 as patches, or landed here if that lane
   agrees.
3. **`vesper-std-host` on libc** for Linux and macOS once G1, G2, G4, G5 and
   G6 land; Windows after G7 and G11. `vesper-std-driver` becomes a
   `vesper build` executable and its Rust goes in the same PR.
4. **Pulse**: the portable scheduler and pool first, then epoll and kqueue,
   then io_uring and IOCP once G9 exists.
5. **fiber-abi and simd** once G10 exists.

## Earlier claims

#585 (Pulse in executables), #598 (fiber ABI unwinding into host frames)
and #611 (Bridge pointer results and narrow integers) are open Wave 9 claims
with no activity since 2026-11-16. They are feature work in the same crates,
not ports; this lane does not take them over. G1 overlaps #611's "pointer
results", so whoever lands G1 should say so there.

## Census

The STATUS rows for these six crates live in `scripts/rust-census.py`, which
arrives with the plan PR (#1415). This audit changes no row's class; the rows
are updated in the first port PR after #1415 merges.


## Update (#1629, #1633): `@vesper/vsys` and `@vesper-std/env` on Windows over `kernel32`

- **PR #1629 (`@vesper/vsys`, `#1627`):** Ported `vesper-ffi::sys` (`.vsys` lexer, `wire struct` layout compiler, canonical byte encoder, and library validator) to `packages/vesper-vsys`, verified byte-for-byte against `vesper_ffi::sys::parse` across all `.vsys` files in the repository (`tests/self_host_vsys.rs`).
- **PR (`@vesper-std/env` Windows, `#1633`):** Replaced `vesper_std_host` in `packages/vesper-std/env/env-windows.vsys` with direct `kernel32` bindings (`GetCommandLineW`, `GetCurrentDirectoryW`, `GetEnvironmentVariableW`, `GetEnvironmentStringsW`, `FreeEnvironmentStringsW`, `lstrlenW`, `WideCharToMultiByte`) and `packages/vesper-std/env/src-windows/lib.ves` (`CWStr` + UTF-8/UTF-16). `@vesper-std/env` now uses zero `vesper_std_host` across Linux, macOS, and Windows.

## Update (#1712): `vesper-std-host` deleted

With Windows fs on kernel32 (#1701) and Windows process on kernel32 (#1716),
no `.vsys` library linked the shim any more; only test helpers and a fixture
symbol were left (131 lines). This PR deletes the crate:

- The test oracles' helpers (`IoKind`, `error_word`, `os_bytes`, `path_of`)
  move to `tests/common/std_io_oracle.rs` (oracles stay Rust, owner call
  2026-11-21); the std tests bind only libc or kernel32.
- The `live_handles()` leak counters, constant zeros since the shims went,
  are dropped from tests/std_io_support and the std-driver test.
- `vesper jit`, `run` and `test` no longer bind a compiler-hosted
  `vesper_std_host`: every library comes from the shared library its link
  name names (`src/vsys_commands.rs`; manuscript §16.3 updated). The
  `vesper test` fixture in tests/test_runner_cli.rs calls libc's `abs`.
- The root package, vesper-hybrid and vesper-std-driver drop the dependency;
  the census row goes.

Only the WASI views (`fs-wasip2-wasm32.vsys`, `env-wasip2.vsys`) still carry
the link name `vesper_std_host`; the wasm backend lowers them to WASI 0.2 and
nothing links them.

## Pulse: the epoll completion driver (#1938)

The first of Pulse's real I/O drivers moves to Vesper: `readiness.rs` over
`epoll.rs`, as `epoll_driver_*` in `crates/vesper-pulse/vesper/src-linux`.

- Same protocol as Rust: queue-full and id checks, `F_GETFL` access-mode
  check (`EBADF`), zero-length completes at the next poll, epoll's `EPERM`
  sends regular files to inline `pread`/`pwrite`, a stream offset is refused
  with `ESPIPE` after deregistering, streams go `O_NONBLOCK` and one-shot,
  `EAGAIN` re-arms, cancel yields `ECANCELED`.
- Buffers live in a per-driver byte arena (`start`, `len`); a poll copies its
  completions' bytes out and compacts the arena (reset when the driver is
  empty, rebuilt at 2x live + 4 KiB).
- Rust's errors without an errno get `io_error_*` codes (queue full
  1000001, ids exhausted, invalid length, unknown event, invalid entries).
- `tests/pulse_port.rs` `epoll_completion_driver_matches_rust` hashes one
  scenario through both drivers (JIT and JIT without optimization; the
  evaluator has no PulseSys host functions).

The Rust driver stays until the runtime's `run` step stops using it. Next:
kqueue (macOS, needs `fstat` for the regular-file check), then io_uring
and AFD.

## Pulse: the kqueue completion driver (#1946)

The macOS copy gets the same driver over kqueue, as `kqueue_driver_*`.

- `fstat` sends regular files to inline `pread`/`pwrite`. A registration that answers `EINVAL`, `ENODEV` or `ENOTSUP` is also treated as always ready.
- Streams use `EV_ADD | EV_ONESHOT`. The kernel drops a filter once it fires, so release does nothing.
- `EV_DELETE` treats `ENOENT` as success, because the filter may have fired and been removed before the driver collected it.
- A fired event that carries `EV_ERROR` reports its error, and the event's token is still serviced.
- PulseSys (macOS) now declares one variadic `fcntl` in place of `fcntl_setfd`. Apple arm64 passes variadic arguments on the stack, so the old fixed-argument declaration passed them in the wrong place. It also adds `fstat` with a 144-byte `Stat`, and `read`, `write`, `pread` and `pwrite`.
- The scenario test now runs on both Linux and macOS (`readiness_completion_driver_matches_rust`). The macOS Platforms job had never run `tests/pulse_port.rs`, including the existing kqueue roundtrip test; it runs it now.

## Pulse: the io_uring completion driver (#1960)

`uring.rs` is now `uring_driver_*` in the Linux copy, for x86_64 and aarch64.

- **Rings.** The driver `mmap`s the rings and SQEs from the ring descriptor, as one mapping when the kernel has `IORING_FEAT_SINGLE_MMAP`. It reads and writes ring indices and entries through `memcpy`-style bindings, using the same `u64`-address pattern as `vk.vsys` and the std `fs` bindings. The ring setup and the wait for completions are libc `syscall` calls; each has its own binding with its own signature, and the native backend calls them through the symbol's address.
- **Buffers.** Each request owns a `malloc`ed buffer until its completion: the bytes are copied in at submit and out at completion. The driver needs `IORING_FEAT_NODROP`.
- **Behaviour carried over from Rust:**
  - Entries are queued at submit and handed to the kernel at the next poll. The kernel also gets them when the queue is full, or at once when an outside loop waits on the eventfd.
  - Cancellation sends `IORING_OP_ASYNC_CANCEL`. Its own completion carries the request id with bit 63 set, so it is never mistaken for the request's completion. A cancelled request that ends with `EINTR` is reported as cancelled.
  - The eventfd, shutdown, and a drop path (`uring_driver_close`) are included. The drop path keeps any buffer the kernel may still hold.
- **Barriers.** On aarch64, reading ring indices needs acquire and release ordering. A `.vsys` can now bind its own function name to a `@vesper-std/cpu` leaf with `#[link_name]`: the native backend and the JIT binder look up CPU leaves by link symbol (`src/ffi_lowering/native.rs`, `src/ffi_lowering.rs`). PulseSys's `ring_fence` is `dmb ish` on aarch64 and `sfence` on x86_64; x86_64 is TSO, so there the call only stops the compiler reordering memory accesses around it.
- **Test.** `uring_completion_driver_matches_rust` covers pipe reads before and after data, file I/O at offsets, a zero-length read, an `EBADF` returned by the kernel, a cancel issued twice, stream I/O, the eventfd and shutdown. It sums each completion's hash, so the order in which completions arrive across polls doesn't matter. It passed 5 of 5 runs locally.

The Rust driver stays until the runtime stops using it. Speed is not measured yet.

## vesper-simd: the Baseline128 scanner builder (#1969)

`crates/vesper-simd/vesper/` (`@vesper/simd`) builds the §14 delimiter-count/XOR
scanner and makes the profile decisions in Vesper. It is part 1 of the crate's
port, behind the `vesper-port` feature.

- **How it runs.** The Rust crate keeps the target ISA (the exact SSE4.2 preset
  on x86_64) and the JIT and object modules. It lends the module to the port's
  `define_scanner` with `vesper_cranelift_sys::lend_module_as`. The port declares
  the exported `vesper_simd_scan_baseline128_v1` and builds its body through
  `@vesper/cranelift`, in the same block, parameter and instruction order as the
  Rust builder. Its answer is a status byte followed by the CLIF text (or the
  reason it failed). `check_features` and `profile_support` return the crate's
  `Error` variants as codes.
- **Tests.** `vesper_port_builds_the_reference_scanner` checks the Vesper and Rust
  builders against each other. They produce the same CLIF and byte-identical ELF
  objects for x86_64 and AArch64, and the same CLIF in a JIT module.
  `vesper_port_profile_decisions_match_rust` covers every simulated host and
  every profile. The crate's other tests (all byte values, seeded tails, guard
  pages, the Lean model) run against the port unchanged.
- **Divergence check.** `vesper-port-divergent` counts each delimiter three times;
  `aspect crate simd test-port` requires the scanner tests to fail. It runs in
  Platforms Linux port shard 6.
- **Speed.** The scanned code is the same machine code. Building it (400
  `JitScanner::compile` and `ObjectArtifact::emit` calls, release,
  `-C target-cpu=native`) takes 0.76 to 1.18 ms per call with either builder. The run
  noise is larger than the difference, so the ratio is about 1.0x.
- **Left.** The SVE and RVV `asm!` backends of `vvec` wait on G10. The Rust
  builder stays the default until the flip.

### Part 2: the `vvec` scalar profile

`vesper/src/vvec.ves` is the scalar arm of every `VVec` and `VMask` operation:
the governing predicate (`prefix_bits`), mask logic, splat, masked load and
store, compare, the four lane-wise operations, the XOR reduction, and the
whole-loop scan kernel. `src/vvec/scalar.rs` lends the 256-byte lane and 4-word
mask buffers in place. With the `vesper-port` feature the `dispatch!` scalar
arm calls the port; the Rust arms stay as `scalar::reference`. Profile tokens,
bounds checks, the mask queries (`count_ones`, `first_active` and the like,
one instruction per word) and the SVE and RVV arms stay Rust.

- **Tests.** `vesper_port_vvec_scalar_arms_match_rust` runs every arm against
  the Rust one at about 40 lane widths from 1 to 256, with masks that are full, empty
  and random per word. Each masked load reads a source cut right after its last
  active lane, so a read of an inactive lane past the end would fail. The scan
  runs at every length below 300 and at 4,096 and 65,537 bytes, from three start
  offsets. `tests/vvec.rs` (strip-mining at many widths, guard pages) runs
  against the port unchanged. No sequence is left allocated.
- **How it runs.** The port works eight lanes per word: equality goes through
  the exact zero-byte test and a multiply that gathers one bit per byte, the
  lane-wise add is a carry-free SWAR add, and the scan keeps a per-byte count
  of the bytes that differ from the delimiter and adds those counts with one
  multiply every 252 words.
- **Speed** (release, `-C target-cpu=native`, best of 7):

  | Workload | Rust | Vesper | Ratio |
  | --- | ---: | ---: | ---: |
  | `kernels::scan`, 1 MiB | 144 us | 238 us | 1.65x |
  | `scan_record_delimiters`, 64 KiB, 16 lanes | 238 us | 497 us | 2.09x |
  | same, 64 lanes | 125 us | 150 us | 1.20x |
  | same, 256 lanes | 110 us | 100 us | 0.91x |
  | mask, load, compare, and, XOR, reduce at 64 lanes | 0.15 us | 0.27 us | 1.8x |

  LLVM vectorizes the Rust scan loop; the Vesper object has no vector types, and
  each checked `try_read_u64_le` costs about a dozen instructions (bounds test,
  `Option` tag, select), so unrolling to four words per step did not help. Short
  strips are bound by the crossing: about six calls per 16-byte strip. The Rust
  arms stay the default until the checked reads get cheaper (a compiler gap:
  the bounds test is not hoisted out of a loop whose index is known to be in
  range).
