# P3b gaps lane 1: f32 vector lanes and slice loads

Claim #1086. Base `9305b24` (origin/main). Prompt:
[01-f32-vectors.md](../agent-prompts/p3b-gaps/01-f32-vectors.md).

**Result.** `Vec[f32, N]` and `Mask[f32, N]` exist at 128, 256 and 512 bits
with every lane operation `f64` lanes have, plus `sqrt`, `abs`, `neg`,
`mul_add` and four lane conversions. Vectors load from and store to a
`Slice[T]` or `ChunkSeq[T]` of every lane type with one bounds check per
call. The Resonance repro kernel (16 lanes, `a * b + c`, samples kept as
`u32` bits) drops from **582 to 357 instructions per call** written with
`Vec[f32, 4]` (**345** with `Vec::mul_add`), a 1.6-1.7x cut. The vector form
is still far from the ~30 instructions of hand-written SSE because every
vector value lives in a stack slot (see "What is left").

## What changed

| Area | Change |
|---|---|
| Lane type | `Elem::F32` (`src/vector.rs`): 32-bit float lanes; native chunks are `f32x4`, masks `i32x4`; evaluator lanes are `Value::Float32` with IEEE binary32 arithmetic. |
| Float ops | `Vec::sqrt`, `Vec::abs`, `Vec::neg`, `Vec::mul_add(a, b, c)` for `f32` and `f64` lanes. `mul_add` rounds once everywhere: Cranelift emits `vfmadd`/`fmla` where the ISA has FMA and otherwise calls the correctly rounded `fmaf`/`fma` per lane; the evaluator uses Rust's `mul_add`. |
| Conversions | `Vec::to_bits` (`f32` to `u32`, `f64` to `u64`), `Vec::from_bits`, `Vec::widen_f64` (exact), `Vec::nearest_f32` (nearest even). The lane count stays; a conversion whose result is wider than its operand needs the result width's profile, and a result above 512 bits is `E1401`. |
| Slice I/O | `Vec::load[T, N](in s, at)`, `Vec::store(inout s, at, v)` (one check of `at + N <= len`, `Fail[RuntimeError]` `IndexOutOfBounds`, nothing written on failure) and `Vec::load_masked[T, N](in s, at)`, `Vec::store_masked(inout s, at, v)` (total: inactive lanes load zero and are not stored). `load_masked` on a `String` keeps its old meaning. |
| IR | Slice accesses mutate or read a sequence in place and can fail, so they lower to `Op::Seq { op: SeqOp::Vector(SliceIo) }` (an effect-ordered sequence operation, `src/seq/ir.rs`) rather than the total, pure `Op::Vector`. No Place IR type changed. |
| Native | `src/native_vector.rs`: f32 lanes, the new ops, `vector_slice_io` (whole-vector path; masked accesses fall back to a lane loop when the tail is short). Vector results are no longer zeroed before the chunks overwrite them. |
| Lean | Refuses `SeqOp::Vector` like the other block operations. |
| Spec | §14.3 of `docs/manuscript/content.typ` lists the implemented lanes, float ops, conversions and slice accesses (flagged in the PR). |

### Decision: `ChunkSeq[f32]` stride

A `ChunkSeq[f32]` stores each element in the low half of an eight-byte word
(`seq::elem_bytes` packs only `u8`/`bool` at 1 byte and `u32` at 4). Changing
that would change the export ABI and the verified compiler's sequence
layout, so this lane keeps it: `Vec::load[f32, N]` gathers the low halves of
`2N` words with one byte shuffle per chunk, and `Vec::store` spreads lanes
back with zeroed high halves. The fast path for float kernels is the one
Resonance already uses: keep samples as `u32` bits, `Vec::load[u32, N]` and
`Vec::from_bits`, which is one unaligned load per chunk.

## Tests

- `tests/vector_f32.rs`: checker rules; every f32 lane operation over 16
  edge patterns (zeros, subnormals, infinities, NaN, fused-only products)
  crossed with each other plus 64 pseudo-random vectors, checked against a
  Rust model of binary32; `mul_add` rounding once (`(1 + 2^-12)^2 - (1 +
  2^-11)` is `2^-24` fused, `0` unfused) for `f32` and `f64`; 256-bit f32
  vectors and the conversions; bit round trips.
- `tests/vector_slice_io.rs`: every lane type at 128, 256 and 512 bits:
  loads and stores through whole sequences, slice parameters and sub-slices,
  masked tails (short, empty, offset past the end), checked against a Rust
  model; unmasked out-of-bounds accesses raise `IndexOutOfBounds` (including
  `at` near `u64::MAX`) and a failed store writes nothing; typing errors.
- `tests/vector.rs`: the existing every-operation differential now covers
  `f32` at every shape, plus the float ops for `f32` and `f64`.

Every program runs in the evaluator (optimize on and off), the JIT
(optimize on and off), builds x86_64 and AArch64 objects and runs as a
`vesper build` executable. Local host: AVX-512, so the 256- and 512-bit
branches ran in the JIT and executables.

The kernel `k` in the repro is inlined into its caller in the optimized
executable (no call remains), so functions holding slice accesses still
inline.

## Stage 1 guard

No file under `src/verified_codegen/**`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`.
The new lowering is reached only from the reserved `Vec::` paths, which the
compiler packages never call. The bootstrap-stage1 workflow run on this
branch is linked from the PR.

## Measurements

Machine: cloud x86-64 container, 4 vCPU, AVX-512. Compiler: this branch's
`vesper` (the scalar kernel compiles the same on main: it uses no vector
code). `vesper build` defaults (optimize on). Instructions: callgrind `Ir`
of the whole process at 1,000 and 2,000 iterations of two kernel calls;
per call = difference / 2,000. A kernel that only does `s.set(0, a[0])`
costs 72 per call (call setup and slice headers); the table subtracts it.

| Kernel (16 lanes, `s[v] = a[v] * b[v] + c[v]`, `u32` bits) | Ir per call |
|---|---|
| Scalar loop (the report's repro) | 582 |
| 4 x `Vec[f32, 4]`: `load`, `from_bits`, `mul`, `add`, `to_bits`, `store` | 357 |
| Same with `Vec::mul_add` | 345 |

Not measured here:

- Resonance's `k_render` rewrite and its ABBA benchmark, and Kinetic and
  Tensor: their ports are on open PRs (#1082, #1078) or need the export
  ABI's `f32` (lane 2, #1087), so the re-measure belongs to the re-score
  lane (#1097) once they merge.

## What is left

1. **Vector values live in stack slots.** Every vector operation loads its
   operands from memory and stores its result to a new slot, so a
   `from_bits` or `to_bits` is a 16-byte copy and a chain of `n` operations
   does `2n` memory round trips. Keeping vectors in SSA registers (an
   `Op::Vector` home in registers, like the scalar locals of #983) would
   take the repro from 345 toward ~100.
2. **Slice headers are reloaded per access** (port-p3b-resonance gap 2,
   lane 3 #1088): each `Vec::load` re-reads `{handle, start, len}` and the
   data pointer.
3. **Conversions keep the lane count**, so `widen_f64` of a `Vec[f32, 4]`
   needs `Isa[Avx2]`. Half-width forms (`widen_low_f64`, `widen_high_f64`,
   a two-operand `nearest_f32`) would let Baseline128 code convert.
4. **No float-to-integer value conversions** (`Vec[f32, N]` to `i32`/`u32`
   values, truncating or saturating) and no `i32` lanes.
5. **`ChunkSeq[f32]` is strided** (eight bytes per element), see above.
