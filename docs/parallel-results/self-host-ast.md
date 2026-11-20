# Self-hosting SH1 lane 9: the AST in Vesper

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1152. Base: `main` at `e29e63d` (SH1 plan PR #1158). Lane prompt:
`docs/agent-prompts/self-host-sh1/09-ast.md`.

## Summary

`packages/vesper-ast` (`@vesper/ast`) is the AST of `vesper_core::ast` in
Vesper, built from the tables of the front end v2 (`@vesper/front`, #1129).
The Rust parser stays the default and no Rust code changed.

- **Nodes** (`src/lib.ves`): one copy struct per Rust node type (`ExprNode`,
  `PatNode`, `TypeNode`, `StmtNode`, `FnNode`, `AdtNode`, `ImplNode`,
  `TraitNode`, `ConfNode`, `UseNode` and 19 more), each in a typed
  `Arena[T]` of `@vesper-std/arena`. A node names its children by arena
  index (`u32`, `none()` = `u32::MAX` for an absent `Option`), and child
  lists are `ListRef { first, len }` ranges of one shared `u32` pool (list 0
  is the empty list). Kinds are numbered in the order of the Rust enums
  (`Expr` 1-22, `Pattern` 1-5, `Type` 1-6). Names, labels and string payloads
  are `Symbol`s of one `@vesper-std/intern` `Interner`. Readers go through
  accessors (`expr_at`, `pat_at`, `type_at`, `list_at`, ...), so the index
  representation can become `Box` once lane 6's box matching lands without
  changing callers.
- **Lowering** (`src/lower.ves`): the AST-building code of
  `crates/vesper-syntax/src/parser.rs`, transliterated function by function
  (items, declarations, impls, functions, consts, parameters, place trees,
  types with arrays, shapes, projections and `dyn Fn` signatures, effect
  rows, `use` items, traits, models, conformances, where clauses, blocks,
  expressions at every precedence, postfix forms, closures, handlers,
  regions, loops and `for`, struct literals, slice intervals, array literals,
  patterns, and the `const` use rewrite). It walks the front end's
  significant tokens again, with the diagnostics, depth and height bookkeeping
  removed because the front end already decided them: a halted or limited
  parse gives the empty module, as in Rust. Where the Rust parser looks ahead
  to choose index or generic arguments, the lowering asks the CST
  (`index_ahead`), and the `bound_values` scope trail is kept for
  `break`/`continue`/`for` desugarings. A `sync` counter checks that every
  token the lowering consumes is the one the CST put next; the differential
  test requires it to stay 0.
- **Literals** (`src/text.ves`): string literals decode with serde_json's
  rules (`\uXXXX` surrogate pairs, an invalid escape gives `""` as in Rust),
  and float literals convert with exact decimal-to-binary rounding (bigint
  limbs, 800 significant digits plus a sticky digit, ties to even,
  subnormals, overflow to infinity), equal bit for bit to Rust's
  `str::parse::<f64>`.
- **Dump** (`src/dump.ves`): the canonical text dump, written out below.

Entry points: `ast_dump(in source: Str) -> ChunkSeq[u64]` (the dump's bytes
and the desync count), `ast_count` (the number of expression, pattern and type
nodes) and `ast_rows` (row counts per arena, for the memory report).

## Canonical dump

One line per node in preorder, indented one space per level. Absent values
print as `-`, spans as ` @start..end`, strings quoted with bytes outside
`0x20..0x7e` plus `"` and `\` as `\xHH`, floats as `float 0x` and their 16
hex bits. Both dumpers write exactly this: the Rust one is `dump_module` in
`tests/self_host_ast.rs` (over `vesper_core::ast::Module`), and the Vesper one
is `dump_ast` in `src/dump.ves` (over the arenas). It covers every field of
every node kind, including desugared forms (the `for` and `continue`
rewrites, `const` uses as calls, folded negative literals, region and splat
names) and the module's model uses, trait bounds, `use` items and public
names.

## Differential test

`tests/self_host_ast.rs` compiles `ast_dump` once as optimized native code and
compares its dump with the Rust dump of `vesper_syntax::parser::parse`. A
source passes only when the dumps are equal byte for byte and the desync count
is 0.

| Test | Corpus | Differences |
|---|---|---|
| `repository_sources_match_the_rust_ast` | every `.ves` file in the repository | 0 |
| `parser_test_corpus_matches_the_rust_ast` | string literals of `crates/vesper-syntax/tests/*.rs`, `tests/self_host_parse.rs`, `tests/self_host_front_end.rs`, `tests/self_host_front_v2.rs` | 0 |
| `desugarings_and_limits_match_the_rust_ast` | hand cases for each desugaring, literal edge cases, recovery, the depth, height, diagnostic, token and 1 MiB limits | 0 |
| `fuzzed_corpus_matches_the_rust_ast` | the front end v2 fuzz generator, 3,000 sources (`VESPER_AST_FUZZ`) | 0 |
| `float_literals_match_rust_parse` | 2,000 random literals plus edge cases (subnormals, ties, overflow, long digit strings) | 0 |
| `lowering_frees_its_arenas` | live sequence count before and after building and dropping a large AST | equal |
| `backends_agree_on_small_sources` | a driver package's dump digest under the evaluator, the JIT (optimized and not) and a built executable | equal |

As a mutation check, renaming one dump label in `dump.ves` makes the
repository test fail on the first file that has the node.

## Numbers

Corpus: the 903 repository `.ves` files up to 1 MiB (4,236,338 bytes,
450,710 expression, pattern and type nodes). The host is a 4-core x86-64
cloud machine with a release build, using the median of 7 ABBA rounds
(`tests/self_host_ast_bench.rs`). Rust is `vesper_syntax::parser::parse`,
which builds the rowan CST and `vesper_core::ast` together. Vesper is the
front end v2 plus this lowering, which builds the CST tables and then the
arenas.

| | Time | M nodes/s | vs Rust |
|---|---|---|---|
| Rust parse to AST | 484.7 ms | 0.93 | 1.00x |
| Vesper parse to AST | 267.3 ms | 1.69 | **0.55x** |
| of which front end v2 | 176.3 ms | | 0.36x |
| of which lowering | 91.0 ms (34%) | | |

Callgrind (`once_for_callgrind`, one pass over the corpus, inclusive Ir of
each `#[inline(never)]` driver; the JIT compile is outside them):

| | Ir | vs Rust |
|---|---|---|
| Rust `run_rust` (parse to AST) | 2,334,022,658 | 1.00x |
| Vesper `run_vesper` (front end v2 + lowering) | 2,064,864,052 | **0.88x** |
| Vesper `run_vesper_front` (front end v2 alone) | 1,491,087,278 | 0.64x |
| lowering (difference) | 573,776,774 (28%) | |

The wall-clock gap (0.55x) is wider than the instruction gap (0.88x)
because the Rust parser builds a rowan tree and boxed nodes and spends its
time in the allocator and pointer chasing, while the Vesper tables and arenas
are flat sequences.

Memory per AST node (expression, pattern or type). Rust bytes come from a
counting global allocator. Vesper bytes are each arena's row count times its
element's native stride (`Layout::of`), plus the pool and the interner,
because Vesper sequences allocate with `malloc`, which the Rust allocator does
not see. They are row payload, before capacity slack (up to 2x for a doubled
`ChunkSeq`).

| | B/node |
|---|---|
| Rust `Module` retained | 374.7 |
| Rust parse peak (CST included) | 743.1 |
| Rust allocated during parse | 806.4 |
| Vesper AST | **87.2** (arenas 79.9, pool and lists 2.0, interner 5.3) |

Element strides: `ExprNode` 64 B, `PatNode` 48 B, `TypeNode` 40 B,
`FnNode` 72 B, `ConfNode` 80 B. Each `u32` field takes 8 bytes (see Compiler
gaps), so packed fields would roughly halve the arenas.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow run on this branch (`bootstrap/run.sh stage1
--reproduce --outcome`) is linked from the PR, and its verified artifact
digests equal `bootstrap/stage1.lock`.

## Changes outside the lane's paths

- `BUILD.bazel`: the test's data (the root package's `.ves` files, the AST,
  front end and std packages, and the parser test sources, all as anchored
  globs) and a `long` tag.
- `tests/fixtures/self_host/driver.rs` and `tests/self_host_check.rs`: skip
  `packages/vesper-ast`, which imports with `use` (like `packages/vesper-front`).

## Compiler gaps

1. **Aggregate elements store every scalar field in a full word.**
   `ChunkSeq[S]` for `struct S { a: u32, b: u32 }` has stride 16, not 8.
   `ChunkSeq[u32]` alone is packed (stride 4). The AST arenas are about 2x the
   bytes their fields need (`ExprNode`'s eight `u32` fields take 64 B), which
   is most of the Vesper AST's 80 B/node. Repro:

   ```vesper
   struct S { a: u32, b: u32 }
   fn f() -> u64 { let mut s = ChunkSeq::new[S](); s.push(S { a: 1, b: 2 }); s.len() }
   // Layout::of(S).size == 16
   ```

2. **No decimal-to-`f64` conversion in `@vesper-std`.** Matching Rust's
   `parse::<f64>` took about 500 lines of bigint code in `src/text.ves`
   (32-bit limbs held in `u64` words, so that each limb product fits). Any self-hosted lexer, JSON reader or config loader
   needs it. It belongs in `@vesper-std/fmt` or a `@vesper-std/num`, ideally
   with a fast path (Eisel-Lemire) that the bigint path backs.

3. **Keywords cannot be field or function names, and there is no raw
   identifier.** `struct S { effect: u32 }` fails with E0002, so the Rust
   field `effect` became `effect_nodes` and the function `handle` became
   `handle_expr`. A port of any Rust type with such a field has to rename it.
   A raw-identifier form (`r#effect` or similar) would keep names one to one.

4. **`Arena[T]` and `Interner` expose no capacity or byte size.** The memory
   report has to recompute bytes from row counts and `Layout::of` on the host,
   and cannot see capacity slack. `Arena::capacity` and `Interner::text_len`
   accessors would make memory accounting a Vesper-side call.

5. **Host code cannot measure Vesper heap bytes.** `native_runtime` counts
   live allocations, not bytes, and allocates with `malloc` outside Rust's
   global allocator. A byte counter next to `live_allocations` would let the
   benches report real peak and retained memory.

## What is left

- The checker and later lanes (12, 13, 14) can read this AST through the
  accessors. Switching child links from arena indices to `Box` waits on lane
  6's box matching, and only changes `lib.ves`'s accessors.
- The lowering re-reads significant tokens instead of the CST rows, so the
  front end v2 and this package must both follow every later `parser.rs`
  change, so the parser mirror rule should name `packages/vesper-ast` too.
- Diagnostics come only from the front end, and the lowering produces none.
  A source the front end halts or limits lowers to the empty module, as in Rust.
- Gap 1 (packed fields) would bring the AST to about 45 B/node.
