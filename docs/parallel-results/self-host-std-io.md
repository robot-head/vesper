# Self-hosting SH0 lane 3: `@vesper-std/fs`, `process` and `env`

Claim #1125, prompt `docs/agent-prompts/self-host-sh0/03-std-io.md`. Base:
`main` at `b71ac81` (2026-11-19).

A Vesper compiler driver has to read sources and manifests, walk `src/**` in
a fixed order, write objects atomically, run the linker and read its
arguments and environment. Before this lane `@vesper-std` had no way to do
any of that. Ports kept their OS edge in Rust.

## What changed

| Path | What |
|---|---|
| `packages/vesper-std/fs/` | `@vesper-std/fs`: `IoError` and its kind codes, `read`, `read_string`, `write`, `append`, `create_new`, atomic `replace`, `rename`, `remove_file`, `remove_dir`, `remove_dir_all`, `create_dir`, `create_dir_all`, `metadata`, `symlink_metadata`, `exists` (each with a `*_path` byte-path variant where a driver needs one); module `walk`: `Entries`, `list_dir`, `walk`, `entry_*` accessors, `entry_join`. `fs.vsys` is the `StdFs` import. |
| `packages/vesper-std/process/` | `@vesper-std/process`: `command`, `arg`, `env`, `env_remove`, `env_clear`, `cwd`, `stdio`, `discard`, `spawn`, `wait`, `stdout`, `stderr`, `child_id`, `release`, `output`, `success`, `exit`, `id`. `process.vsys` is `StdProcess`. |
| `packages/vesper-std/env/` | `@vesper-std/env`: `arg_count`, `arg`, `arg_bytes`, `var`, `var_bytes`, `var_count`, `var_at_bytes`, `current_dir`, `current_dir_bytes`, `os`. `env.vsys` is `StdEnv`. |
| `crates/vesper-std-host/` | The C-ABI host shim: 39 `extern "C"` functions over `std::fs`, `std::process` and `std::env`, the JIT symbol table, live-handle counters for leak tests. No dependencies, so the root package's tests can link it. |
| `crates/vesper-std-driver/` | The sample driver: a Loom package in `vesper/`, compiled under feature `vesper-port` by `vesper build --lib` through the port harness and linked in (`port/binding.rs`, `tests/driver.rs`). A package of its own because its build script links the compiler: in `vesper-std-host` it made a Bazel cycle through the root package's dev-dependency. |
| `tests/std_fs.rs`, `tests/std_process.rs`, `tests/std_env.rs`, `tests/std_io_support/` | Differential tests: every scenario runs in the JIT with the optimizer on and off and in the evaluator, against the real OS, checked against `std`, with a leak check on every run. |
| `src/ffi_lowering.rs` | One additive method, `ForeignChecked::jit_with_options`, so the JIT can run imports with the optimizer off (outside the lane's exclusive paths; flagged in the PR). |
| `scripts/test-port-harness.sh` | The driver test joins the export ABI tier, which runs on Linux, macOS and Windows. |
| `BUILD.bazel`, `Cargo.toml`, `Cargo.lock` | Test data for the three tests, the new workspace member and root dev-dependency (root patch rule). |
| `docs/manuscript/content.typ` | The std catalog entry for the three packages and the effect decision. |

## API

All three packages are plain Vesper over `ChunkSeq[u8]` and `Slice[u8]`.
Paths are `in Str` in the public API and bytes underneath: on Unix a path is
any bytes except NUL, and on Windows it must be UTF-8.

- **Errors.** `IoError { kind: u32, os: u32 }` holds a stable kind code
  (`not_found()` = 1 through `other()` = 13, matching `std::io::ErrorKind`
  groups) and the raw OS error. The host packs both into one `u64` error
  word, which every fallible import writes through an `inout u64`.
- **Files.** Each public `fs` function opens and closes its own
  `linear extern type FsFile`. No handle leaves the package, so the checker
  proves that every path closes it exactly once. `read` reads the length the
  file has now, then keeps reading until EOF, so a file that grows during the
  read is still read whole.
- **Atomic replace.** `replace` writes `<dir>.vst-<pid:32><counter:32 hex>.tmp`
  beside the target with create-new (a short name of its own, so a target
  name near the 255-byte limit still works; the host's `vstd_fs_parent_len`
  splits off the directory by the platform's rules, so on Unix a backslash
  stays a name byte), `fsync`s it, renames it over the target (`MoveFileEx`
  with replace-existing on Windows) and `fsync`s the directory (Unix). On
  any failure it removes the temporary file and raises. The test checks that
  no temporary file is left behind.
- **Walk.** `Entries` is one byte arena of `/`-separated relative paths, with
  end offsets and kinds. `list_dir` and `walk` sort with a stable bottom-up
  merge sort of indices by path bytes, so the order is the same on every OS.
  `walk` enters directories but does not follow symbolic links.
- **Processes.** `ProcCommand` and `ProcChild` are linear. A function that
  takes a live handle (`spawn`, `wait`) returns a `Result`, because a caller
  may not keep a linear handle live across an unhandled `Fail` (E0402).
  `output(sink c)` owns its handles and raises. `release` of a child that was
  never waited kills it and reaps it, so a dropped child leaves no zombie.
  `wait` collects captured stdout and stderr while the child runs, which
  avoids the full-pipe deadlock.
- **Text out.** Every variable-length result uses a copy-out protocol: the
  host returns the length and copies only into a buffer that is big enough,
  and the Vesper side grows the buffer and asks again. A long directory entry
  is not consumed until it fits.

## Effect decision (spec call, flagged)

The I/O packages perform the Bridge capability of their library:
`Ffi[StdFs]`, `Ffi[StdProcess]` and `Ffi[StdEnv]`. I did not add a separate
`Fs` effect. A `.vsys` capability already names exactly one library, §16.3
already makes Loom confine it through `capability_ceiling`, and the three
names already split file, process and environment access. A package that
reads files declares `"Ffi[StdFs]"`, and pure code cannot reach any of it.
The manuscript entry (§13, std catalog) says this. The three std packages
declare `capability_ceiling = ["Fail", "Alloc", "Ffi[Std…]"]`.

## Platform notes

- **Shim, not direct libc/Win32.** The `.vsys` ABI has no NUL-terminated
  string type and no `errno`/`GetLastError`, and every path and argument
  crosses as `(ptr, len)`. So `open`, `stat`, `opendir`, `posix_spawn`,
  `CreateFileW` and `CreateProcessW` cannot be imported directly. `read` and
  `write` could be imported directly, but nothing else could. The shim is
  Rust `std`, which already gets UTF-16 paths, `CreateProcessW` command-line
  quoting and `MoveFileEx` replace semantics right on Windows. See gap 2.
- **Windows.** Paths, arguments and variables must be UTF-8 on the way in,
  and text coming back is UTF-16 converted to UTF-8 (unpaired surrogates
  become U+FFFD). `sync_dir` is a no-op. A file whose name is not valid
  UTF-16 cannot be named.
- **Unix.** Names are raw bytes. `entry_path` returns `None` for a name that
  is not UTF-8, and the byte accessors still work.
- **Exit status.** `ExitStatus` holds `code` and `exited`, plus the signal
  on Unix.

## Tests and evidence

| Test | Modes | What |
|---|---|---|
| `tests/std_fs.rs` (6 + 1 ignored bench) | JIT opt, JIT no-opt, evaluator | round trips, `read_string` UTF-8 rejection, atomic replace with no leftover temporary file (also for a 240-byte name and, on Unix, a name with a backslash), typed errors (NotFound, AlreadyExists, DirectoryNotEmpty, InvalidInput for a NUL byte, a directory read as a file), rename/remove/exists/metadata, sorted `walk`/`list_dir` against a Rust model (names chosen so OS order, creation order and sorted order all differ), 200 opens and closes (JIT) |
| `tests/std_process.rs` (3 + 1 child) | all three | arguments, a variable, the directory, both captured streams and exit code 3 of `/bin/sh` against `std::process`; `env_clear` and `env_remove`; spawn of a missing program (NotFound from `spawn` and from `output`); wait twice (InvalidInput); a discarded command; a child released while running is killed (three runs of `sleep 30` finish in under 25 s); `exit(7)` in a child test process |
| `tests/std_env.rs` (3) | all three | every argument against `std::env::args_os`, variables (a 1,000-byte value that needs a regrown buffer, unset, empty key, UTF-8 text, the full `KEY=VALUE` listing), the current directory and `os()` |
| `crates/vesper-std-driver/tests/driver.rs` (2, `--features vesper-port`) | AOT object linked into the test binary | the sample driver (below) over `packages/vesper-std/fs` and `packages/vesper-compiler`; the report must equal one computed with `std`, no live handle or Vesper allocation left; the driver's `.vsys` copies must equal the packages' files |

**Leak checks.** `vesper_std_host::live_handles()` counts files, listings,
commands and children. Every JIT and evaluator run must leave all four counts
where they started, and the driver also checks the Vesper library's
`live_allocations() == 0`.

**Evaluator.** `ForeignChecked::evaluate` hands foreign calls to a host
closure. `tests/std_io_support` implements it once, generically: it takes the
parsed `.vsys` descriptors, marshals the source values into the C ABI (words,
`(ptr, len)` byte buffers, `inout` scalar slots, the two wire structs) and
calls the same `extern "C"` host functions the JIT binds. So the evaluator
does real I/O too, and all three modes agree on every scenario.

**Sample driver.** `crates/vesper-std-driver/vesper` reads three variables
(`VESPER_STD_DRIVER_ROOT`, `_OUT`, `_CC`). It walks `ROOT/src`, takes each
`.ves` file's size from `metadata`, reads the file and hashes its contents,
then runs `cc --version` and writes this report through `replace`:

```text
file <relative path> <size> <FNV-1a 64>
files <n>
bytes <total>
cc <exit code> <first stdout line>    (or: cc error <kind> when cc is missing)
```

It runs in the port harness's ABI tier, so the Linux shard and the required
macOS (AArch64) and Windows (x86_64 MSVC) jobs all run it. `vesper build`
cannot yet make a standalone executable with `.vsys` imports (gap 1), so the
"executable" leg is the AOT object linked into a Rust test binary, the same
way the ports link.

**Stage 1 guard.** No file under `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt`, `lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed.
The `bootstrap-stage1` workflow run on the branch is linked from the PR.

## Numbers

Reading the repository's `.ves` sources: walk `packages/`, `crates/`,
`src/`, `examples/` and `tests/` and read every `.ves` file (852 files,
3.60 MB per round, 5 rounds), Vesper in the optimized JIT against a plain
`std::fs` recursion, release build, ABBA over 4 rounds, median, with the
JIT's compile time subtracted (`bench_none`):

| | time (5 rounds) | throughput |
|---|---|---|
| Rust `std::fs` | 34.8 ms | 516 MB/s |
| `@vesper-std/fs` | 59.2 ms | 304 MB/s |
| ratio | 1.70x | |

The Vesper side does more work than the Rust model. It lists and sorts every
directory entry under `crates/` (most of them are not `.ves`), it builds each
path by copying bytes, and it compares sort keys one byte at a time through
`try_get`. The reads themselves are a single in-place `read` into a resized
`ChunkSeq[u8]`, with no copy. Wall clock on these hosts moves by up to 30%
between runs. Before this lane there was no Vesper number to compare.

## Compiler gaps

1. **`.vsys` imports reach only library builds, and only from the root
   module.** `ffi_lowering::check_module` takes imports out of band. The
   CLI's only source for them is `// @export vsys` directives in the root
   package's root module, read by `vesper build --lib`. A dependency package
   such as `@vesper-std/fs` cannot carry its own import, and `vesper build`
   or `vesper run` of an executable cannot import at all. So every consumer
   repeats the three `.vsys` texts in its root module
   (`crates/vesper-std-driver/vesper/src/lib.ves`, kept equal to the packages
   by `driver_vsys_copies_match_the_packages`). Repro: move
   the `// @export vsys` block from the driver into
   `packages/vesper-std/fs/src/lib.ves`. `export_directives_source` blanks
   every file outside the root package before it reads directives, so the
   block is ignored and the dependency's `StdFs::…` calls no longer check.
   Fix: a Loom manifest
   field naming the package's `.vsys` files (§16.3 already says importing
   `sys/lz4.vsys` synthesizes `Ffi[LibLz4]`), read by `packages::load` for
   every package in the graph.
2. **No C string parameter type in `.vsys`.** Without it, a path cannot go
   to libc or Win32 directly, which is why the lane needs a shim. Fix: a
   `nocapture in path: CStr` parameter that the lowering copies from a
   `Slice[u8]` into a NUL-terminated stack or heap buffer (refusing interior
   NULs), plus an `errno`/`GetLastError` read after the call.
3. **Bound string views.** `let want = ".ves".as_bytes();` is E1531: a view
   of a literal cannot be bound, only passed. Byte-literal comparisons
   become four `at(...) == 46` tests (`is_ves` in the driver).
4. **Variable shifts raise.** `(n >> shift) & 15` with a variable `shift`
   performs `Fail`, so a function without `Fail` in its row is E0401. The
   workaround is `wrapping_shr_u64(n, wrapping_u32(shift))` (known, P3b
   lane 5).
5. **A non-NULL handle result has an impossible error arm.**
   `ForeignVal[ProcCommand]` comes back as `Result[ProcCommand,
   ForeignInvalid]`, and `command` spells the arm the host never takes as
   `Err(_) => loop {}`. A `.vsys` result that the library promises is never
   NULL (`ForeignVal[!H]`?) would remove it.
6. **`ForeignChecked::jit` was optimize-only.** Added
   `jit_with_options(entry, symbols, optimize)` (an additive hunk in
   `src/ffi_lowering.rs`).
7. **Throughput.** The walk's sort comparison reads one byte per `try_get`
   call. A `Slice[u8]` three-way comparison builtin (`cmp_bytes`, or `<` on
   byte slices as `String` already has) would turn it into one `memcmp`.

## Left for later

- Streaming file APIs (`FsFile` read and write in chunks, seek). The
  compiler driver needs only whole files, so the handles stay internal.
- Stdin pipes for children, a timeout on `wait`, and environment
  mutation (`set_var` is unsound beside other threads, so it is left out).
- A Bazel `vesper_port_object` target for the driver. Bazel builds the shim
  crate and runs the root tests; the driver object is built by the Cargo
  harness on all three platforms.
- File locking, permissions, symlink creation, canonicalization and temp
  directories.
