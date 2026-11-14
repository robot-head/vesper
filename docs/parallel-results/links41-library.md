# Bounded local intra-doc link library (#178)

The published `links41-contracts.md` and claim #178 preceded source writes. This
lane changes only `crates/vesper-doc/src/{lib,links}.rs`, its new `tests/links.rs`
and this report. No dependency, Cargo/Bazel, workflow, wrapper, root adapter or
existing test files changed.

The pure library analyzes actual lexer doc-comment tokens, retaining absolute
UTF8 byte ranges for every resolved or rejected reference. It distinguishes local
targets by complete path and kind; ambiguous cross-namespace paths do not choose
a preferred namespace. Explicit Self, package and generic forms are unsupported.
Unknown, ambiguous and unsupported references retain structured diagnostics and
a Rejected inspection report. Configuration, lexical, allocation and resource
failures return an error without an accepted report.

Escapes, matching inline code runs, fenced blocks, image/link labels, explicit
destinations/titles and reference-style definitions remain literal. Excluded
constructs can span contiguous doc lines without exposing inner brackets. Ordinary
source code, strings, comments and blank source lines break doc groups. The
profile is intentionally a finite intra-doc grammar rather than a general
Markdown parser or renderer.

Finite accounting covers lexer storage, target inventories/copies, vector growth,
delimiter scans, matching/resolution, diagnostics and new render-stage temporary
anchors/scans. Sorting uses allocation-free unstable sorting with a prepaid
comparison envelope; unique descriptors give deterministic canonical ordering.
Token-dense sources can conservatively exhaust the finite lexer envelope before
the source-byte ceiling. Existing serialized/output caps remain separate. These
are logical guards, not a timing or RSS guarantee.

Document has an additive default NotAnalyzed link report. Actual analyzed source
is supplied by the checked compiler adapter. The public inspection DTOs do not
certify source checking, proof, authority or original-source lexical analysis for
arbitrary deserialized values. Typed HTML rendering independently recomputes the
Document's target inventory and validates report byte count, exact retained
comment bracket/display bytes, UTF8 boundaries, ordering, targets and anchors.
It cannot reconstruct omitted intervening original source bytes from public
comment DTOs; that original-source boundary belongs to the checked adapter.

Every emitted fragment uses a fixed kind tag and full path bytes in ASCII hex,
and has exactly one actual element in a directly typed declaration section.
Linked original comment text is escaped. Deep JSON overview refusal cannot omit
the required targets. Generic legacy rendering remains escaped and unverified;
malformed, rejected, foreign-target and altered-anchor typed reports refuse.
Source methods on builtin owners retain their real documented method target
without inventing a builtin type declaration.

Independent review prompted regressions and fixes for multiline excluded link
constructs, apostrophes in bare URI paths and parenthesized titles, code-span
closing brackets inside link/image labels, and continued reference-definition
titles. Temporary effect-operation path allocation, sorting and anchor rendering
are prepaid before allocation; existing API/profile interfaces remain frozen.

Actual validation in the exclusive `/tmp/stylus41-macro-target` cache, one Cargo
job with debug0/incremental0:

- `cargo test -p vesper-doc --locked`: 38 tests across two nonempty suites pass:
  all 24 new link tests and all 14 existing contracts, zero failures or ignores.
- New tests independently inspect exact UTF8/CRLF/repeated occurrence bytes,
  grammar exclusions and their outside-reference boundaries, true namespace
  ambiguity, every limit field, lexer refusal, target ordering, actual HTML
  anchor cardinality, hostile public metadata, deep JSON fallback and legacy
  behavior. No compiler-checking evidence is fabricated by these pure tests.
- `cargo clippy -p vesper-doc --locked --all-targets -- -D warnings`, package
  formatting and whitespace checks pass.

Root owns combined actual compiler/CLI validation, external-platform evidence and
protected stack integration. This lane makes no native execution or whole-design
completion claim. Claims stay reserved through reviewed merge.
