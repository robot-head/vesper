# Self-hosting SH0 lane 7: front end v2 on sequences and strings

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1129. Base: `main` at `b71ac81` (plan PR #1131), merged up to `798bef4`. Lane prompt:
`docs/agent-prompts/self-host-sh0/07-front-end-v2.md`.

## Summary

`packages/vesper-front` (`@vesper/front`) is a Vesper lexer and lossless CST
parser that works on a `Str` and produces tables, beside the Rust front end
(`crates/vesper-syntax`), which stays the default:

- **Lexer** (`src/lex.ves`): the logos rules of `crates/vesper-syntax/src/lexer.rs`
  written out by hand (longest match, keywords over identifiers, logos's error
  tokens for a stray character, an unterminated string or an unterminated
  nested block comment), with the default `LexLimits` (1 MiB, 65,536 tokens,
  128 diagnostics) and their capped-stream behaviour. The token table is the
  rows `kind` (the `TokenKind` discriminant), `start` and `end`, trivia
  included, so the tokens concatenate back to the source. Identifier rows also
  carry an interned symbol and a contextual-word number, so the parser never
  compares text.
- **Parser** (`src/parse.ves`): `parse_with_limits` of
  `crates/vesper-syntax/src/parser.rs` in batch mode, transliterated function
  by function. It emits rowan builder events (start node, token, finish), so
  the CST has the same node kinds (`MODULE`, `ITEM`, `EXPR`, `PATTERN`,
  `TYPE`), the same token placement (trivia goes where `bump` puts it) and the
  same ranges. It keeps only what steers the Rust parser: each expression's
  kind, span and AST height (`expr_heights`, which drives the depth limit), each
  type's shape, the scoped name sets (`row_names` and `bound_values`, as counts
  plus an undo trail, like `parser/scope.rs`), and the pending trait-bound
  spans. Diagnostics have the Rust parser's codes, messages (byte for byte,
  including `{k:?}` kind names and the `LexLimit` text) and spans, and the
  depth and diagnostic limits halt the parse the same way.
- **Tree** (`src/tree.ves`): the events folded into node rows (kind, parent,
  start, end, in preorder) and leaf rows (token, parent).

Entry points (`src/lib.ves`): `parse_tables(in source: Str) -> ChunkSeq[u64]`
returns the token, node and diagnostic rows; `parse_counts`, `lex_count` and
`lex_raw_count` are for benchmarks. Covered syntax is everything the Rust
parser has on `main` at the base: `use` items (all forms, including the error
paths), `..` slice intervals, `continue`, `for`, `loop`, `break`, qualified
`T::A` and `<T as Tr>::A` projections, `dyn`, shapes, `tiled(N)`, `wire`,
regions, handlers, traits, models and impls, plus what PR #1106 added
while this lane was open (merged into this branch and mirrored): qualified
`self::`, `super::` and `@scope/name::` paths in expressions, types,
patterns, effect rows, handlers and type arguments, `pub use` and `use super`,
`state`/`linear`/`with` as names, the keyword-name message of `lower`, and
open slice intervals `s[..]`, `s[a..]`, `s[..b]`. Fixed arrays and `const`
items (P3b gaps lane 6, PR #1115) are not on `main` yet; when they land, the
differential test fails on their fixtures until the same change is made here.

## Differential test

`tests/self_host_front_v2.rs` loads the package, compiles `parse_tables` once
as optimized native code (`native::WordFunction`, the `Str` passed by address)
and compares its rows with rows read off the rowan tree and `Parse::diagnostics`
of `vesper_syntax::parser::parse`: every CST token's kind, range and parent,
every node's kind, parent and range, and every diagnostic's code, message and
span.

| Test | Corpus | Result |
|---|---|---|
| `repository_sources_match_the_rust_parser` | every `.ves` file in the repository (866 at the base; Bazel sees the root package's 597) | all identical |
| `parser_test_corpus_matches_the_rust_parser` | the source strings of `crates/vesper-syntax/tests/*.rs`, `tests/self_host_parse.rs` and `tests/self_host_front_end.rs` | all identical |
| `edge_cases_and_limits_match_the_rust_parser` | hand-written and generated cases: each lexer corner, JSON string escapes, integer and tile-width overflow, region capacities, every recovery path, the depth limit through parentheses, blocks, types, patterns, unary, postfix, binary and assignment chains, AST-height limits through parenthesized chains (where the height, not the depth, trips first), the diagnostic limit, the token limit and the 1 MiB source limit with a split UTF-8 character | all identical |
| `fuzzed_corpus_matches_the_rust_parser` | 3,000 sources from a fixed seed: repository files with 1-6 random deletions, duplications, truncations and inserted fragments, plus token soup (`VESPER_FRONT_FUZZ` sets the count) | all identical |
| `backends_agree_on_small_sources` | a driver package folds `parse_tables` over small sources; the evaluator, the JIT with and without optimization and an executable give the Rust parser's digest | agree |
| `parsing_frees_its_tables` | the live sequence count is the same before and after a parse | no leak |

Recovery differences: none. The test was checked against deliberate
divergences (a changed message, disabling the `-1` literal fold that the
height limit depends on); each one fails it.

## Numbers

Corpus: the 865 repository `.ves` files (3.65 MB, 1,212,165 tokens with trivia,
235,398 nodes), on a 4-core x86-64 cloud host, release build, median of 7 ABBA
rounds (`tests/self_host_front_v2_bench.rs`; wall clock moves by up to 30% on
these hosts).

| Phase | Rust | Vesper | Vesper / Rust |
|---|---|---|---|
| Parse (`parse` vs `parse_counts`) | 380 ms, 3.2 M tokens/s, 0.62 M nodes/s | 142 ms, 8.5 M tokens/s, 1.66 M nodes/s | 0.37x time |
| Lex (`lexer::lex` vs `lex_count`) | 18.7 ms, 65 M tokens/s | 54.3 ms, 22 M tokens/s | 2.9x time |
| Lex without identifier classification (`lex_raw_count`) | 18.7 ms | 35.6 ms, 34 M tokens/s | 1.9x time |

The Rust parse builds the AST, rowan green nodes and owned `String`s for every
name; the Vesper parse builds only the tables, so its parse is faster overall.
The lexer is slower than logos's generated DFA: it reads each byte through a
bounds-checked `try_get`, and it also interns identifiers and classifies
contextual words, which logos does not do (the Rust parser compares text
instead). `find_not_in` (SIMD byte classes) for whitespace, identifier and
digit runs and `find_either` for string bodies took the raw lexer from 2.5x to
1.9x, and classifying each symbol once instead of each token took the full
lexer from 3.8x to 2.9x.

Callgrind instruction counts (`once_for_callgrind`, inclusive Ir):

| Function | Instructions | Vesper / Rust |
|---|---|---|
| Rust parse (`run_rust_parse_only`) | 1,967 M | |
| Vesper parse (`run_vesper`) | 1,218 M | 0.62x |
| Rust lex (`run_rust_lex`) | 75.2 M | |
| Vesper lex (`run_vesper_lex`) | 532 M | 7.1x |
| Vesper raw lex (`run_vesper_lex_raw`) | 374 M | 5.0x |

The lexer's instruction ratio (7.1x) is higher than its time ratio (2.9x):
the Vesper lexer's extra instructions are mostly well-predicted bounds checks
and `Option` branches, while logos's table-driven DFA spends its time on
dependent loads.

Memory (`memory_per_node`; Rust counted by a counting global allocator):

| | Bytes per CST node |
|---|---|
| Rust `Parse` retained (green tree, AST, diagnostics) | 784 |
| Rust green tree alone | 116 |
| Rust peak during a parse | 1,233 |
| Vesper result tables (tokens 72, CST 54) | 126 |
| Vesper parser working rows (significant-token index, events, source copy) | 85 |

Before this lane there was no Vesper front end that produced a CST: the Stage 1
front end in `packages/vesper-compiler` folds its CST into a digest.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow run on this branch (`bootstrap/run.sh stage1
--reproduce --outcome`) is linked from the PR; its verified artifact digests
equal `bootstrap/stage1.lock`.

## Changes outside the lane's paths

- `BUILD.bazel`: the test's data (the root package's `.ves` files, the package
  and the parser test sources) and a `long` tag (about 140 s unoptimized).
- `tests/fixtures/self_host/driver.rs` and `tests/self_host_check.rs`: skip
  `packages/vesper-front` in the Stage 1 corpora, one line each, like every
  port package: the frozen Stage 1 front end does not parse `use` (#880).

## Compiler gaps

1. **No or-patterns in `match`.** The precedence and punctuation tables are
   one arm per token kind.

   ```text
   fn p(k: u8) -> u64 {
       match k {
           1 | 2 => 5,    // E0002: expected FatArrow, found Pipe
           _ => 0,
       }
   }
   ```

2. **`vesper fmt` refuses long `else if` chains.** A chain of 21 or more arms
   fails with `cannot format source: DepthLimit`, so tables must be `match`es
   or split into helpers (`lex.ves` `punct`, `parse.ves` `precedence`).

   ```text
   fn f(k: u64) -> u64 {
       if k == 0 { 0 } else if k == 1 { 1 } ... else if k == 20 { 20 } else { 9 }
   }
   ```

3. **No module constants.** Token kinds, node kinds and contextual words are
   functions (`fn k_l_paren() -> u8 { 32 }`), and `match` arms must use the
   bare numbers. P3b gaps lane 6 (PR #1115) adds `const` items.

4. **Byte loops cost a bounds check and an `Option` per byte.** Scanning with
   `s.try_get(i)` and a `match` is what keeps the lexer behind logos; the
   byte-class builtins only help for runs. A slice indexing form whose bounds
   check the compiler can drop inside `for i in a..s.len()` (P3b gaps lane 3,
   bounds facts, PR #1118) and a byte-hash builtin for interning would close
   most of it.

   ```text
   fn hash(in b: Slice[u8], s: u64, e: u64) -> u64 {
       let mut h: u64 = 14695981039346656037;
       for i in s..e {
           h = wrapping_mul(h ^ widen_u64(byte(in b, i)), 1099511628211);  // try_get + match
       }
       h
   }
   ```

5. **Checked arithmetic is an effect.** `a + 1` raises `Fail[ArithOverflow]`,
   so a total front end writes `wrapping_add` everywhere (helpers `inc` and
   `add` in `util.ves`). Known; listed for the census.

6. **String literals are `String`, and `Str` cannot be returned.** A table of
   names is `fn kind_name(k: u8) -> String { match k { 0 => "Whitespace", ... } }`;
   `-> Str` is refused twice (`expected Str, found String`, then `E1530`).

7. **Host calls.** A JIT entry takes no arguments, so the test drives the
   package through `native::WordFunction` with the `Str`'s three words passed
   by address, and reads the returned `ChunkSeq[u64]` header itself. A host API
   for calling an exported function with `Str` arguments and a sequence result
   (what the export ABI binding does for objects) would make this a one-liner.

## What is left

- Mirror PR #1115 (fixed arrays, `const`) when it merges; the differential
  test catches it.
- The editor mode (`BLOCK`, `ERROR_NODE`, `INCOMPLETE` nodes, recovery sites,
  header notes) and macro sources are not ported; batch parsing never produces
  them.
- An AST (or typed CST views) over the tables for the checker, once SH1 plans
  the checker port; `@vesper-std/arena` (lane 5) can then replace the plain
  sequences.
- Lexer speed: see gap 4.
