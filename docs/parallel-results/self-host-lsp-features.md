# Self-hosting SH5 lane 17: LSP features, part 1 (navigation)

Claim #1340. Stacked on lane 16's PR #1361 (head `6b5cc69`, which merges
`origin/main` `52ee700`); `origin/main` was `75268b3` when this part started.
Prompt: `docs/agent-prompts/self-host-sh5/17-lsp-features.md`.
Rust stays the default: the language server still runs `src/lsp`. This part
adds no Rust code to the compiler and deletes none.

## What changed

- **`packages/vesper-lsp-features`** (`@vesper/lsp-features`, new, about
  1,850 lines): `src/lsp/navigation.rs` in Vesper.
  - `nav.ves`: the lexical index (`Index`), in the Rust traversal order:
    - the declarations of every item (functions, ADTs, effects and their
      operations, traits, conformance models, impl methods), each with its
      selection span found among the identifier tokens of its span;
    - the global function table, the lexical environment of parameters,
      `let`, `match`, `for` and closure bindings, and the callee rule for an
      unbound effect receiver;
    - handler bodies traversed without targets;
    - the `Budget` charges (16,384 nodes, depth 64) and every `Error` of
      navigation.rs, with session.rs `nav_error`'s JSON-RPC error objects.
  - `lib.ves`, the entries:
    - `lsp_document_symbols`: `textDocument/documentSymbol`, hierarchical or
      the session's legacy `SymbolInformation` shape, with the per-symbol
      and total output budgets.
    - `lsp_workspace_symbols`: `workspace/symbol` over the open documents in
      store order, with the document, query, symbol and output limits.
    - `lsp_definition`: `textDocument/definition` by lexical resolution.
    - `lsp_definitions`: many cursors on one document whose parse is kept,
      with the index rebuilt per request (the server's shape: its checked
      snapshot keeps the AST).
  - `json.ves`: serde_json's compact writer for these shapes (sorted keys,
    serde_json's string escapes, decimal numbers).
  - The source is parsed with `@vesper/front` and `@vesper/ast`; UTF-16
    positions and ranges use `@vesper/span`. The package does not depend on
    `@vesper/serde`, so it can use the builtin `String` (lane 16, gap 1).
- **`tests/self_host_lsp_features.rs`** (new): the differential below.
- `BUILD.bazel`: the test's data (anchored globs plus
  `//crates/vesper-span:files`) and the `long` tag.

## Differential

`cargo test --release --test self_host_lsp_features` (Linux x86-64; the same
tests run unoptimized in CI).

| Test | Compared | Differences |
|---|--:|--:|
| `navigation_matches_rust`: Vesper against `navigation::*` called directly, on every program under `examples`, `tests/fixtures` and `src/lsp/fixtures` that the editor can check (editor profile, then the raised Lens profile, as `Session::snapshot` does), plus generated sources | 192 documents: 384 symbol answers, 51,431 definitions (2,117 with a target), 224 workspace queries | 0 |
| the same definitions through `lsp_definitions` (one parse per document) against `lsp_definition` | 51,431 | 0 |
| `sessions_match_server`: every navigation response the live `vesper lsp` server wrote, against Vesper on the server's own document text (mirrored with `DocumentStore`) | 26 sessions: 46 symbol, 758 definition and 24 workspace responses | 0 |

The cases:
- Cursors: every token start, the middle of every token, the end of the
  source, a line past the end, a character past a line's end, and a cursor
  inside a surrogate pair (each refused as `InvalidPosition`).
- URIs: empty, 4,096 and 4,097 bytes (`Resource("URI bytes")`), and URIs
  with quotes, backslashes, control bytes, DEL and non-ASCII text, which the
  writer must escape as serde_json does.
- Workspace windows: windows of 8 documents, 32 and 33 documents (past
  `max_documents`, `Resource("documents/query")`), 48 documents, and three
  110-symbol documents with 1,000-byte names whose answer passes
  `max_output_bytes` (`Resource("output bytes")`). Queries: empty, `main`,
  `f`, `x`, `new`, no match, and 256 and 257 bytes.
- Sessions: tests/lsp_cli.rs's unicode navigation scenario in both symbol
  shapes (CRLF, astral characters, edits that move every range, a cursor
  past the end), 24 example sessions with an edit that shifts every
  declaration, and a 12-document workspace session.
- Limits the checked sources cannot reach. The editor's own preflight (16,384
  tokens, the function-token limit, 8,192 AST nodes, parser depth 64)
  refuses sources before navigation can exceed `max_symbols` (1,024),
  `max_nodes` or `max_depth`. The generated sources that try (1,025
  declarations, nesting to depth 70) do not check, so these refusals are
  implemented but not exercised.

**Deferred: the documentation-link fallback.** For a cursor inside a `///` or
`//!` comment with no lexical target, Rust answers from
`navigation/doc_links.rs` over `vesper_doc::links`, which is not ported.
Vesper applies the same token test (the cursor's token is a doc comment that
contains it) and answers `D`. The tests count those cases and skip them: 80
of the 51,431 direct definitions, and none of the session ones. Every other
definition, including `null` answers, is compared.

## Numbers

ABBA best of six, `CHECK_HARNESS_SPEED=1 cargo test --release --test
self_host_lsp_features lsp_features_speed -- --ignored`, on a 4-core Linux
cloud host, where wall time moves by up to 30% between runs. The Vesper side
includes the JIT call, decoding the input and copying the answer out. The
Rust side starts from the checked snapshot, whose AST is already parsed.
"Before" is the Rust implementation, which stays the default.

| Request | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| `documentSymbol`, per document (192), Vesper parsing each time | 64 µs | 195 µs | 3.0x |
| `definition`, per request (6,509), Vesper parsing each time | 8.8 µs | 170 µs | 19x |
| `definition`, per request (6,509), parse kept per document (`lsp_definitions`) | 8.8 µs | 34 µs | 3.8x |

- The fair comparison is the last row. The server keeps the parse in its
  snapshot, so a Vesper server would too; the per-request index (lexing is
  reused with the parse, the traversal is redone) is what remains.
- The index stores every table in `ChunkSeq`s read through `try_get`. Rust
  indexes slices and borrows the AST in place.
- Not at parity, so Rust stays the default.

## Stage 1 guard

This part edits no file under `src/`, `bootstrap/`, `lean/`,
`packages/vesper-compiler/` or any crate: only the new package, the new test,
`BUILD.bazel` and this report. The verified backend and its inputs are
untouched. The bootstrap-stage1 workflow runs on the branch once #1361 has
merged and `main` is merged in (the project's CI rule).

## Compiler gaps

No new gaps. Two known ones shaped the code:

1. A slice is second-class: `let lines = x.lines.as_slice();` is refused
   with E1521, so the line table is lent as `in x.lines.as_slice()` at each
   call. This is the other side of lane 16's gap 4, where binding the view
   first is the workaround.
2. Lane 16's gap 2 (a variant constructor in a `handle` body leaves a type
   parameter as `_`): every `handle Fail[SpanError]` site uses typed
   constructors (`range_ok`, `offset_ok`, `line_ok`).

One trap for later lanes: `@vesper/ast`'s `list_len` counts words, so a list
of pairs (match arms, struct fields, constructor pattern fields) has
`list_len / 2` elements. Reading past the end with `list_at` returns another
list's words, not `NONE`.

## What is left

- **Part 2**: hover (needs the checker facts: `expression_types`, the
  World's function results and parameter types, ADT modes and
  `World::mode`), the documentation-link fallback (needs
  `vesper_doc::links`), and signature search (`src/lsp/search.rs`).
- **Part 3**: code actions, Anvil actions, formatting edits and macro
  preview (`actions.rs`, `anvil_actions.rs`, `macro_preview.rs`).
- **Part 4**: lenses, the proof lens and model, completion, inlay hints and
  the 2 ms lens budget measured on the Vesper build (`lens.rs`,
  `proof_lens.rs`, `proof_model.rs`, `session/lens_requests.rs`), and the
  editor front-end glue (`src/tooling/frontend.rs`, `recovery.rs`).
- **The hybrid module `lsp_features`**, after lane 16's `lsp_core`.
- **Speed**: the index in flat slices instead of per-element `try_get`.
