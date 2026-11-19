# P3b gaps lane 6: fixed-size arrays and constant tables (#1091)

Part 1 of #1091. Base `9305b24`, with `origin/main` merged at `96b110e`.
Part 2, which adds `[u8; N]` fields to ABI v3 mirrors, follows in its own PR.
Lane 2 (#1107) has merged, so part 2 is unblocked.

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

### Warp ACK ranges: not measured, deferred to part 2

- Warp's ACK ranges are Rust's `ReceivedPackets`.
- The SACK slots of warp-tcp (`Opts.b0..b3`) are fields of a copy struct
  that crosses the v3 export boundary.
- Both rewrites need `[T; N]` fields in ABI v3 mirrors, which is part 2.
  Part 2 will measure them against warp-tcp's `abba drive codec`.

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
- Packed element storage: `[u8; N]` takes `8 * N` bytes.
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
