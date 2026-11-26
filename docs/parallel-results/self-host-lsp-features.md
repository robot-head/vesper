# Self-hosting SH5 lane 17: LSP features, part 1 (navigation)

Claim #1340. Built on lane 16's PR #1361, which merged into `main` at
`81cdb52`; that `main` is merged into this branch.
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
- `tests/fixtures/self_host/driver.rs`: `self_host_front_end` skips the new
  package, like the other `pub fn`/`use` packages (#880).
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
untouched. The bootstrap-stage1 workflow runs on the branch now that #1361
has merged and `main` is merged in (the project's CI rule).

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

# Part 1b: navigation under the flip bar (#1828)

Claim #1828 (the tooling and LSP chunk: `src/lsp/`, `src/tooling/`,
`src/database.rs`), continuing this lane. Rust stays the default: hover and
the documentation-link fallback are still Rust-only, so `navigation.rs`
cannot go yet.

## What changed

- `nav.ves` `spells` compares the source bytes of a token with an
  interned name through `Interner::symbol_eq` on the source sub-string, in
  place. It used to copy nothing but read both sides one `Option` per byte
  through `String::as_bytes` and `text_byte`; that one loop was about 60%
  of the per-request index build (found by stubbing pieces of `reindex`).
- `lib.ves`: the documentSymbol answer is one function
  (`symbols_answer`), and a new entry `lsp_document_symbols_kept` answers
  `n` requests on one parse with the index rebuilt per request, the server
  shape `lsp_definitions` already had for definitions.
- `tests/self_host_lsp_features.rs`: `navigation_matches_rust` also checks
  three kept answers per document and shape against Rust; the speed test
  has a kept documentSymbol row.

## Differential

`cargo test --release --test self_host_lsp_features`: 0 diffs.

| Test | Compared | Differences |
|---|--:|--:|
| `navigation_matches_rust` | 194 documents: 388 symbol answers (each also as 3 kept answers), 52,991 definitions (2,100 targets, 80 doc-link fallbacks deferred), 232 workspace queries | 0 |
| `sessions_match_server` | 46 symbol, 758 definition and 24 workspace responses | 0 |

## Numbers

ABBA, `CHECK_HARNESS_SPEED=1 cargo test --release --test
self_host_lsp_features lsp_features_speed -- --ignored`, 4-core Linux cloud
host. "Before" is this branch's base (`main` at `eb17555`) on the same host.

| Request | Rust | Vesper before | Vesper after | Ratio after |
|---|--:|--:|--:|--:|
| `definition`, parse kept (`lsp_definitions`), index rebuilt per request | 11.7 µs | 23.4 µs (1.96x) | 9.6 µs | **0.82x** |
| `documentSymbol`, parse kept (`lsp_document_symbols_kept`, 32 per document) | 61.7 µs | n/a | 40.2 µs | **0.65x** |
| `documentSymbol`, Vesper parsing each time | 58 µs | 128 µs (2.19x) | 101 µs | 1.50x |
| `definition`, Vesper parsing each time | 11.6 µs | 153 µs | 143 µs | 12x |

The kept rows are the fair ones: the Rust side starts from the checked
snapshot, whose AST is already parsed, and builds `Index::new` per request;
the Vesper kept entries do the same (one parse, one index per request). The
"parsing each time" rows are dominated by `@vesper/front`'s parse.

## Still Rust-only in navigation.rs

- `hover`: needs the checker's facts (`expression_types`, the World's
  function results, parameter types and ADT modes). `@vesper/check-data`'s
  `World.expr_types` (Stage 2) is the candidate source.
- The documentation-link fallback (`navigation/doc_links.rs`) over
  `vesper_doc::links` (crates/vesper-doc/src/links.rs, not ported).

# Part 2 (#1828): hover and documentation links

navigation.rs is now fully ported. Hover and the doc-comment link fallback
answer in Vesper with 0 diffs, and every request is within the 1.5x bar.
Rust stays the default until the hybrid wiring part.

## What changed

- `packages/vesper-lsp-features/src/hover.ves`: `hover_answer` over the
  checker's facts. The facts are the `@vesper/check-data` harness context
  (types, World), plus the expression types and a table of the source's
  functions and parameters. It covers the type and effect printers, ADT
  modes with the Rust work budget charge for charge, signatures, selectors
  and `///` docs.
- `doc_links.ves`, `links.ves`, `markdown.ves`: `navigation/doc_links.rs`
  and `vesper_doc::links::analyze`. They include the declaration inventory
  with its counting-down budget, and the Markdown scan with the counting-up
  link budget. Each budget charges the Rust `size_of` of every value and
  the exact capacity growth, so budget errors match too. The code is split
  across three files because `vesper fmt` has a per-file token limit.
- New entries: `lsp_hovers` (facts, then n cursors), and `lsp_definitions`
  now takes doc facts. The single `lsp_definition` entry answers `D`
  (defer) on a doc comment when it has no facts.
- The test encodes the facts from `CheckedSource`, and checks every UTF-16
  position on every doc-comment line. A `doc_link_budget_sizes` test pins
  the Rust sizes that the budgets charge. Generated `doc_*` sources reach
  the work, metadata and token limits; they stay out of the speed corpus.

## Differential

`cargo test --release --test self_host_lsp_features`: 4 passed, 0 diffs.

| Test | Compared | Differences |
|---|--:|--:|
| `hover_matches_rust` | 52,991 hovers (29,665 with contents) | 0 |
| `navigation_matches_rust` | 68,796 doc-comment cursors (282 with targets), 52,350 budget errors, plus the part 1b set | 0 |

## Numbers

| Request (parse kept) | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| `hover` (includes encoding and decoding the facts once per document) | 14.3 µs | 16.8 µs | **1.18x** |
| `definition` | 11.6 µs | 9.9 µs | **0.85x** |
| `documentSymbol` | 60.4 µs | 38.7 µs | **0.64x** |

Rust lines deleted: 0. Rust code is deleted only after the hybrid swap
(session and dispatch part).

# Part 3 (#1828): proof-lens models

`src/lsp/proof_model.rs` (`proof_model::all`: the scalar model of each
checked function, with its exact-integer and bit-vector Lean renderings and
the model key) is ported, with 0 diffs. Rust stays the default until the
hybrid wiring part.

## What changed

- `packages/vesper-lsp-features/src/proof.ves`: the builder (`Builder`:
  scopes, kinds from the checker's expression types, inlining, the node
  budget and every reason a function has no model) and the entry
  `lsp_proof_models`.
- `proof_types.ves`: `Kind::of` of source types, and `Display` and `Debug`
  of `ast::Type` for the reasons.
- `lean.ves`: `Exact::render` and `BitVecs::render`, byte for byte.
- The checker's functions come from `World::functions`, with their spans.
  Their trees are parsed from one text: the source, then each prelude the
  checker added (String, Atomic, workers). A per-function `delta` maps that
  text's spans back to the checker's, because preludes are parsed at an
  offset past the source.
- The key is BLAKE3 over both renderings. The entry hashes it with the
  portable `@vesper-std/hash` BLAKE3, or leaves it to the host. The hosted
  build would hash through `@vesper-std/hash-sys` (the C BLAKE3), as the
  formatter port does.
- `src/lsp.rs`: a hidden `proof_models_for_port` hook for the test (the
  module is private).
- Generated sources cover every scalar shape and each reachable reason.
  These include the inlining depth limit (8), the node budget (4,096),
  17 parameters, a disallowed effect and a non-scalar `let`.

## Differential

`proof_models_match_rust`: 203 documents, 490 models and 507 reasons, 0
diffs. Keys hashed by the host also match.

## Numbers

Per document round, with the parse and facts kept (the session builds every
model once per snapshot). The one-time encode, decode and parse is timed
apart and taken off.

| Key hashed by | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| the host's C BLAKE3 (the hosted build's `@vesper-std/hash-sys`) | 18.3 µs | 17.5 µs | **0.96x** |
| the portable Vesper BLAKE3 | 18.3 µs | 32.5 µs | 1.78x |

The portable BLAKE3 alone is about 15 µs of a round, so the hybrid wiring
must hash through hash-sys. Rust lines deleted: 0.

# Part 4 (#1828): Lens ownership facts

The ownership facts behind every Lens view in `src/lsp/lens.rs` are ported
with 0 diffs, at 0.76x Rust's time. That covers the `facts` walker
(`Walker`), and the live linear obligations at a cursor
(`View::outstanding` through `live` and `resolve`). Rust stays the
default until the hybrid wiring part.

## What changed

- `packages/vesper-lsp-features/src/ownership.ves`: the walker over the
  source's tree (@vesper/ast). It records each binding of each checked
  function with its origin, scope, type, linearity and ownership. It also
  records each consumption with the branch arms around it. The walker
  follows the Rust rules for:
  - parameter passes (explicit, from the callee, else `sink`)
  - method receivers (`Type::method` with a `self` first parameter)
  - effect operations (`Effect::op`)
  - function values (their type's passes)
  - constructor patterns (field types with the ADT's arguments
    substituted, or `_` when an unknown argument would decide the mode)
  - closures and handler clauses
  - loops and early exits, which make the document opaque

  The resource budget is the same: 2^18 steps, and 4,096 bindings per
  function. The queries use the same rules: the function at a cursor is the
  shortest one containing it, and the innermost binding of a name wins.
- Types are check-data type ids. Annotations are interned into the
  checker's type table, an unannotated `let` takes the type the checker gave
  its value, and the mode is `World::mode` (`mode_of`). The harness lays out
  the callee table from `World::functions` and `World::effects`, with each
  parameter's pass.
- Two lookups are cached per parse, as Rust caches modes per world: the
  mode of each type and `World::constructor` for each name. A query index
  per view holds each function's owned linear bindings and allows a binary
  search over the function spans when they are in order.
- `src/lsp/lens.rs`: `facts_dump` writes the facts and the answers at given
  cursors as text. `src/lsp.rs` adds a hidden `lens_facts_for_port` hook
  for the test.
- `tests/self_host_lsp_features.rs` adds `lens_facts_match_rust` and a speed
  row. It also adds generated Lens sources, which cover:
  - the linear transaction of `src/lsp/lens_tests.rs`, with lets, branches,
    shadowing, a closure value, destructuring, a handler and an early exit
  - generic payloads (`Option` and a generic linear enum), match arms,
    `in` and `sink` parameters and struct fields

  Generic constructors send the editor guard through tuple admission, which
  refuses inherent `self` receivers, so those sources use free functions.

## Differential

`lens_facts_match_rust` covers 212 documents, 8 of them without facts
(loops or early exits). In all, 955 functions, 1,939 bindings (140 linear)
and 877 consumptions are compared at 42,536 cursors, with 0 diffs. The
cursors are every offset a fact names and its neighbours, plus a stride of
29 bytes over each source.

## Numbers

Each row is one view and its queries per document round, with the parse and
facts kept. The one-time encode, decode and parse (273 µs per document) is
timed separately and taken off.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| facts and outstanding obligations at every cursor | 25.1 µs | 18.9 µs | **0.76x** |
| facts only | 6.2 µs | 7.6 µs | 1.22x |

Rust lines deleted: 0.

## Left in lens.rs

- The repaired view of syntax-damaged functions (`repair`, `candidates`,
  `open_at`; part 5), and the semantic-error view.
- Member completion (`complete_member`) and `signature`.

# Part 5 (#1828): Lens repair of syntax-damaged source

The repaired view's text and regions in `src/lsp/lens.rs` are ported with
0 diffs, at 0.92x Rust's time. That covers `repair`, `candidates`,
`open_at`, `closers`, `apply` and `parses`. Rust stays the default until
the hybrid wiring part.

## What changed

- `packages/vesper-lsp-features/src/repair.ves` (entry `lsp_lens_repair`)
  follows the Rust rules:
  - Each damaged declaration gets its candidate cuts, most specific first:
    - a statement cut at each enclosing block, innermost first (in an
      impl, first keeping the later methods)
    - a header cut at the last complete parameter, keeping the body
    - a member cut for structs, enums and effects
  - Each candidate is validated by parsing it alone: the declaration with
    its patch, and every other damaged declaration blanked. Parses are
    capped at 64 across all declarations.
  - The chosen cuts are then applied together, keeping every byte offset.
  - Region names, cuts, scopes and the unfinished-code span are worked out
    as in Rust, including Unicode white space after the cut.
  - Every `Unavailable` reason is reported as in Rust.
- The tokens and the validating parses come from @vesper/front
  (`lex_with_limits`, `parse_tokens`). The recovery parse's declarations,
  sites and diagnostic spans come from the harness, as the checker's facts
  do for the other entries (@vesper/front's `parse_recovery_tables` is
  tested separately against Rust).
- `src/lsp/lens.rs`: `repair_dump`. `src/lsp.rs`: a hidden
  `lens_repair_for_port` hook.
- `tests/self_host_lsp_features.rs` adds `lens_repair_matches_rust` and a
  speed row. The damaged sources are:
  - Lens's own scenarios: an unfinished member access, `let`, nested
    block and match arm; a damaged impl method; a broken parameter list;
    an unfinished struct member
  - variants of every corpus document: an unfinished `let`, member
    access, call or `if` at a few statement ends; truncated prefixes; a
    dropped `)` in the first signature; an unfinished struct member

## Differential

`lens_repair_matches_rust` ran on 1,043 sources with a recovery parse (10
more have none). Rust repaired 670 of them, with 621 regions. The other
373 have no repaired view:

| Reason | Sources |
|---|--:|
| `repaired_prefix_does_not_parse` | 216 |
| `opaque_declaration` | 121 |
| `damaged_function_header` | 34 |
| `damaged_non_function_context` | 2 |

The Vesper port matched every regions list and repaired text, with 0 diffs.

## Numbers

One repaired view per damaged document, including its validating parses.
The one-time encode and decode is timed separately and taken off.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| repair, per damaged document | 51.2 µs | 47.4 µs | **0.92x** |

Rust lines deleted: 0.


# Part 6 (#1828): Lens views of semantic-error and repaired code

The Lens views of source with semantic errors and of repaired prefixes in
`src/lsp/lens.rs` are ported with 0 diffs, at 1.04x Rust's time. Each view
comes with its queries:

- `View::from_text`, with the scope and the count of absorbed diagnostics
- `outstanding`, read at the cut inside a damaged body
- `poisoned` and `binding`
- `complete_member`, with `signature` and the `rank_methods_within`
  completion ranking of `crates/vesper-lsp`, under an optional Loom.toml
  capability ceiling

Rust stays the default until the hybrid wiring part.

## What changed

- `packages/vesper-lsp-features/src/ownership.ves` adds the entry
  `lsp_lens_view`. It reuses the part 4 facts walker on the view's text.
  - From the harness it takes the editor guard's outcome, the regions and
    the checker's diagnostic spans, along with the checker's facts.
  - `World::functions` now also carries each parameter's name and type,
    the result type and the effect instances. `World::effects` carries
    the linear flags.
  - The query layer follows Rust: region-aware `function_at` and
    `region_function`, absorbed diagnostics, the poisoned sets, name
    resolution, the candidate methods, their signatures, ceiling grants,
    and ranking with a stable sort by tier, then label.
- The callee input is now read by shared `read_callees`/`link_callees`
  helpers. `lens_facts` is unchanged otherwise.
- `src/lsp/lens.rs`:
  - The guard, parse and check moved into `Prepared`, and
    `View::from_prepared` builds the facts. The repaired basis holds an
    `Arc<Checked>`, so a prepared text can be viewed again.
  - Adds `view_dump`, `prepared_dump` and `regions_from_dump`.
  - `src/lsp.rs` adds the hidden hooks `lens_view_for_port` and
    `lens_view_rounds_for_port`.
- `tests/self_host_lsp_features.rs` adds `lens_view_matches_rust` and a
  speed row.

## Differential

`lens_view_matches_rust` ran on 1,137 views, each with and without a
capability ceiling:

- 212 corpus documents
- 255 semantic-error sources: 4 Lens scenarios (a leaked, a
  double-consumed and a blocked transaction, and an unknown callee), and
  251 corpus documents each missing one of a few statements
- 670 repaired prefixes of the part 5 damaged sources

The queries asked at each view are:

- `outstanding` at every fact offset, its neighbours, a 29-byte stride
  and around each cut
- `binding` and `complete_member` at each binding's origin end and scope
  end, and just after each cut

511,478 rows matched with 0 diffs:

- 632 poisoned sets
- 6,250 ranked completions, covering all four tiers: 1,418 discharge a
  live linear value, 3,939 are frame-compatible, 234 are blocked by the
  frame and 659 exceed the ceiling
- every scope (checked by the Lens profile, semantic-error partial facts,
  recovered prefix) and every `Unavailable` reason

## Numbers

Each row is one view and its answers per document round. On both sides
the editor guard, parse and check are done once and the view is rebuilt
32 times. The Vesper port takes the checker's facts as input, and its
one-time encode, decode and parse is taken off.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| view and queries, per document | 35.1 µs | 36.4 µs | **1.04x** |

Rust lines deleted: 0.

## Left in lens.rs

Nothing in the presentation analysis. The proof lens requests
(`src/lsp/proof_lens.rs`) and the session's use of these views are next.

# Part 7 (#1828): the Lens request layer

The Lens request layer of the server (`src/lsp/session/lens_requests.rs`)
is ported with 0 diffs, at 0.85x Rust's time. It answers on a document and
its Lens view:

- the poison notices (`VESPER_POISONED_RECOVERY`) and the Lens metadata
  that ride on `publishDiagnostics`
- `textDocument/completion`: the `receiver.` scan, then the ranked member
  completions as LSP items (sort text, tier data, badge, deprecated tags)
- the obligation text that hovers append, with the consuming calls
- `textDocument/inlayHint`: one hint per owned linear binding in the range
  (consumed on line N, outstanding, or poisoned)

Rust stays the default until the hybrid wiring part.

## What changed

- `src/lsp/session/lens_requests.rs`: the request bodies moved out of
  `Session` into `poison_of`, `completion_of`, `obligation_text_of` and
  `inlay_hints_of`, which take the text, line index and view. The session
  methods call them, so the server's answers are unchanged.
  `requests_dump` writes their answers as JSON rows, and `src/lsp.rs`
  adds the hidden hook `lens_requests_for_port`.
- `packages/vesper-lsp-features/src/ownership.ves` adds the entry
  `lsp_lens_requests`. It reads a view's input as `lsp_lens_view` does
  (status 5 is a checked snapshot's view, scope `checked`), then the
  document, its line-index limits, the regions' names, depths and damage,
  and the requests. Positions go through `@vesper/span`.
  `complete_member` is split into `rank_members` and its row writer, so
  completion reuses the ranking. `lens_view` now reads its input with the
  shared `view_setup`.
- `tests/self_host_lsp_features.rs` adds `lens_requests_match_rust` and a
  speed row.

## Differential

`lens_requests_match_rust` ran on 1,137 documents (212 checked corpus
documents, 255 semantic-error sources and 670 damaged sources answered on
their repaired prefixes), each with and without a capability ceiling. The
requests per document are completion just after each `.` and one
character later, hovers at 48 evenly spaced characters, inlay hints over
the whole document, each half, and a range past the end, plus a position
past the last line in each list.

155,498 answers matched with 0 diffs: 108 poison notices, 4,528
completion items, 8,820 obligation hovers, 3,812 inlay hints and 8,084
error answers (position errors and unavailable views).

## Numbers

Each row is one round of every request on one view per document. On both
sides the view is built once and taken off.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Lens requests, per document | 33.6 µs | 28.5 µs | **0.85x** |

Rust lines deleted: 0.

## Left in the Lens

- `proof_lens.rs`: the Lean runner is ported in part 8 below.
- The code lenses and `vesper.proofAudit` command, which wait on the
  proof runner.

# Part 8 (#1828): the proof-lens Lean runner

The runner of `src/lsp/proof_lens.rs` is ported to the new package
`@vesper/lsp-proof-run` with 0 diffs, at 0.87x Rust's time end to end. It
takes a prepared job and does what `proof_lens::run` does:

- finds Lean through the launcher and requires Lean 4.35.0
- compiles the extraction module and each semantic model in a private
  staging directory (mode 0o700), with only `LEAN_PATH` and
  `LEAN_ABORT_ON_PANIC` in the environment
- reuses `.olean` files and recorded outcomes from the content-keyed cache,
  and makes the cache directory private as `Cache::open` does
- runs the `bv_decide` rendering when `omega` fails, and renders a
  counterexample as source values
- replays every theorem through one `vesper proof-audit` run and reads its
  JSON report with serde_json's rules, error messages included

Each child runs in its own process group, with both streams captured up to
8 MiB, and is killed with its tree at the deadline. This uses
`@vesper-std/process` timeouts (#1991) and the fs modes, permissions and
copy of #2012.

Rust stays the default until the hybrid wiring part. Cancellation from the
server's `CancelToken` is not ported yet; the wiring adds it.

## What changed

- `src/lsp/proof_lens.rs` adds `job_words`, the prepared job as words, and
  `outcome_rows`, an outcome as text rows. `src/lsp.rs` adds the hidden
  hooks `proof_job_for_port` and `ProofJobForPort` (`words`, `modules`,
  `run`). `proof_model::Kind` gets a `code`.
- `packages/vesper-lsp-proof-run`: `lib.ves` (job, files, children, cache,
  stages, the `proof_run` entry), `json.ves` (serde_json `from_slice`),
  `text.ves` (lossy UTF-8, trimming, `first_error`, error displays) and
  `numbers.ves` (a copy of `@vesper/lsp`'s number range check; depending
  on `@vesper/lsp` puts `@vesper/serde`'s `Kind::String` in the scope of
  `String::` paths, a resolver bug worth its own issue).
- `tests/self_host_proof_lens.rs` adds `proof_runs_match_rust` and the
  ignored `proof_run_speed`.

## Differential

Both sides run the same job against a fake toolchain of shell scripts: a
launcher, a `lean` whose compiles succeed, fail, stall or report a
`bv_decide` counterexample per module, and an auditor that builds a report
from its `--theorem` arguments or replays a fixed one. The test compares
the outcome rows, a log of every Lean and auditor call (arguments,
directory, `LEAN_PATH`, environment), and the cache tree with permission
bits. Cached scenarios run twice on each side's own cache, then once on
the other side's cache.

53 scenarios matched with 0 diffs (927 rows and log lines; about 4.5 min):

- all proved, with and without the cache
- no auditor, a missing or failing launcher, the wrong Lean version
- an extraction or run module that does not compile, a compile timeout
- omega failing with bv_decide finding nothing, a counterexample (hex,
  decimal, `+`, CRLF lines, noise lines, values that do not parse,
  overflow), a bv_decide failure with and without output, a bv_decide
  timeout
- forbidden or non-string axioms, a failing or stalled auditor
- 34 fixed reports: kernel not replayed, missing or duplicate fields,
  non-UTF-8 bytes, every serde_json syntax error class, numbers out of
  range, trailing characters, and nesting at and past the 128 limit

All scenarios run in the JIT with the optimizer on and off. The evaluator
runs the 19 toolchain scenarios and the first 4 fixed reports.

## Numbers

One all-proved job (2 obligations, 3 models: 5 Lean compiles and one
audit), JIT, mean of 20 runs. Both sides are dominated by starting the
child processes.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Proof run, all proved | 128.7 ms | 111.4 ms | **0.87x** |

Rust lines deleted: 0.
