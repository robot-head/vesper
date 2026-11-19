# Effects and function values through trait objects (F7c)

Claim: #1063 (port wave P3b lane 1). Baseline: `main` at `8212ddf`.
Follow-up to second-class `dyn` (#1036, PR #1051) and boxed objects
(#1057, PR #1060).

The lane lands in two parts. Part 1 (this report's first section) lets trait
methods that perform handler and `Ffi[Lib]` effects be called through every
object form. Part 2 adds function objects (`in dyn Fn`, `inout dyn FnMut`,
`Box[dyn FnMut]`, `Box[dyn FnOnce]`).

## Part 1: effectful slots

### What changed

- **Object safety** (`src/dyn_trait.rs`). A trait method may perform any
  effect a static call may: `Fail[E]`, capability effects, `Ffi[Lib]` and
  handler effects. Only an open row (a row variable) is still E0630. Calls
  through an object were already typed by the trait signature, so the
  method's row reaches the caller's row as for a static call (E0401 when the
  caller lacks it).
- **Native backend and JIT** (`src/dyn_trait/native.rs`, `src/native.rs`).
  The specializing backend compiles a handler effect by inlining the handler
  clause at the operation, so there is no run-time handler record to pass to
  a separately compiled slot. A slot whose method performs a handler effect
  therefore holds no function (the vtable word is 0), and a call through it
  (`Codegen::dyn_call_effectful`) compares the receiver's vtable address with
  every vtable the program has for that object type and, in the matching
  branch, calls the selected conformance method under the caller's handlers
  exactly as a static call would. The set of vtables is closed in a
  whole-program build; an unknown vtable traps. This is the same strategy the
  backend already uses for closures (a switch over the statically known
  targets of a function value). The receiver may be an `in`/`inout`/`sink`
  `dyn` parameter, a boxed local or field, or a boxed sequence element; a
  `sink self` method on a boxed receiver frees the block after the call.
  Slots whose methods perform only `Fail`, capability or `Ffi[Lib]` effects
  keep the compiled slot function and the indirect call.
- **Evaluator** (`src/runtime.rs`): no change needed. It dispatches through
  the vtable and performs the effect on its dynamic handler stack, and it
  agrees with the JIT on every new test.
- **Box drop elaboration fix** (`src/fip.rs`). In a function without loops,
  `s[i]` of a `Box[dyn Trait]` element was treated as an owned temporary and
  dropped after the call it was lent to, so the sequence freed the box a
  second time (a crash on `main` for any such call, effectful or not). The
  element is now only lent.

### Evidence layout

There is no run-time evidence vector in the specializing backend: evidence
is the lexical handler the inlined call resolves at compile time. The
`EvidenceVector` register mentioned in §6 describes the handler ABI
backend (`src/native_handlers.rs`), which does not support `dyn` yet. Passing
a real evidence vector to compiled slots would need that backend's fibers
and handler records for every resuming handler; it is recorded as a gap.

### Manuscript change

§7.3, "Second-Class Trait Objects": object safety now requires a closed row
instead of "only `Fail` and capability effects", says the object type
carries its methods' rows, and that handler effects are discharged by the
handlers in scope at the call, not where the object was built. A new
paragraph gives the native lowering of effectful slots (zero slot word,
dispatch on vtable identity over the closed vtable set, trap otherwise).

### Tests

`tests/dyn_effects.rs` (JIT with the optimizer on and off, which must agree,
free every sequence and box exactly once; the evaluator; AArch64 and x86-64
objects):

- a resuming stateful handler reached through `inout dyn` and `in dyn` with
  two implementations
- a `Box[dyn Transport]` stored in a struct field, built outside any handler
  and called later under two different handlers
- a `ChunkSeq[Box[dyn Transport]]` called element by element under a
  stateful handler and a `Fail` handler
- `sink self` effectful methods through a box and a `sink dyn` parameter,
  drop counts checked
- `Ffi[Bridge]` through `in dyn` and a box (checked with the Bridge frontend,
  emitted as an x86-64 object linking the library)
- rejections: a caller without the slot's effect (E0401, `dyn` and boxed),
  an open row (E0630)

`tests/dyn_trait.rs`: the "handler effect" object-safety rejection became an
"open effect row" one.

### Benchmark

`cargo test --test dyn_effects -- --ignored --nocapture bench`, 20 million
iterations of one effectful method call under a tail-resumptive handler
installed per iteration, optimized JIT (debug-built compiler, compile time
included):

| call | ns per iteration |
|---|---|
| effectful slot through `in dyn Step` | 9.6 |
| effectful slot through `in Box[dyn Step]` | 8.3 |
| monomorphized (`T: Step`) | 7.5 |
| direct call | 6.7 |

The dispatch adds one load and compare per vtable of the object type.

### Gaps found (not this lane's paths)

- A resuming handler around a loop that performs its operation hangs in the
  native backend (`handle Net in { for i in 0..3 { Net::emit(1) } }`), on
  `main` too and without `dyn`. The tests install handlers per call instead.
- A handler-specialized function with a `sink` parameter that owns a
  sequence leaks the sequence natively, on `main` too and called
  statically. The `sink self` test uses a value without a sequence.

### Stage 1 guard

No file under `src/verified_codegen/**`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`.
