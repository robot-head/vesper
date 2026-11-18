# Port gaps lane 2: native drops free what a dropped value owns

> Claim #985. Plan: [port gaps wave](../agent-prompts/port-gaps/README.md),
> lane 2. The [manuscript](../manuscript/README.md) is the sole
> specification; this lane needs no language change (§13 already says a
> dropped sequence or string frees what it owns).

Base: `main` at `07e7d4e` (after #983), with `origin/main` (`2e5e941`) and
PR #1001 (the #972 follow-up: `let`-bound struct locals) merged in.

## What changed

Native code (the JIT, executables and C-ABI libraries) freed only a function's
own `ChunkSeq` and `String` bindings, and only the sequence's own buffer.
Everything below now frees what it owns, with the optimizer on and off:

| Shape | Before | Now |
|---|---|---|
| A dropped `ChunkSeq` of sequences, strings, structs, `Option`s or boxes | its buffer only | its elements too, through one generated helper per element type (`__vesper_drop_elems_*`), recursive types included |
| `set` / `try_set` / `clear` / `truncate` on owning elements | old elements leaked | dropped; a `try_set` that misses drops the value it was given |
| A field replaced through `inout` (`h.s = ChunkSeq::with_capacity[u64](8)`), or a whole `inout` value, sequence or string replaced | old value leaked | freed before the store |
| A field of a local or `sink` parameter replaced | leaked | freed when a forward "maybe moved" analysis proves the place still owns its value |
| A by-value (`sink`) aggregate parameter dropped (Aura's dropped `Prepared` handle) | leaked | freed (`SeqDrop::Owned`) |
| A `let`-bound struct (#1001 frees its sequences, optimize path only) | strings and nested elements leaked; unoptimized path leaked all | everything its fields own, both paths |
| Unbound temporaries: `size(in row(3))`, a discarded `row(4);`, and the result of an inlined call | leaked | freed after their last borrow |
| Destructured bindings: `let (a, b) = pair();`, `let P { a, b } = make_p();` | leaked | owned and freed like plain bindings |

Design:

- **Drop glue** (`src/seq/native.rs`, `emit_drop_glue`): a sequence frees its
  elements (when [`fip::elem_needs_drop`]) and then its header; a `String`
  releases its buffer; a tuple, struct or enum drops the owning fields of its
  live variant. A zero handle is skipped (the executable runtime does not
  accept 0). Boxes are freed only as elements of a dropped sequence: every
  other Box drop is elaborated by `fip::elaborate_non_fip_drops`.
- **Plan** (`fip::seq_drop_plan`): adds `SeqDrop::Owned` for `sink` aggregate
  parameters, lets destructuring `let` names own what they bind (not under a
  `Box` pattern), lets `let`-bound strings be assigned (they have their own
  storage), and counts a by-value field read as a move only when the field
  may own something (`fip::may_need_drop`, which treats an unsubstituted
  generic as owning). `Owned` and `Fields` slots are emptied on a move by
  zeroing the value (the read copied it), so a later whole assignment still
  has storage to write to.
- **Replacing writes** (`fip::replace_drops`): `inout` parameters always own
  their place; a local or `sink` parameter does unless a closure or handler
  names it or, on some path, part of it was moved out since its last whole
  binding or assignment.
- **Temporaries** (`fip::temporary_drops`): a value from a call, a sequence
  constructor, a move out of a planned slot (or of an unplanned `let`
  string), or a block parameter fed only such values (an inlined call's
  result), whose every other use is an `in`/`inout` argument of a call in
  its block.

`@fip` functions and freestanding images are unchanged (no drops).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. Every new
drop fires only for a type that owns a sequence or string, and no program of
the Stage 1 lock corpus has one. The `bootstrap-stage1` workflow runs on the
PR branch; its digests are in the PR.

## Repros: live allocations left per call, and instructions

Each repro is a `pub fn` of `tests/native_drop_glue.rs`, built with
`vesper build --lib` (optimizer on) by the compiler at `main` and at this
branch, called 10 times from C; "left" is the change in the library's live
allocation counter per call. Instructions are `objdump -d` counts of the
exported function (drop code is new code, so counts grow).

| Repro | Left per call, main | Left, branch | Instructions, main | Branch |
|---|---:|---:|---:|---:|
| `nested.seq_of_seqs` | 11 | 0 | 105 | 121 |
| `nested.seq_of_strings` | 4 | 0 | 118 | 132 |
| `nested.seq_of_items` | 10 | 0 | 183 | 203 |
| `nested.three_levels` | 9 | 0 | 151 | 185 |
| `nested.seq_of_options` | 4 | 0 | 186 | 199 |
| `nested.popped_then_dropped` | 4 | 0 | 179 | 217 |
| `nested.set_clear_truncate` | 18 | 0 | 466 | 561 |
| `nested.sink_seq_of_seqs` | 2 | 0 | 105 | 133 |
| `replaced.field_replaced` (`inout` field) | 6 | 0 | 211 | 322 |
| `replaced.nested_field_replaced` | 7 | 0 | 169 | 353 |
| `replaced.sink_handle_dropped` | 2 | 0 | 64 | 117 |
| `inout_whole.seq_replaced` | 0 | 0 | 161 | 181 |
| `inout_whole.string_replaced` | 3 | 0 | 151 | 190 |
| `temporaries.discarded` | 10 | 0 | 192 | 228 |
| `temporaries.passed_in` | 11 | 0 | 235 | 276 |
| `temporaries.in_a_loop` | 19 | 0 | 76 | 85 |
| `destructured.tuple` | 4 | 0 | 110 | 142 |
| `destructured.record` | 3 | 0 | 110 | 159 |
| `destructured.in_a_loop` | 20 | 0 | 129 | 168 |
| `local_structs.dropped` | 12 | 0 | 253 | 357 |
| `local_structs.rebound` | 29 | 0 | 289 | 409 |
| `local_structs.field_replaced` | 11 | 0 | 318 | 449 |

The JIT checks the same programs in the test: no allocation is left for its
teardown and none is freed twice, optimizer on and off, against the
evaluator's results; executables return the same exit status.

## Ports

Measured on the cloud x86-64 host with `VESPER_PORT_COMPILER` set to the
compiler built at `main` or at this branch. No port change is committed
(lane 9 adopts them).

- **Quarry** (gap 2, `free_d`): with the three `free_d(ds)` calls removed
  (`entry.ves`, `paths.ves`), `tests/port_memory.rs` fails on `main`
  ("leaked over 200 cycles": 7,437 live against 37) and passes on this
  branch. `D` holds sequences of structs that hold sequences, so this needs
  both #1001 and the element drops here. The committed port passes either
  way, and the whole `vesper-quarry --features vesper-port` suite passes on
  this branch.
- **Aura** (gap 4): with `do_reconciler_prepare` rewritten to build one
  `Prepared` up front and replace it on the tree path (`p =
  prepared_for(..)`, the natural shape the port avoided),
  `port_frees_every_allocation_it_makes` fails on `main` (4,600 live
  allocations left) and passes on this branch. The whole `vesper-aura` suite
  passes on this branch with the committed port.
- Timing: no workaround is undone in a committed port, so the ports' ABBA
  benchmarks are not re-run here; lane 9 re-scores them with the rewrites.

## What is left

- Elements of a sequence inside a `Box`, and the payload of a dropped `Box`
  that holds a sequence or string (Box drops free boxes only).
- Partial moves: a value with a field moved out is not freed (the rest of it
  leaks); a write into a place that may have been moved from does not free.
- Locals named by a closure or a handler (alias or move), external
  captures, and match-arm bindings.
- `Wild` parts of a destructuring pattern (`let (a, _) = pair();` leaks the
  second part).
- Temporaries used across blocks, or by a builtin (`Seq`) operand or a
  projection, and a `Fail` between a temporary's definition and its last
  use.
- A `let`-bound struct whose sequence sits in an enum payload (#1001's
  eligibility rule, kept).
- Export ABI: handles owning sequences are still refused by value; the glue
  is what a by-value handle would need.
