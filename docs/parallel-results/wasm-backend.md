# Compiler Wasm target (§30, §7): wave-seven lane 25 report

Claim: #348. Baseline: `283c811` (origin/main at start, a descendant of
`c025b9a`). New backend `src/wasm_backend.rs` and `src/wasm_backend/**`, tests
`tests/wasm_backend.rs`, `tests/wasm_backend_cli.rs` and fixtures under
`tests/fixtures/wasm_backend/`, one routing hunk in `src/main.rs`, and a root
patch (dependencies, lockfile, `pub mod wasm_backend;`).

## What was built

`vesper::wasm_backend::compile(world, entry, &WasmOptions)` lowers the checked
Place IR of every function reachable from `entry` to one core Wasm module with
`wasm-encoder` 0.261 and wraps it as a component with `wit-component` 0.261 (the
versions Wasmtime 50.0.1 already pulls in). `vesper build --target
wasm32-wasip2 file.ves -o out.wasm` writes that component.

| Item | Delivered | Evidence |
| --- | --- | --- |
| Scalars | `i64`, `u64`, `u32`, `u8`, `bool`, `f64`, `Unit` | `scalars` |
| Checked arithmetic (`vesper.arith.checked/1`) | `+ - *`, compound assignment and negation raise `Fail[ArithOverflow]`; `/ %` by zero raise `Fail[RuntimeError]` before the overflow check; `i64::MIN / -1` overflows, `i64::MIN % -1` is 0; `wrapping_*` mask narrow kinds | `checked_arithmetic_matches_the_evaluator`: 31 boundary cases, each differential |
| Aggregates | tuples, structs, enums (incl. `Option`/`Result`), nested patterns, literal patterns, copy-on-write field writes, structural `==`/`!=`, dynamic tuple indexing with `IndexOutOfBounds` | `aggregates`, `structural_equality` |
| Calls and loops | direct and mutually recursive calls; a direct call in tail position is a `return_call` (10,000,000 iterations in constant stack); `inout` arguments including field places | `calls_and_mutation`, `recursion_and_loops`, `tail_calls_do_not_grow_the_stack` |
| Effects, portable | `Fail` instance selection (exact instance, else by name), `?` on `Result`, clauses that drop the continuation (abort), tail-resumptive clauses with and without handler state, labelled handlers used lexically, clauses that perform outer effects | `failure_selects_its_own_instance`, `tail_resumptive_and_aborting_handlers_are_portable` (15 programs, each also run with `EffectLowering::Portable`) |
| Effects, WasmFX | any other one-shot clause: the body runs as a continuation (`cont.new`), operations `suspend`, the handler is a `resume` loop; nested handlers forward foreign operations; `Fail` inside a resumed body | `general_one_shot_handlers_use_stack_switching` (5 programs incl. `examples/resumptions.ves`) |
| Component | `vesper:program/entry@0.1.0` exports `main` typed by the entry (records, variants per instantiation such as `option-u8`, tuples); `wasi:cli/run@0.2.0` for parameterless entries; no imports | `command_component_exports_wasi_cli_run` |
| Nexus | `WasmWorld::NexusPlugin` emits the `plugin` world's root `run(u32, u64) -> s64`; the component runs on `vesper_nexus::NexusEngine` under fuel, including an overflow abort and out-of-fuel | `runs_as_a_nexus_plugin`, `nexus_plugin_runs_out_of_fuel` |
| Typed rejections | `WasmError::Unsupported { construct, function, detail }` | `unsupported_constructs_are_typed_errors`, `entry_and_limit_errors` |
| CLI | `vesper build --target wasm32-wasip2`; `--verify`/`--tee` rejected for it | `tests/wasm_backend_cli.rs` |

Every differential test compiles with and without the IR optimizer, runs the
component under Wasmtime 50.0.1 (Cranelift, fuel metering) and compares the
result, rendered the way the evaluator prints values, with the evaluator's.

## Lowering

* **Values.** Every Vesper value is one `i64`: integers zero-extended or as
  bits, `bool` 0/1, `f64` as IEEE bits, aggregates as pointers to blocks of
  8-byte slots (enums store the variant index first). Generic functions are
  compiled once over this representation.
* **Memory.** An 8-byte-aligned bump allocator grows linear memory on demand
  up to `max_memory_pages` (default 1024 = 64 MiB) and traps beyond it
  (`memory_exhaustion_traps`). Nothing is freed.
* **Control flow.** The acyclic block graph is emitted in topological order in
  nested `block`s; each forward edge is a `br` to the end of the block that
  precedes its target. Block parameters are locals.
* **`inout`.** Callees receive a cell; callers pass a fresh cell holding the
  current value of the place and copy it back after the call on every exit,
  aborts included. An `inout` parameter is passed through unchanged. This is
  copy-in/copy-out, equivalent to reference passing because the checker's
  exclusive loans forbid aliases.
* **Aborts.** A global holds the target handler frame; every call site and
  operation checks it and returns. A handler site whose frame is the target
  runs the clause there, in the handler's own evidence environment.
* **Handlers.** A handler is a frame in linear memory. Each evidence key
  (`$effect:E`, `$effect:E[args]`, `$label:l`) is a global holding the
  innermost frame; installing a handler snapshots all of them into its frame,
  and the handler body sees writes to its captured places (as the evaluator
  does) through cells. An operation calls the frame's dispatch function
  through the function table. Tail-resumptive clauses run at the operation
  with the evidence environment swapped to the snapshot, so effects the clause
  performs (including `Fail` from its own arithmetic) reach the handlers
  outside it, never ones installed inside the body.
* **WasmFX.** One tag `(frame, op, args...) -> i64`, one continuation type
  `(i64) -> i64`. The runner resumes the body; a suspension for another frame
  is forwarded with `suspend` and the result resumed back, so nested
  stack-switching handlers compose. `resume` in a clause calls the runner
  again (deep handlers). Continuations are one-shot: Wasmtime traps on reuse.
* **Unknown IR operations** (and `Op::Closure`) reach a wildcard arm that
  returns `Unsupported::Operation`/`Closure`, so lane 28's vector operations
  cannot miscompile this backend.

## Decisions

* `wit-component` is used for the component encoding (with an embedded WIT
  world) rather than hand-assembling component sections; it validates the core
  module with all features, including stack switching.
* WasmFX is used only for clauses that need a captured continuation. Aborting
  and tail-resumptive clauses, which cover `Fail` and every handler in the
  native test corpus except `examples/resumptions.ves`, use the portable
  lowering, so those programs run on every Wasmtime platform.
  `EffectLowering::Portable` rejects the rest with
  `Unsupported::NonTailResumptiveHandler` instead of emitting WasmFX.
* Results cross the boundary in the canonical ABI (records, variants, tuples
  with a return area in exported memory); parameters are scalars.

## Platforms

WasmFX is **exercised** only where `vesper_nexus::wasmfx::probe()` reports it
(x86_64 Linux and macOS with Wasmtime 50); on macOS AArch64 and Windows the
stack-switching programs are compiled and checked to be components but not run,
and the test prints that it skipped them. Everything else runs on all four CI
platforms.

## Checks

```sh
cargo test --test wasm_backend --test wasm_backend_cli   # 21 tests
cargo +1.100.0 clippy -p vesper --locked --all-targets -- -D warnings
cargo fmt --all -- --check
vesper fmt --check tests/fixtures/wasm_backend/
```

## Remaining gaps

* **Closures and function values** (`Op::Closure`, `Op::Indirect` on a value,
  a function name used as a value) are rejected (`Unsupported::Closure`,
  `FunctionValue`).
* **Type-directed operations on generic parameters** (for example `a == b` on
  `T`) are rejected (`GenericOperation`); there is no monomorphization.
  Handlers and operations whose effect arguments mention a generic parameter
  are rejected (`GenericEffectInstance`).
* **Strings**, **`f64` `%`** (fmod has no Wasm instruction and no exact
  lowering here), **`inout` effect operation arguments**, and **labelled effect
  rows of ordinary functions** (`fn f() eff [l: E]`) are rejected.
* **No CPS lowering** for non-tail-resumptive handlers on hosts without
  WasmFX: they are rejected under `EffectLowering::Portable`, and the default
  output needs a stack-switching Wasmtime to run.
* **No reclamation**: the bump heap never frees; long-running programs that
  allocate are bounded by `max_memory_pages`.
* **Entry parameters** are scalars only; results may be aggregates. `Unit`
  inside exported aggregates and empty structs have no WIT image and are
  rejected at the export.
* **No `wasip3`**, no WASI imports (the command prints nothing; `wasi:cli/run`
  reports success), and no Iris-WasmFX validation.
* The Vesper source typing rules are enforced by the checker before lowering;
  nothing in this backend re-proves them, and Wasm validation is not evidence
  for them.
