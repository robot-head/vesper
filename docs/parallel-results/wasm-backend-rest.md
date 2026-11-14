# Compiler Wasm target, rest (§30, §7): wave-eight lane 14 report

Claim: #463. Builds on lane 25 (#348, PR #377, report
`docs/parallel-results/wasm-backend.md`). Edited paths: `src/wasm_backend.rs`,
`src/wasm_backend/**` (new `host.rs`), `tests/wasm_backend.rs`,
`tests/wasm_backend_cli.rs`, `tests/fixtures/wasm_backend/**` (new
`unhandled.ves`), this report, and one root patch in its own
commit: the dev-dependency `wasmtime-wasi = { version = "50",
default-features = false, features = ["p2"] }` (the lockfile gains one edge; it
adds no new crate versions beyond what Wasmtime 50 already pulls in).

## What was built

| Item | Delivered | Evidence (`tests/wasm_backend.rs`) |
| --- | --- | --- |
| Strings | `String` literals in a read-only data segment (pointer to `len` + UTF-8 bytes), `==`/`!=` byte comparison, string literal patterns, strings inside tuples, structs and enums, structural equality through them; exported as WIT `string` (lowered into the canonical ABI layout, lifted from it for parameters through an exported `cabi_realloc`) | `strings` (9 programs), `aggregate_entry_parameters` |
| Exact `f64 %` | IEEE `fmod` in integer arithmetic on the bit patterns (a port of musl's algorithm): exact for subnormals, huge ratios, signed zeros, NaN and infinities; `%=` too | `float_remainder_is_exact`: `remainder.ves` fixture plus a 16×16 grid of awkward operands (256 pairs) and 5 NaN/∞/0 cases, each differential |
| Monomorphization | A function is compiled once per distinct tuple of concrete type arguments (`base<T=…>`), taken from the checker's call-site instantiations or explicit type arguments; type-directed operations (`==` on a type parameter, generic `Fail[E]`/`Get[T]` instances) resolve per instance; closures and handlers inside an instance see its substitution. A function whose arguments are not all concrete is still compiled once over the uniform representation | `generic_functions_are_monomorphized` (6 programs) |
| `inout` operation arguments | The argument place is passed as a cell; a tail-resumptive clause writes it, and the caller copies it back when the operation returns (field places included) | `labelled_rows_and_inout_operation_arguments` (3 inout programs); an aborting clause is rejected with `InoutEffectArgument` |
| Labelled effect rows | `eff [l: E]` on a function binds `l` to whatever handler the caller's `l` (or unlabelled `E`) selects; labels pass through nested labelled callees and closures | `labelled_rows_and_inout_operation_arguments` (3 labelled programs) |
| CPS handlers (Portable) | Any one-shot clause that is neither aborting nor tail-resumptive: under `EffectLowering::Portable` (or when WasmFX is unavailable) the clause is split after each `resume` call; the prefix runs at the operation and returns the resumed value to the performer, leaving a record of the clause's live locals on the handler frame's pending list; when the handled body returns (or aborts back to the handler) the pending suffixes run LIFO, each receiving the previous result. No stack switching is emitted | `cps_handlers_run_without_stack_switching` (10 programs incl. `examples/resumptions.ves`; each asserts a `HandlerMode::Cps` clause and no WasmFX under Portable), `general_one_shot_handlers_use_stack_switching` (Portable reports `Cps`) |
| WASI host capabilities | An entry whose closed row names `Stdout` or `Clock`, declared exactly as `linear effect Stdout { fn write(s: String); }` / `linear effect Clock { fn now_ns() -> u64; }`, imports `wasi:cli/stdout@0.2.0` + `wasi:io/streams@0.2.0` (`blocking-write-and-flush`, 4096-byte chunks; a stream error traps) or `wasi:clocks/monotonic-clock@0.2.0`. The export wrapper installs host frames, so the capabilities can be performed anywhere below the entry and shadowed by source handlers. Any other unhandled effect, or a mismatched declaration, is a typed `WasmError::Entry`; a Nexus plugin cannot import them | `wasi_stdout_and_clock_behind_declared_capabilities` (run under `wasmtime-wasi` with a captured stdout pipe and a deterministic tick clock, compared with the evaluator hosting the same effects), `undeclared_or_mismatched_capabilities_are_rejected`, `tests/wasm_backend_cli.rs` |
| Aggregate entry parameters | Tuples, records, variants (`Option`, user enums) and strings as `main` parameters, lifted from the flat canonical ABI (at most 16 flat values) | `aggregate_entry_parameters` (records, nested tuples, enums, options, strings incl. non-ASCII and empty) |
| Reclaiming allocator | Region reset at tail calls: a function whose row holds only `Fail`, capability or host effects marks the heap on entry; before a self or sibling tail call to such a function it evacuates the arguments (deep copy of aggregates and strings to just above the mark), moves them down to the mark and resets the heap pointer, so a loop's garbage is reclaimed every iteration. The mark is handed to the callee through a global | `tail_loops_reclaim_their_garbage`: three loops (tuple accumulator; nested structs, enums and strings with extra garbage; mutual recursion under a `Fail` handler) run 2,000,000 iterations in 4 pages (256 KiB) in both lowerings; 1,000 iterations are differential |
| Typed rejections | A continuation captured by a closure or nested handler is `FirstClassContinuation` (previously an `Invalid` internal error) | `unsupported_constructs_are_typed_errors` |

Every differential test compiles with both effect lowerings
(`StackSwitching`, `Portable`) × with and without the IR optimizer, runs the
component under Wasmtime 50.0.1 and compares it with the evaluator; it also
asserts that the Portable artifact never uses WasmFX. WasmFX artifacts run only
on x86_64 Linux/macOS hosts (as in lane 25); elsewhere they are compiled and
validated but not run.

## Decisions

* **CPS by splitting the clause, not the body.** Only the clause is
  transformed: its IR is cut after each resumption call into a prefix and
  suffix functions (`$resume_i`) that reload the saved locals from a record.
  The handled body and everything it calls stay direct style, so no other
  function changes and tail calls keep working. This handles any clause that
  resumes at most once per path (the checker's one-shot rule); a clause that
  resumes from inside a closure or nested handler stays rejected.
* **Abort after resumption** drops the pending suffixes of the handlers it
  unwinds past, matching the evaluator (the test with an overflow after a
  resumption checks it).
* **Monomorphize only when fully concrete.** Partial instantiation would need
  type-level evaluation of row and function types; those keep the uniform
  representation, which is exact for everything except type-directed
  operations, and those still produce `GenericOperation`.
* **Capabilities are opt-in by signature.** Matching the declaration exactly
  keeps an unrelated effect that happens to be called `Stdout` from silently
  becoming a WASI import.
* **Reclamation is conservative.** It applies only where no evidence frame,
  closure or continuation can hold a pointer into the region being reset: the
  callee's row (and so everything it performs) has no user effects, and every
  argument type is evacuable (no function values). Shared substructure is
  copied, not shared, after an evacuation.

## Checks

Run on x86_64 Linux at the branch head (main merged in, with main's open fix
for the `Op::Vector` compile error, PR #490, applied locally and not
committed):

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo test --locked --test wasm_backend --test wasm_backend_cli` | 28 + 3 passed |
| `cargo test --workspace --locked --no-fail-fast` | @TESTS@ |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | clean |

## Remaining gaps

* Generic functions whose type arguments include function or row types are
  not specialized (they run over the uniform representation).
* `inout` operation arguments only in clauses that resume in tail position.
* A continuation used from a closure or nested handler inside its clause, and
  multi-shot use, are rejected (`FirstClassContinuation`).
* Entry parameters limited to 16 flat canonical-ABI values; no lists.
* Only `Stdout` and `Clock` host capabilities; no `wasi:cli/stdin`,
  filesystem or wasip3 async.
* Reclamation only at tail calls described above; non-tail loops and
  handler-carrying loops still allocate monotonically.
* These are differential tests over a corpus, not a proof that the lowering
  preserves the source semantics.
