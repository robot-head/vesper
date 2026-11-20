# `.vsys` imports from dependency packages, C strings and handles (SH1 lane 7)

Self-hosting wave SH1, lane 7. Claim #1150, prompt
`docs/agent-prompts/self-host-sh1/07-vsys-packages.md`. Base `e29e63d`.

Part 1 (PR #1164, merged as `284a16a`): packages carry their own `.vsys`
imports. Part 2 (base `284a16a`, its own PR): C string parameters, OS error
codes, non-NULL handle results, the shim's `bool` flags and an evaluator
host for `vesper run`. Part 2's section is at the end.

## Repros on main first

| Gap (report) | On `e29e63d` | Scope |
| --- | --- | --- |
| std-io 1: imports only from the root module of a library build | fails: a dependency's `StdFs::…` calls do not check, `vesper check`/`jit`/`build` of an executable cannot import | **fixed here** |
| shim 1: no `bool` in `.vsys` | **passes**: `fn f(b: bool) -> ForeignVal[bool]` checks and runs in the JIT (`bool` crosses as `uint8_t`, which is C `_Bool` on all four targets) | dropped from the compiler scope; `bool_parameters_and_results` keeps it working. The shim's `u32` flags are a shim-side cleanup (part 2). |
| std-io 2: no C string parameter | fails: `unknown pointee CStr` | part 2 |
| std-io 5: non-NULL handle result has an error arm | fails: `ForeignVal[!H]` is an unsupported token | part 2 |

## What changed

* **Manifest field.** `[package] vsys = ["fs.vsys"]` in `Loom.toml`
  (`vesper_loom_calculus::Package::vsys`; the schema lives in
  `crates/vesper-loom-calculus/src/lib.rs`, which the seed package builds, so
  the seed and the port-backed package both read it; the port's Vesper
  validation is unchanged). Paths are relative to the manifest, inside the
  package directory, with `/` separators and no `..`, each named once. Struct
  literals of `Package` in the Loom calculus and solver tests and examples got
  `vsys: vec![]`. The field is skipped when empty, so serialized manifests are
  unchanged.
* **Loader.** `packages::load` reads the files for every package in the graph
  into `Assembled::vsys` (`PackageVsys { package, display, text }`), with
  manifest errors (`E0115`) for a missing file, a non-`.vsys` path, an
  absolute or escaping path and a repeat. `PackageVsys::source_for(target)`
  fills `@TARGET@`/`@ABI@`; `Assembled::imports(target)` builds the Bridge
  `Imports`. `packages::vsys_target` is now the one table of `.vsys` targets
  (the export directives use it).
* **`vesper build --lib`.** `native::export::compile_package_module` imports
  the carried libraries beside the root module's own `// @export vsys`
  blocks. A root module may set how it links a carried library with the new
  body-less form `// @export vsys StdFs [direct|isolated(mpk)] [rust=crate::fs]`;
  naming a library no package carries is an error.
* **Executables.** `vesper check`, `vesper jit` and `vesper build` of a
  package whose graph carries imports go through the Bridge lowering
  (`src/vsys_commands.rs`):
  * `check` reports Bridge diagnostics at their own files;
  * `jit` binds the standard I/O libraries (link name `vesper_std_host`) to
    the host shim, which the compiler now links (`vesper-std-host` moved
    from dev-dependencies to dependencies; it has no dependencies), and
    refuses any other library before running;
  * `build` emits `main` with the foreign calls lowered and links each called
    library by its link name (`-l<name>`), searching the new
    `--link-search <dir>` directories. Unix only for now.
  * `run` refuses with a pointer to `jit`/`build` (part 2).
  * Review fixes: `build` compiles through the CLI's `native_code`, so the
    verifiers that `ConstTime`, `Realtime` and `DetFloat` imply still run;
    a `link = "static"` library links its archive (by path from
    `--link-search`, else `-Wl,-Bstatic`) even beside a shared one.
  * `vesper-hybrid` lists `vesper-std-host` too (its manifest test requires
    every root dependency), and the CLI reaches the host through
    `ffi_lowering::std_host_symbols`.
* **Packages.** `@vesper-std/fs`, `process` and `env` name their `.vsys` in
  their manifests.
* **Driver.** `crates/vesper-std-driver/vesper/src/lib.ves` drops its three
  copied `.vsys` texts for three one-line directives naming the Rust
  modules, and `driver_vsys_copies_match_the_packages` is gone. The driver's
  object is byte-identical to the one built from the copies, and the
  generated binding equals the committed `port/binding.rs`.
* **Manuscript §16.3** (flagged): a "Packages Carry Their Imports" bullet.

## Tests

`tests/vsys_packages.rs`:

* `a_dependency_carries_its_vsys_import`: `@t/app` (an executable) over
  `@t/flip`, which carries `Probe`; checked from `packages::load` and run in
  the JIT with optimize on and off. Without the field, the calls do not check.
* `manifest_vsys_paths_are_checked`: the five manifest errors.
* `cli_checks_jits_and_runs_an_executable_over_std_fs`: an executable package
  over `@vesper-std/fs` as a dependency, with no `.vsys` text of its own:
  `vesper check`, and `vesper jit` (both optimizer settings) write, read back
  and remove a real file. Runs on Linux, macOS and Windows.
* `build_links_an_executable_with_a_dependency_library`: `vesper build` links
  the executable with a C `libprobe.a` the dependency's `.vsys` names, and
  runs it (Unix).
* `build_keeps_capability_implied_verifiers`,
  `static_libraries_link_their_archive`,
  `jit_refuses_a_library_it_does_not_host`,
  `cli_reports_a_dependency_error_at_its_file`,
  `a_library_build_links_a_carried_import_by_name`,
  `bool_parameters_and_results`.

Locally (Linux x86-64) the executable over `@vesper-std/fs` was also built
with `vesper build --link-search <dir of libvesper_std_host.a>` and read a
290-byte file correctly.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
bootstrap-stage1 workflow (Stage 1 reproducibility and cross-backend
ratchet, Stage 0 two-seed build) passed on the branch.

## Numbers

No code generation changed: the driver's object is byte-identical before and
after (the imports are the same text, from a different file). The interface
JSON lists the three libraries in package load order instead of directive
order.

## Compiler gaps

1. **`vesper run` cannot reach foreign libraries.** The evaluator hands each
   foreign call to a host closure; the CLI has none. Repro:
   `vesper run` of the `@t/tool` package in `cli_checks_and_jits…`. Fix
   (part 2): move the descriptor-driven marshaller of
   `tests/std_io_support/mod.rs` (`EvalHost`) into the compiler, with wire
   struct layouts taken from the descriptors.
2. **Windows executables with imports are not linked.** `link.exe` needs
   `<name>.lib` and the Rust staticlib's system libraries. Repro:
   `vesper build --target x86_64-pc-windows-msvc` of the same package.
3. **`vesper jit` hosts only the standard I/O libraries.** Other libraries
   would need `dlopen` of a `link = "dynamic"` artifact, checked against its
   pinned BLAKE3.
4. **Capability ceilings are not checked against calls.** A package may call
   `StdFs::…` without `"Ffi[StdFs]"` in its `capability_ceiling`
   (pre-existing; the loader does not compare rows with ceilings).
5. **Absolute package paths display with a doubled slash** (`//tmp/…`) in
   diagnostics: `packages::display` joins the root component with `/`.

## Left for part 2

C string parameters (`nocapture in path: CStr`, copied into a NUL-terminated
buffer, interior NULs refused, errno/`GetLastError` read after the call),
non-NULL handle results, the Cranelift shim's `bool` parameters in place of
`u32` flags (family tests byte-identical), and the evaluator host.

## Part 2

### What changed

* **`nocapture in s: CStr`** (`vesper_ffi::sys::ParameterType::CStr`; `in`
  only, never `Option`). In source it takes a `Slice[u8]`. The native
  lowering (`src/ffi_lowering/native.rs`) copies the bytes into a `malloc`
  buffer with a NUL appended, passes C one pointer, and frees it after the
  call; `memchr` finds an interior NUL, and then C is not called. A function
  with a `CStr` parameter returns `Result[R, ForeignInvalid]`, with
  `Err(ForeignInvalid { raw })` naming the first NUL's offset. JIT and
  objects use the same lowering (`malloc`/`memcpy`/`free` are the ones the
  sequence runtime already imports, plus `memchr`).
* **`-> ForeignVal[T] errno`** (`FunctionDescriptor::reads_errno`): the OS
  error code is read right after the call (after the MPK restore, before
  copy-backs and frees): `__errno_location()` on Linux, `__error()` on
  macOS/FreeBSD, `GetLastError()` on Windows (the JIT binds a Rust stand-in,
  since its symbol lookup does not search kernel32). The source result is
  `(R, u32)`. Canonical descriptor bytes are unchanged for functions without
  it (`errno/after-call` is appended only when declared).
* **`ForeignVal[NonNull[H]]`** (`ValueType::NonNullHandle`): source code gets
  `H` itself, no error arm. A `NULL` traps (user trap 6,
  `ffi_lowering::NULL_HANDLE_TRAP`; trap 7 is a failed C-string allocation).
  `CStr` and `NonNull` are now reserved type names in `.vsys`.
* **`vesper run`** of a package with imports runs in the evaluator through
  `ffi_lowering::EvalHost` (`src/ffi_lowering/eval_host.rs`), the
  descriptor-driven marshaller moved out of `tests/std_io_support`: wire
  structs from their compiled layouts (integers of every endianness, byte
  arrays, nested structs), slices of scalars or structs as `Slice` or tuple,
  nullable pointers and slices, `CStr`, `errno`, and every result mapping.
  It hosts the same libraries as `vesper jit` (`vesper_std_host`). The
  std I/O tests' evaluator mode now uses it.
* **Shim flags** (own commit): `vcl_module_keep_clif(keep)`,
  `vcl_declare_data(writable, tls)` and `vcl_trap_if(if_nonzero)` take
  `bool` in the Rust exports, the header (`<stdbool.h>`) and the `.vsys`;
  `@vesper/cranelift`'s wrappers pass their `bool`s through.
* **Manuscript §16.3** (flagged): "C Strings and OS Error Codes", and
  `ForeignVal[NonNull[H]]` in "Opaque Extern-Type Handles".

### Tests

In `tests/vsys_packages.rs`:

* `cstr_arguments_are_terminated_copies`: a whole slice, a sub-slice
  (`p[1..3]`), an empty slice and a refused one (no call made), JIT with
  optimize on and off, and the evaluator.
* `errno_is_read_after_the_call`: a host function that looks up a missing
  path; the code Vesper reads equals the one the host saw (2 on Unix, 3 on
  Windows), JIT and evaluator.
* `an_executable_passes_a_path_to_libc`: `vesper build` of an executable
  that calls libc `access(2)` directly through a `CStr` and gets `ENOENT`
  (Unix).
* `non_null_handle_results_have_no_error_arm` (JIT and evaluator; matching
  the result as a `Result` no longer checks) and
  `a_null_non_null_handle_traps` (an executable linked with C, Unix).
* `cli_checks_jits_and_runs_an_executable_over_std_fs`: `vesper run` now
  prints the same answer as `vesper jit`.

### Numbers

No existing code generation changed: calls without `CStr`, `errno` or
`NonNull` lower as before, and the shim's checked family tests pass
unchanged (the flag change touches no code they compile). A `CStr` call
costs one `malloc`/`memcpy`/`memchr`/`free` per string argument.

### Compiler gaps (part 2)

1. **`CStr` arguments always allocate.** A short string could use a stack
   buffer; paths are usually under 256 bytes.
2. **No `CStr` result** (a C function returning `const char *`), and no
   `CStr` fields in wire structs.
3. **The std I/O shim still takes `CSlice[u8]` paths.** It could take
   `CStr` and drop its own copies; that is the std I/O lane's call.
4. **The evaluator host calls through at most eight integer words**, which
   covers every current `.vsys` function.
5. **`@vesper/cranelift` keeps its library in a `// @export vsys`
   directive.** With part 1 it could move to a `[package] vsys` file.
