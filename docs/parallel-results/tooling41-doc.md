# §40 checked API documentation and structural search (#66)

Implemented [issue #66](https://github.com/robot-head/vesper/issues/66) in the
fresh `tooling41/doc` worktree from `a1a8d938dd8103803219a05c8364cb7a198be8d9`.
Read the issue, `docs/rfcs/tooling41-contracts.md` and the complete refreshed §40.
Only new `crates/vesper-doc/**`, `src/doc_commands.rs`, `tests/doc_cli.rs` and this
report are delivered. Existing syntax/core/checker/layout/interface, arithmetic,
field-modality premise, compiler, native ABI, other lanes and external Warp work
are unchanged. Root Cargo/main/lock edits were temporary testing wiring only and
are restored before the lane commit.

## Actual source extraction

`src/doc_commands.rs::document_source(source, Limits)` invokes the real parser
and `check::check` on the complete current source. Any parse, typing, capability,
Pact or PCG diagnostic prevents documentation from being returned. Its adapter
reads checked `World` declarations and resolved `world.function_results`, then
applies the existing numeric defaults. This matters for returned closures: their
inferred/resolved latent effect instances can differ from the source annotation.
Both the declared and resolved result are preserved. Bodyless effect operations
use their own checked declarations, never an unrelated same-named ordinary
function's inferred result.

Metadata retains complete core Type trees, parameter passing modes, computed
modes, all generic bounds/row binders, full return types, effect names/arguments/
lexical labels, effect resumption modes, field selectors and source spans.
Type documentation distinguishes the declared modal floor from the actual checked
concrete mode (generic instantiations remain unavailable). It retains all variants,
field names/types and field modalities. Effect operations retain their owning declaration and generic
context. Source traits and checked conformance/model summaries remain explicit. Complete typed source conformance heads (self type, trait arguments, associated bindings and where clauses) are retained separately without private implementation bodies.
Generated private Pact functions are excluded using
`World.elaboration.generated`, rather than a name-prefix heuristic.

Module/item documentation is extracted from actual lossless lexer comment
tokens. Strings cannot forge doc comments. Blank lines/ordinary comments/code
break item attachment. Every `///`/`//!` token is additionally retained with its
source span and text, including field/variant comments whose current core AST
has no dedicated field span. No field attachment is guessed. The current source
visibility schema does not expose general `pub fn`/`pub struct` declarations;
documentation follows accepted source World functions/types and excludes the
explicitly private generated helpers.

Native layout metadata comes from actual `Layout::of`, with a bounded expansion
preflight. It records eight-byte alignment, actual sizes/field offsets/field
widths, enum variants and the eight-byte tag. For example, the real example's
`u8` field occupies an eight-byte native slot, and its following `u64` field
starts at byte eight. These are not padding-free wire/tiled layouts. Generic or
excessive layouts are reported unavailable, with a reason.

## Structural offline search

The root-free `vesper-doc` leaf depends only on existing `vesper-core`,
`vesper-syntax` and serde/serde_json. It never depends on the facade. `Query`,
`QueryParameter`, `FunctionDoc`, `search` and `parse_query_json` provide actual
host search, independent of HTML or a browser.

Matches require complete structural parameter/result types and identical passing
modes. Nominal arguments, tuple members, and closed pure arrow parameter/result/
passing/capture-mode contracts are retained. Mode checks are
`query parameter <= function parameter` and
`function result <= query result ceiling`. Candidate field selectors must be
within allowed query subtrees. Full effect instances, including arguments and
labels, must fit the ceiling. Unit spellings and effect-set ordering/duplicates
are normalized. Selector order does not change semantic exactness. Exact
contracts rank above allowed narrower contracts; names deterministically break
ties. Equal mode vectors cannot match incompatible types.

Generic functions, open rows, unresolved/row-valued types and effectful arrow
binder contracts have explicit unsupported reasons and are excluded. Queries
cannot request wildcard instantiation or unknown open-row matching. Core AST
serde accepts some unknown fields for compiler compatibility; the separate
strict query reader rejects duplicate decoded keys and unfamiliar nested contract fields so constraints
are never silently ignored. Lexical labels are conservatively matched by their
documented spelling; no binder alpha-renaming or capability-identity inference
is claimed. Source functions/inherent methods form the search/transition index;
effect operations remain documented under their owner, rather than being
flattened into ordinary calls with invented effect semantics.

## Validated graph and honest evidence

Concrete nominal state identities contain the complete bounded structural type.
Graph validation rejects duplicate/unknown state, function and parameter
identities, invalid targets, receiver/result mismatches, ambiguous multiple-owned
input attribution, and borrowed terminal edges. An inout edge keeps its nominal
state; a sink terminal edge requires a non-linear result. Unsupported/ambiguous
transitions are explicitly omitted. Even unused concrete linear source types
appear as dead ends.

Reverse graph reachability reports `terminal_path_exists`. Cycles can coexist
with a terminal path: this does not establish all-path cleanup or termination.
No boolean, function name or source `@proof` text grants a verified badge. There
is no proof-evidence input API in this slice. Every generated HTML page explicitly
states that audited artifact-bound evidence is unavailable and the documentation
is unverified. No source typing, kernel proof, certificate audit or performance
result is inferred from the metadata.

## Rendering, CLI and bounds

```text
vesper doc FILE [--json] [-o OUTPUT] [--query QUERY.json]
```

Default output is deterministic static HTML on stdout; `--json` selects a complete
metadata report with optional structural search results. `-o` creates a new local
output file and preserves any existing file. Generation/checking/budget failures
occur before opening output. Invalid `--json` invocations return a structured
invalid report and nonzero status. Diagnostics are capped to 4096 UTF-8 bytes plus
a truncation marker before the main stderr path. Source is never rewritten.

HTML provides readable item/doc overviews plus the complete typed metadata. All
source/doc content escapes `& < > " '`. Fixed embedded CSS is local; no script,
remote stylesheet, CDN, dynamic markdown or executable doc block is included.
The CSP defaults to no external content. No network, source execution, JIT or
doctest execution occurs. Doctest work is separately owned by #67.

Default limits: 128 KiB source, 16,384 lexer tokens, nesting 48, weighted unfinished
expression/signature segments 256, 128 function tokens, 64 conditional tokens; iterative parsed AST audit of 16,384 nodes/depth 128; 256 metadata items
(including fields/variants/operations), 8192 structural type/effect nodes, type depth 48, 32 KiB query
input and 2 MiB output. Configurable hard maxima are checked by `Limits::validate`.
A conservative parsed nominal-expansion guard runs before the unchanged checker:
repeated sibling type visits are charged, and recursive user nominal dependencies
are unsupported. This rejects compact doubling DAGs and mode-changing generic
cycles before expensive `World.mode` traversals. The guard approach was shared
read-only with lint/doctest owners; no other lane was edited.


The supported pre-check profile also conservatively refuses nonempty tuple
construction, generic function/method values or calls, generic constructors
(including Option/Result), generic conformances, and unconstrained inferred
closures. These can grow inferred types exponentially even in compact shallow
source. Complete tuple/generic declarations/signatures remain documentable;
closures directly constrained by complete arrow annotations remain supported.
This is an explicit unsupported-source boundary, not a change to the language.
Weighted delimiter frames preserve peak completed-statement cost as well as
unfinished cost, so semicolons cannot hide deep child ASTs before parsing.

These are source/metadata/output bounds, not a general execution-fuel or exact
RSS guarantee for the existing checker. Metadata allocation includes Rust
containers; no zero-allocation claim is made. Other design §40 work remains
separate: WASM browser search, mode-crossing matrices, wire/tiled diagrams,
intra-doc link resolution, proof-certificate ingestion, SemVer API diffs and
source-level Hoogle syntax. Unsupported general generics/row/binder contracts
are displayed rather than falsely certified or partially matched.

## Actual validation

Tests used private `/tmp/vesper-tooling41-doc-target`, one build job, zero dev/test
debug info and disabled incremental compilation. The target occupied roughly
1 GiB including the existing root facade/compiler dependency build.

With temporary coordinator-equivalent wiring:

```sh
cargo test -p vesper-doc --all-targets
cargo test -p vesper --test doc_cli
cargo clippy -p vesper-doc --all-targets -- -D warnings
cargo clippy -p vesper --bin vesper --test doc_cli -- -D warnings
cargo fmt -p vesper-doc -- --check
rustfmt --edition 2024 --check src/doc_commands.rs tests/doc_cli.rs
```

Twenty-five tests pass: fourteen leaf contract tests and eleven real checked-source/
CLI tests. They cover incompatible equal-mode APIs, full nested arrow contracts,
effect arguments/labels, selector containment/order, deterministic ranking,
unknown/duplicate nested query constraints, direct typed-effect DTO budgets, recursive Row audits, generic/open/inferred unsupported contracts,
invalid graph identities/edges/depth, cycles plus terminal reachability, comment
forgery, actual raw field docs, HTML escaping/determinism/output bounds, actual
inferred latent results, hidden Pact helpers, native eight-byte slots,
same-named effect operations, compact nominal expansion, recursive generics, inferred aggregate/generic/closure growth rejection,
weighted nested-block left-spine rejection, iterative parsed-AST depth rejection before World cloning, invalid source/flags/UTF-8/byte limits, local output and existing-file preservation.
Strict targeted Clippy and formatting are clean.

The real self-checking fixture and structural query are:

```sh
cargo run -p vesper -- doc crates/vesper-doc/examples/api.ves --json
cargo run -p vesper -- doc crates/vesper-doc/examples/api.ves --json --query crates/vesper-doc/examples/query.json
cargo run -p vesper -- doc crates/vesper-doc/examples/api.ves -o /tmp/vesper-api.html
```

Actual process-level acceptance checked three source functions, three source
types, a query matching only `close`, explicit unsupported latent binder search,
and parsed 15,781-byte static HTML without active/remote tags. No timing claim is made.

## Exact coordinator wiring

1. Add `"crates/vesper-doc"` to root workspace `members` and `default-members`.
2. Add root dependency `vesper-doc = { path = "crates/vesper-doc" }`; regenerate
   coordinator-owned Cargo.lock. The leaf introduces no new external package.
3. Add `mod doc_commands;` to `src/main.rs`, route `"doc"` to
   `doc_commands::run(args.collect())`, and add the main help entry. No root lib
   registration is needed; the adapter is a binary-facade module.
4. Include the leaf tests and `tests/doc_cli.rs` in focused checks/acceptance.
   The integration test imports the adapter by path to test real extraction,
   while invoking the actual `vesper` binary for CLI behavior.

No temporary manifest/main/lock wiring, leaf lockfile or build artifact is in
the lane commit.
