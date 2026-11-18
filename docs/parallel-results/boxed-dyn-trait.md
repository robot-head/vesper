# Boxed and stored `dyn Trait` (`Box[dyn Trait]`)

Claim: #1057. Follow-up to port wave P3a lane 1 (#1036, PR #1051, second-class
`in`/`inout`/`sink dyn` parameters). Baseline: `main` at `db62471`.

## What changed

- **Syntax.** `Box[dyn Trait[args]]` is a type anywhere a type is written, and
  `Box::new[dyn Trait](value)` parses (the call's type-argument lookahead
  accepts `dyn Upper`).
- **Frontend** (`src/dyn_trait.rs`, `src/pact_frontend.rs`,
  `src/pact_frontend/mono.rs`). The lowering pass admits `Box[dyn Trait]` in
  every position (fields, payloads, `let`, type arguments, results, trait and
  conformance method signatures) and still rejects a bare `dyn` outside a
  parameter's whole type (E0632). Every trait used as a boxed object is
  checked for object safety (E0630) and gets vtable stubs. A method call on
  a `Box[dyn Trait]` receiver resolves to the trait's vtable stub. The
  monomorphization pass records one vtable per (object type, value type) at
  each `Box::new` typed `Box[dyn Trait]` whose argument is concrete, with the
  same selection, `use model` scopes and one-vtable-per-pair rule as a `dyn`
  argument (shared code: `Mono::dyn_vtable`). Lending a boxed object to a
  `dyn` parameter needs no new vtable and must name the same trait (E0608).
- **Checker** (`src/check.rs`, `src/seq/check.rs`). `dyn Trait` has mode
  `affine`. `Box::new(v)` expecting `Box[dyn Trait]` checks `v` without the
  object type as its expectation and refuses a linear value (E0205) or a
  `dyn` parameter (E0631). A `Box(...)` pattern on a boxed object is E0631. A
  sequence element `s[i]` that holds a boxed object may be lent to a call
  (`in` or `inout`), which borrows the whole sequence; method receivers that
  are sequence elements are typed by the element type.
- **Native backend and JIT** (`src/dyn_trait/native.rs`, `src/native.rs`). A
  boxed object is one word: the address of a heap block whose word 0 is the
  vtable address and whose value follows at offset 8 (`BOXED_HEADER`). It
  therefore moves, nests and fills sequence slots like any `Box[T]`, and the
  Box drop elaboration tracks it. `Box::new` allocates `8 + size_of(T)`,
  writes the vtable and moves the value in. A call through a boxed receiver
  passes the box's block whatever the method's `self` pass (`inout self`
  mutates the value in place; the box itself never changes), loads the slot
  from the vtable and passes the value's address. A `sink self` method or a
  `sink dyn` lend frees the block after the call. Lending to an `in`/`inout`
  `dyn` parameter builds the `{data, vtable}` pair on the stack. Dropping a
  boxed object calls the vtable's drop word on the value, then frees the
  block.
- **Loops.** The Box drop elaboration needs an acyclic graph, so a function
  with loops or early exits that mentions `Box` was refused. It is now
  accepted when it owns no box-typed local or `inout`/`sink` parameter, reads
  boxed sequence elements only to lend them, and consumes every other
  box-typed value once (`fip::loop_boxes_reclaimed`). That covers a loop over
  a sequence of processors held by a local sequence or an `inout self`.
- **Review fixes.** A call through `Box[dyn Trait[args]]` binds the vtable
  stub's trait parameters to `args` before checking
  (`mono::specialize_boxed_stub`), so a generic trait's result cannot be
  inferred as another type. The loop rule expands aggregate fields (a local
  struct owning a box is refused). A box consumed by a `sink` call is freed
  right after the call instruction, before a `Fail` status can leave the
  caller; a `sink` lend to a handler-specialized (inlined) callee is refused
  natively. A sequence element is lent `inout` only to a stub receiver or an
  `inout dyn` parameter, never to `inout b: Box[dyn Trait]` (E0300).
- **Evaluator** (`src/runtime.rs`). A boxed object is `Value::Object`: its
  value in a shared cell and its vtable. A stub call on one jumps to its
  slot with the value (an `inout self` gets a reference to the cell), so
  mutations through a sequence element reach the object; a lent object
  passes its own vtable as the callee's `dyn` evidence. `vesper run` agrees
  with the JIT on every test.
- **Refusals.** `dyn_trait::refuse` (verified backend, WebAssembly) now also
  refuses a program with boxed objects (E0634).

## Manuscript change

§7.3 gains the item "Boxed Trait Objects (`Box[dyn Trait]`)" after
"Second-Class Trait Objects": where the type may be written, construction and
coercion rules, method calls and lending, the one-word block layout, drops,
the loop rule and the backend refusals. The second-class item now says the
unboxed form is second-class.

Decisions the manuscript left open:

- Coercion happens only at `Box::new` (with an expected `Box[dyn Trait]` or
  an explicit `[dyn Trait]` type argument). An existing `Box[T]` is not
  coerced; box the value where it is made.
- A thin box (vtable in the block) rather than a fat `{data, vtable}` pair,
  so `Box[dyn Trait]` keeps the one-word layout every `Box` path assumes.
- Calls through a boxed object only read the box, so an `inout self` method
  can be called on a sequence element `s[i]`, borrowing `s` `inout`.

## Tests

`tests/dyn_box.rs`. Every runnable program runs in the optimized and
unoptimized JIT (which must agree, free every sequence once and free as many
boxes as they allocate) and the evaluator, and is emitted as AArch64 and
x86-64 objects.

- locals: dispatch to three implementations (two structs and `i64`),
  `inout` mutation, lending to `in dyn` and `inout dyn` parameters
- struct fields, `Option` fields and results (`Box<dyn CidSource>` shape),
  replacing a field drops the old object
- `ChunkSeq[Box[dyn Processor]]` called element by element in a loop
- drops: end of scope, reassignment, `sink self` through the box, lent to a
  consuming or dropping `sink dyn`, held by a sequence; values owning
  sequences and boxes
- bounded generics boxing their type argument, boxed objects in enum payloads
- trait path calls (`Shape::area(b)`) and a trait method returning a boxed
  object
- loops calling through a sequence held by `inout self` and a boxed field
- rejections: bare `dyn` field and type argument (E0632), non-object-safe
  trait (E0630), method of another trait, lent as another trait and missing
  conformance (E0608), linear value (E0205), boxing a `dyn` parameter and
  unpacking the box (E0631), boxing before instantiation (E0633), backend
  refusal (E0634)

## Benchmark

`cargo test --test dyn_box -- --ignored --nocapture bench`: 50 million calls
of a one-line method in the optimized JIT (debug-built compiler, compile time
included):

| call | ns per call |
|---|---|
| through a boxed object (`in Box[dyn Step]`) | 5.7 |
| through a `dyn` parameter (`in dyn Step`) | 4.5 |
| monomorphized (`T: Step`) | 3.6 |

The boxed call adds the load of the vtable from the block to the `dyn` call.

## Stage 1 guard

No file under `src/verified_codegen/**`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`.

## What the P3b ports still need

- Handler effects and function values in vtable slots (`Box<dyn FnMut>` in
  warp and prism, sources performing `Net`/`Storage` effects in flux).
- Associated types (`dyn Source[Item = u8]`), several models per (object,
  type) pair, and package-qualified traits.
- Lending a boxed object `sink` to a function whose handler effects are
  specialized at the call (native).
- A function with a loop still cannot own a box (a `let` holding a boxed
  object, or a `sink` parameter); move the loop into a function that borrows
  it. Plain `Box[T]` keeps the old rule.
- `Rc<dyn Trait>`/`Arc<dyn Trait>` (shared objects) are not covered.
