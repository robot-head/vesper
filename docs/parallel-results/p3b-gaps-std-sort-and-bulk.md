# P3b gaps lane 11: std sort and bulk sequence operations

Claim #1096. Base `9305b24` (origin/main with the wave plan, #1098).
Prompt: [11-std-sort-and-bulk.md](../agent-prompts/p3b-gaps/11-std-sort-and-bulk.md).

**Result.** `@vesper-std` has a sort package, saturating arithmetic,
`all_zero` and `ct_eq`; `ChunkSeq` has `resize`, ranged `fill`,
`extend_from_within` and `extend_reversed`; and `find_not_in` on x86-64
without SSSE3 skips runs of a repeated class byte 16 bytes per step. The
Aegis reserved-field repro drops from 12,071 to 622 instructions per call,
the reversed 48-byte copy from 2,172 to 387, and an 8,704-element zeroed
buffer from 5.0 us to 1.2 us; Aegis's SNP parse with both workarounds
undone goes from 2.09x to 1.73x Rust's instructions. The std sorts take 0.58-0.62x the
instructions of Kinetic's heap sorts and 2.7-4.3x those of Rust's
`sort_unstable`/`sort`.

## What changed

### `ChunkSeq` and `Slice` builtins (§13)

| Builtin | On | Semantics |
|---|---|---|
| `s.resize(n, v)` | `ChunkSeq[T]`, copy `T` | shorten to `n`, or append copies of `v` up to `n`; one growth; total |
| `s.fill(lo, hi, v)`, `s.try_fill(lo, hi, v)` | sequence or slice, copy `T` | fill `s[lo..hi]`; one bounds check; checked form raises `IndexOutOfBounds`, total form is `false`; nothing written on failure |
| `s.extend_from_within(lo, hi)`, `try_extend_from_within` | `ChunkSeq[T]`, copy `T` | append a copy of `s[lo..hi]`, reading through the grown buffer; one growth, one check |
| `s.extend_reversed(in src)` | `ChunkSeq[T]`, copy `T` | append `src` last first; one growth; total; `src` may not view `s` (`E0301`) |

`fill` with three operands is a new `SeqOp::FillRange`; the one-operand
`fill(v)` is unchanged (`SeqOp::with_arity` picks the form in the checker,
Place IR lowering and the Lean extractor). The evaluator is the reference;
native lowering is in `src/seq/native.rs` (`resize`, `extend_from_within`,
`extend_reversed`) and `src/seq/native_bytes.rs` (`fill`). Byte
`extend_reversed` moves eight bytes per step through `bswap`. Element fills
use `memset` for bytes, and optimized code also uses one `memset` when a
scalar fill value is all zero bits. Lean extraction refuses the new forms,
as it does the other block operations.

### `find_not_in` without SSSE3

`find_not_in` looks its class up with `pshufb`, so on the x86-64 baseline
(which ports are built for: the harness passes `--target`) it ran one byte
per iteration. The fallback (`find_not_in_runs`) still tests a byte at a
time, but a byte that is in the class starts a run: each following 16-byte
step equal to it is skipped with one SSE2 compare and `pmovmskb`, and the
scan resumes at the first byte that differs. Under 16 bytes left it
finishes byte by byte, so it never reads past the view. Zero-filled
reserved fields, padding and indentation are such runs. Executables built
without `--target` use the host's features and are unchanged, as is
AArch64 (which has a shuffle) and unoptimized code (the Stage 1 corpus).

### `@vesper-std/sort` (new package)

| Module | Items |
|---|---|
| `unstable` | `sort` (introsort: median-of-three quicksort, insertion sort at 16 or fewer, heapsort after `2 log2 n` partitions; no allocation), `is_sorted`, `lt`, `swap_at`, `insertion`, `heapsort` |
| `stable` | `stable_sort`: insertion-sorted runs of 16, bottom-up merges between the slice and one scratch sequence, leftover runs copied as blocks |
| `keyed` | `sort_by_key(in keys, inout order: Slice[u32])` (indices by key, ties by index: equal to a stable sort of the identity), `sort_pairs(inout keys, inout vals)` (stable by key) |
| `floats` | `sort_f64`, `stable_sort_f64`, `is_sorted_f64` under `f64::total_cmp` via the named model `TotalOrderF64`; `sort_f32_bits` over `f32` bit patterns in a `Slice[u32]` (keys mapped in place, sorted, mapped back) |

Every sort takes an `inout Slice[T]` of a copy `T: Ord`, reads with
`try_get`, and is total.

### Other std helpers

- `@vesper-std/core::saturating`: `saturating_add_k`, `saturating_sub_k`,
  `saturating_mul_k` for `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`,
  `i64` (Vesper functions; `u64`/`i64` multiply through `mul_wide`).
- `@vesper-std/bytes::ct`: `all_zero(in s)` (`find_not_in(1, 0, false)`)
  and `ct_eq(in a, in b)`, an XOR/OR fold over 8-byte words with no
  data-dependent branch or early exit.
- `@vesper-std/bytes` `grow` (used by the `push_*` appends) is one `resize`.

### Manuscript (flagged)

§13 gains a "Bulk sequence operations" item (the four builtins and their
checks), the `find_not_in` paragraph notes the run-skipping baseline path,
and the std catalog lists `@vesper-std/sort`, `core::saturating` and
`bytes::ct`.

## Tests

- `tests/seq_bulk.rs`: each builtin in the evaluator, the JIT (optimize on
  and off) and an executable against a Rust model: `resize` from the shared
  empty header, a full buffer and spare capacity for `u8`, `u32`, `u64` and
  a struct; `fill`/`try_fill` over every interval of a 19-element sequence
  and a slice argument, including the raising form; `extend_from_within`
  through growth; `extend_reversed` at every length 0..40 and start 0..8
  (word steps and tail) and for `u32`, `u64` and structs; the type errors
  (`E1522`, `E0301`, `E0207`, `E0200`, `E1525`, `E0401`).
- `tests/native_find_not_in.rs`: runs of class bytes broken at every
  position (outside byte, other class byte, high bytes, varied bytes) for
  lengths 0..69 and four classes; on x86-64 Linux the executable leg is
  built for the baseline triple, so it runs the new path. An object test
  checks the baseline build has SSE2 compares and no `swizzle`.
- `tests/std_sort.rs`: a driver package sorts 7 patterns (random, sorted,
  reversed, equal, few distinct, sawtooth, organ pipe) at 14 lengths up to
  4,096 for `u64`, `i64`, `u32`, `u8` (stable), a struct with derived
  `Ord`, `sort_by_key`, `sort_pairs`, `f64` (random bit patterns: NaNs of
  both signs, infinities, zeros) and `f32` bits, and folds them into a
  digest equal to Rust's `sort_unstable`, `sort`, `sort_by_key` and
  `total_cmp` on the same inputs, in the JIT (both settings), an
  executable and (8 lengths up to 40) the evaluator. Saturating arithmetic
  is checked against Rust on 12 edge values with both signs for every
  kind; `all_zero` and `ct_eq` on every single-byte difference up to 69
  bytes.
- Existing: `native_byte_classes`, `native_bytes_builtins`, `seq`.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changes. The new
native paths are new operations (no existing program reaches them) or
gated on `operands_only` (optimized code) and a missing `pshufb`. The
bootstrap-stage1 workflow on the branch gives the digest evidence (see
the PR).

## Measurements

Setup: cloud x86-64 container, 4 vCPU Intel Xeon @ 2.30 GHz, Linux 6.18,
Rust 1.97. "Before" is a release `vesper` at `9305b24`; "after" is this
branch. Programs are built with `vesper build --target
x86_64-unknown-linux-gnu` (the baseline ISA the port harness uses) and
counted with callgrind `Ir`, whole process. Valgrind replaces `memset`
with its own loop, so a zero fill is timed instead.

### Repros from the port reports

| Repro | Before | After |
|---|--:|--:|
| Aegis gap 1: `find_not_in(1, 0, false)` over 600 zero bytes, per call | 12,071 instr | **622** instr (host SSSE3 path: 860) |
| Aegis gap 2: reverse 48 bytes into a new sequence, per call (48 `push`es vs `extend_reversed`) | 2,172 instr | **387** instr |
| eBPF part 4: 8,704 zeroed `u64` (pushes vs `resize`), wall per buffer | 5.0 us | **1.2 us** |

### Sorts against Rust and Kinetic's heap sorts

One process sorts 100,000 random `u64` (or orders 100,000 `u32` indices by
them); instructions net of the same program with no sort. Kinetic's
functions are copied verbatim from `util.ves` on #1078. Wall clock is 20
sorts, median of 3, with a compiler build running beside it, so read it as
a direction only.

| Workload | Rust | std (this lane) | Kinetic heap sort | std / Rust | std / Kinetic |
|---|--:|--:|--:|--:|--:|
| `sort` (`sort_unstable`) | 15.1 M | 52.0 M | 83.9 M (`sort_u64`) | 3.44x | 0.62x |
| `stable_sort` (`sort`) | 20.9 M | 76.4 M | | 3.66x | |
| `sort_by_key` (index order) | 29.3 M | 79.1 M | 135.4 M | 2.70x | 0.58x |
| `sort_pairs` (stable by key) | 33.8 M | 144.6 M | 237.6 M | 4.28x | 0.61x |
| wall, `sort`, 20 x 100k | 24 ms | 156 ms | 244 ms | 6.5x | 0.64x |

The gap to Rust is compiler work, named below; Rust's sort is also
branchless (ipnsort), which shows more in wall clock than in instructions.

### Ports

- **Aegis** (merged on main, Vesper only): see the table below (local
  rewrite of `reader.ves` `zero` to one `all_zero` and `snp.ves`
  `signature_half` to one `extend_reversed`; not committed, per the lane
  rules).
- **Kinetic** (#1078) and **eBPF** (#1083) are open port-lane PRs, so their
  crates are not edited here; the measurements above use their own
  functions (Kinetic) and the reported allocation pattern (eBPF). The
  rewrites (replace `util.ves`'s `sort_u64`, `sort_by_key` and
  `sort_pairs` with `@vesper-std/sort`, and the interpreter's per-run
  pushes with `resize`) are handed to those lanes.
- Warp, Chorus and Prism H3 can drop their private saturating helpers for
  `core::saturating`; their reports name no measured cost for them.

Aegis `snp` workload (`examples/abba.rs window port snp 2000`, callgrind,
the 0-pass setup subtracted; Rust 17,242 instructions per pass):

| Arm | Compiler | Port sources | Instr./pass | vs Rust |
|---|---|---|--:|--:|
| A | `9305b24` | main | 36,003 | 2.09x |
| B | this branch | main | 36,423 | 2.11x |
| C | this branch | local rewrite | **29,759** | **1.73x** |

B is 1.2% above A: the committed `zero` already folds eight bytes at a time
and calls `find_not_in` only on its 0-7 byte tail, where the new path's
run check costs a few instructions per call. The rewrite (C) removes the
port's two workarounds and passes `port_differential` and `snp_v6`.

## Compiler gaps found

1. **Bounded generics resolve only six scalars.** `impl`, `model` and
   `T: Ord` obligations go through the Pact bridge, whose primitive list
   (`crates/vesper-pact-binding/src/lower.rs`) has `u8`, `u32`, `u64`,
   `i64`, `f64` and `bool`: `sort` of a `Slice[i32]`, `u16` or `f32`
   is `BRIDGE0004: unknown type`, and `impl Trait for i32` is
   `BRIDGE0001`. The std sorts therefore cover those six (plus `f32` as
   bits), tuples and structs. Adding the other kinds to the bridge would
   make every generic sort work for them.
2. **`a.cmp(b)` builds an `Ordering` in memory.** `match a.cmp(b) { Less =>
   true, _ => false }` on `u64` stores the tag into one of three stack
   slots and reloads it; the same sort with `a < b` takes 28% fewer
   instructions (73.0 M to 53.3 M on the benchmark above). Folding the
   builtin `cmp` and a match on its result into one compare would close
   most of the sort's gap to Rust.
3. **A `match` binding an `f64` with a `return` arm crashes codegen.**
   `let p: f64 = match s.try_get(i) { Option::Some(v) => v,
   Option::None => { return 0; } };` panics in Cranelift ("declared type
   of variable doesn't match type of value") in the JIT and in builds,
   with or without `optimize`. The std code avoids the form.
4. **`ConstTime` bodies cannot call sequence builtins** (`E0709`, "callee
   that cannot be resolved statically"), so `ct_eq` is constant-time by
   construction but does not carry `eff [ConstTime]`.
5. **Swap helpers stay calls and slice headers reload** in the partition
   and sift loops (lanes 3 and 4 of this wave).
6. **Function values are moved by a call** (`E0305`/`E0312` for a
   `key: fn(T) -> K` parameter called in a loop), so `sort_by_key` takes a
   parallel key slice rather than a key function.

## Decisions

- `fill(lo, hi, v)` overloads `fill` by operand count instead of a new name;
  `try_fill` is the total form.
- Sorts are generic over the builtin `Ord`, with float total orders as a
  named model (`f64`) or a key transform (`f32` bits), rather than a new
  sort trait: a trait could only be implemented for the bridge's six
  scalars and would not cover structs.
- `sort_by_key` breaks ties by index, so its result is unique and equals a
  stable sort; `sort_pairs` is the stable sort of two parallel slices.
- Saturating arithmetic is library code (a builtin would touch every
  backend's integer lowering, outside this lane).
