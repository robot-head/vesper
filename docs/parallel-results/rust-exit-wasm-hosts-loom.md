# Wasm hosts and the Loom store on C APIs (SH6 lane 16)

Rust-exit wave SH6, lane 16. Claim #1411, prompt
`docs/agent-prompts/self-host-sh6/16-wasm-hosts-loom.md` (plan PR #1415).
Base `ab965f2a` (main). Part 1 of the lane: the audit. No crate is flipped
yet, so no Rust is deleted in this part.

## The seven crates

Non-test source lines are counted at `ab965f2a` (`src/**/*.rs`); the census
counts the same thing at `af7dd900`.

| Crate | Src lines | Rust tests | Who links it | What it needs from the OS or C |
|---|---:|---:|---|---|
| `vesper-loom-cas` | 4,166 | 90 | **the compiler** (root dependency: `cas`, `doc`, `shot`, `verify` commands, `anvil_hash` verdict cache, `attestation_commands`), `vesper-hybrid`, `vesper-codegen-verified` (stays Rust), aegis, aether, flux, loom-trust, loom-admission, loom-build, forge-runner, tools | BLAKE3 (`derive_key` and plain hashing), `open(O_NOFOLLOW\|O_NONBLOCK, 0600)`, no-replace rename (`renameat2(RENAME_NOREPLACE)` by `syscall` on Linux, `MoveFileExW` on Windows), reflink (`reflink-copy`: `FICLONE` ioctl on Linux, `clonefile` on macOS, `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on Windows), Unix mode bits, file type (FIFO, socket) checks, directory fsync, a process-wide `AtomicU64` for temporary names |
| `vesper-loom-admission` | 413 | 12 | **the compiler** (`attest` command), `vesper-hybrid` | BLAKE3, `open(O_NOFOLLOW\|O_NONBLOCK\|O_CLOEXEC)` (Windows: `FILE_FLAG_OPEN_REPARSE_POINT`), `tempfile`, Ed25519 quorum through `vesper-loom-trust` (lane 17) |
| `vesper-loom-tuf` | 2,617 | 21 | nobody (leaf) | `tough` 0.24 (TUF roles, delegations, expiry, limits) over a filesystem transport; `tokio` only to drive `tough`'s async API; SHA-256/512; signature verification (tough accepts Ed25519, ECDSA P-256 and RSA-PSS keys through `aws-lc-rs`); OLPC canonical JSON; RFC 3339 times (`jiff`); `geteuid`, `O_NOFOLLOW`, file uid/mode checks; threads and `current_exe` re-exec only in tests |
| `vesper-loom-sandbox` | 2,951 | 63 | forge-runner, loom-build, `vesper-forge` (optional `sandbox-transport`) | Wasmtime 50 component model + `wasmtime-wasi` p2: fuel, aggregate memory/table/instance limits with peak tracking (`ResourceLimiter`), read-only preopens plus one read-write `/out`, captured bounded stdout/stderr, fixed wall clock, non-advancing monotonic clock, seeded SplitMix64 randomness, shadowed `wasi:filesystem/types` wrappers that call the stock implementation and then normalize timestamps, link counts, metadata hashes and directory order (`fs_determinism.rs`), and a `tokio::time::timeout` wall-clock backstop |
| `vesper-loom-build` | 2,077 | 25 | nobody (leaf + `vesper-loom-build` binary) | CAS + sandbox; `renameat2(RENAME_EXCHANGE)`; mode bits; a `build.rs` that bakes source digests, the locked engine dependency set, `rustc -vV` and the host triple into `env!` constants |
| `vesper-forge-runner` | 3,842 | 43 | nobody (leaf) | CAS + sandbox + `vesper-forge`; BLAKE3; `/proc/self/exe` and `/proc/self/maps` digests; CPU feature detection (`is_x86_feature_detected!`, i.e. `cpuid`); `O_NOFOLLOW`; a `build.rs` with `rustc`/target constants; `AtomicU64` counters |
| `vesper-nexus` | 4,600 | 69 | root **dev-dependency** only (`tests/wasm_backend.rs`) | Wasmtime 50 components with `stack-switching` and `gc`, host resources backed by its own generational table, fuel and epoch preemption, guest linear memory read and written in place (`SharedPluginRing`), `wit-parser` 0.261 for `wit_gen` (which emits Vesper traits and Rust host-binding text), SimPulse's seeded run queue |

Totals: 20,666 source lines and 323 Rust tests.

## What Wasmtime's C API covers

I read the headers of `wasmtime-c-api-impl` 50.0.0 (the version matching the
lockfile's `wasmtime` 50.0.1). Everything the three hosts do has a C entry
point except four things, listed under gap 10.

* Engine: `wasmtime_config_consume_fuel_set`, `epoch_interruption_set`,
  `wasm_component_model_set`, `wasm_stack_switching_set`, `wasm_gc_set`,
  `parallel_compilation_set`, `cranelift_opt_level_set`.
* Store: `wasmtime_context_set_fuel` / `get_fuel`,
  `wasmtime_context_set_epoch_deadline`,
  `wasmtime_store_epoch_deadline_callback`, `wasmtime_store_limiter`
  (static limits), `wasmtime_context_fuel_async_yield_interval`.
* Components (`wasmtime/component/*.h`): `wasmtime_component_new`,
  `wasmtime_component_linker_new`, `_add_wasip2`, `_allow_shadowing`,
  `_instance_add_func` (a C callback), `_instance_add_resource` (a C
  destructor), host and guest resources, `wasmtime_component_func_call` and
  `_call_async`, `wasmtime_call_future_poll`.
* WASI: `wasi_config_preopen_dir` with directory and file permissions,
  `wasi_config_set_stdout_custom` / `set_stderr_custom` (a C callback),
  `set_stdin_bytes`, empty argv and env by default.
* Memory: `wasmtime_memory_data` / `data_size` (a raw pointer and length).

The plan's irreducible-minimum table already lists "wasmtime's C API" as a C
dependency that needs no Rust in this repository, so the hosts link the
upstream `libwasmtime` (built from the upstream `wasmtime-c-api` crate, or
its release archive pinned by digest). That packaging choice is gap 9.

## Repros on `ab965f2a`

Each repro is a package `@r/<name>` with `Loom.toml`:

```toml
[package]
name = "@r/<name>"
version = "0.1.0"
edition = "2026"
capability_ceiling = ["Ffi[LibC]"]
vsys = ["sys/libc.vsys"]
```

and a library header shared by every `sys/libc.vsys` below:

```text
sys library LibC (target = "@TARGET@", abi = "@ABI@", link = "dynamic", name = "c",
    artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {
    ...
}
```

Built with `vesper build <dir> -o <exe>` on x86_64 Linux.

**What already works** (so the crates need no Rust shim for these):

* *libc directly.* `fn getpid() -> ForeignVal[i32];`, called from `main`,
  links against the system libc with `link = "dynamic", name = "c"`. The
  executable exits 7 as written. No `vesper_std_host` is involved.
* *Reading and writing C memory.* A raw address is a `usize`. Because a
  `CSlice[u8]` parameter expands to `(pointer, length)`, `memcpy` with
  the slice last copies Vesper bytes into C memory, and `bcopy`
  (`src, dst, n`) copies C memory into a Vesper slice, in place:

  ```text
  fn malloc(n: usize) -> ForeignVal[usize];
  fn free(p: usize) -> ForeignVal[Unit];
  fn memcpy(dst: usize, nocapture in src: CSlice[u8]) -> ForeignVal[usize];
  fn bcopy(src: usize, nocapture inout dst: CSlice[u8]) -> ForeignVal[Unit];
  ```

  A round trip through a `malloc`ed buffer returns the bytes (exit 34). This
  is how a host reads and writes guest linear memory from
  `wasmtime_memory_data`. MSVC has no `bcopy`; `memcpy_s(dst, dstsz, src, n)`
  has the same `(slice, address, n)` shape there.
* *`errno`.* `-> ForeignVal[T] errno` covers every libc error path the crates
  check (`EINVAL`, `ENOSYS`, `EOPNOTSUPP`, `EEXIST`).
* *Atomics.* `Atomic[u64]` (§15.4) covers the process-wide counters.

### Compiler gaps

**Gap 1 (blocks all three wasm hosts): C cannot call Vesper.** `.vsys` has
no function-pointer type, and a Vesper function cannot be exported as a C
callback.

```text
fn qsort(nocapture inout base: CSlice[u64], width: usize, cmp: fn(usize, usize) -> i32) -> ForeignVal[Unit];
```

gives `Vsys: unsupported value layout; use explicit supported carriers`.
Wasmtime needs C callbacks for every host function
(`wasmtime_component_func_callback_t`), every host resource destructor,
`wasi_config_set_stdout_custom`, and `wasmtime_store_epoch_deadline_callback`.
The sandbox's deterministic clocks, seeded randomness and filesystem
normalization are all host functions shadowing WASI's, and Nexus's `blob`
resource and ring are host functions. Without callbacks none of the three
hosts can be written. The manuscript does not define callbacks (no
"callback", "upcall" or C-callable export anywhere in §16), so this needs a
language decision and the smallest manuscript change, for example an
`extern "C" fn` item whose parameters are `.vsys` carriers (`usize`
addresses, scalars, handles) and a `.vsys` parameter type `CFn[sig]` taking
it, plus a `usize` context word. It touches `src/ffi_lowering/**` and the
native backend, which no SH6 lane owns. Lane 13 (#1408) needs the same
feature for Pulse and signal handlers, so one lane should own it.

**Gap 2: variadic C functions are refused.**
`fn open(nocapture in path: CStr, flags: i32, ...) -> ForeignVal[i32] errno;`
gives `Vsys: unsupported token (variadics, attributes and imperative code
are forbidden)`. `open`, `openat`, `ioctl`, `fcntl` and `syscall` are
variadic. Declaring them with fixed arguments is correct on x86_64 SysV,
Windows x64 and Linux AArch64, but **wrong on Apple AArch64**, where
variadic arguments go on the stack. That is a required CI platform.
Workarounds exist for every call these crates make: `mkstemp` (always
`O_EXCL`, mode 0600) plus `rename` for new files; `open` without `O_CREAT`
needs no mode argument; `clonefile` and `renamex_np(RENAME_EXCL)` on macOS;
glibc's non-variadic `renameat2`. The crates can be ported without this
feature, but a `...` tail in `.vsys` would be the honest fix.

**Gap 3: `CSlice[u16]` cannot be lent.** Passing a `Slice[u16]` gives
`SliceArgument: a CSlice[u16] argument may be a Slice[u16] only when ... u16
is u8, u32, u64, i64 or usize`. Windows wide paths (`MoveFileExW`,
`CreateFileW`) are UTF-16. The workaround is to encode to little-endian
bytes in a `ChunkSeq[u8]` and copy them into a `malloc`ed buffer (above).
Adding `u16` (and `i32`) to the lent strides is likely small.

**Gap 4: no pointer-only buffer parameter.** A `CSlice` always passes a
length too, and `Ptr[T]` points to one element. C functions that take
`const T *` with no length (wide strings, `wasmtime_component_val_t *`
argument arrays) need the `malloc` + `memcpy` + `usize` workaround.

**Gap 5: a `.vsys` symbol that the runtime also imports cannot be declared.**
`fn write(fd: i32, nocapture in buf: CSlice[u8]) -> ForeignVal[i64] errno;`
fails at codegen: `foreign symbol 'write': Function write signature ...
I32 ... is incompatible with previous declaration ... I64`. The native
runtime already declares `write` (with an `i64` descriptor). Any libc
function the runtime uses internally is unavailable to `.vsys` code unless
the two declarations agree. Small, but it is in the native backend.

**Gap 6: Windows executables with `.vsys` imports are not linked** (SH1
lane 7 gap 2, noted in `tests/vsys_package_imports.rs`). The Windows CI job
is required, so every Vesper-only crate here needs either that gap closed
or its Windows tests run through a `vesper build --lib` static library
linked into a Rust test harness.

**Gap 7: no build-time constants.** `include_str!` (the WIT texts) and the
`build.rs` fingerprints of loom-build and forge-runner (`VLB_*`, `VFR_*`:
source digests, locked engine dependencies, compiler version, host triple)
have no Vesper equivalent. WIT text can be a generated Vesper string
constant checked by a test. The fingerprint needs Loom to hand a build
identity to the package (the compiler's own identity replaces `rustc -vV`).

**Gap 8: CPU feature detection.** forge-runner's `HostIdentity` records
`is_x86_feature_detected!` results. Vesper has no `cpuid`. Workaround:
`getauxval(AT_HWCAP/AT_HWCAP2)` on Linux, `sysctlbyname("hw.optional.*")`
on macOS, `IsProcessorFeaturePresent` on Windows, all non-variadic C.

**Gap 9: packaging `libwasmtime` and BLAKE3's C library.** A `.vsys`
library is linked by name with `--link-search`, but nothing builds C sources
or fetches a pinned C library. BLAKE3's portable C (plus its SIMD `.S`/`.c`
files) can be vendored, and `libwasmtime.a` can come from Wasmtime's release
archives pinned by digest, but Loom or the build needs a step that produces
them. Lane 12 (#1407) needs the same BLAKE3 library for `loom-audit`, so it
should be one shared package (for example `packages/vesper-blake3`), not two.

**Gap 10: what Wasmtime's C API cannot do** (upstream, not the compiler):

* *No custom WASI clocks or randomness.* `wasi_config_t` has no clock or RNG
  setters. The sandbox must shadow `wasi:clocks/*` and `wasi:random/*` with
  its own host functions (`_allow_shadowing` + gap 1).
* *A shadowing host function cannot call the stock one.* The Rust sandbox
  wraps the stock `wasi:filesystem/types` functions and normalizes their
  results. The C API exposes no handle to the shadowed implementation, so
  the Vesper sandbox would implement those `wasi:filesystem` functions
  itself over libc (`fstatat`, `readdir` sorted, renumbered metadata hashes)
  under the preopens' permission rules. That is the largest single piece of
  sandbox work.
* *Only static store limits.* `wasmtime_store_limiter` takes fixed maxima,
  with no callback, so the sandbox's peak-memory report and its typed
  denial reasons have to come from `wasmtime_memory_size` after the call and
  from the trap. Whether that reproduces every `SandboxError` variant the
  tests check is open.
* *No `tokio`.* The wall-clock backstop becomes
  `wasmtime_context_fuel_async_yield_interval` plus a
  `wasmtime_call_future_poll` loop that checks the clock between polls, on
  one thread.

**Gap 11: libraries the ports depend on that are not on `main` yet.** The
Vesper WIT reader (SH5 lane 11, PR #1355, open) for Nexus's `wit_gen`; a
Vesper JSON reader and writer with OLPC canonical form for TUF, admission
and the sandbox's flat records (the `vesper-serde` port, lane 11, #1406);
SHA-256/512, Ed25519, ECDSA P-256 verify and **RSA-PSS verify** for TUF
through lane 17's `vesper-crypto-sys` (#1412). RSA is not in lane 17's
prompt; TUF repositories may use RSA keys, so either lane 17 adds it or
loom-tuf refuses RSA keys, which would be a behaviour change.

**Gap 12: compiler-linked crates.** `vesper-loom-cas` and
`vesper-loom-admission` are root dependencies, so their Rust cannot be
deleted before Stage 2 unless lane 3 (#1398) moves the `cas`, `doc`, `shot`,
`verify` and `attest` commands and the `anvil_hash` verdict cache out of the
compiler. Until then they flip with the seed pattern (#1102).
`vesper-codegen-verified` (the DDC auditor, which stays Rust by the owner's
decision) uses only `blob_id`, `Digest`, `Domain`, `ObjectId` and
`ObjectKind` from loom-cas. When loom-cas goes, the auditor needs a ~60-line
Rust copy of the object-id scheme (BLAKE3 `derive_key` over the domain and
kind context) with a differential test against the Vesper store.

## Order of work

1. **`vesper-loom-cas`** first: no Wasmtime, the most users, and gaps 1, 4,
   7, 8 and 10 do not apply. Needs the BLAKE3 package (gap 9) and the libc
   file layer (`O_NOFOLLOW` opens, `mkstemp`, no-replace and exchange
   renames, `fsync` of directories, mode bits, file types, reflink). Lane 13
   (#1408) is moving `@vesper-std/fs` onto libc; these calls either extend
   that `.vsys` (lane 13's path) or live in a CAS-private `.vsys` in
   `crates/vesper-loom-cas/port/`. Seed pattern until lane 3 or Stage 2.
2. **`vesper-loom-admission`**, over the CAS port and lane 17's Ed25519.
3. **`vesper-loom-tuf`**, once the JSON port and lane 17's verify functions
   exist.
4. **`vesper-loom-sandbox`**, once gap 1 is closed and `libwasmtime` is
   packaged; it carries the largest new code (gap 10's filesystem layer).
5. **`vesper-loom-build`** and **`vesper-forge-runner`**, which are glue over
   CAS and the sandbox.
6. **`vesper-nexus`**, after the sandbox has proved the C-API binding and
   #1355's WIT reader has merged.

Each step runs the crate's Rust tests against the Vesper build as a
differential, then flips at the bar (0 differences, ABBA upper bound at or
below 1.5x).

## Flip verdicts

None yet: nothing is ported in part 1.

| Crate | Differential | ABBA (95% interval) | Rust deleted | PR |
|---|---|---|---|---|
| (none) | | | 0 | |

## Coordination asks

* Gap 1 (C callbacks) needs an owner and a manuscript decision. It blocks
  three of these crates and part of lane 13.
* Gap 9: one shared BLAKE3 C package with lane 12.
* Gap 11: RSA-PSS verify in lane 17's shim, or an owner decision that
  loom-tuf drops RSA keys.
* Gap 12: lane 3 moving the CAS/admission commands out of the compiler, and
  a decision on where the DDC auditor's copy of the object-id scheme lives.

## What is left

All of part 2 (the ports) and part 3 (the flips). The census row update in
`scripts/rust-census.py` waits for plan PR #1415, which adds that file.
