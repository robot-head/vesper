# Port gaps lane 1: the native sequence runtime

Claim #984. Base `07e7d4e` (origin/main, 2026-11-18). Prompt:
`docs/agent-prompts/port-gaps/01-seq-runtime.md`.

## What the gap really was

The port reports blamed a thread-local `HashMap` for the sequence runtime's
share. That map exists only in the JIT runtime (`src/seq/native_runtime.rs`).
Ports are `vesper build --lib` objects, which use the generated AOT runtime
(`src/seq/aot_runtime.rs`), and that runtime has no map. It pays for two
`malloc`/`free` pairs per sequence that holds data (header, then a separate
buffer on the first push), plus one atomic counter update per allocation.
Callgrind of the Quarry repro on main makes this clear. Per empty sequence,
`malloc` costs 45 instructions, `free` with `_int_free` 82, and the runtime
functions 59.

## What changed

- **One allocation per small sequence.** Generated code now calls a new
  `vesper_seq_make(cap, stride)`. It puts the header and room for `cap`
  elements in one `malloc` block (`data == header + 24`) when the elements
  fit in `INLINE_BYTES` (64 KiB). `ChunkSeq::new` reserves 32 bytes inline
  (4 `u64`s, 32 bytes), so its first pushes allocate nothing. Growth past
  the inline room moves the elements to a separate buffer and leaves the
  inline bytes unused until the header is freed.
- **No bookkeeping to tell the two apart.** A separate buffer starts
  `BUFFER_PREFIX` (8) bytes into its own `malloc` block. So no other live
  object (a header, or a `String` value anywhere) can end exactly where a
  separate buffer starts, and `cap != 0 && data == header + 24` means "inline"
  without a lookup. Capacity 0 with data still means borrowed bytes.
- **`vesper_seq_new` keeps its export ABI meaning.** It still makes a bare
  header with a separate buffer, because `vesper_port_abi::with_string_owned`
  takes over that buffer. Every `vesper_seq_*` symbol is unchanged.
- **Growth copies with `memcpy`.** Growth out of inline or borrowed bytes
  copied byte by byte before. It now calls `memcpy`, and executables import
  `memcpy` and `free` too.
- **JIT runtime: the same layout, on the C allocator.** The JIT now calls
  `malloc`/`realloc`/`free` like executables do, and `live_allocations()`
  reads a thread-local counter. The JIT keeps its sets of live headers and
  string buffers in every build. `finish()` frees a sequence or string that
  a JIT entry returns (it is copied out first), and `stray_frees()` catches
  double frees. An earlier revision kept the sets only under
  `debug_assertions`, which leaked returned values in release JIT runs
  (Codex review). The ports never use the JIT, so this costs them nothing.
- **`s.reserve(n)` on `ChunkSeq` (§13, manuscript change flagged).** It is a
  capacity hint only. It changes no element or length and raises nothing.
  Like any growth, it ends an executable with status 71 only when the
  allocation fails. It works in the evaluator (no-op), JIT/AOT (one
  `seq_reserve`) and Lean (identity). It mirrors `String::reserve`, which
  already existed.

## Stage 1 guard

No edits to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`. The verified
subset has no `ChunkSeq`, so verified artifacts cannot change. Cranelift
artifacts of corpus programs that use sequences change, which
`compare_outcome` reports as stale and not as a regression. The
bootstrap-stage1 workflow on the PR branch is the evidence.

## Measurements

Setup: cloud container, 4 vCPU x86-64, Linux 6.18, rustc stable, release
`vesper` binaries built from main (`07e7d4e`) and from this branch. Ports
were built with `VESPER_PORT_COMPILER` set to each binary. Instructions come
from `callgrind`. Wall clock uses each port's committed ABBA driver with 9
epochs.

### Repros (instructions per call, from a C driver, `vesper build --lib`)

Per-call cost is `(Ir(20000 calls) - Ir(10000 calls)) / 10000`.

| Repro | main | branch |
| --- | ---: | ---: |
| Quarry gap 1: `make()` with 3 empty sequences, then `S_drop` | 671 | 705 |
| `new` + 0 pushes, dropped | 288 | 301 |
| `new` + 3 pushes | 559 | 343 |
| `new` + 4 pushes | 573 | 357 |
| `new` + 5 pushes (grows out of inline room) | 977 | 600 |
| `new` + 40 pushes | 2162 | 1968 |
| `with_capacity(3)` + 3 pushes | 509 | 365 |
| `with_capacity(40)` + 40 pushes | 1027 | 883 |

A sequence that is never pushed still costs one `malloc`/`free` pair. That
case got slightly slower (+11 instructions per sequence), because
`vesper_seq_make` checks the byte count. The exact Quarry repro is that case.

### Ports

| Port | Measure | main | branch |
| --- | --- | ---: | ---: |
| Quarry | `planner_abba window 20` instructions (Rust 19.47 M) | 32.94 M (1.69x) | 31.30 M (1.61x) |
| Quarry | seq runtime inclusive (`vesper_seq_*`) | 8.08 M (24.5%) | about 6.0 M |
| Quarry | ABBA `driver` median B/A (2nd-8th order stat.) | 2.00x (1.83-2.07) | 1.93x (1.72-2.07) |
| Prism | `abba measure 20` instructions, whole run (Rust 53.31 M) | 84.26 M (1.58x) | 76.58 M (1.44x) |
| Prism | ABBA decode / route / encode / connection | 2.01x / 2.80x / 2.48x / 2.20x | 1.92x / 2.65x / 2.03x / 2.10x |
| Ignis | `abba measure 5` instructions, whole run (Rust 82.74 M) | 118.44 M (1.43x) | 115.14 M (1.39x) |
| Ignis | ABBA build 64/256, plan 64/256, lower 64/256 | 2.76 / 2.79 / 1.65 / 0.78 / 3.08 / 2.62x | 2.47 / 2.61 / 1.48 / 0.75 / 3.23 / 2.53x |
| Aura | `abba measure 20` instructions, port only | 184.12 M | 183.17 M (-0.5%) |
| Aura | ABBA port(main) vs port(branch), one-text / reorder / encode-audit | 1 | 0.95x / 1.01x / 0.99x |

The wall-clock intervals overlap for most workloads, and the instruction
counts are the firmer evidence. Prism's encode (2.48x to 2.03x, with
disjoint intervals) and Ignis's build and plan 64 gained the most. Aura's
Rust build no longer exists on main (#979), so Aura was compared port
against port. Its change is within noise. The Prism ABBA arm used an
earlier 512-byte inline limit, but its instruction count is identical to
the 64 KiB build (76.58 M).

## Tests

`tests/native_seq_runtime.rs` runs everything in the evaluator, the JIT
(optimizer on and off, with no teardown and no stray frees) and an
executable. It covers growth from inline room, from no room (48-byte
elements), and of a separate buffer, small and large `with_capacity`,
`reserve` and its arity errors, string growth from literals and `String::new`,
and a C-ABI library. The library test checks the live counter exactly (3 for
three empty sequences, 4 or 5 as they grow or exceed the inline limit) and
checks that it returns to its start after `S_drop` and 200 churn calls. The
existing `native_seq_drop`, `seq`, `strings`, `native_export_v2/v3` tests and
the port memory tests (`port_memory`, `kit_memory`, which covers
`with_string_owned`) cover the rest.

## What is left

- **Empty sequences still allocate a header.** Rust's `Vec::new()` allocates
  nothing. Closing this needs no header for an empty sequence: either the
  header inline in its owner (`{len, cap, data}` by value) or a shared empty
  header that growth replaces. Either way generated code has to write the
  new handle back to its place, which is a codegen change in
  `src/seq/native.rs`/`src/native.rs` outside this lane. A lock-free header
  cache was measured in C as an alternative and lost to glibc's tcache:
  26 ns against 12 ns per sequence.
- The atomic live counter costs one locked add per allocation and per free.
  It stays atomic because port tests call one library from several threads.
- Elements are still not freed with their sequence (lane 2, #985).
