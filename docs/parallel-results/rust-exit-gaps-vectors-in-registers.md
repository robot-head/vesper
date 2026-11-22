# Rust-exit gap wave 1 lane 3: vectors in registers

Claim [#1500](https://github.com/robot-head/vesper/issues/1500). Prompt:
`docs/agent-prompts/rust-exit-gaps-1/03-vectors-in-registers.md` (plan PR
#1511). Base `main` at `66255e17` (2026-11-22).

## The gap

Item 2 of the Kinetic/Resonance report
([rust-exit-kinetic-resonance.md](rust-exit-kinetic-resonance.md)): a
`Vec[T, N]` value (§14) is an aggregate in a stack slot. Every vector
operation in `src/native_vector.rs` loads its operands' chunks from memory
and stores its result into a fresh slot, so the ported SIMD paths ran at
26-40x of Rust and vector code was slower than the port's own scalar loop.

The CLIF clean-up (`src/native_clif_opt.rs`, #1426) already has a mem2reg
(`promote_slots`) that accepts 128-bit vector cells. Vector slots never
qualified, for two reasons that show in the report's `acc4` repro:

1. **One type per offset.** A slice load stores the loaded chunk as `i8x16`,
   and the operation that reads it loads `i32x4` (or `f32x4`) at the same
   offset. `promote_slots` needs one type per cell, so the slot stayed in
   memory.
2. **Copies move 8-byte words.** A `let` of a vector, an argument or a
   result is an aggregate copy (`copy_bytes` in `src/native.rs`): two `i64`
   loads and stores per 16 bytes. Those overlap the 128-bit cell, so the slot
   escaped again.

`acc4`'s loop on `main` used four 16-byte slots per iteration: the two
loaded vectors, the sum and the `let` copy of the sum, with 6 vector stores,
5 vector loads and 4 word moves between them.

## The fix

`native_vector::registers`, a CLIF pass on the native optimize path, runs
inside the clean-up just before `promote_slots` (one line in
`native_clif_opt::run`, after block merging and constant parameters, so a
slot address that reached a block through a constant parameter is direct by
then). On the slots that hold a vector (accessed by a 128-bit vector at a
16-byte boundary, address not escaping):

- a copy's pair of word moves at a 16-byte boundary (each word read only by
  its store) becomes one `i8x16` load and store, and so does a pair of zero
  words (`alloc`'s zeroing of a masked load's result);
- every 128-bit access becomes an `i8x16` access with a register `bitcast`
  to the lane type.

Fusing can make the other end of a copy a vector slot, so the fusing repeats
(up to four rounds, for chains of `let`s). Then `promote_slots` turns the
cells into SSA values, with block parameters for loop-carried vectors, and
the vectors live in registers. Cranelift spills one only where its register
allocator decides. A function without a vector access to a slot is not
touched, so every non-vector program compiles to the same bytes.

The pass sees every function the backend emits, whether the operation came
from the Rust lowering or the SH4 `numeric` hook (#1285): it rewrites the
CLIF after both. So the Vesper port in `packages/vesper-codegen-numeric`
needs no change and the hybrid differential is unaffected.

What stays in memory, by design: the lane-at-a-time slow path of a masked
load or store (it addresses single lanes of the buffer), a vector whose
address escapes (passed to a call that was not inlined, stored in a heap
aggregate), and `Vec::load_masked` on a `String`.

## Test

`tests/native_vector_registers.rs` reads every emitted function through the
observer and fails on any 128-bit load or store whose address is a stack
slot. Four programs, each also run on the evaluator and both JIT
configurations:

| Program | Shape |
|---|---|
| `acc4` | the report's repro: four-lane `u32` add with the sum bound by `let` |
| `tick` | Resonance's `v_tick4`: `f32` lanes from `u32` bits, splats, `div`/`mul`/`sub`, two stores per step |
| `ramp` | Resonance's `v_ramp4`: compares, masks, selects |
| `sums` | a loop-carried accumulator (vector phi) reduced after the loop, and an eight-lane vector (two chunks) |

With the pass turned off all four fail; with it they pass. `tests/vector`,
`vector_f32`, `vector_kernel`, `vector_slice_io`, `native_clif_opt` and
`tiled` pass unchanged.

## Measurements

`cargo build --release -p vesper-resonance --features vesper-port --example
abba`, cloud Linux x86_64 VM (4 vCPUs), `ABBA_WORKLOADS=simd`, 5 epochs,
before = `main` at `66255e17` on the same host:

| Path (`ABBA_SIMD_PATH`) | Before | After |
|---|--:|--:|
| widest (AVX in Rust; 4-lane vectors in the port, gap 6) | 40.0x (36.7-50.1) | **5.0x (4.6-6.2)** |
| `sse2` | 26.2x (23.5-27.0) | **4.9x (4.2-5.0)** |
| `scalar` | 7.0x (6.8-7.0) | 7.0x (6.3-8.1) |

The port's widest path is now faster than its scalar path (21 ms against
29 ms per window), where it was four times slower. The rest of the gap is
not vectors. `svf_tick` is one call across the export ABI per sample (gap 7,
lane 5, #1502), and the inlined dispatcher is one large function whose slice
headers and failure-status pointer are reloaded from regalloc spills (#1426).
The scalar path's 7x is the same.

Kinetic has no vector code in its port. Resonance's graph sums are scalar
in the port (lane 9 chose scalar because vectors were slower); they can now
use vectors, which is a port change for SH6 lane 9 (#1404).

## Gap 6: `Isa::supported`

`Isa::supported[P]()` already resolves at compile time: native code lowers
it to a constant from the object's target flags
(`crate::vector::isa_enabled`). The report's gap 6 is the other half:
§14.2 describes a runtime dispatcher that probes CPUID or HWCAP and runs code
compiled for each profile, and the backend does not have one. With one
128-bit Cranelift vector type per chunk (Cranelift 0.125 has no 256-bit
types), an `Isa[Avx2]` path compiled for the baseline runs the same SSE
instructions as the 4-lane path. A runtime check alone would not make it
faster, and the flip gate builds ports with `--target-features` (#1454),
which turns the wide paths on at compile time. Per-profile multiversioning is
left (see below).

## Stage 1 guard

The change touches `src/native_vector.rs`, one line of
`src/native_clif_opt.rs` and one re-export in `src/native.rs`. None of
`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/stage1*` or `packages/vesper-compiler/**` changed. The pass runs
only on the native optimize path, and `bootstrap/stage1.rs` builds its
objects with `optimize: false` (`native::object_with_options(.., false)`),
so Stage 1's output cannot change. The pass also returns before changing
anything when a function has no vector access to a stack slot, and the
Stage 1 compiler sources have no `Vec[T, N]` code.

## What is left

- Vectors passed to a function that is not inlined still go through memory:
  aggregates cross calls by pointer. A by-value vector ABI (one `i8x16` per
  chunk in registers) is the next step, with lane 1's results-in-registers
  work (#1498).
- Runtime `Isa` dispatch (§14.2 multiversioning): compile each `Isa[P]`
  function once per profile and pick one by CPUID/HWCAP at the first call.
  Worth it once a profile changes the code (256-bit types or AVX-512
  masks).
- Resonance graph sums on vectors (SH6 lane 9) and warp-shard's SIMD CID
  lookup (census: "source vectors").
