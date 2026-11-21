# Self-hosting SH5 lane 2: the Cranelift shim calls SH4 lacked

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1325. Lane prompt:
[`docs/agent-prompts/self-host-sh5/02-shim-extras.md`](../agent-prompts/self-host-sh5/02-shim-extras.md).
Stacks on #1309 (SH4 lane 1), head `6e22c6c`; `origin/main` was `bbda89e`
when the branch was cut. Owner decision (2026-11-19): Cranelift stays a
vetted external library behind the C shim, so every call below wraps a
Cranelift API and reimplements nothing.

## Summary

Fourteen new `extern "C"` calls in `crates/vesper-cranelift-sys` (new file
`src/extras.rs`), two new `vcl_unary` opcodes, and their `.vsys` imports
and typed wrappers in `@vesper/cranelift`:

| Need (SH4 lane) | Shim call | `@vesper/cranelift` |
|---|---|---|
| `sext`/`uext` on parameters and results (13, #1296) | `vcl_sig_param_ext`, `vcl_sig_return_ext` | `sig_param_ext`, `sig_return_ext`, `ext_none/uext/sext` |
| Define a function from bytes with relocations (13: `isolated(mpk)` helpers) | `vcl_define_function_bytes` | `define_function_bytes`, `reloc_target_*` |
| List what a module declared (11, #1315; 13) | `vcl_module_function_at`, `vcl_module_function_linkage`, `vcl_module_function_signature`, `vcl_module_data_count`, `vcl_module_data_at`, `vcl_module_data_name`, `vcl_module_data_linkage` | `module_function_at`, ..., `module_data_linkage` |
| `fvpromote_low`/`fvdemote` (11) | `vcl_unary` opcodes 20 and 21 (also in `vcl_emit` batches) | `ins_fvpromote_low`, `ins_fvdemote` |
| Read back a compiled function (14, #1305; 17, #1319) | `vcl_module_last_clif`, `vcl_module_last_relocs`, `vcl_module_last_call_sites`, `vcl_module_last_frame_size` | `module_last_clif`, `module_last_relocs`, `module_last_call_sites`, `module_last_frame_size` |
| Entity numbers as bare `u64`s (1, #1309) | (none: Vesper only) | `call_arg_{value,block,var,slot,func_ref,global_value,func,data,ty}`, `call_result_{...}`, `*_of_word`, `*_word`, `is_invalid_*` |

Lane 1's calls keep their signatures. Lookup by name was already there
(`vcl_module_get_function`/`get_data`, lane 1), as were code, unwind and
trap read-back of `vcl_compile` (`vcl_module_last_code`,
`vcl_module_last_traps`). Two of those now also cover `vcl_define_function`:
`vcl_module_last_code` returns the last compiled **or defined** function's
code (it was compile-only), so its relocations have code to apply to.

Rust stays the default build everywhere. No Rust code was deleted, and no
compiler path changed.

## Design

- **Relocations are five words**: code offset, kind (`codes::RELOC`, the 30
  `binemit::Reloc` kinds in Cranelift's order from 1), target kind
  (`codes::RELOC_TARGET`: function, data, libcall, known symbol, offset in
  this function, other user name), target and addend. Functions and data
  are the module's handle indices (Cranelift ids in a lent module), libcalls
  index `LibCall::all_libcalls()`. The words `vcl_module_last_relocs` gives
  are exactly the words `vcl_define_function_bytes` takes, so a lowering
  can compile with `vcl_compile`, inspect the code, and place it. Target
  kind 5 (a user name outside the function and data namespaces) is
  read-back only; defining one is refused.
- **Read-back is per module, of the last function.** Compiling or defining
  records the relocations, call sites (return offset and frame offset words,
  `u64::MAX` when the backend gives no frame offset), frame size
  (`CompiledCode::frame_size`) and, while `vcl_module_keep_clif` is on, the
  function's CLIF text. The CLIF is the text before compilation, as
  `Function::display` prints it, which is what the float check and Silicon's
  analysis read today.
- **Declaration listing** walks `Module::declarations()` in Cranelift's id
  order and answers with handle indices. Data linkage packs the writable
  and thread-local flags into bits 8 and 9. `vcl_module_function_signature`
  copies a declared signature into the module's signature table, so a
  lowering can build or call a function it only knows by name.
- **Typed handles** are the existing copy structs (`Value`, `Block`, `Var`,
  `Slot`, `FuncRef`, `SigRef`, `GlobalValue`, `FuncId`, `DataId`, `SigId`,
  `Ty`) with a word conversion each way, and `call_arg_*`/`call_result_*`
  that read and answer a lent call in those types. The `u64` forms stay.

## Tests

`crates/vesper-cranelift-sys/tests/sh5_extras.rs` (9 tests), run on all
four CI platforms by the crate's test step. Every byte check builds for
fixed triples (`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`),
so each host checks the same objects:

| Test | Check |
|---|---|
| `sext_and_uext_attributes_match_the_rust_builder` | a call through an `i8 sext, i16 uext -> i32 sext` import: object bytes equal the Rust builder's on both targets; an unknown extension is refused |
| `vector_float_conversions_match_the_rust_builder` | `fvpromote_low` and `fvdemote` functions: objects equal; the batch form gives the right result types |
| `declarations_are_listed_with_their_names_linkage_and_signatures` | order, names, linkage, data flags, signature copy, out-of-range answers |
| `a_lent_module_lists_the_hosts_declarations_by_cranelift_id` | the same through `lend_module` |
| `read_back_equals_what_cranelift_reports_to_a_rust_host` | a probe with a stack slot, a call, a data address and a trap: code, relocations, call sites, traps, frame size and CLIF equal what `Context::compile` gives a Rust host |
| `functions_defined_from_bytes_match_the_rust_module` | compile, read back, define from the bytes: the object equals Rust's `define_function_bytes` of the same function |
| `malformed_relocations_are_refused` | short tables, unknown kinds, unknown functions, read-back-only targets |
| `every_relocation_kind_round_trips_its_code` | `codes::RELOC` is a bijection |
| `vesper_reaches_every_new_call` | `tests/fixtures/sh5_extras.ves`, run under the JIT: every new wrapper, typed word round trips, no handle leaks, and the object it publishes is byte-identical to the Rust test's |

Lane 1's harness is unchanged and passes: `header` (exports, header and
`.vsys` name the same 125 functions), `checked_family`, `lend`, `pilot`, and
`crates/vesper-codegen-swap-test`. The hybrid compiler's generated binding
`crates/vesper-hybrid/port/codegen.rs` lists every symbol of the library,
so it gains the 14 new names; the committed file equals what
`vesper build --lib --rust-binding` generates for the current source.

## Stage 1 guard

The diff touches only `crates/vesper-cranelift-sys/**`,
`packages/vesper-cranelift/**`, the generated hybrid binding and this
report: nothing under `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`, and no verified pipeline goes through a swap
point. The shim is not in the Stage 1 compiler's dependency closure. The
bootstrap-stage1 workflow (`--reproduce --outcome` against
`bootstrap/stage1.lock`) runs on this branch once #1309 has merged and the
branch has merged `origin/main` (owner CI rule, 2026-11-21).

## Numbers

The read-back is recorded on every compile and define, so it is the one
cost an existing user pays. Measured with callgrind over 15,000
`vcl_define_function` calls of a small function (a stack slot, a call, a
trap; x86_64, release build), the same test against the shim before and
after this change:

| | Ir total | Ir per define |
|---|---|---|
| before (#1309 head) | 3,341,494,558 | 222,766 |
| after | 3,352,802,612 | 223,520 |
| difference | +11,308,054 | +754 (+0.34%) |

Wall time over the same loop is within noise (27.6-29.6 µs per define
before, 28.3-34.7 µs after, fastest of 5 runs each, shared machine). The
new calls cost nothing until they are made; `vcl_module_last_clif` text is
only kept while `vcl_module_keep_clif` is on, as lane 1's module CLIF is.

## SH4 lanes that can drop a workaround

- **Lane 13, FFI (#1296)**: `sig_param_ext`/`sig_return_ext` for narrow C
  integers; `define_function_bytes` for the `isolated(mpk)` gate helpers,
  which can move out of Rust; `module_function_at`/`module_function_linkage`
  for the declarations lookup.
- **Lane 11, numeric (#1294, #1315)**: the vector hook stops declining
  `widen_f64`/`nearest_f32` (`ins_fvpromote_low`, `ins_fvdemote`). Its
  `get_name` need is lane 1's `vcl_module_get_function`/`get_data` (#1309);
  the listing calls here cover walking all declarations.
- **Lane 14, freestanding (#1305)**: the float check and Silicon's stack and
  interrupt analysis can read `module_last_clif`, `module_last_frame_size`,
  `module_last_call_sites` and `module_last_relocs` instead of a Rust
  `ir::Function`.
- **Lane 17, checks (#1319)**: the verifiers read Cranelift's tables
  (traps, call sites, relocations, frame size) through the same calls.
- **Lane 1, swap point (#1309)**: hooks can read and answer typed entities
  (`call_arg_block`, `call_result_var`, ...) instead of bare words.

## Compiler gaps

Found writing `tests/fixtures/sh5_extras.ves` (all have workarounds in the
fixture):

1. **`match` as an operand of `&&`** is a parse error (E0002 "expected
   expression"):

   ```text
   let ok = a == 1 && match f() { Some(_) => true, None => false };
   ```

   Workaround: bind the `match` to a `let` first.
2. **A method call on a call result** is E0202 "unknown field" (already
   reported by SH4 lane 12, SH5 lane 4's list):

   ```text
   let ok = module_last_traps(in m).len() == 2;
   ```

   Workaround: `let traps = module_last_traps(in m); traps.len() == 2`.
3. **`Some(mut c) =>`** (a `mut` binding in a pattern) is a parse error.
   Workaround: `Some(c) => c` into a `let mut`.

## What is left

- A structured read-back of the function's instructions (opcode and types
  per instruction) would save the float check from parsing CLIF text; the
  text is enough for what lane 14 reads today.
- Exception-handler tables of call sites are not read back (no SH4 lane
  uses `try_call`).
- `vcl_module_last_code` now also reports defined functions; nothing
  relied on the compile-only behaviour, but a caller that compiles and then
  defines another function before reading the code reads the second one.
- #1305 carries a one-hunk change to `isa_for` in `src/lib.rs`; whichever
  merges second merges a trivial conflict-free hunk (different lines).
