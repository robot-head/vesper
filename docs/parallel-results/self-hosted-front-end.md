# Self-hosting Stage 1: a Vesper-source lexer and parser (§42.3, §38)

Wave eight, lane 3. Claim: #452. Base: `1094e64` (descendant of the required
baseline `ea1ed29`). Branch: `claude/project-thread-a203g6`.

## What was built

`packages/vesper-compiler/` starts `@vesper-lang/compiler`: a lexer and a
recursive-descent parser for the core expression, statement and item
subset, written in Vesper and run by the Stage 0 compiler (the Rust seed).

| File | Contents |
| --- | --- |
| `Loom.toml` | `@vesper-lang/compiler`, `capability_ceiling = ["Fail"]`, file order |
| `src/lexer.ves` | Token and parser-state types, `Diag`, byte classes, the full seed token language |
| `src/cursor.ves` | Digest, lexer phase, parser cursor with one cached lookahead, literal checks |
| `src/types_items.ves` | Types, type arguments and effect rows, generics, fields, items |
| `src/expressions.ves` | Blocks, statements, expressions (Pratt precedence), patterns |
| `src/driver.ves` | Module parser and the two entry points `lex_main` and `parse_main` |

The five files are one compilation unit (Loom does not assemble multi-file
packages yet; the formatter's 8192-token file limit forced the split). The
test appends a generated module defining `source_len()` and `source_byte(i)`
for the input.

### Design under today's language

* **Source.** Vesper has no strings with byte access, arrays or buffers, so
  the input is a pure function `source_byte(i) -> i64` (-1 past the end).
  The generated module stores 64 bytes per leaf function as a tuple indexed
  dynamically, under a tree of dispatch functions with fan-out 8. (A single
  comparison tree over every byte is correct on both engines, but the
  native backend's CLIF grows quadratically with the `if` nesting of one
  function: 4.3 million CLIF lines for 593 leaves. Small functions avoid it,
  and leaves of 32 bytes hit the JIT's 1024-instance monomorphization cap on
  10 KB inputs.)
* **Tokens.** `lex_at(i)` returns the raw token at byte `i`, longest match,
  with exactly the seed's `logos` rules: trivia kinds, nested block
  comments, keyword-versus-identifier, int/hex/float longest match,
  `([^"\\]|\\.)*` strings and every one- and two-byte operator. Kinds are the
  seed's `TokenKind` discriminants. The parser keeps no token buffer: its
  state caches the next significant token and re-lexes only for the second
  and third lookahead.
* **CST.** The parser mirrors the seed's batch parser node for node (MODULE,
  ITEM, EXPR, PATTERN, TYPE, with trivia attached as rowan attaches it). With
  no byte buffer to hold the flat CST, it is emitted as a preorder event
  stream (open: kind and start offset; close: end offset) folded into a
  63-bit digest. Errors are `-1 - (code << 32 | offset)`.
* **Loops.** There are no loops; the lexer phase is a balanced doubling
  recursion (`lex_steps(k - k/2, lex_steps(k/2, s))`), so its depth is
  logarithmic in the token count on both engines. Parser recursion depth is
  bounded by nesting and by the number of items, statements and list
  elements; CST depth is capped at 96 (`Diag::Limit`).
* **Errors.** Every failure is a typed `Fail[Diag]`: `Lex(offset)`,
  `Expected(kind, offset)`, `Invalid(offset)` (literal checks the seed does
  after lexing: u64 range, hex without digits, JSON string escapes, region
  capacity), `Limit(offset)` and `Unsupported(construct, offset)`. The parser
  does not recover; where the seed reports any diagnostic, the Vesper parser
  fails instead of returning a tree.
* **Unsupported subset** (typed `Unsupported`, never a tree): `trait`,
  `model`, `use model`, `impl Trait for`, `impl @...`, associated-type
  projections (`T::Assoc`, `<T as Tr>::A`), `\u` string escapes, and `x[a]`
  where `a` is a lower-case name while a row generic is in scope (the seed
  decides that case from its set of bound names, which needs strings).

## Checks

Differential test `tests/self_host_front_end.rs`:

* `differential_over_repository_sources`: every `.ves` file under
  `examples/`, `tests/fixtures/` and `packages/` (145 files, including the
  compiler's own five sources) on the evaluator; every sixteenth file (10)
  also on the Cranelift JIT, which must return the same results. Result:
  **145/145 token streams identical** (kinds and spans, trivia and EOF
  included), **77 CSTs identical**, 4 typed `Unsupported` (the trait/model
  examples), 64 seed-rejected inputs rejected (all macro-syntax fixtures:
  `source_macro*`, `macro_preview`, `stylus`, one `format_proof`, where the
  batch seed reports "expected a declaration"). Assertion floors: 100
  sources, 100 token matches, 50 CST matches, 8 JIT runs.
* `edge_cases_and_typed_errors`: 31 hand-written inputs against the seed:
  number longest-match cases, nested comments, all trivia bytes, compound
  operators, keyword prefixes, escapes, region capacity bounds, literal
  overflow, invalid escapes, raw newlines in strings, syntax errors, lexical
  errors with exact offsets (`&`, unterminated string and comment, escaped
  newline, non-ASCII), and each unsupported construct with its code.

Commands and results (cloud host, 4 cores, Rust 1.97; Clippy with 1.100):

```text
cargo test --test self_host_front_end            2 passed, 85 s (debug)
cargo fmt --all -- --check                       ok
cargo +1.100.0 clippy --locked --test self_host_front_end -- -D warnings   ok
vesper fmt --check packages/vesper-compiler/src/*.ves                      unchanged
```

Measured with a release `vesper` on `crates/vesper-sim-session/fixtures/warp_ack.ves`
(10 336 bytes): evaluator lex 1.3 s, parse 1.4 s; JIT including compilation
0.9 s and 1.1 s. These are single runs, not benchmarks.

## §42.3 requirements: checked or convention

| Requirement | Status |
| --- | --- |
| Only `Fail` effects | **Checked by the compiler**: every function's effect row is checked; the package's rows name only `Fail[Diag]` and `Fail[ArithOverflow]` (the generated source module adds `Fail[RuntimeError]` for tuple indexing, handled inside `source_byte`). `capability_ceiling = ["Fail"]` in `Loom.toml` is enforced by Loom only across package edges; no single-file check reads it. |
| No ambient I/O | **Checked** in the sense above: I/O is only reachable through effects, and none is declared. |
| Phases inside `Region::scoped` | Each entry runs its phase in `region lexer_arena(4096) { ... }` / `region parser_arena(4096) { ... }` (the wave-seven lexical regions). The phases allocate nothing (there is nothing to allocate into: no collections), so the bound only caps the region; "bounded arena" is trivially true, not a property of a real CST arena. |
| Zero global heap churn | **Not checked.** The Rust evaluator allocates for every value; the JIT uses stack frames. No counting-allocator test was run, so no allocation claim is made. |
| Rewrite-certificate validation before codegen | Out of scope for Stage 1 (no Stage 4/5 here). |

Rust ownership plays no part: the Vesper typing rules (copy structs, copy
enum, effect rows, checked arithmetic) are what the seed checked.

## Language features the self-hosted compiler needs (ranked)

1. **Byte strings / byte slices with indexing** (`ByteSlice`, §42.3's
   `in src_bytes`). The source is a generated function today.
2. **Growable buffers or arrays** (`ByteSliceMut`, `Vec[T]` in a region). The
   CST is a digest, not a buffer; a real flat CST needs index-addressed
   storage, and the parser re-lexes lookahead because it cannot keep tokens.
3. **Integer conversions** (`i64` ↔ `u64`, narrowing with checks). The u64
   literal check compares digit strings because there is no conversion.
4. **Loops or guaranteed tail calls.** Iteration is recursion; the lexer
   needs a doubling recursion to keep depth logarithmic.
5. **Generic collections and maps** (`Map[String, T]` for row names, bound
   names and symbol tables): the one ambiguous seed rule needs them.
6. **Strings with equality and hashing** for identifiers and keywords
   (keywords are matched by packing up to six bytes into an integer).
7. **Struct update syntax and shared constants** (`P { pos: x, ..p }`,
   `const`): every state change rebuilds the whole struct, and token kinds
   are integer literals with a comment table.
8. **Traits used by the checker** (an `Eq`/`Hash`/`Ord` family on the
   collections above, and a `Visitor`-style trait over CST nodes).
9. **Bitwise operators on integers** (digest mixing and error encoding use
   multiplication and division).
10. **Multi-file packages** in Loom (the five files are concatenated by the
    test).

## Seed issues found (not fixed here; not this lane's paths)

* Native backend: CLIF size grows quadratically with the nesting depth of
  `if` within one function (593 nested-comparison leaves produce 4.3 M CLIF
  lines and 35 s of compilation).
* `vesper run` and `Runtime::new` run equality saturation by default; on the
  2 000-line front end it dominates (12 s for a 3-byte input in a debug
  build, against 0.3 s without it). The test sets
  `runtime.optimize = false` and the JIT is called with `optimize = false`.
* `vesper fmt` lays out long `&&`/`==` chains one operand per line (output is
  token-preserving but hard to read); the sources use helper functions and
  `match` instead.

## Remaining gaps

* No byte-buffer CST, no token buffer, no recovery and no diagnostics text:
  the result is a digest and a typed error with an offset.
* The unsupported subset above, macros (`$`, `macro`), and the editor-only
  CST kinds (BLOCK, ERROR_NODE, INCOMPLETE).
* Only every sixteenth source runs on the JIT, to keep the debug test under
  two minutes; AOT executables are not used because native object output of
  source regions needs a region runtime library.
* No unsafe code. No new dependencies; no root manifest or Bazel change (the
  root package owns `packages/` and `tests/`).
