# Port speed lane 1: empty sequences without a header

Claim #1021, PR #1033. Base `72209eb` (merged with `3c075a4`, which changes
no compiler source). Prompt: `docs/agent-prompts/port-speed/01-empty-seqs.md`.

## Representation

A shared static empty header, chosen over a header stored in its owner.

- `ChunkSeq::new` and `with_capacity(0)` return the address of one read-only
  header `{0, 0, null}`: the local data symbol `vesper_seq_empty` in an AOT
  object, a static in the JIT runtime (`native_runtime::empty_header()`).
  Nothing is allocated, as with Rust's `Vec::new`.
- A handle stays one word, so every reader (`len`, `try_get`, `as_slice`,
  bounds checks, hoisted headers, the export ABI's `SeqRef`, `seq_copy`)
  works unchanged: it sees length 0 and capacity 0. A header in its owner
  would have changed the size of every struct and element holding a
  sequence and every place that moves one.
- The empty header is recognized by its address, not its contents. A real
  header can also hold `{0, 0, null}`: `vesper_seq_new(0, 1)` is the scratch
  header of `with_string_mut`, which must still be freed.
- Nothing writes the empty header, so threads share it without a race and
  it lives in read-only data, where a stray store faults at once. `clear`,
  `truncate`, `extend` and `extend_ascii_lowercase` skip a length store that
  changes nothing (Codex review: these stores were plain writes of 0 into
  memory other threads read).

## What changed

- `vesper_seq_grow` returns the header to use afterwards. Growing the empty
  header makes a fresh one through `vesper_seq_make`, with room for at least
  `max(needed, 4, 32 / stride)` elements inline, which is what `new` used to
  reserve. Any other header, and every `String` value, grows in place and is
  returned unchanged.
- Write-back. Growth on a `ChunkSeq` receiver (`push`, `extend`, `reserve`,
  `extend_ascii_lowercase`) assigns the grown handle to the receiver's place,
  which the checker guarantees exists (E-code "updates its receiver, which
  must be a place"). The place comes from the IR argument
  (`Argument.place`) or the AST receiver. The assignment goes through the
  normal `Assign` lowering, so locals in registers, `inout` pointers
  (functions, closures, inlined calls) and field paths all get it. It runs
  only on the growth path.
- `push` and `extend` emit their tail (element store, new length) on both
  the fast path and the growth path (`seq_reserve_then`). A first version
  merged the handle through a block parameter, which cost a register move
  and a jump on every push (+2 instructions per push).
- `vesper_seq_make(0, _)` returns the empty header. `vesper_seq_free` of it
  is a no-op, in the JIT before its live-set check, so it is not a stray
  free. The live counter does not count it.
- `vesper_seq_new` (the export ABI constructor) is unchanged.

## Stage 1 guard

No edits to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`. The verified
subset has no `ChunkSeq`, so verified artifacts cannot change. The
bootstrap-stage1 workflow was dispatched on the PR branch; its result is on
the PR.

## Measurements

Setup: cloud container, 4 vCPU x86-64, Linux 6.18, release `vesper` built at
`72209eb` (main) and on this branch. Ports were built from the same sources
with `VESPER_PORT_COMPILER` set to each binary. Instructions are callgrind
`Ir`, whole process, with the commands of port-gaps-rescore.md. The main
arm reproduces that report's "after" port counts exactly, so its Rust counts
are reused for the ratios.

### Repros (instructions per call from a C driver, `vesper build --lib`)

Per-call cost is `(Ir(20000 calls) - Ir(10000 calls)) / 10000`.

| Repro | main | branch |
| --- | ---: | ---: |
| `make()` with 3 empty sequences, then `S_drop` | 700 | 107 |
| `new` + 0 pushes, dropped | 300 | 105 |
| `with_capacity(0)` + 0 pushes | 299 | 122 |
| `new` + 3 pushes | 342 | 378 |
| `new` + 4 pushes | 356 | 392 |
| `new` + 5 pushes (grows out of inline room) | 599 | 643 |
| `new` + 40 pushes | 1967 | 2035 |
| `with_capacity(3)` + 3 pushes | 341 | 340 |
| `with_capacity(40)` + 40 pushes | 859 | 858 |

A never-pushed sequence no longer calls `malloc` or `free`. What is left of
its cost is the call to `vesper_seq_free`, which returns at once. A sequence
from `new` that is pushed pays about 36 instructions more for its first
growth (`vesper_seq_grow` calls `vesper_seq_make`), as Rust's first push
calls `RawVec` growth. Later growth costs 3 instructions more (the
empty-header compare). Pushes that fit cost what they did.

### Ports (callgrind `Ir`, whole process)

| Port | Command | Rust | main | branch | Change | Ratio main | Ratio branch |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Quarry | `planner_abba window 20` | 19.36 M | 30.77 M | 28.11 M | -8.6% | 1.59x | **1.45x** |
| Prism | `abba measure 20` | 53.31 M | 65.01 M | 64.60 M | -0.6% | 1.22x | **1.21x** |
| Ignis | `abba measure 20` | 116.17 M | 165.60 M | 159.70 M | -3.6% | 1.43x | **1.37x** |
| Aura | `abba measure 40` | 362.86 M | 313.35 M | 313.32 M | 0.0% | 0.86x | 0.86x |

Wall clock, port against port (the ABBA drivers with main's port as arm A
and this branch's as arm B):

- Quarry `planner_abba driver`: median B/A regression -7.6% (B faster); the
  later epochs read 0.75x to 0.91x.
- Ignis `abba compare 15 100`: every workload's interval contains 1.0x except
  plan 256 (1.03x to 1.13x) and the others overlap it. The instruction count
  is the firmer number; the rescore found up to 30% wall-clock noise for one
  binary on this host.

## Tests

- `tests/native_empty_seq.rs` (new): evaluator, JIT (optimize on and off, no
  teardown, no stray frees) and executable agree on growth from empty
  through locals, fields, nested fields, `inout` parameters, inlined calls,
  closure `inout` parameters, `swap`/`replace`, sequences of sequences,
  `extend`, `reserve`, `extend_ascii_lowercase`, 500 rows of mostly empty
  sequences, and handler state. A C-ABI library checks that empty sequences
  count zero live allocations, that a by-value empty sequence returns the
  shared header, and that `inout` growth from C gives C the new handle.
- `tests/native_seq_runtime.rs`: three empty sequences now count 0, not 3.
- `crates/vesper-aura/src/ported/mod.rs`: `port_frees_every_allocation_it_makes`
  asserted that creating a recording backend allocates. It no longer does,
  so the test checks the counter's peak during the cycle instead. The leak
  assertion is unchanged.
- Passing locally: native_seq_runtime, native_seq_drop, native_seq_access,
  seq, strings, native_export_v2/v3/cli, native_bytes, native_bytes_builtins,
  native_simd_bytes, native_copy_within, native_drop_glue,
  native_hoist_headers, native_aggregates, native_inline, native_inline_reach,
  and the Quarry, Prism, Ignis (`--features vesper-port`) and Aura crate
  tests with this branch's compiler, including `port_memory`.

## What is left

- The final handler state is not dropped. This is on main too (valgrind shows
  the same 56-byte block lost), so the handler test compares values only.
- Dropping an empty sequence still calls `vesper_seq_free`. Inlining the
  address compare into drop glue would save the call (about 30 instructions
  per empty sequence).
- The first growth of a `new` sequence goes through two runtime calls. The
  generated growth path could call `vesper_seq_make` itself with a capacity
  known at compile time.
