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
| G1 | **No reads through a foreign pointer.** A result can be an address (`ForeignVal[u64]`), but Vesper cannot read the memory behind it, and there is no `CStr` result. | `fn getenv(nocapture in key: CStr) -> ForeignVal[CStr];` | `unknown foreign type 'CStr'; aggregates/handles are unsupported` | `getenv`, `readdir`'s `struct dirent *`, `environ`/`_NSGetEnviron`, `strerror`; io_uring rings | parser: this lane; lowering: SH4 lane 13 |
| G2 | **No arrays of C strings.** `posix_spawn`/`execve` take `char *const argv[]` (NULL-terminated). | `fn execv(nocapture in path: CStr, nocapture in argv: CSlice[CStr]) -> ForeignVal[i32] errno;` | `unknown slice element 'CStr'; declare it as a wire struct` | `@vesper-std/process` spawn (argv, envp) | parser: this lane; lowering: SH4 lane 13 |
| G3 | **No variadic functions.** *Fixed in this PR* (asked by SH6 lane 14, #1422): `fn fcntl(fd: i32, cmd: i32, ..., arg: i64)`; see "Variadic functions" below. | `fn fcntl(fd: i32, cmd: i32, ...) -> ForeignVal[i32] errno;` | was: `unsupported token (variadics, attributes and imperative code are forbidden)` | `fcntl`, `ioctl`, `syscall` (pkeys, `close_range`), `open` with a mode, `snprintf` | done: parser in `crates/vesper-ffi`, two small lowering hunks in `src/ffi_lowering/` (SH4 lane 13's port must mirror them) |
| G4 | **No target-conditional imports or constants.** `O_CREAT` is `0o100` on Linux and `0x200` on macOS; `struct stat`, `struct dirent` and `kevent` layouts differ per target; the language has no `cfg`. | `[package] vsys = ["fs.vsys"]` takes one file for every target; the `.vsys` text only substitutes `@TARGET@` and `@ABI@`. | one layout and one set of constants for every target | `@vesper-std/fs` (flags, `stat`), Pulse (`epoll_event` is packed on x86_64 only) | `src/packages.rs` (`[package] vsys` per target, e.g. an `@OS@` token in the path): SH2 lane 6 follow-ups (#1178) |
| G5 | **`vesper jit`, `run` and `test` bind only `vesper_std_host`.** `host_symbols` refuses every other link name before running. | a package whose `.vsys` says `name = "c"`, run with `vesper jit` | `library LibC links 'c', which this vesper does not host; link it into an executable with vesper build` | every std test (`tests/std_*.rs` run in the JIT and the evaluator) once the std packages import libc | `src/vsys_commands.rs` `host_symbols` (resolve `name = "c"` with `dlsym(RTLD_DEFAULT, …)` / `GetProcAddress`); no active owner, small, but it adds Rust to the compiler, so it waits for the std-host PR that needs it |
| G6 | **Foreign symbols collide with the AOT runtime's libc imports.** The sequence runtime imports `write`, `exit`, `malloc` and `free` with all-`i64` signatures; a `.vsys` that declares `write(fd: i32, …)` fails to build. | `fn write(fd: i32, nocapture in buf: CSlice[u8]) -> ForeignVal[i64] errno;` | `Codegen: foreign symbol 'write': … signature … is incompatible with previous declaration` (works with `fd: i64`) | `exit(i32)`, `write`, `free` in the std packages | `src/seq/aot_runtime.rs` (SH4 lane 9, #1283) or the foreign-import declaration in `src/ffi_lowering/native.rs` (SH4 lane 13) |
| G7 | **No UTF-16 C strings.** Win32 takes `LPCWSTR` (one pointer). | `fn DeleteFileW(nocapture in path: CWStr) -> ForeignVal[i32] errno;` | `unknown pointee 'CWStr'` | every Win32 path, argument and variable | parser: this lane; lowering: SH4 lane 13. Workaround: `CSlice[u16]` with a NUL pushed by hand, relying on the callee ignoring the extra length argument (true on Win64 and SysV, but not a contract). |
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
| open, read, write, fsync, close, rename, unlink, rmdir, mkdir, file length | `open`, `read`, `write`, `fsync`, `close`, `rename`, `unlink`, `rmdir`, `mkdir`, `fstat` | Linux yes (proved above). macOS needs G3 (`open` mode) and G4 (flags, `stat` layout). |
| metadata | `stat`/`lstat` | needs G4 for the layout (a `wire struct` per target) |
| list a directory | `opendir`/`readdir`/`closedir` | needs G1 (`readdir` returns a pointer). Linux-only fallback: `getdents64` into a `CSlice[u8]` |
| remove a tree, create parents | built from the above in Vesper | yes, once listing works |
| unique temp names, parent split | pure Vesper (`getpid` for the pid) | yes |
| arguments, variables | `argc`/`argv` from `main`, `environ` | needs G1 (or Linux-only `/proc/self/cmdline` and `/proc/self/environ`) |
| current directory | `getcwd(buf, n)` | yes |
| spawn, pipes, wait, kill | `pipe`, `posix_spawn` with file actions, `poll`, `read`, `waitpid`, `kill` | needs G2 (argv/envp) |
| exit, pid | `exit`, `getpid` | needs G6 (`exit(i32)` collides) |
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
