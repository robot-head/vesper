# Self-hosting SH4 lane 1: the codegen swap point (#1275)

Stacks on #1259 (SH3 lane 2, the IR swap). Base: #1259's head `8ac0f94`,
which merges origin/main `e73cfd6`.

SH4 ports the native backend to Vesper one lowering family at a time. This
lane builds what every later SH4 lane needs to do that:

- **lent Cranelift handles**: the Rust `Codegen` lends its live
  `FunctionBuilder` and module to a Vesper hook for one request and takes
  them back;
- **the swap point** `src/codegen_swap.rs`: one optional hook per family,
  called where the Rust code dispatches it;
- **the lowering differential** (`crates/vesper-codegen-swap-test`): per-function
  CLIF text, object bytes and JIT runs, with the optimizer on and off;
- **the pilot**: the census lowering family emitted in Vesper
  (`@vesper/codegen-base`) through the `aggregate` hook, in the harness and
  in the hybrid compiler (module `codegen`).

The Rust backend stays the default and is not changed. Nothing in the
default compiler installs a hook.

## What changed

### Shim: lent handles, JIT modules, builder extras

`crates/vesper-cranelift-sys`:

- `lend(fb, module, op, args, hook)` and `lend_module(module, op, args,
  hook)` (`src/lend.rs`). The handles live in `lend`'s frame and own
  nothing. The hook gets one word, the address of a `Call`, and reads the
  request through `vcl_call_op`, `vcl_call_arg(_count)`, `vcl_call_args`,
  `vcl_call_builder` and `vcl_call_module`. It answers with
  `vcl_call_result` and gives the handles back with
  `vcl_call_return_builder` and `vcl_call_return_module`. A magic word
  rejects stale or forged call words. The consuming calls
  (`vcl_define_function`, `vcl_compile`, `vcl_builder_free`,
  `vcl_module_finish`, `vcl_module_free`) refuse or ignore lent handles.
  `Crossing` reports the sticky error of a failed call and any handle the
  hook did not return.
- Values, blocks, variables and stack slots are Cranelift's own entity
  numbers, so a value the host made is the same number in the hook. In a
  lent module, function and data indices are Cranelift's `FuncId` and
  `DataId`.
- The module handle has three backends: `Object`, `Jit` (`JITModule`,
  through `vcl_jit_module_new`, `vcl_module_finalize` and
  `vcl_function_address`) and `Lent`.
- Builder extras: `vcl_set_srcloc`, `vcl_set_cold_block`,
  `vcl_current_block`, `vcl_set_func_name` (`UserFuncName::user`),
  `vcl_import_function_colocated` (colocated `ExtFuncData`) and
  `vcl_switch` (`cranelift_frontend::Switch`).
- Module extras: `vcl_module_function_count` and `vcl_module_function_name`
  (`declarations()`), and `vcl_compile` with `vcl_module_last_code` and
  `vcl_module_last_unwind` (code bytes and Windows x64 unwind info).
- For the family lanes (lanes 8 and 14):
  - `vcl_module_get_function` and `vcl_module_get_data` are `Module::get_name`. A miss returns
    `INVALID` without setting an error.
  - `vcl_module_kind` returns object, JIT, or freestanding (an object for
    an OS-less target). A host lends a module as JIT with `lend_as` or
    `lend_module_as`. `codegen_swap` passes `Lent::jit` from the module
    type of `define_instances`, and `Lent::optimize` from the build.
  - `vcl_define_function` into a JIT module applies the Rust backend's
    `DefinitionLinkage::Jit.normalize_references`: references are not
    colocated.
  - `vcl_module_last_traps` returns the trap records of the last compile,
    as offset and code words.
  - `vcl_import_data_colocated` is a data import with an explicit
    colocated flag, which RISC-V freestanding objects need.
- 28 new `.vsys` imports with typed wrappers in `packages/vesper-cranelift`.
  The `.vsys` text is now also a file, `cranelift.vsys`, named by the
  manifest's `[package] vsys` field, so packages that depend on it get the
  library (`tests/header.rs` checks that it matches the directive).

Not added, on purpose:

- **Dynamic stack slots.** The compiler has no uses of them (`grep
  dynamic_stack` finds none in src/).
- **`ControlPlane`.** Cranelift only uses it for chaos-mode fuzzing. The
  compiler always passes the default.

Both are one small shim call each if a later lane needs them.

### The swap point

`src/codegen_swap.rs` (module docs list every hook, its dispatch site and
its requests):

| Hook | Dispatch site | Requests |
|---|---|---|
| `scalar` | `Codegen::ir_graph_at`, each instruction | `INSTRUCTION` for constants, reads, writes, binds, operators, `try`, loop and exit |
| `call` | the same | `INSTRUCTION` for `Op::Call` and `Op::Indirect` |
| `aggregate` | the same, and the layout helpers | `INSTRUCTION` for tuples, data, projections and indexing; the `aggregate::*` ops |
| `drops` | `Codegen::drop_box_value`, and `Op::Fip` | `drops::DROP_BOX`; `INSTRUCTION` |
| `seq` | `ir_graph_at` | `INSTRUCTION` for `Op::Seq` |
| `control` | the same | `INSTRUCTION` for closures, handlers and effect operations |
| `numeric` | the same | `INSTRUCTION` for `Op::Vector` and `Op::Tiled` |
| `ffi` | the same | `INSTRUCTION` for a call of a foreign (`.vsys`) effect operation |
| `export` | the v2 export glue (`native_export`) | the whole glue, over a lent module |
| `freestanding` | `native_freestanding::build_image` | the whole image |
| `wasm` | `wasm_backend::compile` | the whole component |
| `ebpf` | `ebpf_backend::compile_world` | the whole program |
| `verifiers` | `native_verifiers::verify_emitted` | the whole report |

How a request works:

- A hook gets a `Lent` with the following:
  - the builder and module;
  - the opcode and argument words;
  - a `Site` (function, block, instruction);
  - the `World` and `Program`;
  - the type, when the request is about one.
- It returns `Ok(true)` (handled, with its result words), `Ok(false)`
  (declined, so Rust emits) or `Err` (the compile fails).
- Errors from sites that cannot fail are recorded and surfaced by
  `take_error()` after the function is finalized.

How hooks are installed:

- `install` sets a process-wide table, which is what the hybrid uses.
- `with_hooks` scopes a table to one thread, which is what the harness uses.
- An `ACTIVE` flag keeps the default compiler to one atomic load per
  dispatch.

The pilot family's dispatch hunks in src/native.rs are listed below. Each
one sends the request through the hook first and falls back to the
unchanged Rust emission:

- `alloc`, `alloc_uninit`, `copy_bytes`;
- the scalar `load` and `store` paths, and `offset`;
- `data_into`'s zeroing and tag store;
- `pattern_condition`'s constant true, `and`, tag test and diamond.

### The pilot in Vesper

`packages/vesper-codegen-base` (`@vesper/codegen-base`) emits the 13
`aggregate` ops (`ALLOC` … `AND`) through the lent builder.
`aggregate_hook(call: u64) -> u64` takes the builder and module, dispatches
on the op and returns both handles. Vesper's linear extern types make
forgetting to return one a compile error. The crate also has `empty_hook`
(take and return, emit nothing, decline), which the crossing measurement
and the declined test use.

The hybrid compiler (`crates/vesper-hybrid`, module `codegen`) installs it
as the `aggregate` hook. Its object is built by `vesper build --lib`, which
links the `.vsys` symbols from `vesper_cranelift_sys`.
`VESPER_HYBRID_STATS=1` prints how many requests it emitted.

### Harness

The harness is its own workspace crate, `crates/vesper-codegen-swap-test`.
The root package cannot depend on vesper-cranelift-sys, because the shim's
own tests depend on the root package: Cargo accepts that, but Bazel
reports a dependency cycle. Its tests include the root package's
`tests/common/check_harness.rs` by path. That file's `repo()` now finds the
nearest ancestor holding `packages/` and `examples/`, so it works from any
crate.

Its library (`src/lib.rs`) provides the comparison and the hook glue:

- `compare(name, world, entry, hooks, optimize, run)` builds the world
  twice, once with no hook and once with the hooks, and compares:
  - the CLIF text of every function, keyed by instance;
  - the object bytes;
  - with `run`, the JIT result.

  It reports the first differing line of each function.
- `vesper_hook` runs a JIT-compiled Vesper word function over
  `vesper_cranelift_sys::lend`.
- `foreign_package` and `hook_function` load and JIT a hook package.

Port lanes 6-17 use it unchanged: they install their family's hook and read
the counts.

`tests/self_host_codegen_swap.rs` in that crate:

- the default compiler installs no hook;
- type codes match the shim's;
- the pilot over the corpus (every check-harness world with a `main`), with
  optimize on and off, and every fourth effect-free `main` run under the
  JIT both ways;
- a deliberately wrong hook (constant true emitted as `iconst.i8 2`) is
  caught;
- a declining hook leaves the build unchanged, and a hook error fails the
  compile with its message.

Bazel: the crate's `test/self_host_codegen_swap` takes the corpus as data
and is marked `long`. `vesper_package` gained a `vsys` attribute so the cranelift
package's `.vsys` file reaches port builds. There are new
`//packages/vesper-cranelift:cranelift_package` and `//:codegen_base_package` targets.

## Stage 1 guard

Backend lanes must not touch the verified path. This lane edits none of
`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**`. The check was `git diff
--name-only 8ac0f94` filtered by those paths, which prints nothing.

Every hook is on the native path. The verified backend
(`crates/vesper-codegen-verified`, `src/verified_codegen`) never calls
`codegen_swap`.

The bootstrap-stage1 workflow was dispatched on the branch (see the PR).

## Numbers

All on Linux x86_64. Release builds, except the test suite.

**The default compiler is unchanged.** I compiled every `.ves` file under
examples/, tests/ and bootstrap/ under 200 KB (528 files) with `vesper emit
--emit object`, with and without `--no-opt`, using both the base compiler
(`8ac0f94`) and this branch. 316 objects built (158 files × 2) and **0
differ**. Every file that failed, failed on both sides.

**The hybrid with the pilot installed matches.** Over the same files, its
158 objects are **byte-identical** to the default compiler's. The Vesper
hook emitted 7,734 requests and declined none.

**The pilot differential** (`the_pilot_swap_matches_the_rust_lowering`, debug build, 365 s):
298 world builds (149 corpus worlds with a `main`, optimize on and off),
882 functions and 74 JIT runs, with **0 CLIF, 0 object, 0 run and 0
one-sided differences**. 25,809 requests were answered from Vesper.

The corpus tests run on a thread with a 256 MiB stack, as the CLI does. The
deepest worlds overflow a test thread's 2 MiB in debug builds.

**The cost of the swap** (callgrind Ir). The file is
`tests/fixtures/integer_widths/kinds.ves`, the file with the most requests
(943). Counts are collected inside the hybrid's hook only:

| Part | Ir | Per request |
|---|---:|---:|
| Whole hook (`vesper_aggregate`: lend, Vesper emission, take back) | 2,333,455 | 2,474 |
| Vesper code (`vesper_function_*` self) | 403,002 | 427 |
| Shim (`lend`, `vcl_*` wrappers, handle checks, crossing) | 440,088 | 467 |
| Cranelift builder and allocation (the same calls Rust makes) | 1,490,365 | 1,580 |

The Cranelift part is work the Rust emission does as well. So one swapped
request costs about 900 Ir more than Rust: about 470 Ir for the crossing
and the shim's handle checks, and about 430 Ir for the Vesper emission
logic around the same builder calls. That makes these small ops about 1.6x
Rust per request. Over the whole compile of `kinds.ves` (676 M Ir with the
default compiler), the extra cost is about 0.85 M Ir, or **0.13%**.

The whole hybrid run is 911 M Ir, but most of that gap is the other swapped
modules: `check_layout` alone is 191 M. That is why the codegen numbers
above are toggled to the hook.

## Decisions

- **One `INSTRUCTION` request per IR instruction, plus small typed ops for
  the helpers.** Family hooks see whole instructions, and a port can take
  over one op at a time. The pilot's ops are the census family's helpers,
  because those are what `data_into`, `pattern_condition` and the
  projections call.
- **Hooks are plain `fn` pointers.** They sit in a process table plus a
  thread-local scope, the same shape as the SH1/SH2 swap tables. A hook
  that declines costs nothing on the Rust path. The default compiler pays
  one relaxed atomic load per dispatch site.
- **The JIT for word-function hooks.** `ForeignChecked::word_function`
  JIT-compiles a `fn(u64) -> u64` entry whose effect row holds only foreign
  effects (`WordFunction::compile_foreign`). This is the harness's way to
  run a Vesper hook without an AOT build. The edit to `src/ffi_lowering.rs`
  is additive, and it is outside this lane's exclusive paths.
- **The hook compile runs with no hook in effect.** JIT-compiling the hook
  package runs the native backend. Under `with_hooks`, that would call the
  hook being compiled while its own `OnceLock` is initializing, and the
  test would wait forever. The test compiles hooks under
  `with_hooks(CodegenHooks::default(), …)`.

## Compiler gaps

1. **A `.vsys` extern type cannot be named in `use`.** Repro: a package
   that depends on `@vesper/cranelift` and writes
   `use @vesper/cranelift::{Builder};` gets `E0111 module @vesper/cranelift
   has no item 'Builder'`. Extern types are global, so it works without the
   import, but a reader cannot see where `Builder` comes from.
2. **Byte comparison alone misses lowering differences.** The wrong hook in
   the divergence test emits `iconst.i8 2` for a constant true. Every
   function's CLIF differs, but the objects are byte-identical, because
   Cranelift folds a branch on either nonzero constant to the same code.
   Later lanes must keep the CLIF check, not only object bytes.
3. **Entity numbers cross as bare `u64`s.** The wrappers `value_of_word`
   and `block_of_word` rebuild typed handles from words. A typed
   `Lent`-request ABI, with `Value` and `Block` as export-ABI scalars, would
   remove that boilerplate from every family.

## What is left

- Lanes 6-17 add their families' Vesper emission and a hybrid install. The
  hooks, the dispatch sites and the harness are in place.
- The pilot covers the census helpers. `INSTRUCTION` requests for the
  aggregate family (tuples, data, projections, indexing) are dispatched but
  declined until lane 8 ports them.
- There is no hybrid `divergent` edit for `codegen` yet. The harness's
  wrong-hook test covers failure detection.
- Dynamic stack slots and `ControlPlane` stay out of the shim (see above).
