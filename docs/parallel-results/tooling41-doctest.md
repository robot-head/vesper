# Shared native batch and checked documentation tests (#67)

Read issue #67, `docs/rfcs/tooling41-contracts.md`, the complete §40 design and
the existing native compiler/checker/IR interfaces. Implemented new
`src/native_batch.rs`, `src/doctest.rs`, `src/doctest_commands.rs`, matching new
integration tests, and one batch-module registration in `src/native.rs`.
Existing JIT/object/session/compiler schemas remain unchanged.

## Reusable native API

```rust
use vesper::native::batch::{BatchLimits, NativeBatch};
let mut batch = NativeBatch::compile(&checked.world, &["first", "second"], BatchLimits::default())?;
let first_value = batch.run("first")?;
let second_value = batch.run("second")?;
let stats = batch.stats();
```

`NativeBatch` owns **one** Cranelift `JITModule`. It lowers the checked World,
computes the union of reachable ordinary monomorphic functions, and calls the
existing parent's `declare_instances` and `define_instances` once each. Shared
helpers are defined once, not once per entry. Finalization also happens once.
Optimization is **off**: this avoids a second optimizer profile or assumption
about reassociation/termination. Existing single-entry APIs are untouched.

Private entries cache finalized code pointers and result kinds. Compilation
checks no-argument entry signatures, absence of hidden effect/sret parameters,
exact scalar return ABI and host calling convention. `run(&mut self, name)`
returns owned scalar `runtime::Value` data, validates canonical bool/Unit/narrow
unsigned results and enforces an aggregate run-count budget before execution.
It does no repeated World/Layout/signature traversal. No code pointer, callable
closure, reference into executable memory or lifetime-erased entry escapes.
The owner is neither Send nor Sync. Drop frees its module after all calls have
returned; the same owner guard frees partially compiled modules on errors.

The supported execution closure is deliberately small: pure monomorphic
ordinary functions, primitive scalar signatures/values, local scalar operations
and resolved direct calls. Source arithmetic intrinsics already recognized and
lowered by the existing compiler remain available. Aggregate values, inout
parameters, generics, closures, handlers, indirect targets, effect calls,
fallible primitives and body-less/ambient foreign imports are rejected before
JIT invocation. The internally registered existing `fmod` arithmetic helper is
not a source-selected external import. Source callers supply a fully checked
World; the doctest adapter performs that check itself.

Every function CFG is checked for cycles/invalid targets. Call-graph recursion
is rejected. A separate bottom-up expanded-cost calculation adds each callee's
cost at **each call site**, rejecting exponentially expensive acyclic call DAGs.
Call depth, AST/IR volume, entries and repeated calls have explicit bounds. This
is a conservative structural work bound, not a wall-clock timeout or sandbox.

`BatchLimits` defaults / hard ceilings:

| Field | Default | Ceiling |
| --- | ---: | ---: |
| max_entries | 64 | 128 |
| max_functions | 256 | 512 |
| max_ast_nodes | 20,000 | 50,000 |
| max_ast_depth | 64 | 64 |
| max_ir_instructions | 50,000 | 100,000 |
| max_ir_blocks | 4096 | 4096 |
| max_call_depth | 32 | 64 |
| max_execution_work | 100,000 | 1,000,000 |
| max_runs | 1,000,000 | 100,000,000 |

Additional fixed metadata limits are 256 ADTs, 128 effects, 64 parameters and
32 generics per source function; checked type/call maps are bounded by the AST
node budget. AST volume counts expression nodes; IR totals include all lowered
functions, including source definitions outside the requested entry closure.
`BatchStats` exposes module/compilation counts, defined function/entry counts,
actual run attempts and maximum statically admitted entry work.

## Documentation extraction and checking

Only actual lexer Comment tokens starting `///` or `//!` contribute doc lines.
Strings and ordinary comments containing fence-looking text do not become tests.
Contiguous doc groups support backtick or tilde fences with three or more marks;
non-Vesper fenced blocks are ignored as prose. Vesper metadata is strict:
`vesper`, `vesper,entry=name` or `vesper,compile_fail(E0302)`. Unknown flags such
as no_run/ignore, malformed diagnostic codes and unterminated Vesper fences fail
explicitly. A file without Vesper examples fails instead of reporting a passing
empty suite.

Body snippets are wrapped in Unit-returning no-argument functions. Declaration
snippets use `entry=name`, or `main` by default. Positive declaration names must
be unique across the merged package; collisions fail checking. Snippets can use
already checked package declarations. Declaration-only negative examples need no
entry. Selected positive entries must originate in their own snippet, checked
both independently and after merging; a base-package main cannot substitute for
an example. Private body wrapper names avoid all base functions and authored
snippet identifiers. These constraints are explicit supported syntax, not skips.

Before processing examples, the base file undergoes independent full parsing and
checking. Base diagnostics prevent the suite from passing. Each example then
gets a separate full base-plus-snippet parse/check, including actual type,
effect, modal and Place-IR PCG validation. compile_fail succeeds only if at least
one diagnostic exists, **all** actual codes equal the requested code, and none
belongs to original base source or synthetic wrapper headers/trailers. Only
an authored code-segment diagnostic origin counts. For errors attached only to
a generated wrapper scope, use a declaration fence with an authored function
signature. Actual module grammar also
classifies contextual trait/model/pub declarations, avoiding artificial block
parser errors for valid top-level examples. Parser errors, unrelated codes and a base
error with even the requested code cannot satisfy a negative example.

Checked positive fragments are merged, parsed and checked again. All runnable
entries then compile through one NativeBatch; each executes twice by default.
Repeated result strings must agree. Unsupported native execution marks positives
failed with an explanation. No separate JIT or linker is invoked per snippet.
Diagnostics map generated offsets back through stripped doc prefixes to original
UTF-8 byte offsets and one-based source line/character-column positions.

Source guards run **before recursive parsing**: file/snippet/merged/cumulative
check bytes, lexical token/declaration counts, delimiter nesting and an
unfinished outer-segment token weight. The last guard also rejects long unary,
assignment and operator spines hidden inside nested groups. After parsing but
**before checking**, a bounded nominal-type expansion walk unfolds every
referenced field independently, so compact doubling ADT DAGs cannot trigger
unbounded `World::mode` expansion. Recursive user nominal types are explicitly
unsupported: a name-only cycle shortcut would not bound the checker's
argument-mode-keyed exploration. Simple linear types still reach actual PCG
checking, as the negative tests demonstrate. No source/core parser/checker file
is modified by this lane.

DocTestLimits defaults / hard ceilings: source 128/512KiB, snippet 16/64KiB,
merged 512KiB/2MiB, total checked bytes 8/32MiB, tokens 8192/16384, nesting 32,
unfinished segment weight 128, declarations 128/256, examples 64/128, repeated
runs 2/100, nominal expansion 8192/32768 and nominal expansion depth 48. Native
limits additionally apply. Guards bound admitted structures and reject some
otherwise valid language programs; they do not establish hard compiler or
execution deadlines. Host allocation failure remains ordinary Rust behavior.

## CLI and integration

The adapter is `doctest_commands::run(Vec<String>) -> Result<(), String>` and
supports exactly:

```text
vesper test --doc FILE [--json]
```

It reads at most the source cap plus one byte, rejects nonregular paths before
opening, uses a nonblocking Unix open to handle a file-to-FIFO race, rechecks the
opened handle, validates
UTF-8, reports each example's source location and returns nonzero on any failure.
JSON includes file, overall passed flag, base diagnostics, per-example expected
code/entry/result/diagnostics and actual shared-module statistics. Guard errors
also emit a structured failed JSON result. No unsupported case becomes a skip.

Coordinator-owned registration still required:

- `src/lib.rs`: `pub mod doctest;`
- `src/main.rs`: `mod doctest_commands;` and dispatch command `test` with its
  remaining arguments to `doctest_commands::run` before legacy command matching.
- Help and acceptance workflow wiring as appropriate.

Temporary local lib/main registration was used for integration tests and removed
from the commit. No root dependency, manifest, lockfile or workflow changes are
required. The root benchmark adapter can use the concrete NativeBatch API above.

## Evidence and boundaries

Validation uses private `/tmp/vesper-target-tooling41-doctest`, one build job,
zero development/test debug information and incremental caches disabled.

Focused checks:

```sh
cargo test -p vesper --test native_batch --test doctest_cli --locked --offline
cargo test -p vesper --lib native::batch --locked --offline
cargo clippy -p vesper --lib --bin vesper --test native_batch --test doctest_cli --locked --offline -- -D warnings
```

Evidence includes one actual module with two entries/shared helper defined once,
40 repeated calls compared against the existing evaluator, signed/u8/u64/f64/
bool/Unit ABI results surviving module drop, run-count exhaustion, rejected
synthetic checked-World body-less imports, recursion, indirect/aggregate/argument
entry rejection, IR/call-expansion budgets, malformed/cyclic CFGs, exact linear
leak negatives, wrong/mixed diagnostic codes, unrelated base errors, actual
comment extraction, CLI JSON/source positions, selected-entry provenance, contextual declaration
classification, synthetic-name/error exclusion and nominal-DAG/source guards.
The final focused suite passes **14 integration tests plus one new CFG unit**;
all **68 existing backends tests** also pass. Strict focused Clippy and exclusive
file formatting checks pass. Only Linux native execution was run locally.

No general SimPulse I/O, deterministic virtual clock, 64KiB source arena,
all-language execution isolation, PMU/timing result, sub-100ms claim, generated
Lean certificate or cross-platform/native-device evidence is provided here.
