# Rust exit: FFI lowering and the numeric, control and aggregate backend

Claim: #1830. Scope (the Rust-exit handoff's "chunk 4", about 18k lines):
`src/ffi_lowering/`, `src/tiled/`, `src/vector/`, `src/native_vector.rs`,
`src/native_handlers/`, `src/native_session.rs` and
`src/native_aggregates.rs`. It continues the SH4 lanes that own them:
lane 8 (#1282, aggregates), lane 10 (#1284, control and sessions), lane 11
(#1285, vectors, tiles, floats, regions, atomics) and lane 13 (#1287, FFI).

All of these are `compiler` rows of the census: the Rust goes in the Stage 2
retirement PR, not module by module. So this lane's job is that nothing in
them is left without a Vesper port by then, and that the hybrid's swaps of
these families decline nothing.

## Results

| Change | Differential | Rust lines | PR |
|---|---|--:|---|
| `src/native_handlers` deleted (owner's choice, 2026-11-24) | n/a (dead code: no command reached it) | -2,293 now | #1836 |
| `scalar_enums` decisions (#1330) ported to `@vesper/codegen-aggregates` | 0 of 354 corpus worlds differ (52,690 graphs; 15,563 enum values, 116 locals, 11,064 calls and 271 returns kept as words) | 0 now (goes at the retirement) | #1854 |
| `scalar_enums` graph rewrite ported (`scalar_enum_rewrite`) | 0 of 54,236 graphs differ, 2,347 of them rewritten (`Debug`-equal `FunctionIr`) | 0 now (goes at the retirement) | this PR |
| `ffi_lowering/cpu.rs` machine-code tables ported (`@vesper/ffi-lowering` `cpu.ves`) | 0 of 72 symbol and target pairs differ (every function, unknown and plain symbols, x86_64, AArch64, RISC-V) | 0 now (goes at the retirement) | this PR |

### `native_handlers`: deleted

`src/native_handlers` (wave nine lane 9, #586) was a second native backend,
`vesper.native.handler-abi/1`, that compiled each handler and function value
once. No CLI command reached it; only `tests/native_handlers.rs` did. The
live handler lowering is `src/native.rs`, and the hybrid already swaps it to
`@vesper/codegen-control-emit`. The owner chose deletion over a port.
`LOOP_RESUME_REFUSAL` moved to `vesper::native`. The Vesper port of its key
helpers (`codegen-control/src/keys.ves`) went too, since nothing else used
it. Its fixtures stay because other suites run them.

### Scalar enums in `@vesper/codegen-aggregates`

`native_aggregates::scalar_enums` (#1330, the G10 work of gap lane 1, #1498)
decides which small enum values (at most 4 words, every field one integer or
`bool` word, no `Box`) a graph keeps as word values instead of in memory. It
was the one decision of `native_aggregates.rs` with no Vesper port. The port
adds, per graph of the native optimize path:

- `enum_shape`: the shape of each value's and local's type, interned so
  that equal shapes compare by id (Rust compares `EnumShape`s by value);
- the banned locals (field paths, writes, place arguments, closures and
  handlers that name them), the definition and use rules, and the fixpoint
  that drops values and locals until both are stable;
- the "taken apart" test, and the word values: a constructor's field
  values, else fresh ids after the graph's largest.

The differential prints, for every graph, `enum vV W..` (each value kept as
words and its word values), `enumlocal L`, `enumcall vV` and `enumret vV`.
Rust's side (`aggregate_decisions::scalar_enum_decisions`) calls the real
`scalar_enums` as `Codegen::ir_graph_at` does for a top-level graph of an
optimized build with entry `main`. Two inputs come from the ABI plan, not
from this port, and travel as facts after the program
(`aggregate_decisions::scalar_enum_facts`): whether the graph's own result
travels as the words of its enum shape, and, per call instruction, whether
the callee returns them (`call_words`). `ScalarEnums` gained two fields
(`words`, `locals`) so the printer can read the decisions; no lowering
changed.

Cost (`decisions_cost`, `CHECK_HARNESS_SPEED=1`, release, every 4th
corpus world, best of 5): over 89 worlds, Rust's decisions and printer
6.01 s, the Vesper entry (decode, every aggregate decision including
scalar enums, text) 2.41 s, **0.40x**; the input's encoding is another
1.27 s. This is the decision pass alone, on a 4-core cloud host, not an
ABBA of the whole compile.

Found on the way: an inlined graph can repeat a call's destination value,
so the Vesper side prints each `enumcall`/`enumret` value once, as Rust's
map does.

### The scalar-enum rewrite

`rewrite_scalar_enums` makes the graph `scalar_enums` returns on the
`@vesper/place-ir` tables, from the decisions above (`scalar_plan`): a kept
constructor becomes the constants of its tag and of the words its variant
leaves unused, a read of a kept local reads its `$wordN` locals, a binding
binds the words (a unit when it binds none), block parameters and jump
arguments carry the words, and a `match` on a kept value tests its tag word
with literal arms. The tables only grow, as in bounds hoisting: the function
row points at a new header (the value table gains the fresh words and
bindings, the locals table the `$wordN` locals) and new blocks, and an
instruction that stays keeps its operand words. The entry writes the indices
of the wide-integer graphs it leaves to Rust, then the program
(`put_program`); `rewrite_matches_rust` decodes it and compares every other
graph with `aggregate_decisions::scalar_enum_rewrite` (the real
`scalar_enums(..).f`), `Debug` for `Debug`.

Cost (`rewrite_cost`, `CHECK_HARNESS_SPEED=1`, release, every 4th corpus
world, best of 5): over 90 worlds, Rust's `scalar_enums` of every planned
graph 1.69 s, the Vesper entry 1.56 s, **0.92x**. The Vesper time includes
what Rust does not do here: decoding the input, every graph's decisions and
writing the whole program back as words. This is the pass alone, on a
4-core cloud host, not an ABBA of the whole compile.

The emission side of scalar enums (`enum_words` and the split-local reads and
writes in `Codegen`) stays with the native walk (#1509), like the rest of
the instruction walk.

### CPU instruction tables

`cpu.ves` holds the machine code of `@vesper-std/cpu`'s functions
(`cpu::function`) and `code_for`'s architecture check, with its error
messages. `cpu_code_matches_rust` (tests/self_host_ffi_lowering.rs) runs
`code_for` on both sides for every function, an unknown `vstd_cpu_` symbol
and a plain symbol, on x86_64, AArch64 and RISC-V targets. The callee
declaration that uses it is still Rust's (`foreign_perform` keeps the
callee's declaration; the hook emits the rest).

## Hybrid swap coverage

The release `vesper-hybrid` against the release default compiler, `emit
--emit object` on every corpus file (`bootstrap/corpus`, `examples`,
`tests`; 554 files, 170 of which build an object), `VESPER_HYBRID_STATS=1`:

| Family | Requests from Vesper | Declined |
|---|--:|--:|
| `control` (closures, handlers, sessions, `dyn`) | 8,943 | 0 |
| `numeric` (vectors, `TiledSeq`, floats, regions, atomics) | 511 | 0 |
| `seq` | 622 | 0 |
| `aggregate` | 9,492 | 331 (the graph walk's `INSTRUCTION` requests, which the pilot hook hands back by design; the typed requests that follow are all answered) |
| `ffi` | 0 (no corpus file builds against a `.vsys` library; lane 13's `self_host_ffi_emission` covers it: 0 CLIF and 0 object differences) | 0 |

Objects: 2 of 170 differ, `examples/traits/shapes.ves` and
`tests/fixtures/wide_literals.ves`, both listed in
`OPTIMIZE_EQUAL_COST_TIES` (the AST optimizer's accepted tie-breaking
difference, unrelated to these families). Wall time over the 170 builds:
default 2.3 s, hybrid 2.0 s (0.85x; one run, not ABBA).

## What is still Rust in these modules

Non-test lines (`scripts/rust-census.py`'s count) and where the Vesper
side stands:

| File | Lines | Vesper | Left |
|---|--:|---|---|
| `src/ffi_lowering.rs`, `ffi_lowering/types.rs` | 1,830 + 840 | `@vesper/ffi-lowering` (plans, type mapping), `@vesper/ffi-rewrite` (the source rewrite) | |
| `ffi_lowering/native.rs` | 2,555 | `@vesper/ffi-emit` (every step swapped) | the walk's orchestration (native walk port, #1509) |
| `ffi_lowering/drops.rs` | 533 | `@vesper/ffi-drops` | |
| `ffi_lowering/buffers.rs` | 523 | none | the foreign-memory check pass (§16.3 pinned buffers) |
| `ffi_lowering/cpu.rs` | 99 | `@vesper/ffi-lowering` `cpu.ves` (0 diffs) | the call that picks it in `foreign_perform` |
| `ffi_lowering/eval_host.rs` | 596 | none | the evaluator's foreign-call host (`vesper run`) |
| `src/native_aggregates.rs` | 1,746 | `@vesper/codegen-aggregates` (decisions and the scalar-enum rewrite), `@vesper/codegen-base` and `@vesper/codegen-aggregates-emit` (emission) | `enum_words` and the split-local reads and writes (the native walk, #1509) |
| `src/native_session.rs` | 1,478 | `codegen-control/session.ves` (plans), `codegen-control-emit` (ops 38-42) | `copy_bytes`/`store_constructor`, the session object and entry symbol glue |
| `src/tiled.rs`, `tiled/native.rs`, `tiled/aot_runtime.rs` | 468 + 612 + 697 | `@vesper/codegen-numeric` (`tiled.ves`, `tiled_ops.ves`, AOT runtime) | |
| `tiled/check.rs`, `src/vector/check.rs` | 577 + 441 | `@vesper/check-regions-seqs` (`tiled.ves`, `vector.ves`) | |
| `tiled/eval.rs`, `vector/eval.rs` | 311 + 432 | none | the evaluator's tile and vector operations |
| `tiled/native_runtime.rs` | 377 | AOT runtime only | the JIT's Rust host symbols |
| `src/vector.rs` | 1,020 | `codegen-numeric/shapes.ves` (lane shapes) | the expression typing walk |
| `src/native_vector.rs` | 1,131 | `codegen-numeric/vector.ves` (every intrinsic) | slot fusion and access retyping (gap lane 3, #1500) |

## Checks

- `CODEGEN_AGGREGATES_STEP=1 cargo +1.100.0 test --locked --release -p vesper --test self_host_codegen_aggregates`: 2 passed; `decisions: 0 of 354 worlds differ, 0 not encodable, 52690 functions, 1983283 lines` (265 wide-integer graphs left to Rust, as before)
- `CODEGEN_AGGREGATES_STEP=1 ... --test self_host_codegen_aggregates rewrite_matches_rust`: `rewrite: 0 of 54236 graphs differ (2347 rewritten), 0 worlds not encodable`
- `cargo +1.100.0 test --locked --release -p vesper --test self_host_ffi_lowering cpu_code_matches_rust`: `cpu: 72 symbol and target pairs equal`
- `vesper check` and `vesper fmt --check` on `packages/vesper-codegen-aggregates`, `packages/vesper-codegen-control`, `packages/vesper-ffi-lowering`.
- `cargo +1.100.0 clippy --locked -p vesper --lib --test self_host_codegen_aggregates -- -D warnings`, `rustfmt +1.100.0 --edition 2024 --check` on changed files, `tools/bazel/sync_build_files.py --check`.
