# P3b gaps lane 6: fixed-size arrays and constant tables (#1091)

#1091 in two parts. Part 1 (#1115, base `9305b24`) added arrays and
constants. Part 2 (base `b8c6046`) adds array fields to ABI v3 mirrors and
measures the Warp and warp-tcp rewrites; it is described under
[Part 2](#part-2-array-fields-in-export-abi-v3-mirrors).

## What changed

- **`[T; N]` types and literals.** These are parsed in
  `crates/vesper-syntax/src/parser.rs`:
  - `N` is an integer literal below 2^16.
  - `[a, b, c]` lists the elements.
  - `[x; N]` repeats `x`. A literal or a name is cloned; any other
    expression is bound once to `$splat` and evaluated once.
  - An array is the homogeneous tuple `(T, ..., T)`, so it reuses the
    tuple's layout, copies, reads, patterns, struct fields, parameters and
    results on every backend.
  - `vesper fmt` keeps `[T; N]` on one line.
- **Checked indexing.** `a[i]` raises `Fail[RuntimeError]`
  `IndexOutOfBounds` unless the checker proves `0 <= i < N`. The checker
  accepts these proofs (`src/arrays/check.rs`):
  - a constant index;
  - the existing range facts (dominating guards, `x % m`);
  - `i + c` and `i - c` of a proved index;
  - a mask `x & m` of any `x`;
  - an enclosing `for i in lo..hi` whose bounds are constants or `a.len()`.
    For-loop bindings now carry their range (`src/loops/check.rs`).
- **Writes and `len`.** `a[i] = v`, `a[i] op= v` and `a.set(i, v)` work on
  mutable places: `let mut`, parameters and field paths.
  - Writes need copy elements (`E0311`).
  - A write to anything that is not such a place is `E1540`.
  - Compound writes use the element's checked arithmetic.
  - `a.len()` is the constant `N`.
  - After checking, `crate::arrays::lower_world` turns each write into
    `Op::Write { op: "[]op" }` on the array's place. Place IR is unchanged.
- **Module constants `const NAME: T = e;`**
  - The parser emits a parameterless function `const$NAME`. It also turns
    each use of `NAME` in the same module into a call of that function.
    This runs before a package renames items, so constants in non-root
    package modules work too.
  - Initializers must be constant expressions: literals, constructors,
    tuples, structs, arrays, operators, `let` blocks and other constants
    (`E1541`).
  - A constant cannot share a name with a type, constructor or effect
    (`E1542`).
  - A use is a value, not a place (`E0300`).
  - A constant aggregate whose value is a tree of literals becomes a
    `$table$` call that no pass inlines.
  - Native code (`src/native_arrays.rs`) emits that call as read-only data,
    8-byte aligned. When every use only reads an element or a field, the
    code reads it in place. Otherwise it first copies the data to a stack
    slot.
- **Native code.**
  - An array element write is one unsigned bounds check, then a store at
    `&a + i * 8`.
  - The check raises where the checker recorded a possible failure and
    traps where the index is proved in bounds.
  - The `(index, value)` pair is never built.
  - Index reads of a place read the place itself; they do not copy the
    whole array first.
- **Manuscript, flagged in the PR.** In §2.2, `ConstDecl`, `ArrayType` and
  `ArrayExpr` are added to the grammar, plus a paragraph on
  *Fixed-size arrays and module constants* that also lists E1540-E1542.

## Tests

- `tests/arrays.rs` has 12 tests and `tests/const_items.rs` has 6.
- They are differential: the evaluator and the JIT each run with
  optimization on and off, x86_64 and AArch64 objects are built, and a
  linked executable runs.
- They cover:
  - copies;
  - nested arrays;
  - struct fields;
  - returned arrays;
  - bounds failures;
  - compound overflow;
  - totality, including a mask of a call;
  - mutable-place diagnostics;
  - closures and handlers;
  - a 528-entry scratch DP;
  - a 256-entry table whose bytes are found in the object;
  - chained constants;
  - constants in a non-root package module;
  - the diagnostics.
- These existing suites still pass:
  - `vesper-syntax` and `vesper-fmt`;
  - `loops`, `native_aggregates`, `forge_tuples` and `backends`;
  - `native_inline`, `native_block_param_homes`, `native_seq_access`,
    `native_handlers` and `packages`.

## Repro from the reports

`fn f() -> [u64; 4] { [0; 4] }` gave `E0002 expected RBracket, found
Semi`. It now checks and runs on every backend (bench gap 2). Chorus gap 3
(no module constants) is covered by `const`.

## Port measurements (local rewrites, not committed)

All counts are callgrind `Ir` on this Linux x86_64 container. Both port
builds use this branch's compiler. "Before" is the committed port, and
"after" is the port with the local rewrite. The rewrite diff is kept
with the project's files, not in the repository (owner rule). Every after-build gives
the same output or digest as its before-build.

### Bench `add_rank`: estimator workload

The local rewrite changes only `accepts`:

- the `ChunkSeq[u64]` distribution becomes a local
  `let mut dist: [u64; 1057] = [0; 1057];`;
- the subset-sum loop is inlined, with a guard
  `if t < 1057 && sum < 1057` that proves both indices;
- the tail sum reads the array.

| Workload (`BENCH_ABBA_ONLY`) | Rust | Port before | Port after | Ratio before -> after |
|---|---:|---:|---:|---|
| n = 8, 1,000 batches | 0.883G | 0.681G | 0.657G | 0.77x -> 0.74x |
| n = 16, 1,000 batches | 7.90G | 12.76G | 10.90G | 1.62x -> 1.38x |
| n = 32, 100 batches | 16.56G | 32.82G | 27.36G | 1.98x -> 1.65x |

The rest of the gap is the emulated 128-bit locations (`W`, bench gap 1).
There is also one bounds check per access, because the guard is not
hoisted out of the loop.

### Prism H3: constant Huffman tables

The local rewrite:

- generates `const CODES: [u64; 257]`, `const NIB: [u64; 4096]` and
  `const INFO: [u64; 256]` with the same construction as `huff_tables`;
- reads them in `huff_encoded_len`, `huff_encode` and `huff_decode` as
  `CODES[b & 255]`, `NIB[(node * 16 + nibble) & 4095]` and
  `INFO[node & 255]`. These replace `word(in h.nib.as_slice(), ...)`
  reads, each of which was a checked `try_get`.

Counts are from `scripts/bench.sh` with `CALLGRIND=1`, over 5 batches:

| Workload | Rust | Port before | Port after | Ratio before -> after |
|---|---:|---:|---:|---|
| decode | 175.61M | 175.19M | 160.21M | 1.00x -> 0.91x |
| encode | 63.67M | 79.13M | 73.05M | 1.24x -> 1.15x |

### Warp and warp-tcp

See part 2 below.

## Stage 1 guard

- This PR does not touch `src/verified_codegen/**`, `src/ir.rs`,
  `ir_opt::optimize`, `lean/**`, `bootstrap/**` or
  `packages/vesper-compiler/**`.
- The bootstrap-stage1 workflow runs on this branch, and its digests must
  equal `bootstrap/stage1.lock`. The PR gives the run.

## Remaining gaps

- `in a[lo..hi]` and `inout a[..]` are not supported as `Slice[T]` views.
  The two representations differ in stride:
  - a native slice is `{handle, start, len}` over a seq header, with
    elements packed at `elem_bytes` (1 for `u8`);
  - an array stores 8-byte words, like a tuple.

  A view needs either packed array storage or a copy-in/copy-out
  boundary.
- Packed element storage: `[u8; N]` takes `8 * N` bytes, in memory and in
  an export mirror.
- Writes to a nested element: `a[i][j] = v` and `a[i].f = v`.
- Importing a constant across modules or packages with `use`, and
  constant names as lengths (`[0; LEN]`).
- `f32` and enum constants are not placed in read-only data. They are
  built at each use instead.
- Guards are not hoisted: `add_rank` still pays a compare per access.
- The parser bounds nested array types and literals to 2^16 elements in
  all. A type alias or a large constant repeated by name (`[BIG; 65536]`)
  is not bounded until checking.
- Array element writes of function values are refused (E1540): native
  call-target tracking does not follow dynamic element stores.

## Part 2: array fields in export ABI v3 mirrors

### What changed

- **Bindings** (`src/native_export/binding.rs`, `binding_v2.rs`). The
  interface cannot tell `[u8; 2]` from `(u8, u8)`, because an array is the
  homogeneous tuple. So the binding names a homogeneous tuple of more than
  eight elements, the longest Rust tuple with an `Abi` impl, as the Rust
  array `[T; N]`. A shorter one stays a Rust tuple, so every committed
  binding is unchanged (none of the 50 has a longer homogeneous tuple).
- **Mirror cells.** An array field of a copy struct is its element cells
  in order, so the struct stays `#[repr(C)]` and is lent in place:
  `[u8; 32]` is `[Word<u8>; 32]`, `[u64; 16]` is `[u64; 16]`, and an array
  of mirrored structs is an array of their mirrors. A short array is the
  tuple's `Words` cell, as before.
- **`vesper-port-abi`.** `Abi for [T; N]` (elements in order) and
  `Mirror for [T; N]` (`N * T::WORDS` words).
- **Checker.** Part 1 proved `a[i + 1]` in bounds but still required
  `Fail[ArithOverflow]` for the `+`. Now `+`, `-` and `*` cannot overflow
  when every corner of the operands' bounds fits the kind
  (`src/check.rs`), and index bounds go through `+` and `-` of any bounded
  operands, so `a[15 - i]` is proved too (`src/arrays/check.rs`). Both are
  needed for the scoreboard shifts below.
- **Manuscript.** Two sentences in the §2.2 arrays paragraph: the loop
  arithmetic rule and the export rule.

### Tests

- `tests/arrays_export.rs` (5 tests): the interface marks the structs
  `repr_c`, the committed pilot binding is what the compiler writes, the
  cells and Rust types (nested arrays, `types_path`), and an 8-element
  array keeps its tuple binding.
- `tests/arrays.rs` gains `loop_index_arithmetic_is_total`, differential
  on every backend, with four near misses that still need `Fail`.
- `vesper-port-abi` unit tests for both impls.
- The pilot's new `arrays` library (`crates/vesper-port-pilot`, flagged
  in the PR as outside the lane's paths): `vesper/arrays.ves`, the
  committed `port/arrays.rs`, and `tests/arrays.rs` (5 tests) under
  `vesper-port`. It has a record with `[u8; 32]`, `[u64; 16]` and
  `[u8; 2]` fields, written in place, an `IndexOutOfBounds` raise through
  `set`, whole arrays as parameter and result, and a 16-block SACK-style
  scoreboard checked against a Rust model over 200 random blocks.
  `build.rs` and `BUILD.bazel` gain the library next to `widen`.
- Still passing: `arrays`, `const_items`, `native_export_v2`, `_v3`,
  `_cli`, `_f32`, `_handle_seqs`, `_lent_handles`, `loops`, `numeric`,
  `range_facts`, `integer_widths`, `effect_optimization`, and the pilot's
  `gauge` and `widen` tests.

### Port measurements (local rewrites, not committed)

Callgrind `Ir` on this Linux x86_64 container, release builds of each
crate's `abba` example. Each before/after pair gives the same digest. The
rewrite diffs are kept with the project's files, not in the repository.

**Warp ACK ranges.** The rewrite gives `ReceivedPackets` a mirror
`AckRanges { r: [AckRange; 32] }` in place of the lent `[AckRange; N]`
slice. In `ack.ves`, each checked `try_get`/`try_set` becomes
`ranges.r[i & 31]`. The ranges are live below `len <= 32`, so the mask
changes nothing.

| Workload (`abba once`, 20 batches) | Rust | Port before | Port after | Ratio before -> after |
|---|---:|---:|---:|---|
| ack | 111.2M | 181.5M | 166.0M | 1.63x -> 1.49x |
| transfer | 498.4M | 510.6M | 510.4M | 1.02x -> 1.02x |

The rewrite only works for `N = 32`. `ReceivedPackets<N>` is generic in
`N`, and a Vesper array length must be a literal, so the tests that use
`ReceivedPackets::<4>` and `::<8>` hit the rewrite's `N == 32` assert
(6 of `ack_tracking`'s tests). Adopting it needs const-generic lengths
or one Vesper struct per `N` that is used.

**warp-tcp SACK slots.** The rewrite replaces `Opts.b0..b3` with
`sack: [Blk; 4]`, and the `if k == 0 ... else` chains in
`opts_blk`/`opts_set_blk` with `o.sack[k & 3]`. Four elements is below
the array threshold, so the field still crosses as a tuple and part 1
alone was enough.

| Workload (`abba worker`, 64 batches) | Rust | Port before | Port after | Ratio before -> after |
|---|---:|---:|---:|---|
| codec | 33.94M | 21.82M | 21.95M | 0.64x -> 0.65x |
| transfer | 131.2M | 185.6M | 186.1M | 1.41x -> 1.42x |

There is no gain here. The four-way branch on a by-value `Opts` is
already as cheap as the indexed read, which copies the element out of the
by-value struct. With `min64(k, 3) & 3` instead of `k & 3`, the codec
count is +1.7%.

### Remaining gaps (part 2)

- Array lengths cannot be const generics, so a Rust `[T; N]` generic in
  `N` has no single Vesper mirror (Warp's `ReceivedPackets<N>`).
- `[u8; N]` fields take a word per byte in the mirror, like every narrow
  field. A packed mirror would need packed array storage (part 1 gap).
- An array of up to eight elements crosses as a Rust tuple.
- `integer_bounds` does not yet bound `+`, `-` or `*` results, so a
  nested sum like `i * 2 + 1` still needs `Fail[ArithOverflow]`.
