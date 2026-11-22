# Rust exit gap wave 1, lane 10: wasm seed input and heap (#1508)

Status: part 1. The wasm seed can now read its arguments, environment and
files and write its outputs, and `Box` and recursive enums lower. No
compiler package builds to wasm yet: every compiler package uses `Fail`,
and `Fail` handlers (`Op::Handle`) are lane 9's next step (#1506).

Base: PR #1528 (lane 9, sequences and strings) at a54d5f5c, on `origin/main`
76a27738. Prompt: docs/agent-prompts/rust-exit-gaps-1/10-wasm-seed-input.md.

## Design: the standard I/O libraries over WASI 0.2

The compiler driver does its I/O through `@vesper-std/env` and
`@vesper-std/fs` (see crates/vesper-std-driver). Their `.vsys` libraries
`StdEnv` and `StdFs` are lowered by the Bridge (src/ffi_lowering.rs) into
the effects `Ffi$StdEnv` and `Ffi$StdFs`. Natively the C-ABI shim
crates/vesper-std-host implements them. So the seed needs no new source
API: the same driver source builds to native and to wasm.

The port (`packages/vesper-wasm-backend`, new `src/host.ves`) lowers every
call of the 24 operations to a generated shim function over WASI 0.2
imports:

| Library | Operations | WASI |
|---|---|---|
| `StdEnv` | `arg_count`, `arg`, `var`, `var_count`, `var_at`, `current_dir`, `os` | `wasi:cli/environment` `get-arguments`, `get-environment`, `initial-cwd` |
| `StdFs` | `open` (4 modes), `file_len`, `read`, `write`, `sync`, `close`, `rename`, `remove_file`, `remove_dir`, `create_dir` (and recursive), `metadata`, `sync_dir`, `parent_len`, `unique`, `read_dir`, `dir_next`, `dir_close` | `wasi:filesystem/preopens` `get-directories`; `wasi:filesystem/types` `open-at`, `read`, `write`, `stat`, `stat-at`, `sync`, `create-directory-at`, `remove-directory-at`, `unlink-file-at`, `rename-at`, `read-directory`, `read-directory-entry`, and the two resource drops |

Semantics follow the native shim:

- An absolute path resolves against the preopen with the longest name that
  is a whole-component prefix of it. A relative path resolves against the
  preopen named `.`, or else the first one. With no matching preopen a call
  fails with NotFound (ENOENT), and a NUL byte gives InvalidInput.
- WASI `error-code`s become the shim's error words: the `IoKind` in the
  low 8 bits and the Linux errno of the same condition in the high 32. A
  wasm run therefore returns the same words as a native Linux run.
- Files and directory listings live in a static table of 32 slots. The
  heap is a bump region that tail loops evacuate, and a handle is a plain
  `u64` that no evacuation follows, so handle state cannot live there. A
  file slot keeps the position (WASI reads and writes take offsets), and
  append mode writes at the size `stat` reports. A listing slot keeps the
  entry that did not fit the caller's buffer, with names up to 256 bytes.
- `vstd_env_os` is 0 (wasm is no particular platform). `current_dir`
  without a host-given initial directory, and `remove_dir` with
  `recursive`, fail with Unsupported. `sync_dir` opens the directory and
  does not sync it.

`cabi_realloc` is exported whenever a library is used, because the imports
return lists through it. The WIT world imports the interfaces, and the
harness wrapper (tests/common/wasm_words.rs, shared with `vesper-hybrid`)
gained the WASI 0.2 WIT they need, with every type spelled as WASI defines
it. `vesper build --target wasm32-wasip2` of a package with `.vsys`
imports (src/vsys_commands.rs) checks the foreign calls against the host's
`.vsys` view and hands the world to the wasm backend. `vesper-hybrid`
serves the build from the port. The Rust backend refuses the program (the
entry must handle `Ffi$StdEnv`), as it does every program only the port
lowers.

## Heap: `Box` and recursive enums

`Op::Fip` (`Box::new`, `Box::new_in_caller`, `@fip` slot reuse) was
"IR operation unknown" in both backends. `Box[T]` is the prelude's
one-field struct, so `Box::new(v)` is a one-word block holding `v`.
Patterns (`Box(x)`) were already handled as struct patterns. The heap is
the backend's bump region, which tail loops reset after evacuating the
live values. So a reused `@fip` slot is a fresh block too, and the
`box_drops` reclamation points need no code. Frees are not observable, so
the semantics match native.

## Tests (local)

- `tests/wasm_seed_package_io.rs` (new):
  - `file_system_runs_like_the_native_host`: two programs over every
    `StdFs` operation run natively (the JIT bound to crates/vesper-std-host)
    in one fresh directory, and under Wasmtime with another fresh directory
    preopened at its absolute path, optimizer on and off. The folded values
    agree, and the files left behind are checked. The programs cover
    write, truncate, append, create-new clash, missing files and
    directories, short buffers, metadata, listings through a small buffer,
    rename, removal (including of a non-empty directory) and
    `parent_len`. This test runs on Linux only.
  - `relative_paths_use_the_dot_preopen` checks relative paths, and that a
    path outside every preopen fails with NotFound.
  - `arguments_and_environment_come_from_wasi` checks every `StdEnv`
    operation against the WASI context's values.
  - `boxes_and_recursive_enums_run_like_the_evaluator` runs 5 heap
    programs: a boxed list, a boxed tree, a tail loop that evacuates while
    a 300-cell boxed list is live, a struct with an `Option[Box[_]]`
    field, and `@fip` reuse. Each runs under Wasmtime equal to the
    evaluator, optimizer on and off.
- `vesper-hybrid build --target wasm32-wasip2` of a package that calls
  `StdEnv` and `StdFs` builds a component. `vwasm --dump` lists its 18
  WASI imports as missing (see below).
- `tests/self_host_wasm_backend.rs` and `tests/wasm_seed_breadth.rs`: see the
  PR body. A program without `Ffi$StdEnv` or `Ffi$StdFs` gets no imports
  and no static data, so modules Rust also lowers stay byte-identical.

There is no Rust baseline for speed: Rust compiles none of these programs.

## Stage 1 guard

`git diff a54d5f5c -- src/verified_codegen src/ir.rs src/ir_opt* lean
bootstrap packages/vesper-compiler` is empty. The only Rust source change
is the `.vsys` build path in src/vsys_commands.rs, which the Stage 1
compiler does not contain. `bootstrap/stage1.lock` is unaffected.

## For SH6 lane 2 (#1397): the runner's new imports

`bootstrap/seed/runner/vwasm.c` needs these core imports, with the
canonical ABI lowering (every result goes through a return area, and
lists come from `cabi_realloc`):

```text
wasi:cli/environment@0.2.0      get-arguments(ret)            get-environment(ret)
                                initial-cwd(ret)
wasi:filesystem/preopens@0.2.0  get-directories(ret)
wasi:filesystem/types@0.2.0     [method]descriptor.open-at(self, path-flags, ptr, len, open-flags, descriptor-flags, ret)
                                [method]descriptor.read(self, len: i64, offset: i64, ret)
                                [method]descriptor.write(self, ptr, len, offset: i64, ret)
                                [method]descriptor.stat(self, ret)
                                [method]descriptor.stat-at(self, path-flags, ptr, len, ret)
                                [method]descriptor.sync(self, ret)
                                [method]descriptor.create-directory-at(self, ptr, len, ret)
                                [method]descriptor.remove-directory-at(self, ptr, len, ret)
                                [method]descriptor.unlink-file-at(self, ptr, len, ret)
                                [method]descriptor.rename-at(self, ptr, len, new-desc, ptr, len, ret)
                                [method]descriptor.read-directory(self, ret)
                                [method]directory-entry-stream.read-directory-entry(self, ret)
                                [resource-drop]descriptor(handle)
                                [resource-drop]directory-entry-stream(handle)
```

All parameters are `i32` unless marked. The shims read these result
layouts: `result<descriptor, error-code>` uses disc@0 and payload@4;
`result<_, error-code>` uses disc@0 and error@1; `read` gives ptr@4, len@8
and eof@12; `write` gives the count or the error at @8; a stat result has
type@8, size@24 and modification time disc@56, seconds@64, nanos@72; a
directory entry has option disc@4, type@8, name ptr@12 and len@16.

## Gaps found

1. Every `@vesper-std/fs` and `@vesper-std/env` function raises
   `Fail[IoError]`, so a whole-package seed build waits for `Fail`
   handlers (lane 9, `Op::Handle`). The tests call the `.vsys` functions
   directly from an `eff [Ffi[StdEnv], Ffi[StdFs]]` entry.
2. `StdProcess` (spawning the C compiler) has no WASI equivalent. The seed
   driver has to skip it, and the port refuses the program (entry must
   handle `Ffi$StdProcess`).
3. The evaluator overflows the default 2 MiB test stack on a 500-deep
   boxed-list recursion. The test runs on a 256 MiB thread.

## What is left

- After lane 9's handlers: build `packages/vesper-front` (then the checker
  packages) to wasm, run it under Wasmtime and `vwasm`, and compare it with
  the native build on the corpus. This is step 3 of the prompt and needs
  this PR and lane 9's.
- Closures and handlers as the compiler uses them (lane 9 owns `Op::Handle`).
- The runner imports above (lane 2).
