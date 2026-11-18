# Port wave P3a, lane 1: second-class `dyn Trait` (F7b)

Claim: #1036. Baseline: `main` at `3c075a4`. Prompt:
[docs/agent-prompts/port-p3/01-dyn-trait.md](../agent-prompts/port-p3/01-dyn-trait.md)
(plan PR #1044).

## What changed

- **Syntax.** `dyn Trait[args]` parses as a type (`DynType` in the manuscript
  grammar), encoded as `Named("dyn", [Named(Trait, args)])`
  (`vesper_core::ast::DYN_TYPE`). `dyn` stays a plain identifier elsewhere.
- **Frontend** (`src/dyn_trait.rs`, hooks in `src/pact_frontend.rs` and
  `src/pact_frontend/mono.rs`). Each parameter `p: dyn Trait[args]` becomes a
  hidden type parameter `__dyn__p` bounded by `Trait[args]`, so its body is
  checked with the bounded-generic rules and its method calls are resolved by
  Pact. The bound is never monomorphized. Its calls name a bodiless *vtable
  stub* `vtable$Trait$method`, and each call that passes a concrete `T`
  selects `T: Trait[args]` at the call (under its `use model` scopes) and
  records one vtable per (object type, `T`). The function is compiled once.
  Bounded generic functions with `dyn` parameters are instantiated per static
  type argument only.
- **Native backend and JIT** (`src/dyn_trait/native.rs`, `dyn` hunks of
  `src/native.rs`, `src/layout.rs`, `src/fip.rs`, `src/seq/native.rs`). The
  hidden parameter is erased to the object type, whose layout is the 16-byte
  pair `{data, vtable}`. A coercion builds the pair on the caller's stack (the
  argument's place for `in`/`inout`, the moved value for `sink`). A vtable
  call loads the slot and calls it indirectly with the trait method's calling
  convention. An unconsumed `sink dyn` parameter is dropped once through the
  vtable's drop word; consuming it empties the parameter.
- **Evaluator** (`src/runtime.rs`): a `dyn` call passes its vtable as hidden
  evidence, and a stub call jumps to the receiver's slot function, so
  `vesper run` agrees with the JIT.
- **IR inliner** (`src/ir_inline.rs`, one refusal): a function with `dyn`
  parameters is not inlined, because its single body serves every vtable.
- **Refusals.** `vesper build --codegen verified` and the WebAssembly target
  refuse a program with `dyn` parameters (E0634, `vesper::dyn_trait::refuse`),
  before `src/verified_codegen/**` is reached. The export ABI refuses a
  `pub fn` with a `dyn` parameter with `ExportError::Type`.

## Manuscript change

§7.1's bullet now points to a new §7.3 item, "Second-Class Trait Objects",
which specifies: where `dyn` may be written (E0632), the second-class use
rules (E0631), object safety (E0630), the coercion and its conformance
selection (E0608, E0633), the `sink dyn` mode bound (at most `affine`), the
pair lowering and the vtable layout, drops, and the backend refusals (E0634).
§7.3's monomorphization item no longer says `dyn` is outside every mechanism.

Decisions the manuscript left open:

- Object safety: no associated types; each method has no type parameters,
  takes `self` first by `in`, `inout` or `sink`, mentions `Self` nowhere else,
  and performs only `Fail[E]` and capability effects. Handler effects are
  excluded in Stage 1 because the native backend specializes their evidence
  into each caller, so one slot cannot carry them.
- A `dyn` value may be a method receiver, the first argument of a trait path
  call, or a direct argument for another `dyn` parameter. Nothing else.
- `dyn` is not allowed in trait method signatures, conformance methods or
  effect operations yet.

## Vtable layout

One read-only data object per (object type, implementing type) per
compilation unit, 8-byte aligned:

| word | contents |
|---|---|
| 0 | drop: `fn(data)` dropping a `T` in place, or 0 when `T` owns nothing |
| 1 | move: 0, meaning a bitwise move of `size` bytes |
| 2 | size of `T` |
| 3 | alignment of `T` (8 for every Stage 1 layout) |
| 4 + i | slot of method `i`, in trait declaration order |

A slot takes `self` by address and has the trait method's calling
convention. Each slot is a generated function `vtable$Trait$method$<T>` with
the trait method's signature that calls the selected conformance, so an
implementation that raises fewer effects than its trait still fits. A
by-value scalar `self` (for example `impl Shape for i64`) is reached through a
generated Cranelift adapter that loads it.

## Tests

`tests/dyn_trait.rs` (sources inline, because the self-hosted parser does not
parse `dyn`). Every runnable program runs in the optimized and unoptimized
JIT and the evaluator, which must agree, and is emitted as AArch64 and x86-64
objects. The JIT must free every sequence itself and nothing twice.

- dispatch to three impls (two structs and `i64`) through one `describe`, and
  forwarding `in dyn` to another `dyn` function and to a trait path call
- `inout dyn` mutation through two forwarding levels
- `sink dyn`: consumed by a `sink self` method, forwarded, dropped unconsumed,
  and dropped on one branch only, with the leak check as the drop counter
- a trait whose method raises `Fail`, with a pure implementation in the same
  vtable
- bounded generic functions taking `in dyn` (one instance per static type
  argument)
- zero heap allocation per object: the test's thread-local counting allocator
  sees the same allocation count for 10 and 10,000 iterations
- every rejection: `dyn` as a result, field, `let` annotation, type argument,
  trait method parameter and closure parameter (E0632); binding, tuple,
  capture, return and generic argument (E0631); a concrete parameter (E0200);
  generic method, no receiver, `Self` argument and result, associated type
  and handler effect (E0630); a missing conformance (E0608); a generic type
  coerced before instantiation (E0633); a refusing backend (E0634)

The CI's four required checks run these on Linux, macOS (AArch64) and
Windows.

## Benchmark

`cargo test --test dyn_trait -- --ignored --nocapture bench`: 50 million calls
of a one-line method in the optimized JIT (debug-built compiler, compile time
included, other tests running in parallel):

| call | ns per call |
|---|---|
| through a vtable slot (`in dyn`) | 4.5 |
| monomorphized (`T: Step`) | 2.4 |

The monomorphized call is inlined into the loop; the vtable call is a load of
the slot, an indirect call and the slot function's call of the
implementation. Devirtualizing a coercion whose `dyn` function is small
(inlining the slot function at a known vtable) is future work.

## Stage 1 guard

No file under `src/verified_codegen/**`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`.
A program without `dyn` gets an empty `World::dyn_objects`, and every new
code path is keyed on it, so the Stage 1 corpus compiles as before. The
bootstrap-stage1 workflow on this branch is the evidence (see the PR).

## Which P3b crates this unblocks, and what they still need

- **Chorus** (12 traits): traits whose objects are only borrowed for a call
  can move now. Traits with associated types (`type Item`), generic methods
  or `Self` results are not object safe; they need first-class objects or
  enum dispatch.
- **Flux** sources and sinks: `inout src: dyn Source[...]` works when `Item`
  is a trait parameter rather than an associated type. Sources that perform
  `Net`/`Storage` handler effects need handler effects in slots.
- **Warp** ring abstraction and **Aura** pluggable backends: borrowed
  backends passed into a frame or render call work. Backends stored in a
  struct need first-class objects.
- **Aegis** (`dyn` callbacks): only borrowed callbacks.

## Remaining gaps (first-class `dyn` would need)

- Boxed objects (`Box[dyn T]`): a heap pair with the vtable's drop and size
  words, owned-object moves, and `Alloc` in the row.
- Objects in fields, results and collections: the pair layout in `Layout`
  for those positions, an escape analysis replacing the syntactic second-class
  rule, and drops of stored objects.
- Handler effects in slots: passing the caller's evidence vector to the slot
  instead of specializing it.
- Associated types: per-object projection types (`dyn Source[Item = u8]`).
- `dyn` in trait methods and effect operations, and package-qualified traits.
- Verified backend, WebAssembly and export ABI support for the pair.
