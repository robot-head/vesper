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
- The code lenses and `vesper.proofAudit` results are ported in part 9
  below.

# Part 8 (#1828): the proof-lens Lean runner

The runner of `src/lsp/proof_lens.rs` is ported to the new package
`@vesper/lsp-proof-run` with 0 diffs, at 0.28x Rust's time end to end. It
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
  ignored `proof_run_speed`, over the fake toolchain in
  `tests/fixtures/proof-lens-toolchain`.

## Differential

Both sides run the same job against a fake toolchain: one native Vesper
program (`tests/fixtures/proof-lens-toolchain`, built by `vesper build`)
that acts as the launcher, as a `lean` whose compiles succeed, fail, stall
or report a `bv_decide` counterexample per module, and as an auditor that
builds a report from its `--theorem` arguments or replays a fixed one. The test compares
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
audit, 8 child processes with the launcher and `--version`), JIT, mean of
20 runs, against the native fake toolchain. Rust polls each child every
5 ms (`try_wait`, then `thread::sleep`) and reads both pipes on two
threads per child; `wait_timeout` in `@vesper-std/process` waits for the
child, so most of the gap is Rust's polling.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Proof run, all proved | 45.3 ms | 12.5 ms | **0.28x** |

Rust lines deleted: 0.

# Part 9 (#1828): proof code lenses and `vesper.proofAudit` results

The proof-lens answers of the server (`src/lsp/session/lens_requests.rs`)
are ported with 0 diffs, at 1.10x Rust's time:

- `textDocument/codeLens`: an extraction lens per proof-lens function (at
  most 256) and a semantic lens per function with a model, with the
  status titles, command arguments and `data.vesper` fields the server
  sends, a model's reason cut to 80 characters
- the two bounded proof ledgers (`ProofLedger` in
  `crates/vesper-lsp/src/lens.rs`): outcomes keyed by source or model key
  and function, with the oldest entry evicted when a ledger is full
- which outcomes are recorded: a semantic failure before the prover ran
  (toolchain or I/O) is dropped
- the `vesper.proofAudit` command result: the requested obligation's
  outcome with the cache counts and time, not attempted when the job has
  none for it, null for a job no command asked for
- the `vesper/proofStatus` parameters of a finished background run, and no
  notice when the buffer changed while it ran

Rust stays the default until the hybrid wiring part. The session state
around these answers (argument validation of `workspace/executeCommand`,
the debouncer and pending-request table) is a later part.

## What changed

- `src/lsp/session/lens_requests.rs`: the code-lens body moved out of
  `Session` into `code_lenses_of`, which takes the line index, source hash,
  functions, model keys and both ledgers. Recording, the command result
  and the background notice moved into `record_of`, `proof_result_of` and
  `proof_status_of`, and the function list into `lens_functions`. The
  session methods call them, so the server's answers are unchanged.
- `src/lsp.rs` adds the hidden hooks `proof_lens_inputs_for_port` (the
  functions and model keys the session reads) and `proof_lenses_for_port`
  (a sequence of `ProofLensOp`s over fresh ledgers, as JSON rows).
- `packages/vesper-lsp-features/src/proof_lenses.ves` adds the entry
  `lsp_proof_lenses`: the ledgers, the outcome filter and the three
  writers. Strings are copied from the input in runs between escaped
  bytes, eight bytes at a time.
- `tests/self_host_lsp_features.rs` adds `proof_lenses_match_rust` and the
  ignored `proof_lenses_speed`.

## Differential

`proof_lenses_match_rust` runs one session on each of the 213 corpus
documents with proof-lens functions. A session renders the lenses with
empty ledgers, records five command jobs (extraction and semantic
requests, an unrequested job, and requests for a missing function), then
two background runs, one for the current buffer and one for an edited
buffer, and renders the lenses again for the current and the edited
source. Outcomes cover every status, failures at the prover, kernel,
toolchain and cache, a stale model key, and texts that need escapes. The
ledgers are roomy (1024 and 4096), small enough to evict (3 and 2), or
single-entry. Every third document has a model reason past 80 characters
with multi-byte characters at the cut, and every seventh a function span
past the end of the document.

7,920 lenses, 1,065 command results, 213 background notices and 175 span
errors matched with 0 diffs.

## Numbers

Each row is one session per document: 12 steps on fresh ledgers. On both
sides the one-time setup is taken off.

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Proof-lens session, per document | 334.7 µs | 368.2 µs | **1.10x** |

Rust lines deleted: 0.

# Part 10a (#1828): debounce, pending requests and cancel tokens

The Lens session's request bookkeeping in `crates/vesper-lsp/src/lens.rs` is
ported with 0 diffs, at 0.45x Rust's time:

- `Debouncer`: per-document generations, deadlines and one armed timer per
  document, refusal past the document bound, and the run token each
  generation keeps, cancelled by a newer edit, a newer run, a close or
  server exit
- `PendingRequests`: unanswered requests in table order, duplicate and
  capacity refusals, cancellation, answers only for the job that began the
  request, and exit cancellation in table order
- `CancelToken`: tokens numbered as they are made, each live or cancelled
- the limits `Debouncer::new` and `PendingRequests::new` accept

`cancel_params` and the `vesper.proofAudit` argument checks read
JSON-RPC parameters, so they move to part 10b over the `@vesper/serde`
token tape that `@vesper/lsp` parses messages into. Rust stays the default
until the hybrid wiring part.

## What changed

- `packages/vesper-lsp-features/src/lens_state.ves` adds the entry
  `lsp_lens_state`, which runs a sequence of operations and writes a row
  per operation and the final flag of every token.
- `tests/self_host_lsp_features.rs` adds `lens_state_matches_rust`, which
  drives the Rust types directly (times as offsets from one `Instant`), and
  the ignored `lens_state_speed`. No Rust hook was needed.

## Differential

140 seeded sessions, 34,372 operations: valid and invalid limits (a zero,
4,097 documents, 1,025 requests, a delay past one minute), edits and
timers that fire early, on time and late over six URIs, runs started for
current and stale generations, closes, integer IDs at the i32 bounds and
string IDs that repeat, requests answered by the right job and a stale
one, and exit cancellation. 1,458 timers armed, 754 fired, 2,684 errors
and 1,633 cancelled tokens matched with 0 diffs.

## Numbers

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Bookkeeping session, per session | 10.2 µs | 4.6 µs | **0.45x** |

Rust lines deleted: 0.


# Part 10b (#1828): parameter checks

The session's parameter checks now run in `@vesper/lsp` over the
`@vesper/serde` token tape, with 0 diffs, at 1.28x Rust's time:

- `validate_input` (src/lsp/session.rs): the method label (empty, over 256
  bytes, or holding a C0 or C1 control), then a walk of the parameters
  that refuses past 64 levels, 2 MiB of key and string bytes, or 65,536
  nodes. Rust walks a `serde_json::Value` whose objects are `BTreeMap`s,
  so the walk here visits members in key order too, and the first bound
  broken is the same one. A linear scan of the tape passes values that
  are within every bound however they are walked, which is nearly all of
  them. The walk counts an object's members before reading its keys, as
  Rust does, and orders them with a merge sort.
- `cancel_params` (crates/vesper-lsp/src/lens.rs): an `i32` id, or a
  string id of at most 1,024 bytes.
- The `vesper.proofAudit` checks of `workspace/executeCommand`, in Rust's
  order: the command, one argument object, its allowed keys, `uri` and
  `function` (at most 1,024 bytes), `obligation` (`extraction` or
  `semantic`), the open document, and the expected version (`-32801` when
  it changed).

## What changed

- Rust: the argument checks of `Session::execute_command` move unchanged
  into the free function `proof_command`
  (src/lsp/session/lens_requests.rs), which `execute_command` calls. The
  hidden hook `vesper::lsp::command_checks_for_port` runs all three checks
  as rows.
- `packages/vesper-lsp/src/commands.ves` adds the entry `lsp_commands`.
  `parse_i32` and `negative_zero` in envelope.ves are now public, so
  integer ids and versions follow serde_json's number rules as they do
  for envelope ids.
- `tests/self_host_lsp_core.rs` adds `commands_match_rust` and the ignored
  `commands_speed`.

## Differential

635 checks: 441 `validate_input` cases, 30 cancel parameter cases and 164
`vesper.proofAudit` calls. They cover:

- method labels at the 256-byte bound and with each kind of control
- depths 63 to 66, and 65,535 to 65,537 nodes
- strings and keys at 2 MiB
- objects whose key order decides which bound breaks first, including
  escaped keys and objects of 1,000 and 4,000 members in reverse order
- 400 seeded random values
- ids and versions at the i32 bounds, `-1`, `-0`, floats and exponents
- escaped strings at the 1,024-byte bounds
- every argument refusal

171 refusals and 464 passes matched with 0 diffs.

## Numbers

Rust is timed on parameters that are already `serde_json::Value`s.
Vesper parses each tape once per 64 rounds. The 2 MiB and 65,536-node
cases are left out of the timing.

| Checks | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| `validate_input` (437) | 52 ns | 64 ns | 1.24x |
| `cancel_params` (30) | 49 ns | 154 ns | 3.12x |
| `vesper.proofAudit` (137) | 216 ns | 261 ns | 1.21x |
| All 604 | 88 ns | 112 ns | **1.28x** |

The cancel cases are slow because of four 1 KiB string ids, two of them
with escapes. serde's string decoder copies those a character at a time,
where Rust clones the string. Without parameters over 200 bytes the cancel
checks run at 0.64x and all checks at 1.08x.

Rust lines deleted: 0. The checks are not wired: Rust stays the default
until the hybrid wiring part.

# Part 11a (#1828): the formatting action

`textDocument/formatting` (src/lsp/actions.rs) now runs in the new
package `@vesper/lsp-format`, over the formatter port `@vesper/fmt`, with
0 diffs, at 0.58x Rust's time:

- The action document checks of `ActionDocument::new`: the line table,
  the URI (1 to 4,096 bytes) and the 128 KiB source bound.
- The stamp check: an expected version and BLAKE3 source digest must both
  match, or the answer is `document version/source identity changed`.
- `format_document`: the canonical formatter with its 128 KiB, 64 Mi work
  limits. A changed document gives one whole-document edit; an unchanged
  one gives none.
- On a syntax error or an opaque macro, `format_complete_declarations`:
  the recovery formatter's edits to the complete declarations, refused
  past 256 edits, past 128 KiB of edit text, when no declaration was
  formatted, or when an edit range is out of order or off a character
  boundary.
- The JSON of `Formatting`: the edits, the workspace edit and the
  provenance (`source_blake3`, the declaration count and the scope of a
  recovery), with sorted keys as serde_json writes `json!` maps.

The action is its own package because `@vesper/fmt` links C BLAKE3
(`@vesper-std/hash-sys`), which a Wasm entry does not have. Adding it to
`@vesper/lsp-features` would make that package fail under Stage 2. The
action hashes the source with the portable BLAKE3 of `@vesper-std/hash`
and never reaches a foreign call.

## What changed

- Rust: the hidden hook `vesper::lsp::formatting_for_port` builds the
  action document and runs `format_document`, as the session does.
- `packages/vesper-lsp-format` (new): `formatting.ves` and the entry
  `lsp_formatting` in `lib.ves`. `put_json`, `lines_of` and `put_pos` in
  `@vesper/lsp-features` (proof_lenses.ves) are now public.
- `tests/common/check_harness.rs`: a package whose dependencies bind
  `.vsys` libraries is checked with their imports
  (`ffi_lowering::check_module`).
- `tests/self_host_lsp_features.rs` adds `formatting_matches_rust` and the
  ignored `formatting_speed`.
- `tests/wasm_seed_package_io.rs` lists the package as covered by the JIT
  test instead of Stage 2.

## Differential

434 cases. Every seventh `.ves` file under `packages/` up to 128 KiB,
as it is and in five variants: re-indented, CRLF line ends, an unclosed
function appended, `) ) )` inserted in the middle, and cut in half. Their
versions run from -40 upward. Hand-written cases cover empty documents,
multi-byte text, comments, broken declarations between good ones, URIs
at 0, 4,096 and 4,097 bytes and with escapes, stale versions and digests,
and sources at and over the 128 KiB bound.

151 whole-document replacements, 45 recovery proposals, 73 unchanged
documents and 165 refusals matched with 0 diffs.

## Numbers

Both sides build the action document and format it on every round, 4
rounds per case, timed ABBA.

| Cases | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| All 428 (before merging main) | 3.72 ms | 2.17 ms | **0.58x** |

Rust lines deleted: 0. The action is not wired: Rust stays the default
until the hybrid wiring part.

# Part 11b (#1828): the Vesper Anvil code actions

`anvil_code_actions` (src/lsp/anvil_actions.rs) now runs in
`@vesper/lsp-features` (`anvil.ves`, entry `lsp_anvil_actions`), with 0
diffs, at 0.85x Rust's time:

- the `vesper.anvilRun` command action for one `@test` function;
- with failing arguments, the quickfix that inserts
  `@test_case(<args>)` above the function. The arguments are trimmed with
  `str::trim`, which removes Unicode white space. The insertion line is
  clamped to the number of `str::lines`. The indent is copied from the
  leading spaces and tabs of that line.

The JSON is written with sorted keys, as serde_json writes `json!` maps.

## What changed

- No Rust changes. The test calls `anvil_code_actions` directly.
- `packages/vesper-lsp-features/src/anvil.ves` (new) and the entry
  `lsp_anvil_actions` in lib.ves.
- `tests/self_host_lsp_features.rs` adds `anvil_actions_match_rust` and
  the ignored `anvil_actions_speed`.

## Differential

1,809 cases, 1,448 of them with failing arguments. They cover:

- every fifth package `.ves` file, at up to six lines that start an
  `@test` or a function, the first and last lines, and one and two lines
  past the end;
- 15 hand-written sources: empty, newline-only, CRLF, tab and mixed
  indents, NBSP and ideographic indents (not copied), a lone `\r`, and
  multi-byte text;
- at lines 0 to 3, 99 and `u32::MAX`, each with 10 kinds of arguments:
  none, empty, blank, Unicode white space at both ends (U+3000, U+2028,
  U+0085, U+00A0, U+2000 to U+200A, and U+200B, which is not trimmed),
  quotes, backslashes, control bytes and emoji;
- URIs and names with escapes, control bytes and spaces, and empty ones.

All 1,809 cases matched.

## Numbers

Both sides build the actions and write their JSON on every round, 16
rounds per case, timed ABBA.

| Cases | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| All 1,809 | 6.60 µs | 5.64 µs | **0.85x** |

Rust lines deleted: 0. The actions are not wired: Rust stays the default
until the hybrid wiring part.

# Part 11c (#1828): the macro preview

`vesper/macroPreview` (src/lsp/macro_preview.rs `Document::new` and
`render`) now runs in `@vesper/lsp-features` (`macro_preview.ves`, entry
`lsp_macro_preview`). It has 0 diffs, at 1.26x Rust's time when the
document is hashed with C BLAKE3, as the hosted build would hash it.

- **Inputs.** The compiler snapshot comes in as words that
  `macro_preview::port_words` writes:
  - the snapshot's source association;
  - the expanded source, source map and invocation audits of a success;
  - the stage, message, expanded source and diagnostics of a rejection.

  The macro expander itself is still Rust's.
- **`Document::new`.** The URI bound, the source bound, and the
  association checks: the snapshot's source, its byte length and BLAKE3
  identity, and the expanded artifact's original source and hash.
- **`render`.**
  - The limits checks, then the source and URI bounds.
  - The prepaid work: 8 per output byte, 4 per source byte, the URI, and
    4,096.
  - The `Success` or `Rejected` record.
- **Writing the record.** Rust serializes the record through a bounded
  writer that charges every write call, then parses the bytes into a
  `serde_json::Value`. Here the record is built as a tree in Rust's field
  order, including:
  - serde's externally tagged `OperationKind` and `AuthorityArg`;
  - `Diagnostic`'s skipped empty `related` and absent `help`;
  - the empty arrays of a rejection.

  A walk in that order replays serde_json's write calls, so the
  `serialization work` and `output bytes` refusals happen at the same
  write as in Rust. Each quote, punctuation mark, number, run between
  escapes and escape is one write. The answer is then written with sorted
  keys, as the `Value` writes it.

The document digest comes from the host's C BLAKE3 when given. That is
what the hosted build's `@vesper-std/hash-sys` computes, and the same
choice the proof model port made in part 3. Otherwise the port hashes with
the portable `@vesper-std/hash` BLAKE3. `@vesper/lsp-features` keeps no
foreign calls, so it still builds under Stage 2.

## What changed

- `src/lsp/macro_preview.rs`: `port_words`, the snapshot encoder that the
  hybrid wiring will use.
- `src/lsp.rs`: the hidden `MacroPreviewForPort`. It holds one compiler
  snapshot and returns Rust's rendering or the port's words for a
  document.
- `packages/vesper-lsp-features/src/macro_preview.ves` (new) and the entry
  `lsp_macro_preview` in lib.ves.
- `tests/self_host_lsp_features.rs`: `macro_preview_matches_rust` and the
  ignored `macro_preview_speed`. They build one database at a time; all
  182 at once ran out of memory.

## Differential

182 compiler snapshots:

- the 16 source macro fixtures and the two other macro fixtures;
- the escape-free one-line programs in tests/forge_*.rs;
- damaged copies of every third one: cut in half, with an unclosed macro
  appended, and with a bad string appended;
- 15 package sources;
- five hand-written programs.

They give 744 cases:

- the document as compiled;
- for every fourth snapshot, also:
  - a stale document (one byte longer);
  - work allowances from the prepaid bound up to 40,000 over it;
  - output caps from 1 byte to 60,000;
- invalid limits;
- URIs of 0, 4,096 and 4,097 bytes and with escapes.

97 expanded (30 with audited operations), 301 rejected and 346 refused
cases matched with 0 diffs. The refusals cover every reason, including
186 serialization work and 106 output bytes refusals. Every case also
matches with the host's digest.

## Numbers

The 232 cases at default limits, 8 renders per call, best of five ABBA
rounds for each snapshot. The compiler snapshot is built outside the
timing on both sides. Rust's time includes parsing the bounded bytes into
a `Value` and writing it out. The host digest is computed inside the
timing on every render.

| Document hashed by | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| the host's C BLAKE3 (the hosted build's `@vesper-std/hash-sys`) | 8.34 µs | 10.48 µs | **1.26x** |
| the portable Vesper BLAKE3 | 8.34 µs | 18.12 µs | 2.17x |

As with the proof models, the portable BLAKE3 is most of the difference,
so the hybrid wiring must hash through hash-sys. Rust lines deleted: 0.
The preview is not wired: Rust stays the default until the hybrid wiring
part.

# Part 12a: the recovery gate (#1828)

`analyze` in `src/tooling/recovery.rs` gives diagnostics for a document
with syntax errors by checking its complete components alone. Before it
parses any complete item, it runs a gate over the recovery syntax:

- the limits, the source bound and the work budget;
- the source identity (its BLAKE3 against the snapshot's);
- the bound on the syntax diagnostics;
- the declaration census (spans, names, duplicates, reserved names,
  declaration kinds and statuses);
- the recovery sites, and every original token's coverage by a
  declaration.

The gate either refuses, blocks the whole document (every component
unavailable, with a reason), or opens it for the component check with the
budget left. Part 12a ports that gate as `lsp_recovery_gate`
(`recovery.ves` in `@vesper/lsp-features`). The component check after it
parses and type checks items with the Rust checker, so it waits for the
checker in Vesper.

## What changed

- `src/tooling/recovery.rs`: the gate is split out of `analyze` as `gate`
  (behaviour unchanged), with the hidden `gate_answer` (Rust's answer as
  text), `gate_words` (the encoder the hybrid wiring will use) and
  `gate_tags`. The token loop's trivia and identifier classes moved to
  `token_class`, which the encoder shares.
- `packages/vesper-lsp-features/src/recovery.ves` (new) and the entry
  `lsp_recovery_gate` in lib.ves.
- `tests/self_host_lsp_features.rs`: `recovery_gate_matches_rust` and the
  ignored `recovery_gate_speed`.

## Differential

330 recovery snapshots: eight hand-written programs of the accepted
declaration kinds and 40 package sources, each cut at a quarter, half and
three quarters, with a damaged function, a macro call, a stray token, a
trait, a long identifier or one brace fewer, and as it is (those that
parse under the default syntax limits). Each gets 24 limit sets: the
defaults, work allowances swept across the gate's own charges, small
identifier, diagnostic, function and context limits, a source limit one
byte short and three invalid limit sets.

7,920 cases, 0 diffs, with the portable BLAKE3 and with the host's
digest: 303 open, 2,626 blocked and 4,991 refused, covering every refusal
the gate can reach from a real snapshot (the source mismatch needs a
forged snapshot hash).

## Numbers

Default limits, 8 gates per call, best of five ABBA rounds per snapshot,
release build. The recovery syntax is parsed and encoded outside the
timing. Rust's time includes walking the rowan tree's tokens, which the
encoder hands the port as a flat list; the host digest is computed inside
the timing on every gate.

| Source hashed by | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| the host's C BLAKE3 (`@vesper-std/hash-sys`) | 73.1 µs | 21.9 µs | **0.30x** |
| the portable Vesper BLAKE3 | 73.1 µs | 57.0 µs | 0.78x |

Rust lines deleted: 0. The gate is not wired, and the rest of `analyze`
stays in Rust until the checker runs in Vesper.

# Part 12b: the checked signature snapshot (#1828)

`signature_snapshot` in `src/database.rs` gives the editor database a
semantic identity for a checked file: `extract_signature` turns the
checked world's headers (ADTs, functions, effects and their operations,
the inferred function results and the string capacity) into serde_json
bytes, and the snapshot keeps their BLAKE3 digest and the declaration
names. Salsa compares these, so a body-only edit stops propagating to
signature-dependent queries. Part 12b ports `extract_signature` and the
digest as `lsp_signature` (`signature.ves` in `@vesper/lsp-features`):

- the support checks: model uses, trait bounds, trait and conformance
  items, elaborated traits, conformances, generated functions, package
  and revision;
- the declaration limit;
- the unresolved checks, in Rust's order: each function's inferred result
  (merged with the result map in key order), parameters and effect
  arguments, then ADT fields, then effect operations. `unresolved_type`'s
  node budget and stack order are kept;
- the header JSON, written as serde writes `SemanticHeaders`: fields in
  declaration order, maps in key order, `ast::Type` externally tagged,
  `Field`'s optional `modality` and `temperature`, and 128-bit integer
  literals;
- the 1 MiB byte limit, the digest and the declaration list.

## What changed

- `src/database.rs`: the hidden `signature_answer` (Rust's answer as
  text), `signature_words` (the world header encoder the hybrid wiring
  will use) and `signature_tags`.
- `packages/vesper-lsp-features/src/signature.ves` (new) and the entry
  `lsp_signature` in lib.ves.
- `tests/self_host_lsp_features.rs`: `signature_matches_rust` and the
  ignored `signature_speed`.

## Differential

225 inputs: the checked corpus of the navigation tests (examples, test
fixtures, LSP fixtures and generated programs) and ten hand-written
programs (generics, effects, function types, tuples, projections, a
trait, a source that fails to check). 216 complete snapshots, 5
unsupported trait headers and 4 failed analyses, 0 diffs, with the
port's BLAKE3 and with the digest left to the host. The unresolved,
declaration-limit and byte-limit refusals are not reached by any checked
source in the corpus.

## Numbers

8 snapshots per call, best of five ABBA rounds per input, release build.
The world headers are encoded outside the timing. With the host digest,
the port's answer is unpacked and its header bytes hashed inside the
timing.

| Header bytes hashed by | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| the host's C BLAKE3 (`@vesper-std/hash-sys`) | 14.0 µs | 14.5 µs | **1.03x** |
| the portable Vesper BLAKE3 | 14.0 µs | 44.7 µs | 3.19x |

The first version ran at 1.70x. Writing plain JSON strings and literal
keys as one slice copy, and answering leaf types without a stack, brought
it to 1.03x. The portable BLAKE3 is most of the remaining gap, so the
wiring must hash through hash-sys. Rust lines deleted: 0; the query is
not wired.

# Part 13a: the lint preflight (#1828)

`preflight` in `src/tooling/lint.rs` is the first step of `vesper lint`
over a source: it refuses sources over 256 KiB, lexes them, refuses
lexical errors with the `Debug` text of the lexer's diagnostics, and
checks the token stream against the lint budgets (8,192 tokens, 256
recursive-expression tokens, 128-byte identifiers, 64 conditionals,
nesting 32, runs of 256 tokens between `;`, `{` and `}`) before any
parse. Part 13a ports it as `lsp_lint_preflight` (`lint_preflight.ves`
in `@vesper/lsp-features`), lexing with `@vesper/front`'s lexer.

## What changed

- `src/tooling/lint.rs`: the budgets are named constants, and the hidden
  `preflight_answer` (Rust's answer as text) and `preflight_words` (the
  source as the port reads it) are added.
- `packages/vesper-lsp-features/src/lint_preflight.ves` (new) and the
  entry `lsp_lint_preflight` in lib.ves.
- `tests/self_host_lsp_features.rs`: `lint_preflight_matches_rust` and
  the ignored `lint_preflight_speed`.

## Differential

254 sources: the recovery and signature programs, every fifth corpus
`.ves` file whole and cut in half, and sources at and one past each
budget, lexical errors (an unterminated comment and string, invalid
characters, more than the lexer's 128 diagnostics and 65,536 tokens) and
the 256 KiB limit. 123 admitted, 131 refused across every refusal, 0
diffs.

## Numbers

8 preflights per call, best of five ABBA rounds per source, release
build, source words written outside the timing.

| Rust | Vesper | Ratio |
|--:|--:|--:|
| 35.8 µs | 39.7 µs | **1.11x** |

Rust lines deleted: 0; the lint driver still lexes in Rust.
