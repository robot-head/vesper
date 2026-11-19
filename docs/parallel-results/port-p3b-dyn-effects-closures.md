# Effects and function values through trait objects (F7c)

Claim: #1063 (port wave P3b lane 1). Baseline: `main` at `8212ddf`.
Follow-up to second-class `dyn` (#1036, PR #1051) and boxed objects
(#1057, PR #1060).

Part 1 lets trait methods that perform handler and `Ffi[Lib]` effects be
called through every object form. Part 2 adds function objects (`in dyn Fn`,
`inout dyn FnMut`, `Box[dyn FnMut]`, `Box[dyn FnOnce]`).

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
  `sink self` method on a boxed receiver moves the value to the stack and
  frees the block before the call, so a handler that never resumes leaks
  nothing. The switch covers only vtables whose slot functions the program
  compiles (a vtable built only in unreachable code is skipped). Both points
  were review findings on #1080, fixed in #1084.
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

## Stage 1 guard

No file under `src/verified_codegen/**`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`.

## Part 2: function objects

### What changed

- **Syntax** (`crates/vesper-syntax`): `dyn Fn(A, ..) -> R eff [E]`,
  `dyn FnMut(..)` and `dyn FnOnce(..)` parse wherever `dyn Trait` does,
  including `Box[..]` and `Box::new[dyn FnMut(..)](..)`. The function-type
  tail is shared with `fn(..)` types.
- **Lowering** (NEW `src/dyn_fn.rs`, run first in
  `pact_frontend::check_package_with`): each distinct function-object type
  becomes a hidden object-safe trait `__FnObject{Kind}_{hash}` with one
  method `call` (`in self` for `Fn`, `inout self` for `FnMut`, `sink self`
  for `FnOnce`). A closure literal where a function object is expected
  becomes a hidden struct of its captures plus a conformance whose `call`
  body is the closure body reading captures from `self` (a `FnOnce` body
  destructures `self` instead, so it consumes the object whole). Calls
  `f(x)` on function-object names, fields and sequence elements become
  `f.call(x)`. An `inout` lend of a closure literal binds the object to a
  local of the call's block. Everything after that is the existing object
  machinery, so the native backend, the JIT and the evaluator needed no new
  code, and effectful closures go through Part 1's dispatch. Diagnostics in
  generated code are reported at the closure or call they came from.
- **Box drop elaboration** (`src/fip.rs`): a bounds-hoisted `s[i]`
  (`SeqOp::Hoisted(Get)`) of a boxed element is treated like a checked one,
  both by the loop rule and by Part 1's lend fix, so a loop over a field
  holding `ChunkSeq[Box[dyn ..]]` compiles after hoisting.

### Coercion sites (Stage 1)

The lowering runs before type checking, so it finds the expected type
syntactically: a call argument whose declared parameter is a function object
(functions, `Type::method` paths, and method calls by receiver type, or by
name when every candidate agrees), `Box::new(closure)` expecting a boxed one
(`let` annotation, call parameter, struct field, function result, `return`,
`if`/`match` branches, `s.push(..)` on a sequence of them), and an explicit
`Box::new[dyn FnMut(..)](closure)`. Capture types come from the binding's
annotation or an evident initializer (literal, struct literal, call of a
declared non-generic function, `Box::new[T]`, `X::new[T]`); otherwise E0102
asks for an annotation. Closures whose captures or signature mention the
enclosing function's type parameters are refused (E0633). Captures move
into the object (`[T-Abs]`), so a `FnMut` object's state persists across
its own calls, not in the caller's locals.

### Manuscript change

§7.3 gains the item "Function Objects (`dyn Fn`, `dyn FnMut`, `dyn
FnOnce`)": the type forms, that they are trait objects with one `call`
method, the coercion sites, the no-allocation lend and one-block box, `FnMut`
state, `FnOnce` consumption, linear captures (E0310, E0205), call syntax and
the Stage 1 annotation and generics limits.

### Tests

`tests/dyn_fn.rs` (same harness as Part 1):

- closures lent as `in dyn Fn` (nested too) and `inout dyn FnMut` (state
  persists across calls within one lend)
- `ChunkSeq[Box[dyn FnMut]]` callbacks in a struct field, called in a loop,
  from helper functions returning boxed closures and from an explicit
  `Box::new[dyn FnMut(..)]`
- `Box[dyn FnOnce]` called, passed on `sink` and called, and dropped
  uncalled, each capturing a sequence (free counts checked)
- `in dyn Fn`/`inout dyn FnMut` lends allocate nothing (allocation counter)
- effectful closures: `inout dyn FnMut(..) eff [Log]` and a boxed `FnMut`
  performing `Log` under a stateful handler
- rejections: mutating a `Fn` capture (E0307, reported in the source), an
  unannotated capture (E0102), a second-class escape (E0631), a missing
  effect in the caller (E0401), arity (E0207), linear captures (E0310,
  E0205), generic captures (E0633)

### Benchmark

`cargo test --test dyn_fn -- --ignored --nocapture bench`, 50 million calls,
optimized JIT:

| call | ns per call |
|---|---|
| `in Box[dyn Fn(u64) -> u64]` | 4.8 |
| `in dyn Fn(u64) -> u64` (closure literal lent per call) | 5.3 |
| `fn(u64) -> u64` value (specialized at the call) | 3.2 |
| direct call | 3.5 |

### What this unblocks

- Chorus: `Box<dyn Transport>` over effectful transports (Part 1) and
  callback boxes (Part 2).
- Flux: sources and sinks performing storage effects through `in dyn` /
  `inout dyn` (Part 1).
- Kinetic: the `dyn FnMut` contact pass as `inout dyn FnMut(..)` (Part 2;
  its state lives in the object, so a pass that accumulates into the
  caller's locals should return or hold the accumulator).
- P3a workarounds: Pact, Silicon and Aegis enum dispatch can move to
  closures where the closures capture concrete types.

### Remaining gaps

- Run-time evidence for compiled slots (open-world objects, separately
  compiled code): needs the handler-ABI backend's fibers and records.
- Closures capturing generic values, and coercion sites only the checker
  could find (for example a closure bound by `let` and passed later); a
  type-directed lowering would need the coercion inside the checker.
- Borrowing captures (`&mut` capture of a caller local) for lent `FnMut`.
- `inout` box parameters in functions with loops (lane 2, #1064).
- Associated types on objects, several models per (object, type) pair and
  `Rc`/`Arc` of objects: these need, respectively, projection types in the
  object type, the conformance in the vtable identity, and a shared block
  with a count word before the vtable.
