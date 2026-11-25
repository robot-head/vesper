# Self-hosting SH5 lane 18: Lean extraction in Vesper

Claim #1341. Base `bbda89e` (main after #1322). Prompt:
[18-lean-extraction.md](../agent-prompts/self-host-sh5/18-lean-extraction.md).
Rust stays the default extractor: nothing here replaces or deletes Rust
code, and no default build changes.

## What changed

Part 1 of the lane ports the **head** of the module `lean::extract_report`
builds: every declaration before the first source function.

- **`packages/vesper-lean-extract`** (new, on `@vesper/check-data` and
  `@vesper/lean-syntax`):
  - `src/prelude.ves`: `prelude` (`HandlerCPS`, its `Monad` instance,
    `liftAction`, `$arithmetic_semantics`, `inI64`),
    `arithmetic_simp_bundle` (the 24-rule `$arith_simps` theorem),
    `divergence` (`VesperDiverge` and its three instances) and
    `fail_models`.
  - `src/head.ves`: `adt_groups` and `lean_nodes::recursive_structs`,
    including petgraph 0.8.3's `tarjan_scc` over a `DiGraphMap` (node and
    edge insertion order, Pearce's index backtracking) so the groups come
    out in Rust's order; inductives, structures, recursive structures and
    their getters; each effect's class; each effect's `lift_E` abbreviation
    and instance.
  - `src/ty.ves`: `ty_with_fip` with no ambient monad (the only form the
    head uses), and `ty_error`, which finds the error Rust reports first in
    Rust's evaluation order (function results before `inout` parameters
    before the effect check before the parameters in reverse).
  - `src/build.ves`: the builder. It writes the `@vesper/lean-syntax`
    printer's prefix word stream directly (tags in
    `crates/vesper-lean-syntax/src/port.rs`), back-patching application
    sizes and counts, instead of building a tree and encoding it. Every
    text is a check-data symbol, so the names table's bytes are the text
    pool. `Ident::new`'s check runs on the UTF-8 bytes, and the error
    message reproduces Rust's `{:?}` escapes for the characters it refuses.
  - `src/input.ves`: the effect section of the input (lane 4's world has
    effect names only).
  - `src/lib.ves`: the entries `extract_head` (text) and
    `extract_head_words` (timing without the printer).
- **`tests/self_host_lean_extract.rs`** (new): the effect-section encoder
  and the differential.
- `BUILD.bazel`: the test's data (lane 4's anchored globs plus
  `crates/vesper-lean-syntax/vesper/**`) and the `long` tag.

## Differential

The head reads no function bodies, so the Rust side is `lean::extract` of
each world with `functions` and `function_results` emptied: that output is
exactly the head (`import Std`, the option, `namespace Vesper` and the
head's declarations). `head_is_a_prefix_of_the_full_module` checks that it
is the first declarations of the full module for every world whose full
extraction succeeds.

`cargo test --test self_host_lean_extract` (debug, Linux x86-64):

| Test | Cases | Differences |
|---|--:|--:|
| `head_matches_rust_on_the_corpus` | 296 worlds, 2,592 ADTs, 400 effects, 23,114 lines | 0 bytes |
| `mutants_match_rust` | 11 mutants, 9 refused by Rust | 0 bytes |
| `unoptimized_jit_agrees` | every 7th world | 0 |
| `head_is_a_prefix_of_the_full_module` | 209 of 296 worlds extract in full | 0 |

No corpus world is refused in the head, so the refusals come from mutants:
`i128` and `u128` fields, a row field, an effectful function field, an ADT
name and a field type name Lean cannot spell (`A«B`, `T\u{202e}`), and three that pair a bad name with a bad field type, since Rust checks names first. Two more
mutants cover type shapes the corpus rarely has: function fields with
`inout`, `in` and `sink` parameters and zero parameters, `Result`
argument order, `TiledSeq`, nested tuples, `_` and integer literal types.

On those 209 worlds the head is 2,418,260 of the 3,489,674 bytes of
extracted Lean (69%): the prelude alone is repeated in every module.

## Speed

`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_lean_extract
abba -- --ignored --nocapture`, ABBA, best of six per side, all 296 worlds
per run:

| | Rust `lean::extract` (head world) | Vesper `extract_head` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus head | 180.9 ms | 61.1 ms | 0.34x |

The Vesper time includes decoding the harness input and unpacking the text;
the Rust time includes the empty `lower_world` and contracts passes that
`extract` runs for a world with no functions. The difference is mostly
that Rust builds and clones `Term` trees and Vesper writes the printer's
words once.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changes, and no Stage 1
re-lock was run: the diff is a new package, a new test, this report and
`BUILD.bazel` data. The bootstrap-stage1 workflow (`bootstrap/run.sh stage1
--reproduce --outcome` against `bootstrap/stage1.lock`) passed on the branch
(run 44887193178, 956db66); the later commits touch only the package, its
test, `BUILD.bazel` data and this report, plus merges of `main`.

## Compiler gaps

1. **`effect` cannot be a method name.** `fx.effect(i)` is E0002 (the
   keyword is reserved even after `.`); renamed to `effect_at`. Repro:
   `impl S { fn effect(in self) -> u64 { 0 } }`.
2. **A shift by a variable count raises `Fail`.** `(x >> shift) & 15` with
   `shift: u32` is E0401 outside a `Fail` row; `wrapping_shr_u32` works.
   SH5 lane 5 (#1328, guarded shifts) owns this.

## Part 2: the CFG extractor

`extract_report` extracts each function with the CFG extractor
(`lean_ir::extract_function_nodes`) when its gate admits it: no effects,
no effectful arrow among its values, and no fallible, partial or
overflowing instruction, handler, `try`, index or sequence operation, or
call of an effectful function. The others go to the structured extractor
(`Generator::function`). Part 2 ports the CFG extractor.

- `src/cfg.ves`: the gate, the function tables (parameters, values and
  locals from the place-ir header), `uses_defs`, `arm_bindings` and the
  `liveness` fixpoint. Live sets are bitsets whose bit order is Rust's
  `BTreeSet<Local>` order (slots by name, then values by id), so the
  blocks' binders and call arguments come out in Rust's order.
- `src/emit.ves`: `extract_closures` (each lifted closure's definitions
  first, stopping at the first refusal) and the `Emitter`: block
  definitions over the live locals, every instruction form, patterns
  (with `Box` patterns unwrapped), the four terminators, `project`,
  `update_place_typed` (including recursive structures' `mk`
  rebuilds), `arith_term`, `tiled_op_term`, `typed_literal` (128-bit
  decimal literals and `f64` to `f32` rounding) and the float-intrinsic
  refusal.
- `src/fn_input.ves`: what the extractor reads of `world.functions`,
  `world.calls` and the lifted closures.
- The program comes in as `@vesper/place-ir` words (SH3 lane 1). The
  encoder, `cfg_input` in the test, lifts each closure the way
  `extract_closures` does (it lowers the world extended with the lifted
  function) and adds it to the program, so Vesper does no lowering.
- `src/lean.rs`: a `#[doc(hidden)] pub fn cfg_function_nodes` hook that
  runs `extract_function_nodes` per function. It adds no behaviour.
- `tests/common/ir_words.rs`: `Tables::label_names` is now `pub`, so the
  test can append its own sections with the same encoder.

Rust builds some nodes out of prefix order: a definition's binders and body
before its name and result type, and an application's arguments before its
function. The builder therefore writes those parts quietly (`B::quiet`) and
checks them afterwards in Rust's order. A term Rust builds before the node
that holds it, such as an updated place's new value, is written first and
moved into place (`B::cut`, `B::paste`). This keeps the first refusal the
same as Rust's when several apply.

### Differential

`cargo test --test self_host_lean_extract cfg_` (debug, Linux x86-64).
Per source function, the text is `none` (the function goes to the
structured extractor), the refusal, or the rendered definitions:

| Test | Cases | Differences |
|---|--:|--:|
| `cfg_functions_match_rust_on_the_corpus` | 303 worlds, 22,058 functions; 13,547 on the CFG path, 2,899 of them refused; 111,846 lines | 0 |
| `cfg_functions_match_rust_on_test_sources` | 155 programs from `tests/lean*.rs` and `tests/tiled.rs` plus mutants: 258 functions, 153 on the CFG path, 8 lifted closures, tiled operations, 6 name refusals | 0 |
| `unoptimized_jit_agrees` | every 7th world | 0 |

The corpus's refusals are mostly loops (1,758), bit operators (`&`, `>>`,
`^`, `|`, `<<`, rotations) and integer conversions written as unary
operators (`widen_*`, `wrapping_*`). The tiled-sequence program and the
renamed functions are mutants, because the corpus has no tiled operation on
the CFG path and no name that Lean cannot spell. The renamed functions
also carry `i128`/`u128` types, which tests the refusal order.

### Speed

`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_lean_extract
abba_cfg -- --ignored --nocapture` runs ABBA (best of six per side) over
all 303 worlds per run. The input is the lowered program on both sides.

| | Rust `extract_function_nodes` (per function) | Vesper `extract_cfg_functions` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus functions | 1,287 ms | 1,253 ms | 0.97x |

The Rust time includes re-lowering the world for each lifted closure
(`extract_closures`), which the Vesper side gets from the encoder; the
corpus has one closure on the CFG path. The Vesper time includes decoding
the input, rendering each function as its own module (Rust only builds the
nodes) and packing the text. So the two sides are at parity, not a clear
win. The next speedup is a symbol-indexed local table: `Fun::slot_bit` is
currently a linear search over the function's locals.

## Part 3a: the structured extractor over `Id`

`Generator::function` extracts the functions the CFG gate refuses. Part 3a
ports it for every function that is not polymorphic in its monad, that
is, with no effects, no effectful arrow in its result and no effectful
arrow among its parameters. Those use the `Id` monad.

- `src/ast.ves`: the function bodies. The encoder (`Ast` in the test)
  appends an AST section after the CFG input: per function its body,
  parameters (name, passing mode, type) and result, then the patterns and
  expressions. The encoder also precomputes what Rust looks up in
  `World` tables: sequence and tiled routes (`lean::tiled_route`, a hidden
  hook over `tiled::resolve_ir_call`), name classes (float refusal, integer
  intrinsic, wrapping operator, `Box::new`), `Box` patterns, closures'
  free names in `BTreeSet` order and each expression's type after
  `default_type`.
- `src/tm.ves`: term builders (references, applications, operators,
  `ite`, matches, projections, records, binders, patterns and statements).
- `src/gen.ves`: the generator. Its scope and current-value tables are
  stacks where the last entry wins and a restore truncates. A block
  restores only the scope; `if`, `match`, `&&`, `||` and closures restore
  both, as in Rust. Names come from `fresh` and `temp` with Rust's
  counters.
- `src/build.ves`: terms are word ranges (`T { a, n }`) of the builder,
  so a term can be built before the node that holds it and copied in
  (`B::at`, `B::since`, `B::put`). Construction follows Rust's evaluation
  order, so the first refusal is Rust's.
- `src/lean.rs`: `#[doc(hidden)] pub fn structured_function_nodes`, which
  runs `Generator::function` over `Id` per function. It adds no behaviour.

The port writes `skip` for what the effects part will do: handlers,
effect operations, effectful closures and calls, and types with an
effectful arrow. The test replaces each skipped function's text with
Rust's.

### Differential

`cargo test --test self_host_lean_extract structured_` (debug, Linux
x86-64). Per source function, the text is `none` (CFG path or
polymorphic), `skip`, the refusal, or the rendered definition:

| Test | Cases | Differences |
|---|--:|--:|
| `structured_functions_match_rust_on_the_corpus` | 303 worlds, 22,319 functions: 2,274 definitions and 5,174 refusals from the structured extractor, 119 skipped; 46,912 lines | 0 |
| `structured_functions_match_rust_on_test_sources` | 155 programs from `tests/lean*.rs` and `tests/tiled.rs` plus mutants (including tiled column operations and record patterns off the CFG path): 66 definitions, 10 refusals, 63 skipped | 0 |

### Speed

`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_lean_extract
abba_structured -- --ignored --nocapture` runs ABBA (best of six per side)
over all 303 worlds per run. Both sides run the CFG gate first.

| | Rust `structured_function_nodes` | Vesper `extract_structured_functions` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus functions | 1,924 ms | 4,562 ms | 2.37x |

Taking out the CFG gate (about 1.25 s on each side, from part 2's timing)
leaves roughly 0.6 s for Rust's structured extractor and 3.3 s for
Vesper's, about 5x. Vesper's time also includes decoding the larger input
and rendering each function's text, which Rust does not do here. This part
is not at parity; profiling the generator (name lookup in `fresh`, the
linear scope and current-value stacks, and term copies) is the next step
before the swap.

## Part 3b: effects and handlers

Part 3b ports the rest of `Generator`: the functions that are polymorphic
in their monad `m` (effect rows, effectful arrows in the result or the
parameters). With it the port writes no `skip`.

- `src/gen.ves`: the polymorphic prologue (`{m}`, `[Monad m]`, one
  `[$cap{i} : Effect_E args m]` per labeled requirement, the `$sync`
  keep-writes `optParam`), `m (...)` results, effect operations
  (`perform_op`: the direct lens path and the `Effect_E.op (self := ev)`
  path, with borrowed arguments written back), handlers (`handler`:
  `HandlerCPS` layers, `liftAction`, `lift_E` evidence, state and return
  clauses, direct and fallback resumptions, frames and
  `post_resume_access`), effectful closures, local calls through function
  values, and calls of effectful world functions with their evidence and
  `$sync` arguments.
- `src/ast.ves`: handle expressions, clauses, closure rows and the effect
  declarations (linear flag, generics, operation parameters with their
  projections, results).
- `src/ty.ves`: `emit_ty_m` wraps an effectful arrow's result in the
  monad; `ty_error_m` refuses `m` only outside a monad.
- `src/lean.rs`: `structured_function_nodes` now runs polymorphic
  functions with `m`. Two more hidden hooks, `handler_written_names` and
  `visit_exprs`, let the test encoder precompute the names a handler body
  writes and the free-name spans of each clause. They add no behaviour.

The encoder also precomputes, per call, the `instance_key` of an
unlabeled `E::op` with explicit types, and per handler the entries the
routes check needs (function arrows, typed layers, other callees).

One fix came out of the corpus: indexing a constant table
(`$table$const$T(..)[i]`) refuses the call first, as Rust does, but the
port kept building the tuple selection chain after the refusal. For a
1,024-entry table (`vesper-kinetic`'s Morton tables) the chain's copies
are cubic in the arity and ran out of memory. The index now stops once
the operands fail.

### Differential

`cargo test --release --test self_host_lean_extract structured_` (Linux
x86-64):

| Test | Cases | Differences |
|---|--:|--:|
| `structured_functions_match_rust_on_the_corpus` | 354 worlds, 51,864 functions: 7,234 definitions and 13,643 refusals from the structured extractor, 0 skipped; 110,962 lines | 0 |
| `structured_functions_match_rust_on_test_sources` | 155 programs and mutants: 89 definitions, 16 refusals, 0 skipped | 0 |

### Speed

`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_lean_extract
abba_structured -- --ignored --nocapture`, ABBA over all 354 worlds, CFG
gate on both sides:

| | Rust `structured_function_nodes` | Vesper `extract_structured_functions` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus functions | 4,642 ms | 3,347 ms | 0.72x |

The structured extractor is under the 1.5x bar, including the effects
part. The Rust code cannot be deleted yet: `extract_report` (ordering,
fuel companions, contracts, the proof module) still runs in Rust only.

## Part 3c: ordering, fuel companions, contracts, the proof module

The entry `extract_module` builds the whole module `extract_report`
builds, so the port now covers all of `src/lean.rs`, `src/lean_ir.rs` and
`src/lean_nodes.rs`:

- **Ordering** (`src/module.ves`): the definitions are read back out of the
  word stream; `free_references_at`, `term_end` and `binders_end` (new in
  `@vesper/lean-syntax`, binding regenerated) walk each body. Tarjan runs
  over edges in Rust's `BTreeSet<Name>` order, so groups and members come
  out in petgraph's order. Cyclic groups are `mutual` blocks of the
  recursive kind; nullary `$block_*` definitions are `abbrev`, the others
  `@[simp]`.
- **Fuel companions** (`src/gen.ves`): for the recursive family, the
  generator runs again with a `[VesperDiverge m] ($fuel : Nat)` prologue,
  the `match $fuel` wrapper, and `f$fuel ($fuel := $fuel)` calls; a pure
  closure or a value use of a family function is refused with Rust's
  message. A companion that calls a function with no companion is dropped,
  as in Rust. `Totality` comes out as `total`, `fuel` or `partial: reason`.
- **Contracts** (`src/contract.ves`): predicate and signature checks in
  Rust's order, `$contract` and `$admissible` propositions with `Fail`
  capabilities (`MonadExcept.throw` on `Sum` injections), `inout` before
  and after values, generics substituted by name.
- **Proof module**: the executable definitions without the recursive
  family, the companions as `noncomputable`, then the contracts; the
  omitted names are sorted.

### Differential

`cargo test --release --test self_host_lean_extract modules_` (Linux
x86-64):

| Test | Cases | Differences |
|---|--:|--:|
| `modules_match_rust_on_the_corpus` | 363 worlds (7 with contracts, 26 contracts stated), 152 refused, 84 fuel companions; 23,239 lines | 0 |
| `modules_match_rust_on_test_sources` | 197 programs and mutants (28 with contracts, 10 contracts stated), 73 refused | 0 |

The rest of the suite still passes on the same corpus: head 0 of 363
worlds, CFG 0 of 363 (288,024 lines), structured 0 of 363 (125,609
lines).

### Speed

`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_lean_extract
abba_module -- --ignored --nocapture`, ABBA over all 363 worlds.
`extract_report` lowers the world itself, so the Vesper side runs
`lower_world` too:

| | Rust `extract_report` + render | Vesper `lower_world` + `extract_module` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus modules | 4,925 ms | 4,622 ms | 0.94x |

Lowering is most of both sides; `extract_module` alone is 657 ms. The
test's word encoding (11.4 s) is harness-only and not counted: a Vesper
front end hands the words over directly.

## What is left

- **Verify and proof commands** (`src/verify_commands.rs`,
  `src/proof_commands.rs`): port onto `@vesper/driver-commands`.
- **Hybrid module `lean_extract`**: a swap point for `extract_report`,
  then the Lean emit driver step, then deleting the Rust extractor.
