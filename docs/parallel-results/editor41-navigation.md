# Checked editor navigation (#83)

Implemented pure providers in `src/lsp/navigation.rs` over the private-constructed
`tooling::frontend::CheckedSource` and `vesper_span::LineIndex`. Every query first
validates configured limits and exact snapshot/index text equality. No URI is
opened or treated as filesystem authority.

Public APIs are `hover(snapshot,index,Position,Limits)`,
`definition(uri,snapshot,index,Position,Limits)`,
`document_symbols(snapshot,index,Limits)`, and
`workspace_symbols(&[OpenDocument { uri,index,snapshot }],query,Limits)`.
They return `Result` values with standard LSP JSON payloads: optional Hover or
Location, and vectors of DocumentSymbol or SymbolInformation. `Error` distinguishes
invalid limits, source mismatch, invalid position/span and resource exhaustion.
Hover uses plaintext MarkupContent, so source/docs cannot inject Markdown/HTML.
The coordinator owns module/dependency registration and request routing.

Hover reads actual checked expression types. Where the checker omits a place-base
entry, only a lexically resolved parameter matched by exact name/span in
the checked World may supply its non-inferred type; other absent entries return no result. The bounded structural
printer preserves function passing modes, closure type mode, full effect labels,
arguments/tails, generic modal bounds and parameter selectors. Function signatures
use actual checked result metadata only when the source declaration's span matches
the checked function. Attached `///` comments remain plaintext. Type-mode captions
explicitly distinguish a type's mode from current ownership or a live obligation.
There are no proof/liveness badges.

Before invoking actual `World::mode`, the provider checks its traversal cost with
the same argument-mode-sensitive recursion keys and generic component bounds.
This avoids assuming that nominal name-cycle detection alone bounds generic
expansion. No inferred Type or World is cloned for hover formatting.

Definitions follow source AST lexical environments: parameters, sequential lets,
initializer-before-binding order, nested blocks, closure parameters, tuple and
constructor patterns, and separately scoped match arms. A local binding shadows
an ordinary top-level function. Field/method spelling never selects a matching
bare global. Direct ordinary global references resolve to the actual source name
span. All targets are current UTF-16 ranges from the exact index.

Document/open-workspace symbols report actual declaration spans and exact name
selection ranges for top-level functions, ADTs, effects/operations, traits, named
models and ordinary impl methods. Providers rebuild from the current snapshot;
semantic signature equality never reuses stale source positions. Workspace symbol
search is a case-sensitive substring filter over explicitly supplied open buffers.

Limits default to 128 KiB source, 16,384 tokens/traversal nodes, 8,192 type/mode
work units, depth 64, 1,024 symbols, 32 documents, 4,096 URI bytes, 256 query bytes
and 256 KiB serialized output. Public configurations are validated against finite
hard ceilings. Tokenization is bounded before index allocation; traversal/type
printing/mode expansion and comment/output construction are bounded. Workspace
work is additionally bounded by document count. These are logical work/size
profiles, not hard deadlines or RSS guarantees.

Scope remains conservative: ordinary top-level function bodies are indexed;
impl/trait/model bodies are not advertised as resolved dispatch targets. Handler
subexpressions can expose actual checked types, but all names inside handler
state/body/clauses have no definition target until a frontend scope resolver is
available. Cross-file imports, trait/model dispatch, field/constructor/type
reference navigation and ambiguous/unrecorded types return no guessed target.
The frontend's whole-document supported profile must succeed before any provider
can receive a snapshot; parse/check/unsupported failures never become semantic
navigation. No compiler core, AST, PCG or execution schema changes are involved.

Validation used actual frontend source commit
`14dfbb6a9e1296675aa3a703d57226818699a35f` from #81 and the committed span dependency `d300142cae7e881a91946eb38980817517be6f40` through temporary
private registration; all temporary shared wiring is removed from this delivery.
With one build job, dev/test debug disabled and incremental compilation disabled:

- `cargo check -p vesper --lib --offline`: passed.
- `cargo test -p vesper --test lsp_navigation --offline`: 12 passed.
- `cargo clippy -p vesper --lib --test lsp_navigation --offline -- -D warnings`: passed.
- Owned Rust files pass rustfmt; patch passes `git diff --check`.

Tests cover real expression types/generic linear bounds/plaintext docs, checked
latent and labeled generic effects, pass/selector metadata, parameter/global/local
resolution, own-initializer order, block/closure/match/pattern shadowing, refusal to
guess field/trait dispatch, Unicode surrogate rejection and CRLF, body edits moving
later declarations, open-document workspace locations, parse/check/unsupported
snapshot rejection, source mismatch, nominal mode-work limits, recursive-source
rejection and nested function/effect arguments. They execute no
JIT, native kernel, WASM, macro, proof or ambient file read.
