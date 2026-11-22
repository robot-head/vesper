# Rust-exit gap wave 1, lane 7: platform interop

Claim #1504. Prompt: `docs/agent-prompts/rust-exit-gaps-1/07-platform-interop.md`.
Base: `main` at `66255e17` (2026-11-22).

The gaps come from the runtime-glue report (SH6 lane 13, #1408:
G4, G5, G6, G10, G11) and the os-tools report (SH6 lane 14, #1409: G4).

| Gap | Status | Where |
|---|---|---|
| Per-target `.vsys` imports (runtime glue G4) | **closed, part 1** | `src/packages.rs`, manuscript §16.3 |
| Per-target constants (G4, `O_CREAT`) | open, part 3 | see "Left" |
| JIT/`run`/`test` bind only `vesper_std_host` (G5) | **closed, part 1** | `src/vsys_commands.rs` (`host_symbols`, `host_libraries`) |
| `.vsys` names colliding with the AOT runtime's libc imports (G6) | owned by #1459 (FFI lowering, graphics gap 5) | not this lane |
| Windows executables with `.vsys` imports (G11) | **closed, part 1** | `src/vsys_commands.rs` (`link_windows`) |
| CPU instructions (os-tools G4) | `cpuid`, `rdtsc`, `rdpmc`, `rdrand`, `cntvct`, PMU, `rndr` landed in #1473; **part 1 adds** `rdpkru`/`wrpkru`, `lfence`/`mfence`/`sfence`, `POR_EL0` read/write, `isb`/`dmb ish`/`dsb sy` | `src/ffi_lowering/cpu.rs`, `packages/vesper-std/cpu` |
| Assembly objects in a build (runtime glue G10) | open, part 2 | see "Left" |

## Part 1

### Per-target imports (G4)

Repro at `66255e17`: `@vesper-std/fs` and Pulse need a different `struct
stat`, `kevent` and `epoll_event` layout per OS (`epoll_event` is packed on
x86_64 only), but `[package] vsys = ["fs.vsys"]` names one file for every
target, and the `.vsys` text only substitutes `@TARGET@` and `@ABI@`.

Fix: a manifest path may contain `@OS@` (`linux`, `macos`, `windows`),
`@ARCH@` (`x86_64`, `aarch64`) or `@TARGET@` (the triple):

```toml
[package]
vsys = ["sys/fs-@OS@.vsys", "sys/@ARCH@/epoll.vsys"]
```

Loading reads every target's expansion that exists
(`PackageVsys::targets`, in `VSYS_TARGETS` order); `Assembled::imports(target)`
and `@export vsys` (`PackageVsys::source_for`) pick the object target's file.
At least one target must have a file (else the manifest error `no such file
for any target`), and building for a target without one is an error naming
the expected path. The type names a library declares are the union over its
files. Plain paths behave exactly as before.

### JIT, `run` and `test` bind other libraries (G5)

Repro at `66255e17`: `vesper jit` of a package whose `.vsys` says `link =
"dynamic", name = "c"` failed with `library LibC links 'c', which this vesper
does not host`.

Fix: `host_symbols` keeps the standard I/O host from the CLI itself and binds
every other library from a shared library:
- `c`, `m`, `pthread`, `dl` (and `System` on macOS): the process's own
  symbols (`dlsym(RTLD_DEFAULT, ..)`); on Windows `c`/`m` are `ucrtbase.dll`
  then `msvcrt.dll`.
- any other link name: `lib<name>.so` / `lib<name>.dylib` / `<name>.dll`
  from the `--link-search` directories first, then the system search path
  (`dlopen(RTLD_NOW | RTLD_GLOBAL)`, `LoadLibraryA`). The handle stays open
  for the process.
- a library that does not load, or lacks a symbol, is refused before
  running, with the loader's error.

`vesper run` and `vesper test` (the evaluator's `EvalHost`) use the same
table, so they call libc too.

### Windows executables (G11)

Repro at `66255e17`: `vesper build --target x86_64-pc-windows-msvc` of the
libc probe failed with `executables with .vsys imports link with cc only`.

Fix: `link_windows` links through `native::windows::link` (link.exe from the
Visual Studio installation, or the `--linker` given) with `<name>.lib` for
each link name (from `--link-search` first, else the linker's `LIB` path).
`c` and `m` are the static MSVC runtime every Windows executable links. The
standard I/O host (a Rust staticlib) adds `advapi32`, `bcrypt`, `ntdll`,
`userenv` and `ws2_32`.

### CPU instructions

`@vesper-std/cpu` gains, as machine-code leaf functions like the rest:
- x86_64: `x86_rdpkru`, `x86_wrpkru(value)`, `x86_has_pku` (CPUID.07H:ECX
  OSPKE; both instructions are invalid opcodes otherwise), `x86_lfence`,
  `x86_mfence`, `x86_sfence`.
- AArch64: `aarch64_rdpor`, `aarch64_wrpor(value)` (`POR_EL0`, FEAT_S1POE;
  the write ends with `isb`), `aarch64_isb`, `aarch64_dmb_ish`,
  `aarch64_dsb_sy`.

These are what `vesper-ffi`'s `isolation::hw` (PKU/POE) needs from the CPU.

### Tests

`tests/platform_interop.rs` (10 tests):
- `@OS@`, `@ARCH@` and `@TARGET@` paths select each target's file and layout;
  a missing target is an error naming the expected path; no file at all is a
  manifest error.
- `vesper jit` (optimizer on and off) and `vesper run` call `strlen` and `abs`
  from the process's C library.
- `vesper jit` and `vesper run` load a shared library built with `cc` from
  `--link-search`, and refuse it without the flag; a missing library and a
  missing symbol are refused before running.
- `vesper build` links the libc probe on Unix (exit code 36) and on Windows
  (link.exe; elsewhere the test checks the refusal is gone).
- Objects for all four targets hold the new instructions; the JIT runs the
  fences, and the `rdpkru`/`wrpkru` round trip where the OS enabled keys.

`tests/vsys_packages.rs`'s refusal test now expects the loader's message.
Also passing: `std_cpu`, `vsys_packages`, `vsys_package_imports`,
`vsys_variadic`.

### Stage 1 guard

No guarded path is touched (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/stage1*`,
`packages/vesper-compiler/**`). The changes reach only programs with `.vsys`
imports; the Stage 1 corpus has none.

### Measurements

None of these gaps were speed gaps: they decide whether a port can be
written at all, so there is no ABBA before and after. What each fix unblocks:
- SH6 lane 13 (#1408): G5 lets `vesper-std-host`'s std tests run in the JIT
  and evaluator once the std packages import libc directly; per-target paths
  give `@vesper-std/fs` its `stat` layout and `open` flags per OS (flags need
  part 3's constants, or one `.vsys` per OS declaring `open` with the mode
  argument); G11 lets Windows executables link them.
- SH6 lane 14 (#1409): `rdpkru`/`wrpkru` and `POR_EL0` for `isolation::hw`;
  the fences; `kpc.rs`'s `dlopen` of kperf can now be tried from the JIT
  through G5's loader (a library name, not a path).

## Left

- **Part 2, assembly objects (G10):** a `[package] objects` field (`.s` /
  `.S` files per target, with the same `@OS@`/`@ARCH@` tokens) that `vesper
  build` assembles with the system assembler and links; the JIT and `vesper
  test` refuse them with a diagnostic (or load a shared object built from
  them). Needs a manifest field in `vesper-loom-calculus` and a manuscript
  hunk.
- **Part 3, per-target constants:** `const NAME: <int> = <value>;` items in a
  `.vsys` library, read as `Lib::NAME`, so one call site sees each target's
  `O_CREAT`. Touches the `.vsys` parser (lane 8, #1505) and the Bridge
  checker (SH4 lane 13, #1287); to agree with both first.
- The census rows do not move: this part adds about 300 lines of Rust to the
  CLI and deletes none. The Rust it unblocks is deleted by the lanes above.
