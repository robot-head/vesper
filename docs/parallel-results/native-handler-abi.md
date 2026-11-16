# Native handler ABI (wave nine lane 9, #586)

Baseline: `5a018bd` (origin/main, descendant of the required `37f7b8b`).

## What was built

A second native lowering, `vesper::native_handlers`, that compiles user
effects with resuming handlers and function values **once each** and passes
them at run time, instead of specializing them into every caller as
`vesper::native` does. Contract name: `vesper.native.handler-abi/1`. It is
additive: it uses native/fiber ABI v2 (`vesper-fiber-abi`) unchanged and adds
no IR operations.

- `src/native_handlers.rs`: public API (`jit`, `code_size`, `Run`,
  `CodeSize`, `HandlerAbiError {Unsupported, Invalid, Runtime}`) and the ABI
  description.
- `src/native_handlers/lower.rs`: source AST to Cranelift, one function per
  source function, closure body, handled body, return clause and operation
  clause.
- `src/native_handlers/runtime.rs`: the run-time side (fibers, handler
  records, continuations, arena) behind five `extern "C"` entry points.
- `tests/native_handlers.rs` and `tests/fixtures/native_handlers/*.ves`.

### The ABI (`vesper.native.handler-abi/1`)

- **Words.** Every value is one 64-bit word (`i64`, `u64`, `u32`, `u8`,
  `bool`, unit, function values).
- **Calls.** A source function `f(params) eff [E1..En]` is
  `extern "C" fn(rt, cap_1..cap_n, params..) -> word`. `rt` is the run-time
  context. There is one capability word per operation-carrying effect in the
  row, in canonical order (by effect name, then arguments). Capability-only
  effects (`Realtime`, `ConstTime`, ...) are erased, as in the wave-seven
  capability ABI. Row-polymorphic rows are rejected.
- **Capabilities.** A capability is the address of the handler record the
  enclosing `handle` installed. Records live until the run ends, so a stale
  capability faults instead of dangling.
- **Function values.** A function value is a pointer to a closure record,
  `[code, captures..]`. A call passes `(rt, record, caps.., args..)`. A source
  function used as a value gets a forwarding adapter record.
- **`handle`.** Generated code writes a descriptor (offsets 0 body, 8 env,
  16 return clause, 24 stateful, 32 initial state, 40 clause count, 48..
  clauses) and calls `vesper_handlers_handle(rt, desc)`. The body runs on a
  fresh fiber with a 16 MiB lazily reserved stack.
- **`perform`.** `vesper_handlers_perform(rt, cap, op, args, n)` parks the
  innermost running fiber with the request (up to 4 arguments).
- **Driving.** The `handle` that owns the fiber looks at each request:
  - if it is the owner's capability, the clause runs on the handler's stack
    with a one-shot continuation;
  - otherwise the request is forwarded outward (performed on the outer
    fiber) and the answer resumes the inner fiber.
- **`resume`.** `vesper_handlers_resume(rt, k, value, state)` returns the
  value of the rest of the `handle` (deep handlers). A second resume faults.
  A continuation that was never resumed is cancelled, which unwinds its fiber.
- **Unwinding.** `rt[0]` is the unwinding flag. Generated code checks it
  after every effectful call and returns at once while it is set. The fiber
  trampoline clears it when the cancelled fiber ends.
- **Faults.** `vesper_handlers_fault(rt, code)` records the fault: 1
  unhandled operation, 2 a `raise` resumed, 3 no `match` arm. The run
  reports it as `HandlerAbiError::Runtime`.

## Supported slice

The supported constructs are:

- scalar values and function values;
- checked arithmetic raising `Fail[ArithOverflow]`, plus `wrapping_*` builtins;
- `let`, `if`, `match` on literals, wildcards and bindings;
- assignment to locals;
- closures with captures, and closures returned from functions;
- direct and indirect calls;
- effect operations with up to 4 arguments;
- stateless and stateful (`state x = ..`) handlers, nested handlers, and
  forwarding;
- handlers that abort (never resume).

Everything else is rejected with `HandlerAbiError::Unsupported`, never
miscompiled. That covers aggregates (tuples, structs, enums), generic
functions, floats and strings, fields, indexing and `?`, constructor
patterns, `inout` parameters, row-polymorphic rows, more than 4 operation
arguments, and continuations captured by closures. There is no silent
fallback to the specializing backend.

## Checks

| Check | Result |
|---|---|
| `cargo test --test native_handlers` | 4 passed |
| `cargo +1.100.0 clippy --locked --all-targets -- -D warnings` (root package) | clean |
| `cargo fmt --all -- --check` | clean |
| `vesper fmt --check` on the new fixtures | canonical |

The full `cargo test --workspace --locked` was not run locally because of the
cloud disk allowance. CI runs it on Linux (Bazel), macOS AArch64 and Windows
x64.

### Differential tests (`fixtures_agree_with_the_evaluator`)

Each fixture is run by the evaluator and by the handler ABI, and the two
values must be equal. Each run must also have used at least one fiber.

| Fixture | Covers |
|---|---|
| `resume_then_compute` | resuming handler, computation after `resume` |
| `stateful_counter` | stateful handler |
| `nested_forwarding` | two handlers, inner body performs the outer effect (forwarded) |
| `abort_through_handlers` | `Fail` raised under an inner handler aborts through it |
| `unresumed` | clause returns without resuming; the fiber is cancelled |
| `shared_function` | one effectful function reached under two handlers |
| `deep_recursion` | 2000 nested frames each performing an operation |
| `returned_closures` | closures returned and composed at run time |
| `runtime_closures` | closure chosen at run time, passed to an effectful function |
| `widened_function_value` | a pure function passed where `fn .. eff [Ask]` is expected |
| `forwarded_abandon` | an outer handler abandons an operation forwarded through an inner one |
| `shared_locals` | a handled body assigns enclosing locals; clauses keep snapshots |
| `effect_instances` | `Tell[i64]` and `Tell[bool]` nested, operations with type arguments |

`effectful_functions_are_compiled_once` checks that `twice` in
`shared_function` is one compiled function, although it runs under two
handlers. `unsupported_programs_are_rejected_with_typed_errors` checks the
typed rejections.

### Code size against the specializing path

`code_size_against_the_specializing_path` prints this table. The corpus is
`examples/*.ves` plus the fixtures.

- **Handler ABI** is the sum of the compiled Cranelift function sizes. It
  excludes the Rust run-time (`runtime.rs`, shared by every program, about
  480 lines).
- **Specializing** is the `.text` size of `vesper::native::object`.

Both are x86-64 Linux, debug profile of the compiler; the code itself is
Cranelift `opt_level=none` in both.

| Program | Handler ABI bytes | Specializing bytes |
|---|---:|---:|
| examples/checked_math.ves | unsupported (constructor patterns) | 341 |
| examples/closures.ves | 884 | 566 |
| examples/counter.ves | 1099 | 464 |
| examples/fail.ves | unsupported (`?`) | 647 |
| examples/fibonacci.ves | 852 | 943 |
| examples/projections.ves | unsupported (aggregates) | 153 |
| examples/resumptions.ves | 998 | 360 |
| examples/transaction.ves | unsupported (aggregates) | 149 |
| abort_through_handlers | 1240 | unsupported (recursive closure specialization exceeds 64 inline frames) |
| deep_recursion | 1427 | unsupported (recursive closure specialization exceeds 64 inline frames) |
| effect_instances | 925 | 307 |
| forwarded_abandon | 796 | 70 |
| nested_forwarding | 2086 | 1272 |
| resume_then_compute | 998 | 360 |
| returned_closures | 884 | 566 |
| runtime_closures | 2152 | 4256 |
| shared_function | 1332 | 593 |
| shared_locals | 591 | 335 |
| stateful_counter | 1099 | 464 |
| unresumed | 1432 | unsupported (recursive effect specialization exceeds 64 inline frames) |
| widened_function_value | 785 | 248 |

There are three findings.

- **Small programs.** For small programs with one handler and one call site,
  specializing is 1.5–3.2× smaller, because the handler ABI pays for the
  descriptor, the fiber entry and the clause functions. (`forwarded_abandon`
  is the extreme, 796 against 70: specialization folds the abandoned
  operation away entirely.)
- **Several closures or call sites.** Where several closures reach several
  call sites, the handler ABI is smaller, because specialization duplicates
  each body per combination: `runtime_closures` is 2152 bytes against 4256.
  (`fibonacci` is also smaller, 852 against 943.)
- **Recursion through an effect or closure.** These programs do not compile
  on the specializing path at all (it bounds inlining at 64 frames), and
  compile and agree with the evaluator under the handler ABI.

## Decisions

- **Review fixes (Codex review of the first head).** Four findings, each
  now covered by a fixture above:
  - A function value used at a type with a wider effect row (the checker
    allows the weakening) is wrapped in a *widening adapter* record whose
    code receives the wider row and passes on only the capabilities the
    value takes.
  - Cancelling an inner fiber restores the enclosing `unwinding` state, so
    an outer handler that abandons a forwarded operation keeps unwinding
    past the inner `handle`.
  - A handled body shares the enclosing locals, as the evaluator's handled
    bodies share their slots. Assignments write through to a `cells`
    record (the last word of the environment), and the enclosing function
    reads them back when `handle` returns. Clauses still get the values as
    of installation.
  - An operation written with type arguments (`Tell::tell[i64](..)`) goes
    to the innermost handler of that instance, otherwise to the innermost
    handler of the effect, as the evaluator does.
  - Rows are also deduplicated: an inferred row can list one instance
    several times under the checker's internal labels, and that is one
    capability.

- **Deep handlers by nesting.** A clause's `resume` drives the rest of the
  `handle` on the handler's stack, so N resumed operations nest N `drive`
  frames. Hence the 16 MiB fiber stack (reserved with `MAP_NORESERVE` by
  `vesper-fiber-abi`, so unused pages cost nothing). Tail-resumptive clauses
  could avoid the nesting; that is not done.
- **Canonical row order.** The checker infers a closure's row where the
  closure is built, and the type written at its call site can list the same
  effects in another order. Ordering capabilities by effect instance makes
  the two agree. The checker's internal labels (`$latent:…`, `$fncap:…`,
  `$cap…`) are not source labels. A `$latent` label keeps only the label the
  program wrote.
- **Separate module, no `src/native.rs` hunk.** The specializing backend is
  untouched. The optional hunk that would make `vesper::native` fall back to
  this ABI on its "exceeds 64 inline frames" errors is left for after #576
  (Codex holds `src/native.rs`).
- **`unsafe`.** As in `src/native.rs`, this module calls JIT code and
  dereferences pointers that generated code passes back. The five entry
  points are `unsafe extern "C"` with a documented contract. They are
  reached only from code this lowering generated.
- **Source typing.** Linearity and one-shot use are enforced by the checker
  on the source. The run-time additionally faults on a second `resume`.
  Rust ownership in the run-time is not evidence that the source typing
  rules are implemented.

## Remaining gaps

- Aggregates (tuples, structs, enums), generics, floats and strings, `?`,
  constructor patterns, `inout`, and row-polymorphic rows.
- Continuations stored in closures or escaping their clause (one-shot,
  second-class use only).
- JIT only: no object or executable emission under this ABI yet, and no
  linking with the specializing path's objects.
- No tail-resumption optimization; stack use grows with the number of
  resumed operations in one `handle`.
- Only x86-64 Linux was run locally. macOS AArch64 and Windows x64 are
  exercised by CI through `vesper-fiber-abi`'s ports.
