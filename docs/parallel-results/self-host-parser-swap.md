# Self-hosting SH1 lane 12: the parser swapped into the hybrid build (#1155)

> The [manuscript](../manuscript/README.md) is the sole specification. This
> lane needed no change to it. Confirm live ownership on the issue board
> before acting.

Claim: #1155. Base: `main` at `62b4e34`. It depends on lane 6 (#1189) and
lane 9 (#1168), both merged. Lane prompt:
[`docs/agent-prompts/self-host-sh1/12-parser-swap.md`](../agent-prompts/self-host-sh1/12-parser-swap.md).

The hybrid compiler now parses every source with the Vesper front end v2
and the Vesper AST. The Rust parser stays the default, and no Rust code was
deleted.

## What changed

| Area | Files |
| --- | --- |
| Swap-in point: `ParseTable`, `RUST_PARSE`, `install_parse`, `rust_parse`. The default build installs nothing. | `crates/vesper-syntax/src/parser.rs` |
| The parse module (`@vesper/hybrid-parse`, export ABI v4) | `crates/vesper-hybrid/modules/parse`, `port/parse.rs` |
| Reading the module's result back into a `Parse` | `crates/vesper-hybrid/src/parse_words.rs` |
| Install from the static constructor; `VESPER_HYBRID_STATS` reports parse calls | `crates/vesper-hybrid/src/hybrid.rs`, `build.rs` |
| Tests | `crates/vesper-hybrid/tests/{parse,parse_leaks,parse_bench}.rs`, `differential.rs` |
| Divergence check (`divergent-parse` feature) | `build.rs`, `Cargo.toml`, `BUILD.bazel`, `.github/workflows/hybrid.yml` |
| Front end v2 mirror gaps found by the swap | `packages/vesper-front/src/{parse,lib,util}.ves`, `tests/self_host_front_v2.rs` |
| Bazel: the AST, arena and intern packages, and `vesper_hybrid(module_deps = ...)` | `BUILD.bazel`, `tools/bazel/vesper_hybrid.bzl` |
| Root patch (own commit) | `Cargo.lock` (the hybrid now depends on `rowan`) |

### The boundary

`parse(source)` returns a `Parse`: the rowan `GreenNode`, the diagnostics
and the `vesper_core::ast::Module`. All three have to be identical. The
Vesper side, `parse_words(in source: Str) -> ChunkSeq[u64]`, runs
`lower_source` once. That is the front end v2 parse followed by the
lowering, so the source is lexed and parsed only once. It returns one word
stream with these parts:

- the CST builder events (node start, node finish, or token with its kind
  and range), which Rust replays into `GreenNodeBuilder`;
- the diagnostics, each with its code, span and message bytes;
- the AST arenas of `@vesper/ast` row by row, plus the interner's text and
  offsets. Rust reads each arena in place as a slice of the stream and
  builds the `Module` by walking it from the module's item lists. The walk
  follows `dump.ves` one node kind at a time.

`modules/parse/src/lib.ves` documents the layout, and both sides check a
format word.

**Why a flat stream rather than v4 owned trees.** Lane 6's owned-tree
`decode` copies a Vesper `Box` tree into Rust at about 516 instructions per
node, and the AST is an index graph of arenas, not `Box` trees. The
porting guide's advice applies: use v4 trees when Vesper needs the tree
shape, not to save boundary time. The stream costs one `memcpy`
(`seq_take`), and Rust reads the arenas in place, which is the v4 lent-view
idea applied to flat rows. The module is declared `abi 4`, so later swaps
can lend Rust handles (`World`) into it.

**Only `parse`.** `parse_for_tests` (the test runner) stays on Rust: the
AST lowering skips `test { }` blocks the way `parse` does, and it has no
test mode. `parse_with_limits` and `parse_recovery` (the editor) also stay
on Rust. These are listed under what is left.

## Tests and the differential

`tests/parse.rs` requires `vesper_parse(s) == rust_parse(s)` for the whole
`Parse`: the green tree, every diagnostic and the full module through
`PartialEq`. A failure reports the first differing CST event, diagnostic or
`Debug` line.

| Test | Corpus | Differences |
|---|---|--:|
| `repository_sources_parse_the_same` | every `.ves` file in the repository (911) | 0 |
| `parser_test_corpus_parses_the_same` | the string literals of the parser test suites and of the front end and AST tests | 0 |
| `desugarings_and_limits_parse_the_same` | lane 9's hand cases: every desugaring, literal edge case, recovery, depth, height, diagnostic, token and 1 MiB limits | 0 |
| `fuzzed_corpus_parses_the_same` | 3,000 edited sources and token soups (`VESPER_AST_FUZZ`) | 0 |
| `parse_leaks` | live allocations before and after normal, halted, limited, oversized and empty parses | equal |

The hybrid differential (`tests/differential.rs`) runs `check --json`,
`emit --emit ir` and `emit --emit object` with both compilers on 537 files
under `bootstrap/corpus`, `examples/` and `tests/`:

| Run (Linux x86-64, port-compiler profile) | Differences |
|---|--:|
| hybrid vs the default compiler | 0 |
| hybrid built with `divergent-parse` (every AST flag inverted) vs the default | 1,116 of 1,611 runs (caught) |

`pilot_module_runs_from_vesper` now also requires parse calls answered from
Vesper. macOS and Windows run the same tests in the Hybrid compiler
workflow, and Bazel runs them on Linux.

**The full test suite.** The root test suite runs the compiler library in
process, and the library never installs the table, so those tests exercise
the Rust parser. `Parse` equality on every repository source is stronger
evidence for the swap than rerunning them: every source any test parses
from a file reads back identically. Running the whole suite with the table
installed would need an opt-in in the root package, which is outside this
lane's paths. That is listed under what is left.

### Front end v2 mirror gaps the swap found

The lane 9 AST test compares only the AST dump, and the front end v2 test
compares only its own corpus. The swap compares CST and diagnostics on
lane 9's corpus, and two cases failed. Both are now fixed in
`packages/vesper-front` and added to `tests/self_host_front_v2.rs`:

1. **`dyn Fn(A) -> R eff [E]`** (#1084): the front end read
   `dyn FnMut(in u8)` as `dyn FnMut` followed by type arguments, so the CST
   ended the parameter list early. The lowering already parsed the
   signature. Now the front end does too, through a shared `signature`.
2. **The or-pattern expansion budget** (#1163): when or-pattern
   alternatives copy more than `OR_PATTERN_EXPANSION` tokens,
   `Parser::match_expr` reports E0002. Only the lowering tracked this
   budget, which has no diagnostics. Now the front end keeps the same
   counter (`P.expansion`) and reports the error.

The front end v2 suite (13 tests) and the AST suite (7 tests) pass after
these fixes.

## Numbers

The host is a 4-core x86-64 cloud machine. Both binaries use the
port-compiler profile, which is the one the hybrid is built with.

**The parse itself** (`tests/parse_bench.rs`): 911 repository sources,
4,581,735 bytes, 7 ABBA rounds, medians. Rust is `rust_parse`. The swap is
`parse_words` followed by `decode`.

| | Wall | vs Rust | Ir (callgrind) | vs Rust |
|---|--:|--:|--:|--:|
| Rust parser | 452.1 ms | 1.00x | 2,877,003,769 | 1.00x |
| Vesper parser (the swap) | 454.0 ms | **1.00x** | 3,437,047,837 | **1.19x** |
| of which `parse_words` (front end v2, lowering, stream) | 272.1 ms | 0.60x | 2,327,528,504 | 0.81x |
| of which `decode` (green tree, diagnostics, `Module`) | 183.4 ms | 0.41x | 1,114,571,298 | 0.39x |

The Vesper half is faster than the whole Rust parser, which matches lane
9's 0.55x and 0.88x plus the cost of writing the stream. The Rust half
rebuilds what the Rust parser also allocates: the rowan tree and the boxed
AST with one `String` per name. The stream is 1.59 words per source byte.

**The whole test corpus** (the scoreboard row): `vesper check` on each of
the 537 differential files in turn.

| | Wall (ABBA, 4 runs each, median) | Ir (sum over 537 runs) |
|---|--:|--:|
| default compiler (Rust parser) | 2.51 s | 3,654,421,650 |
| hybrid (Vesper parser and layout) | 2.58 s (1.03x) | 3,874,284,266 (1.06x) |

Process start-up and checking dominate these small files, so the 19% parse
overhead shows as 3% of wall and 6% of instructions.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow (`bootstrap/run.sh stage1 --reproduce
--outcome`) was dispatched on the branch, and the PR links its result. The
default compiler's behavior is unchanged: `parse` calls `rust_parse` through
an empty table.

## Compiler gaps

1. **Moving a field out of a local handle leaks the rest of the handle.**
   In `parse_words`, `let ev = l.p.ev;` (where `l` is the `L` returned by
   `lower_source`) leaked about 70 allocations per parse: every other
   sequence of `l`. The leak test caught it (71 live allocations became 284
   after 3 parses). Borrowing in place (`l.p.ev.try_get(i)`) frees
   everything. The shape:

   ```vesper
   struct H { a: ChunkSeq[u32], b: ChunkSeq[u32] }
   fn f() -> u64 {
       let h = H { a: ChunkSeq::new[u32](), b: ChunkSeq::new[u32]() };
       let a = h.a;   // h.b is never freed
       a.len()
   }
   ```

2. **The stream doubles the AST's size.** Each arena field crosses as a
   full word (`ExprNode` takes 8 words), so the stream is 1.6 words per
   source byte. Packing two `u32`s per word would halve it. That needs
   either packed aggregate elements (lane 9's gap 1, after which the arenas
   could be lent in place as v4 views of `repr` rows) or hand packing in
   `lib.ves`.
3. **Struct fields cannot be read through a view of an arena without a
   copy.** v4 views cover `ChunkSeq`s of handles and `SeqRef`s of packed
   scalars, not `ChunkSeq`s of copy structs held by a foreign package's
   `Arena[T]`. With them, `decode` could read `l.ast` in place and the
   stream would go away.
4. **`decode` allocates one `String` per name occurrence**, as the Rust
   parser does. An interned `Module` (lane 9's symbols) would let the
   checker skip this. That is an SH2 data-model choice, not a parser one.

## What is left

- `parse_for_tests`, `parse_with_limits` and `parse_recovery` (the editor
  path, whose Vesper tables exist from lane 10) are still served by Rust.
  The first needs a test mode in the lowering.
- Running the root test suite with the table installed (an opt-in such as
  an environment variable read by `vesper` tests) would need a change
  outside this lane's paths.
- Parity: the parse is 1.00x wall but 1.19x Ir. Gaps 2 and 3 are the
  boundary half, and lane 9's packed-field gap is the Vesper half. The Rust
  parser stays the default.
- Mirror rule: every later `parser.rs` change now has to land in
  `packages/vesper-front`, in `packages/vesper-ast/src/lower.ves` and, when
  it changes the AST shape, in `crates/vesper-hybrid/src/parse_words.rs`.
  `tests/parse.rs` fails otherwise.
