# Self-hosting SH0 lane 5: std graph, interning, arenas and hashing

Claim #1127. Base `b71ac81` (origin/main with the self-hosting plan, #1131).
Prompt: [05-std-compiler-data.md](../agent-prompts/self-host-sh0/05-std-compiler-data.md).

**Result.** `@vesper-std` has the four data structures compilers are made
of: typed index arenas, a compact index graph with Tarjan SCC, Kahn
topological order with a cycle report, Cooper-Harvey-Kennedy dominators and
reachability, a string interner, and BLAKE3 (portable Vesper, and the
vetted `blake3` crate behind `.vsys`) with FNV-1a and XXH64. Every result
matches its Rust counterpart (petgraph, the `blake3` crate and the official
vectors, a `HashMap` interner, XXH64's C vectors) in the evaluator, the JIT
with and without `optimize`, and executables. Interning costs the same
instructions as `HashMap<String, u32>`; dominators take 1.12x petgraph's
instructions and half its wall time; the CSR graph beats petgraph's wall
clock on every random-graph algorithm though it runs more instructions;
portable BLAKE3 is 63x the crate's (SIMD) instructions, so compiler code
should use `@vesper-std/hash-sys`. No default build changes and no Rust is
deleted.

## What changed

### New packages

| Package | Directory | Contents |
|---|---|---|
| `@vesper-std/arena` | `packages/vesper-std/arena` | `Idx[T]` (copy, `Eq`/`Ord`/`Hash`, `raw`, `index`, `is_none`), `idx_from_raw`, `idx_none`; `Arena[T]`: `arena_new`, `arena_with_capacity`, `alloc`, `get`, `get_or`, `set`, `contains`, `next_idx`, `at`, `len`, `truncate`, `clear`; `IdxMap[K, V]`: `idx_map_new(default)`, `get`, `set`, `len` |
| `@vesper-std/graph` | `packages/vesper-std/graph` | `GraphBuilder` (`graph_builder(n)`, `add_node`, `add_edge`, `build`), `Graph` (`succ_range`/`succ_edge`, `pred_range`/`pred_edge`, `succ_len`/`succ_at`, `pred_len`/`pred_at`, `has_edge`); modules `scc` (`scc` -> `Scc`: `count`, `component`, `size`, `member`), `topo` (`toposort` -> `Topo`: `is_dag`, `order_at`, `cycle_at`), `dom` (`dominators(g, root)` -> `Dominators`: `idom`, `dominates`), `reach` (`reachable_from`, `reachable_from_all`, `reaches`, `postorder`, `reverse_postorder`) |
| `@vesper-std/intern` | `packages/vesper-std/intern` | `Symbol` (copy, `Eq`/`Ord`/`Hash`, `id`, `index`), `symbol_from_id`; `Interner`: `interner_new`, `interner_with_capacity`, `intern`, `lookup`, `len`, `contains`, `symbol_len`, `symbol_eq`, `resolve_into`, `span`, `text_byte`; `hash_str` |
| `@vesper-std/hash` | `packages/vesper-std/hash` | module `blake3`: `blake3`, `blake3_keyed`, `blake3_derive_key`, `Blake3` (`blake3_new`, `blake3_new_keyed`, `blake3_new_derive_key`, `update`, `finalize`, `finalize_xof`, `count`), copy `Digest` (4 LE words, `Eq`/`Ord`/`Hash`), `digest_bytes`, `digest_hex`; module `compress` (generated); module `fast`: `fnv1a64`, `fnv1a32`, `xxh64`, `mix64`, `hash_pair` |
| `@vesper-std/hash-sys` | `packages/vesper-std/hash/sys` | `.vsys` library `Blake3` on `// @export vsys` lines; `hash`, `hasher_new`, `hasher_new_keyed`, `hasher_new_derive_key`, `update`, `finalize`, `finalize_xof`, `hasher_free`, all `eff [Ffi[Blake3]]`; the Rust side is `shim.rs` over the `blake3` crate |

Design notes:

- **Determinism.** Graph algorithms visit nodes in id order and edges in
  insertion order; interned ids are first-insertion order; no result
  depends on a hash value, capacity or address.
- **Totality.** Nothing raises: arena and graph reads are `Option`s or
  answer `none()` (`u32::MAX`), writes return `false`, `add_edge` refuses
  non-nodes. Recursion in SCC, postorder and dominators is an explicit
  stack (a 100,000-node cycle is tested).
- **Graph layout.** `build` freezes edges into compressed sparse rows
  (`ChunkSeq[u64]` offsets, `ChunkSeq[u32]` targets) for successors and
  predecessors, filled by a stable counting sort. Walks use
  `succ_range(v)` and `succ_edge(e)`, which read two sequences and no
  `Option`.
- **SCC numbering.** Components are numbered in Tarjan completion order,
  a reverse topological order of the condensation (every cross edge goes
  from a higher number to a lower), as petgraph's `tarjan_scc` orders its
  vector.
- **Cycle report.** When Kahn's queue empties early, the cycle walks
  predecessors among the unplaced nodes from the lowest id (each has an
  unplaced predecessor) until a node repeats, and is returned forwards.
- **Interner layout.** One `String` holds all text, `starts` the offsets
  and an open-addressing table of `u32` slots (load factor at most 1/2)
  the ids. `Str` is second class, so a symbol's text is read with
  `resolve_into`, `symbol_eq`, or `span` plus `text_byte`, not returned
  as a view.
- **BLAKE3 two ways.** `@vesper-std/hash::blake3` is a port of the
  reference implementation (chunk state, chaining-value stack, XOF). The
  compression function is generated (`hash/gen/blake3_gen.py`, checked by
  `compress_is_generated`): 7 rounds of 8 calls of a quarter round `g` over
  the state in the output slice, with the message schedule applied to the
  arguments. The obvious shape, one straight-line function over 16 locals,
  hits compiler gaps 1 and 6. `@vesper-std/hash-sys` calls the crate. A `Hasher` is a
  linear handle, so the checker makes every path free it once; the
  evaluator test checks no handle leaks.

### Manuscript (flagged)

The std catalog in §13 (`docs/manuscript/content.typ`, after the standard
library packages item) gains a "Compiler data packages" item: the APIs,
the determinism and totality rules above, and that `@vesper-std/hash-sys`'s
`.vsys` library is provided by the linking Rust host. This is new library
surface only; no language rule changes. The hunk is a separate list item
after the catalog line lane 3 (#1125) also edits; merge `main` between.

### Build

`BUILD.bazel`: the four tests read `:files` (the packages), and integration
tests may read `packages/vesper-std/hash/sys/shim.rs` at compile time
(`tests/std_hash.rs` includes it with `#[path]`). `blake3` and `petgraph`
are existing dependencies; there is no root `Cargo.toml` change.

## Tests

Each test builds a temporary driver package that depends on the std
package by path, folds its results into one `u64` per entry and compares
it with the same fold computed in Rust.

- `tests/std_hash.rs`: the 22 official BLAKE3 input lengths (bytes
  `i % 251`, up to 102,400), 131 bytes of XOF for `hash`, `keyed_hash`
  (key `whats the Elvish word for friend`) and `derive_key` (the official
  context); incremental updates of 9,000 bytes split at 1, 7, 63, 64, 65,
  1000, 1023, 1024, 1025 and 4096 bytes, each equal to the one-shot
  digest; FNV-1a 64/32, XXH64 (two seeds), `mix64` and `hash_pair` at
  lengths 0..100, 1000 and 4099. Expected values come from the `blake3`
  crate (checked against the official `""` and `"abc"` digests) and Rust
  references checked against XXH64's C vectors and FNV's published
  vectors. Portable: JIT (both settings), executable, evaluator (7 lengths
  up to 1,025). `.vsys`: the JIT bound to `shim.rs`, and the evaluator with
  the crate as the host (every hasher freed exactly once).
- `tests/std_graph.rs`: 150 random graphs (n up to 1,000, a quarter to
  four edges per node, parallel edges and self-loops) against petgraph:
  SCCs as each node's smallest component-mate and the count, zero cross
  edges against the numbering, `is_dag` against `toposort`, the Kahn order
  itself and its validity (every edge forward, or the cycle is made of real
  edges), `idom` from node 0 against `dominators::simple_fast`, reachability
  against `Dfs` and 8 `reaches` pairs against `has_path_connecting`. Plus a
  100,000-node cycle. JIT (both settings), executable, evaluator (n up to
  30).
- `tests/std_intern.rs`: 60,000 interns of 9,000 distinct names (empty,
  1-byte, 16+-byte and non-ASCII prefixes; the index grows 11 times)
  against a `HashMap<String, u32>`: ids, length, 18,000 lookups (half
  absent), and `resolve_into`/`symbol_eq`/`symbol_len`/`lookup` round trips
  for every symbol. JIT (both settings), executable, evaluator (600 of
  150).
- `tests/std_arena.rs`: a 100,000-node expression tree as a `Box` tree and
  as an arena, evaluated recursively and (arena) in one forward pass, equal
  to the Rust model; checked access (`get`/`set` past the end, `idx_none`,
  `truncate`, `IdxMap`). JIT (both settings), executable, evaluator (300
  nodes).
- Each file also checks its package on its own within the
  `["Fail", "Alloc"]` ceiling.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changes; no
compiler source changes at all. The bootstrap-stage1 workflow on the branch
gives the digest evidence (see the PR).

## Measurements

Setup: cloud x86-64 container, 4 vCPU, Linux 6.18. Vesper programs are
built by a `port-compiler`-profile `vesper` at this branch with `vesper
build --target x86_64-unknown-linux-gnu` (the port harness's baseline ISA);
Rust programs are `--release` against the same crate versions as the
workspace lock (`blake3` 1.8.7, `petgraph` 0.8.3, `twox-hash` 2.1.5 for
XXH64). Instructions are callgrind `Ir`, whole process, net of the same
program with zero repetitions, per repetition. Wall clock is ABBA (A B B A
twice), whole process, in ms for all repetitions including setup; wall
clock on these hosts moves by up to 30% between runs.

| Workload (per rep) | Rust | Vesper | Vesper / Rust Ir | Wall, Rust | Wall, Vesper |
|---|--:|--:|--:|--:|--:|
| BLAKE3, 1 MiB (crate: SIMD) | 2.63 M | 165.5 M | 62.9x | 4-5 (x4) | 54-57 (x4) |
| XXH64, 1 MiB | 0.92 M | 4.19 M | 4.57x | 3-4 (x4) | 5-6 (x4) |
| SCC, 100k nodes / 400k edges | 20.9 M | 114.2 M | 5.46x | 139-187 (x5) | 109-142 (x5) |
| toposort, 100k-node DAG (~400k edges) | 29.8 M | 38.0 M | 1.27x | 137-143 (x5) | 52-68 (x5) |
| dominators from node 0, same random graph | 276.1 M | 308.1 M | 1.12x | 626-731 (x5) | 313-397 (x5) |
| interning, 1M names of 100k (`HashMap<String, u32>` + `Vec<String>`) | 352.4 M | 355.2 M | 1.01x | 151-184 | 167-216 |
| expression tree, 100k nodes, `Box`, build + eval | 27.7 M | 54.5 M | 1.97x | 24-26 (x5) | 30-31 (x5) |
| expression tree, 100k nodes, arena, build + eval | 8.2 M | 41.5 M | 5.05x | 11 (x5) | 33-36 (x5) |
| same arena, forward-pass eval | | 43.5 M | | | 32 (x5) |

Notes:

- **BLAKE3.** The crate dispatches to AVX2/SSE4.1 even with its `pure`
  feature (3.33 M Ir), so the ratio is SIMD against scalar Vesper; a
  scalar reference is roughly 1,000 instructions per 64-byte block, and
  Vesper's compression takes 8,836 (the 16-local straight-line version:
  30,665; see gap 1).
  Through `@vesper-std/hash-sys` the cost is the crate's plus one foreign
  call per `update`/`finalize`.
- **Graphs.** petgraph's `Graph` keeps adjacency as linked edge lists, so
  random graphs miss the cache; the CSR layout runs more instructions but
  less wall time on all three. On the random graph petgraph's `toposort`
  stops at its first cycle (0.83 M Ir against Vesper's full Kahn pass and
  cycle walk, 9.25 M), so the table times a DAG.
- **Interning** nets out the name formatting both sides do. Vesper's FNV
  word hash plus open addressing matches std's SipHash `HashMap` in
  instructions.
- **Box against arena in Vesper.** The arena takes 24% fewer instructions
  than the `Box` tree (41.5 M against 54.5 M) at the same wall time; in
  Rust the arena is 3.4x cheaper. The gap is gaps 2 and 4 below.

## Compiler gaps

1. **Optimized code rematerializes shared values at each store.**
   Repro: `python3 packages/vesper-std/hash/gen/blake3_gen.py out.ves
   --straight-line` writes the compression as 16 `u32` locals updated by
   224 `rotate_right`s, then 16 `out.try_set(i, ...)` calls. Optimized
   `vesper build` emits 3,352 `ror` instructions and runs 30,665
   instructions per call; `--no-opt` runs about 11,400. In the emitted
   code each `try_set` is preceded by its own copy of the operations that
   produce its value, so values shared between stores are computed again
   instead of reused.
   Storing the state to the slice and reloading it after every half round
   gives 696 `ror`s and 8,351 instructions; the committed shape, one `g`
   function over an `inout Slice[u32]`, gives 432 `ror`s and 8,836. A
   DAG-preserving extraction would bring the scalar compression near
   1,000.
2. **Slice headers are reloaded at every access.** In the same function
   every `out.try_set(i, v)` reloads the slice's data, start and length
   words through the parameter and compares the index, although `out` does
   not change in between: 8 instructions per store and per read. Hoisting
   the header of an `in`/`inout` slice parameter (or of a `ChunkSeq`
   field of an `in self`) out of straight-line code would remove most of
   the arena's and the interner's overhead too.
3. **A borrowed recursive enum cannot be matched.** `fn eval(in e: Expr)
   -> u64 { match e { Expr::Add(l, r) => .. } }` is `E0305` (cannot move
   `e` through a borrowed parameter), and `l.as_ref()` has no way in. A
   `Box` tree can only be read by consuming it (`sink e: Expr`, as
   `tests/std_arena.rs` does), so a compiler cannot walk a `Box` AST
   twice; arenas avoid this.
4. **`Option` of a struct read from a sequence is materialized.**
   `ar.get(i)` returns `Option[Node]` of a 24-byte copy struct, matched at
   once; the arena tree spends about 415 instructions per node to Rust's
   82. Not profiled further: the copy into the `Option` and gap 2's header
   reloads are the likely costs, and reading the fields in place when the
   `Option` is matched at once would test it.
5. **Variable `%` and `/` raise `Fail` even under a guard in a loop.**
   `if n > 0 { loop { .. x % n .. } }` still needs `Fail` (range facts do
   not survive into the loop body), so drivers and the graph benchmark use
   `if n == 0 { 0 } else { x % n }` helpers.
6. **Unoptimized compilation of a large function is slow.** The
   straight-line compression with half-round spills (16 state, 16 message
   and 8 chaining `u32` locals, about 1,000 IR operations in one block)
   takes 4 min 50 s to JIT a 100 KiB hash with `--no-opt` in a debug
   `vesper` (35 s optimized); the committed `g` shape takes 3.4 s and
   10 s. Code generators that unroll should split into functions until
   this is fixed.

## Left

- Callers: Loom, fmt and the syntax cache keep the Rust `blake3` crate;
  moving any of them is SH1 work and uses `@vesper-std/hash-sys`.
- `hash-sys` executables need a static library of `shim.rs` to link
  against; the evaluator and JIT are tested, and a Rust host that links a
  Vesper object includes `shim.rs` as a module (`rust=` on its own
  `@export vsys` line).
- Graph extras compilers use and petgraph has: dominance frontiers, a
  condensation graph, mutable graphs with node removal.
- `Interner` has no removal or arena-backed `Str` view (Str is second
  class by design).
