# `.vsys` imports from dependency packages (SH1 lane 7, part 1)

Self-hosting wave SH1, lane 7. Claim #1150, prompt
`docs/agent-prompts/self-host-sh1/07-vsys-packages.md`. Base `e29e63d`.

This is part 1 of the lane: packages carry their own `.vsys` imports. Part 2
(C string parameters, non-NULL handle results, the shim's flag conversions
and an evaluator host for `vesper run`) follows in its own PR.

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
* `cli_checks_and_jits_an_executable_over_std_fs`: an executable package
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
