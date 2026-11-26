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
`extract_report` lowers the world and every closure it lifts, so the
Vesper side runs the test's `lift` (the same lowering) too:

| | Rust `extract_report` + render | Vesper `lift` + `extract_module` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus modules | 5,180 ms | 5,731 ms | 1.11x |

Lowering is most of both sides; `extract_module` alone is 633 ms. The
first measurement (0.94x, in #1937) ran only `lower_world` on the Vesper
side, leaving out the closure lifting. Lifting then cost twice the
lowering: it lowered the whole world once per closure and copied every
world. It now lowers each nesting depth's closures once, in a copy of the
world without bodies, and copies nothing when there is no closure (still
0 differences).

The word encoding (about 4 s more) is the harness's and is not counted:
a Vesper front end hands the words over directly. A hybrid swap of
`lean::extract` does pay it, so the swap needs a cheaper encoder before
it can flip.

## Part 4: the `lean_extract` hybrid swap

`lean::extract` (the text of `vesper emit --emit lean` and the driver's
Lean emit step) is now a swap point, `LeanExtractTable`, with Rust as the
default. The hybrid's `lean_extract` swap (crates/vesper-hybrid/modules/
lean_extract and src/swap/lean_extract.rs) encodes the world with
tests/common/lean_words.rs, the encoder moved out of the test so both
share it, and calls the new entry `extract_text`, which stops after the
module. A world the word format cannot hold keeps Rust's extraction.

The divergent build (`divergent-lean-extract`) empties every text, and
the differential must then fail.

### Differential

`cargo test -p vesper-hybrid --test lean_extract` runs `vesper emit
--emit lean` with the default compiler and the hybrid over every `.ves`
file of `examples/` and `tests/fixtures/`:

| Test | Cases | Differences |
|---|--:|--:|
| `hybrid_lean_matches_the_default_compiler` | 531 files, 194 extracted (3.06 MB of Lean) | 0 |

Hybrid 7.72 s against default 9.65 s (0.80x, debug builds, the whole
command including the word encoding).

Three files (`examples/traits/shapes.ves`,
`tests/fixtures/bounded_generics/nested.ves`,
`tests/fixtures/native_handlers/shared_locals.ves`) already differ in
`--emit hir`: the hybrid's AST optimizer swap reassociates differently
from Rust's (for example `x * 1000 + (y * 100 + result)` stays as written
in the hybrid, while Rust moves `y * 100` first). That is upstream of
extraction, so the test compares those three again with `--no-ast-opt`,
where they agree.

## Part 5: `vesper proof-audit` in Vesper

`@vesper/proof-audit` (packages/vesper-proof-audit) is the whole command
of `src/proof_commands.rs`: option parsing and its errors, the Lean
toolchain probe, the private staging directory, the artifact snapshot
and its limits, Lean run with a deadline and an 8 MiB capture limit
(`wait_timeout`, `kill_tree`, `capture_limit`, #1991), the auditor's
record, the fingerprints of every imported module, the macro provenance
gate with `ProvenanceTable::verify`, and the report in JSON or text.

- **JSON out** (`json.ves`) writes what serde_json writes: compact or
  `to_string_pretty`, its escapes, keys in struct order or sorted for
  `json!` maps.
- **JSON in** (`reader.ves`) reads `RawAudit` and `ProvenanceTable` over
  the @vesper/serde tape with serde_json's nesting limit, unknown fields
  denied, `ByteRange`'s ignored.
- **Hashing** is BLAKE3 in C (@vesper-std/hash-sys). An audit digests
  every imported module (706 MB for a `Std` import), so pure-Vesper
  BLAKE3 would not meet the bar.
- **The auditor** (`tools/proof-audit/Audit.lean`) is a generated string
  constant, `src/auditor.ves`; the test checks it is current
  (`VESPER_PROOF_AUDIT_BLESS=1` rewrites it).

### Differential

`tests/self_host_proof_audit.rs` builds a small executable from the
package and runs each case through it and through `vesper proof-audit`.
stdout, stderr and the exit status must be byte-identical. The staging
directory's name, which holds a process id, is masked.

| Cases | Count | Differences |
|---|--:|--:|
| Option errors, `--help`, a Lean that cannot start | 38 | 0 |
| Real Lean 4.35.0 replays: accepted, rejected and allowed axioms, `sorry`, a non-theorem, a partial definition, a 1 s timeout, a symlink, nested directories | 18 | 0 |
| Macro provenance: with and without manifests, nine broken or forged manifests, stripped and missing markers | 20 | 0 |

When a manifest or auditor record is not the JSON its struct expects,
both sides fail with the same prefix (`invalid macro provenance
manifest: `), but the rest is serde_json's own text (`EOF while parsing
an object at line 1 column 1`). The port names the field or says the
text is not JSON, and the test masks that detail.

### Speed

Over the 38 Lean cases, Rust took 173.5 s and the port 169.1 s (0.97x).
Both spend nearly all of it in Lean.

### Found on the way

- **A variant spelled like a type.** A package whose enum has a variant
  `String` (`@vesper/serde`'s `Kind::String`) made every `String` type in
  the solution an error: "variant 'String' belongs to enum ..., which is
  not declared or imported". Name resolution checked the global variant
  names in the type namespace too. `src/packages.rs` and the port
  (`@vesper/check-names-packages`, `rename.ves`) now check them only for
  values. The fixture `tests/fixtures/packages/accept/variant_named_like_type`
  covers it.
- **Handler clauses get copies.** An assignment to an outer local inside
  a `raise` clause changes the clause's copy and is lost, so an error has
  to come back in the handled value. The port does that throughout.

## Part 5b: the `proof_audit` hybrid swap

`hc_proof_audit_run` (crates/vesper-hybrid/src/commands.rs) now answers
from `modules/proof_audit` when the `proof_audit` swap is on (the
default): the driver-commands layer validates as before, packs the
arguments, and the module runs `@vesper/proof-audit::run` and returns a
status byte, the stdout bytes and the error text. `--json` error
documents and text reports come from Vesper.

- `self_host_cli_commands`'s `proof_audit_matches_the_default_compiler`
  adds three vectors past the command layer (a missing root in text and
  JSON, a Lean that is not there); all 23 vectors match the default CLI.
  `proof_audit_audits_in_vesper` checks the module answered.
- The byte-level differential, with real Lean, stays
  tests/self_host_proof_audit.rs (76 of 76).

**Finding: one variant namespace per world.** Linking
`@vesper/proof-audit` into the driver module fails with E0113: the
driver's world already has the eBPF backend's `MapKind::Array`, and
`@vesper/serde` declares `Kind::Array`; enum variants share one
namespace across every package of a program. The swap uses its own
module, so the two never meet. A Stage 2 driver that links both needs
one of them renamed.

**Build note.** A hybrid module whose closure has `[package] objects`
(here hash-sys's BLAKE3 C) gets them through
`VESPER_HYBRID_OBJECT_PACKAGES_<LIBRARY>` under Bazel (build.rs sets the
harness's `VESPER_PORT_OBJECT_PACKAGES` for that module only); Cargo
finds them through the path dependencies.

## Part 6a: `verify`'s pure pieces in Vesper

`packages/vesper-verify` (`@vesper/verify`) ports the two parts of
`src/verify_commands.rs` that need neither the formatter nor the checked
world:

- `discover` is `discover_theorems` with its tokenizer
  (`tokenize_lean_proofs`): line and nested block comments, strings and
  char literals, `«»` names, namespaces, named and anonymous sections,
  `mutual`, `private`/`protected` modifiers and the 4096-byte error
  truncation on a char boundary.
- `bindings` is `contract_bindings`: the emitted-contract,
  proof-binding and precondition-witness checks and the generated Lean
  source, keys in byte order like the `BTreeMap`s.

tests/self_host_verify.rs runs both against the Rust (now `pub(crate)`):
every `.lean` file in the repository up to 128 KiB, seven char-boundary
prefixes of each, about 70 hand-written edge cases and generated inputs
(long names, more than 128 theorems): 1612 inputs, 1295 of them with
theorems, 0 diffs; 11 binding cases, 0 diffs.
Release build, median of six runs: Rust 99 ms over the 1612 inputs, the
port 106 ms as one process that also reads every file (1.07x; single
runs range 0.6x to 1.5x beside the test's other threads).

**Found on the way.** A slice of a slice is not a value (E1521), so
the scanner keeps the next token as offsets into the source.

## What is left

- **Verify command** (`src/verify_commands.rs`): `--format` needs the
  formatter's proof model (`vesper-fmt` proof_model.rs, proof_lean.rs)
  in Vesper; `--extract` needs the checked world in Vesper. Both reuse
  `@vesper/proof-audit`'s audit (`audit_generated`, `audit_policy`).
- **`extract_report` callers**: `verify --extract` and the forge
  provenance path still build the Rust `Extraction`; they move with the
  verify command, then the Rust extractor goes.
- **Deleting the Rust**: `src/proof_commands.rs` stays while the Rust
  CLI (`src/main.rs`) and `verify`/`publish` call it; it goes with the
  Stage 2 retirement of the Rust driver.
