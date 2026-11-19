# Self-hosting SH0 lane 4: Cranelift through a C-ABI shim

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1126. Base: `main` at `b71ac81` (plan PR #1131). Lane prompt:
[`docs/agent-prompts/self-host-sh0/04-cranelift-shim.md`](../agent-prompts/self-host-sh0/04-cranelift-shim.md).
Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external
library behind a C-ABI shim; only the lowering moves to Vesper.

## Summary

Vesper code can now drive Cranelift. Three pieces:

- **`crates/vesper-cranelift-sys`**: a shim over Cranelift 0.125.4 (the root
  compiler's version and features), built as an rlib and a staticlib, with
  `include/vesper_cranelift.h`. 83 `extern "C"` functions over four opaque
  handles (module, builder, data description, object).
- **`packages/vesper-cranelift`** (`@vesper/cranelift`): the `.vsys` library
  (in the package's `// @export vsys` directive) and typed wrappers, so a
  Vesper lowering reads like the Rust one:

  ```text
  let entry = create_block(inout b);
  append_block_params_for_function_params(inout b, entry);
  switch_to_block(inout b, entry);
  let x = block_param(inout b, entry, 0);
  let r = ins_iadd_imm(inout b, x, 1);
  ins_return1(inout b, r);
  define_function(inout m, f, b)       // consumes the builder
  ```

- **Tests** (`crates/vesper-cranelift-sys/tests/`): a pilot that Vesper
  builds, the system linker links and the host runs; one real compiler
  function family emitted from Vesper and from Rust with byte-identical
  objects; handle leak tests; overhead measurements.

Nothing in the compiler changed. Rust stays the default build everywhere,
and no Rust code was deleted.

## Design

**Handles.** `Module`, `Builder`, `Data` and `Object` are
`linear extern type`s, so the checker makes every Vesper path finalize each
exactly once (`E0302` for a leak, `E0303` for a second use; both are tested).
Each has one finalizer (`vcl_*_free`) and the consuming calls take ownership:
`vcl_define_function(inout m, func, sink b)`,
`vcl_define_data(inout m, data, sink d)`, `vcl_module_finish(sink m)` and
`vcl_object_publish(sink o)`. A `Builder` owns its `Function` and
`FunctionBuilderContext` (boxed, so the `FunctionBuilder` borrowing them is
stored in the handle); a `Module` owns an `ObjectModule`, its signature,
function and data tables and a reused `codegen::Context`.

**Integers for everything else.** Values, blocks, variables, stack slots,
`FuncRef`s, `SigRef`s and global values are Cranelift's own entity numbers,
so a Vesper lowering numbers them exactly as the Rust lowering would (that is
what makes byte-identical output possible). Module signatures, functions and
data are indices into the module's tables (`vcl_declare_function` returns the
same index for a name declared twice). Types are `lane | log2(lanes) << 8`
(`I64` = 4, `I8X16` = `1 | 4 << 8`); opcodes, condition codes, linkages,
calling conventions, trap codes and memory flags are the tables in
`src/codes.rs`. On the Vesper side each is a one-field `copy struct`
(`Value`, `Block`, `Ty`, `Var`, `Slot`, `FuncRef`, `SigRef`, `GlobalValue`,
`FuncId`, `DataId`, `SigId`), so they cannot be mixed up.

**Coarse calls.** Instructions are grouped by shape: `vcl_binary(b, op, x, y)`
covers 38 opcodes, `vcl_binary_imm` 15, `vcl_unary` 18, `vcl_convert` 15,
`vcl_overflow` 6, `vcl_ternary` 4, `vcl_load` 7 and `vcl_store` 4, plus
`vcl_icmp`, `vcl_icmp_imm`, `vcl_fcmp`, constants, stack slots, lanes,
`vconst`, `shuffle`, atomics, addresses, calls and terminators. 94 + 5 = all
99 instruction kinds the compiler's lowering uses are reachable. `vcl_emit`
takes a batch of word-encoded instructions (an operand with bit 63 set names
an earlier result of the same batch) and writes one result per instruction.

**Errors.** Every call runs under `catch_unwind`, so no Cranelift panic
crosses the C boundary. A failed call returns `0xffffffff` (`invalid()`), or
a null handle, and sets a sticky error: a builder refuses every later call,
and `vcl_define_function` reports the error (verifier errors are
pretty-printed). `vcl_module_error`, `vcl_builder_error` and `vcl_new_error`
copy the message out. Tests cover a verifier error, an unknown opcode, a bad
triple and a churn of 50 modules.

**Getting the object out.** `object_bytes(sink o)` copies it into a
`ChunkSeq[u8]`; `object_publish(sink o)` hands it to the Rust host, which
takes it with `vesper_cranelift_sys::take_published(id)` (the JIT tests use
this, since a JIT entry cannot return a sequence).

### Files

| Path | What |
|---|---|
| `crates/vesper-cranelift-sys/src/lib.rs` | the shim |
| `crates/vesper-cranelift-sys/src/codes.rs` | type, opcode and flag codes |
| `crates/vesper-cranelift-sys/include/vesper_cranelift.h` | C header |
| `packages/vesper-cranelift/src/lib.ves` | `.vsys` library and wrappers |
| `crates/vesper-cranelift-sys/tests/pilot.rs` (+ `fixtures/pilot.ves`, `pilot_main.{c,rs}`) | pilot, leaks, errors, checker |
| `crates/vesper-cranelift-sys/tests/checked_family.rs` (+ `fixtures/checked.ves`) | byte comparison, overhead |
| `crates/vesper-cranelift-sys/tests/header.rs` | exports = header = `.vsys`; codes round trip; header compiles as C |

Outside the exclusive paths: the root `Cargo.toml` member line and
`Cargo.lock` (root patch rule, own commit), and one step in
`.github/workflows/platforms.yml` that runs `cargo test -p vesper-cranelift-sys`
on macOS and Windows, which the lane's "tests pass on all four platforms"
needs (Linux runs it under Bazel).

## The pilot

`tests/fixtures/pilot.ves` builds, for the host target with
`opt_level=speed`, an object with:

- `pilot_arith(a, b) = (a + b) * 3 - (a ^ b)`, through an 8-byte stack slot;
- `pilot_sum(n)`, a loop whose header takes block params `(i, acc)` and
  whose exit block takes `acc`;
- `pilot_call(x) = pilot_arith(x, x + 1) + pilot_sum(x) + table[x & 3]`: two
  calls, a `Variable` and a load from a local data object `pilot_table`
  (bytes built in Vesper, aligned to 8).

The test JIT-runs the pilot with the shim's symbols bound, writes the
published object, links it with a host program (`cc` with `pilot_main.c` on
Linux and macOS, `rustc` with `pilot_main.rs` on Windows, so `link.exe`
links the COFF object) and checks the program's output against a Rust model
on seven inputs. Every module, builder, data and object handle is counted:
the live counts are equal before and after each scenario.

## The compared family: checked arithmetic

`tests/fixtures/checked.ves` emits `src/native.rs`'s `checked_arithmetic`
(the overflow test of each operator and kind), `binary` (including signed
`/`'s wrap/divide/join diamond) and `check_overflow` for `+ - * /` over
`i8 i16 i32 i64 u8 u16 u32 u64`: 32 functions `checked_<op>_<kind>(a, b)`.
`tests/checked_family.rs` builds the same module with the Rust code copied
from `src/native.rs` (a private method, so it is mirrored, not called).
The one deliberate difference from the compiler, made in both: the overflow
block traps instead of performing `Fail::raise`, which is the effect
lowering and not this family.

**Result: the object files are byte-identical** in all six configurations:
host with `opt_level=none`, host with `speed`, and cross builds for
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`aarch64-apple-darwin` and `x86_64-pc-windows-msvc`. No differences to
explain.

## Overhead

Cost of emitting one `iadd` (a chain of N into one block, then the builder
is freed without compiling), release build, Linux x86-64. Instruction counts
are callgrind's `Ir` for N = 120,000 minus N = 20,000, divided by 100,000,
which removes the fixed cost (checking the package and JIT-compiling the
entry). Wall clock is the fastest of 5 runs at N = 1,000,000 minus N = 1
(`VESPER_CRANELIFT_BENCH=1`); it moves by up to 30% between runs on these
hosts.

| Path | Ir per `iadd` | vs direct |
|---|---:|---:|
| Rust, `FunctionBuilder` directly | 596 | 1.00x |
| Rust, through the C ABI (`vcl_binary`) | 738 | 1.24x |
| Vesper (JIT), the raw `.vsys` import | 775 | 1.30x |
| Vesper (JIT), the typed wrapper `ins_iadd` | 830 | 1.39x |
| Vesper (JIT), `vcl_emit` batches of 512 | 841 | 1.41x |

Wall clock agrees for the Rust paths (54-66 ns direct, 58-73 ns through
the C ABI over three runs) but is too noisy for the JIT paths on this host
(28-153 ns between runs), so the instruction counts are the result.

Reading: Cranelift's own work is about 600 instructions per emitted
instruction, and the shim adds 142 on top (the `catch_unwind` frame, the
sticky-error check, the handle and entity conversions), +24%. Calling the
shim from Vesper instead of Rust adds 37 more (the JIT's foreign-call
sequence), and the typed wrapper 55 more (one more call level and the
`Value` struct round trip). End to end, a Vesper lowering emits through the
wrappers at 1.39x the cost of the Rust lowering's emission. Batching does not
help: pushing three words per instruction into a `ChunkSeq` from Vesper costs
more than the call it saves, because the call is already cheap next to
Cranelift's work. So the wrappers stay one call per instruction; `vcl_emit`
stays for callers that already hold encoded instructions. The remaining
shim cost is mostly `catch_unwind`, which a release build of Stage 2 could
drop behind a verified-input mode. These numbers cover emission only;
compiling the function (`define_function`) is the same Cranelift code on both
paths.

## API inventory (what the SH4 lanes will need)

Measured over `src/` on base `b71ac81` (the 38k lines of `src/native*.rs`,
`src/seq/native*.rs`, `src/vector.rs`, `src/native_vector.rs`, `src/tiled*`,
`src/ffi_lowering*`, `src/freestanding*`, `src/native_export*` and the AOT
runtimes). Counts are call sites.

**Instructions (99 kinds), all reachable through the shim.** Most used:
`iconst load icmp_imm icmp iadd iadd_imm brif store jump return_ select band
uextend call bor isub ishl_imm imul_imm bitcast band_imm uload8 ireduce trap
imul symbol_value splat ushr_imm umax udiv fcmp istore8 bxor vhigh_bits`.
The rest: `ushr ineg call_indirect sextend ishl f64const f32const sshr ctz
bxor_imm bswap vconst urem umul_overflow uload32 uadd_overflow stack_addr
sshr_imm istore32 fadd bnot bitselect band_not umulhi umin trunc swizzle
stack_store srem sdiv popcnt insertlane func_addr fsub fcopysign fabs
extractlane bor_imm atomic_rmw usub_overflow stack_load ssub_overflow
smulhi smul_overflow smin smax shuffle sadd_overflow rotr rotl global_value
get_stack_pointer fpromote fneg fmul fmin fmax fdiv fdemote fcvt_to_uint_sat
fcvt_to_sint_sat fcvt_from_uint fcvt_from_sint clz atomic_store atomic_load`.

**`FunctionBuilder`.** `switch_to_block` 201, `create_block` 199,
`block_params` 61, `append_block_param` 58, `use_var` 29, `declare_var` 26,
`inst_results` 18, `seal_block` 14, `def_var` 10,
`create_sized_stack_slot` 5, `finalize` 4, `set_srcloc` 3,
`seal_all_blocks` 3, `import_signature` 3, `current_block` 2,
`set_cold_block` 1, `append_block_params_for_function_params` 1, and direct
`func.dfg` access (`value_type`, `constants.insert`, `immediates.push`).

**Types.** `I8 I16 I32 I64 F32 F64 I8X16 I16X8 I32X4 I64X2 F64X2` (I64 at
504 sites).

**Memory flags, traps, calling conventions.** `MemFlags::new` (215) and
`::trusted` (46), endianness for vector bitcasts; `TrapCode::unwrap_user`
(24) and the five reserved codes; `CallConv::SystemV` and the ISA default via
`make_signature` (40).

**Module.** `declare_function` (Local 50, Import 22, Export 12,
Preemptible 8), `declare_func_in_func` 32, `target_config` 47, `isa` 21,
`make_context` 17, `define_function` 15, `declare_data_in_func` 14,
`clear_context` 14, `define_data` 11, `declare_data`, `declare_func_in_data`,
`declare_data_in_data`, `declarations` 4 (symbol names), `finish` 7.
`DataDescription`: `new` 13, `define` 35, `set_align` 8, `define_zeroinit`,
`write_function_addr`, `write_data_addr`.

**Backends and setup.** `ObjectModule`/`ObjectBuilder` (8 sites),
`JITModule`/`JITBuilder::with_isa` (7), `finalize_definitions` and
`get_finalized_function` (JIT); `cranelift_native::builder` (11),
`isa::lookup` (2); flags `opt_level`, `is_pic`, `use_colocated_libcalls`,
`enable_nan_canonicalization`. Also `ctx.set_disasm`/`compiled_code`,
`ctx.func.display` (CLIF dumps), `ControlPlane` (4),
`UserFuncName::user` (5), `ExtFuncData` (2), Windows unwind info
(`isa::unwind::winx`).

## Stage 1 guard

This lane touches none of `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`, and the compiler itself is unchanged (the diff is new
files plus the workspace member line and one CI step). The bootstrap-stage1
workflow ran `bootstrap/run.sh stage1 --reproduce --outcome` on this branch:
[run 44459669923](https://github.com/robot-head/vesper/actions/runs/44459669923)
(result in the PR).

## Compiler gaps

Each was worked around in this lane; minimal repros.

1. **No `bool` in `.vsys` signatures.** `fn f(b: bool) -> ForeignVal[Unit];`
   is refused (`Scalar` has only integer kinds), so the shim takes `u32`
   flags and the wrappers convert. Gap for every C API with `bool`.
2. **No argument lists without a heap sequence.** A Cranelift argument list
   is `nocapture in args: CSlice[u32]`; with no fixed arrays or slice
   literals in Vesper yet (P3b gaps lane 6, #1115, open), `vals1`/`vals2`/
   `vals3` build a `ChunkSeq[u32]` per jump, call and return, one `malloc`
   each. `let a = [x.id, y.id]; ins_jump(inout b, blk, in a[..]);` is the
   form the lowering wants.
3. **No module constants** (also #1115): every opcode and type code is a
   function (`fn ty_i64() -> Ty { Ty { code: 4 } }`).
4. **A package cannot import a `.vsys` file.** The library lives in the
   `// @export vsys` directive comment of `src/lib.ves`, and the tests
   extract it from there (`tests/support/mod.rs::vsys`). A
   `sys library` file next to the package that `use` could name would make
   it one source for the JIT, the evaluator and `vesper build`.
5. **JIT entries take no arguments and cannot return a sequence**, so the
   object leaves through a Rust-side registry (`object_publish`) and each
   benchmark size is a generated entry. A Stage 2 driver is a `vesper build`
   executable linked with the staticlib, which needs lane 1's hybrid build
   (#1123).
6. **Indexing a sequence raises `Fail`.** `out[i]` in an `Ffi`-only function
   is `E0401`; `try_get` works. Fine, but the lowering will read many
   tables, so bounds facts (#1118) matter there.

## What is left

- **JIT modules.** The shim wraps `ObjectModule` only. `vesper run` and the
  JIT tests need `JITModule` (`finalize_definitions`, `get_finalized_function`,
  symbol registration); the handle can become a two-backend enum.
- **Builder extras the compiler uses:** `set_srcloc`, `set_cold_block`,
  `current_block`, `UserFuncName::user`, colocated `ExtFuncData`,
  `declarations()` (symbol names), `ctx.set_disasm`, `ControlPlane`,
  `cranelift_frontend::Switch`, `try_call`, dynamic stack slots and Windows
  unwind info.
- **A C-linked driver test.** C callers have the header (syntax-checked in
  CI) and the staticlib; no test links a C program against the staticlib yet,
  because Bazel builds only the rlib.
- **More families.** The next comparison candidates are the sequence
  runtime calls of `src/seq/native*.rs` and the export ABI wrappers of
  `src/native_export.rs`, which need the JIT module and data relocations.
