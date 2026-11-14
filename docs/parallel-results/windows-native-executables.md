# Native executables on Windows (MSVC objects and linking)

Lane 24 of the [seventh-wave prompts](../agent-prompts/wave-seven/24-windows-native-executables.md),
claim issue [#347](https://github.com/robot-head/vesper/issues/347). The branch
is `claude/project-thread-f5i5r2` (the suggested name was
`wave7/windows-native-executables`). The base is
`283c811a` (`origin/main` when the lane started), a descendant of the required
`c025b9a`.

## What was built

1. **COFF objects for `x86_64-pc-windows-msvc`** (`src/native_windows.rs`,
   mounted as `vesper::native::windows`).
   - `native::object_instrumented` (and so `object`, `object_for_target`,
     `object_with_observer` and `vesper emit --emit object`/`vesper build`)
     routes every Windows target through the new module. On a Windows host
     with no `--target`, the host triple is Windows and takes the same path.
   - Cranelift chooses the Microsoft x64 calling convention
     (`WindowsFastcall`) from the triple: arguments in rcx/rdx/r8/r9, 32-byte
     shadow space, rdi/rsi/xmm6–15 callee-saved. No codegen setting changed;
     `is_pic` stays on, and the `fmod` import goes through a `.refptr.fmod`
     slot that `object` writes for COFF.
   - **Unwind information.** `cranelift-object` writes no unwind tables, so
     the module wraps the `ObjectModule` in an `UnwindModule` that forwards
     every `Module` call unchanged and, after each
     `define_function_with_control_plane`, asks Cranelift for the compiled
     function's `UnwindInfo::WindowsX64`. After `finish()` it appends one
     `UNWIND_INFO` per function to `.xdata` and one `RUNTIME_FUNCTION`
     (begin, end, unwind data) per function to `.pdata`, sorted by address,
     each field an `IMAGE_REL_AMD64_ADDR32NB` relocation. The machine code is
     exactly what the module would have emitted without the wrapper.
   - Functions defined from raw bytes are rejected (they have no prologue
     description); Cranelift's frameless leaf functions get no entry, which
     is what Windows expects for leaf functions.
   - **Pulse session objects** (`native::session::object`, ABI v2 fiber
     entries that import `vesper_fiber_park`) take the same path for Windows
     targets, through `windows::emit_with`. The entry uses the Windows
     convention, which is the one the wave-six Windows `vesper_fiber_start`
     calls with (rcx/rdx). With `.pdata` present, the wave-six vectored
     fault handler and `RtlVirtualUnwind` can walk generated frames on a
     fiber stack down to `vesper_fiber_start`, whose own unwind info stops
     the walk.
   - Every other Windows triple (`aarch64-pc-windows-msvc`,
     `x86_64-pc-windows-gnu`, `i686-…`) is a typed
     `WindowsNativeError::UnsupportedTarget` (or the existing
     "must be x86_64 or AArch64" error), never a wrong object.
   - `windows::inspect` parses a COFF object's header, sections, `.pdata`,
     `.xdata` and symbol table with bounds checks and returns a summary or an
     error; 2,000 random mutations of a real object never panic.
2. **MSVC link driver** (`windows::link_executable`, `windows::link`,
   `windows::Linker`).
   - Without `--linker` (or with the default `cc`), it finds the Visual
     Studio `link.exe` for x64 through `find-msvc-tools` (the crate `cc`
     already uses; it was already in `Cargo.lock`), which also supplies the
     `LIB` and `PATH` environment. It falls back to `lld-link` on `PATH`. It
     never runs a bare `link` from `PATH`, because Git for Windows and MSYS
     put coreutils `link` there.
   - `--linker <path>` picks the flavor from the file name: `lld-link`,
     `rust-lld` (run as `rust-lld -flavor link`), or `link.exe`-compatible.
   - Arguments: `/NOLOGO /SUBSYSTEM:CONSOLE /MACHINE:X64 /DEBUG:NONE /OUT:…`,
     the objects, then the static MSVC runtime `libcmt.lib libucrt.lib
     libvcruntime.lib kernel32.lib`. The CRT's `mainCRTStartup` calls the
     exported `main`; its result is the process exit code. The executables
     depend on no CRT DLL.
   - `-o out/fib` writes `out/fib.exe` (an extension is added only when the
     path has none). A missing linker, a failed spawn and a failed link are
     typed errors; the link error carries the linker's output.
   - `vesper build` routes Windows targets (`--target x86_64-pc-windows-msvc`,
     or no target on a Windows host) to this driver in one hunk of
     `src/main.rs`; other targets still run `cc … -lm`.
3. **CI.** The Windows (x86_64 MSVC) job now runs
   `scripts/test-native-host.sh`, the same script and example list as the
   macOS job, against the Cargo-built `target/debug/vesper.exe` (passed to
   the script through its `VESPER_BAZEL_EXECUTABLES` file). The job's
   `cargo test` also runs two Windows-only unit tests (below).

## Acceptance

The examples `scripts/test-native-host.sh` runs on Linux and macOS
(`fibonacci` 55, `transaction` 42, `projections` 42, `counter` 3, `fail` 42,
`closures` 42, `resumptions` 42, `checked_math` 42) are built for Windows and
must exit with the same status:

- **Windows CI** (`windows-latest`, Visual Studio 2022 `link.exe`): the
  "Validate host native executables" step, and the unit test
  `examples_link_and_run_natively_with_the_evaluator_result`, which also
  checks each expected status against the evaluator's result.
- **Locally under Wine 9.0 (cloud host, Ubuntu package).** No MSVC libraries are available
  here, so the MSVC-target objects were linked with MinGW-w64 GCC 13
  (`x86_64-w64-mingw32-gcc`; the COFF objects and calling convention are the
  same) and run with `wine`: all eight exit with the expected status. This
  validates the objects, not `link.exe`.
- **Unwinding.** `generated_frames_unwind_with_rtl_virtual_unwind`
  (Windows CI) compiles a C sampler with `cl.exe`, links it with a
  generated `fib(34)` object through the driver, suspends the worker thread
  at random points and walks its stack with `RtlLookupFunctionEntry` and
  `RtlVirtualUnwind`. Every sample taken in generated code must reach the
  calling C frame. Under Wine with MinGW the same sampler passed 68 of 68
  samples (maximum depth 29). With `.pdata`/`.xdata` stripped from the
  object (`objcopy --remove-section`), the walk fails, so the check
  discriminates. `llvm-readobj --unwind` decodes the emitted tables
  (`PUSH_NONVOL rbp`, `SET_FPREG rbp`, `SAVE_NONVOL` per callee-saved
  register).

## Checks

Run on the cloud host (x86_64 Linux, Rust 1.97 stable and 1.100.0):

- `cargo fmt --all -- --check`: clean.
- `cargo test -p vesper --lib native::windows`: 9 passed (the two
  `cfg(windows)` tests are compiled out on Linux).
- `cargo clippy -p vesper --lib --tests --bins --target x86_64-pc-windows-gnu
  -- -D warnings`: clean, so the `cfg(windows)` code and tests compile and
  lint.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  see the PR for the result.
- `cargo test --workspace --locked`: see the PR for the result.
- `python3 tools/bazel/sync_build_files.py`: no change.

## Root patch and shared-file hunks

- Root `Cargo.toml`: `find-msvc-tools = "=0.1.15"` under
  `[target.'cfg(windows)'.dependencies]` of the root package; `Cargo.lock`
  gains that one dependency edge (the package was already locked). Own
  commit.
- `src/native.rs` (lane 23's file): the `windows` submodule declaration and a
  three-line Windows branch in `object_instrumented`. `src/native_session.rs`:
  a five-line Windows branch in `object`. Own commit.
- `src/main.rs` (lane 14's file): one routing hunk in `build`. Own commit.
- `.github/workflows/platforms.yml`: the Windows host-native step, and the
  macOS step's comment that said MSVC linking was unsupported.

## Decisions

- Unwind tables are collected by wrapping the module rather than editing
  the per-function loop in `native.rs`, so the hunk in lane 23's file stays
  a target switch and the emitted code cannot change.
- The static CRT (`libcmt`) rather than the DLL CRT, so executables run on
  a machine without the Visual C++ redistributable.
- `-o` without an extension gets `.exe`, matching what Windows looks for
  when it starts a program by a name without an extension.
- `is_pic` is unchanged on Windows; the COFF writer's `.refptr` slot keeps
  `fmod` reachable without a GOT.

## Remaining gaps

- **Pulse executables.** No platform links a session object into an
  executable yet: Linux runs sessions only through the JIT host
  (`native::session::Compiled`, Linux-only because `vesper-pulse`'s
  scheduler is io_uring-coupled), and `vesper-fiber-abi` is an rlib, not a
  linkable runtime library. This lane makes Windows session objects correct
  (Windows convention, unwind tables, `vesper_fiber_park` import) for the
  fibers from wave six; linking them with a runtime and a driver `main`
  waits on a portable Pulse driver (wave-seven lane 2) and a runtime
  library target.
- Only `x86_64-pc-windows-msvc`. Windows on ARM64 would need ARM64 `.pdata`
  (Cranelift can produce the info; the table writer is x64-only), and
  `windows-gnu` is rejected rather than linked with MinGW.
- No debug information (`/DEBUG:NONE`, no CodeView), no structured exception
  handlers in generated code (`UNWIND_INFO` flags are 0), and no
  `/GUARD:CF` metadata.
- `vesper build --tee` measures whatever file the driver wrote; TDX/CCA
  measurement of PE images was not checked.
- The local evidence is Wine plus MinGW; only Windows CI exercises
  `link.exe` and the Windows loader.
