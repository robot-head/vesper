# Bounded editor implementation contracts

Baseline main `90bd8e1e86988d250a6c027eb0d9dd9f7764aa31`
([PR #78](https://github.com/robot-head/vesper/pull/78)). The coordinator checked
the current issue board, unchanged external Warp PR ownership/file timestamps,
current LSP/database/source adapters and specification §21 before claiming this
batch. The document metadata remains 2026-11-12T09:18:30.204Z (§§1–41/Appendix A).

| Issue | Branch | Exclusive implementation |
| --- | --- | --- |
| [#79](https://github.com/robot-head/vesper/issues/79) | `editor41/span` | New `crates/vesper-span/**`, own report |
| [#80](https://github.com/robot-head/vesper/issues/80) | `editor41/protocol` | New `crates/vesper-lsp/**`, own report |
| [#81](https://github.com/robot-head/vesper/issues/81) | `editor41/tooling` | New `src/tooling/**`, doc/lint binary adapters and matching tests; one lib module registration; own report |
| [#82](https://github.com/robot-head/vesper/issues/82) | `editor41/database` | Existing `src/database.rs`, new database/signature tests, own report |
| [#83](https://github.com/robot-head/vesper/issues/83) | `editor41/navigation` | New `src/lsp/navigation.rs`, matching new tests, own report |
| [#84](https://github.com/robot-head/vesper/issues/84) | `editor41/actions` | New `src/lsp/actions.rs`, matching new tests, own report |
| [#85](https://github.com/robot-head/vesper/issues/85) | `editor41/integration` | Existing root LSP dispatcher, new session/search/context helpers and stdio tests, global workspace/lock registration, acceptance/workflow and integration report |

Reports are `docs/parallel-results/editor41-<lane>.md`. External Warp #36–#45
and its production paths remain reserved. Other agents' approved shared root
Cargo/lock patches remain allowed. All unlisted production files are read-only.
Core AST/checker/Place IR/interface/native ABI schemas, arithmetic semantics,
`k_i ⊑ m_i` and the AST-based Lean extractor remain unchanged.

## Shared spans and protocol

The span owner supplies safe, root-free typed positions/ranges and source edits.
Freeze `Position { line:u32, character:u32 }`, `Range { start,end }`,
`ByteSpan { start:usize,end:usize }`, and
`LineIndex::new(Arc<str>,Limits) -> Result<LineIndex>` with
`text()/offset(Position)/position(usize)/range(ByteSpan)` before dependents code.
Reject invalid UTF-8 boundaries, UTF-16 surrogate interiors, reversed ranges and
out-of-bounds locations. Define CRLF/lone-newline/EOF behavior explicitly; never
slice at an unchecked client-provided byte offset or invent a causal location.
Edits are validated/bounded before any returned source is published.

The protocol owner supplies actual bounded Content-Length read/write and
JSON-RPC envelopes plus transactional in-memory document state. JSON duplicate
decoded keys must fail before DOM overwrite. Header limits apply while reading,
not after an unbounded read_line. Output length counts UTF-8 bytes. URI strings
are opaque, bounded labels and never filesystem authority. Open/change/close
must enforce version/size/work/count limits and preserve state on invalid edits.
Send concrete envelope/store APIs early; coordinate span dependency first.

Proposed server defaults are 128 KiB/document, 32 open documents, 2 MiB total
open text, 2 MiB message/reply, bounded headers/edits/queries, and periodic compiler
database reset after a bounded accepted-revision count. Direct APIs additionally
validate their configured limits. These are logical resource profiles, not hard
wall-clock/RSS guarantees.

## Pure checked-source library

The tooling owner extracts existing pure doc/lint logic without weakening or
silently tightening old CLI profiles, default policy behavior or JSON contracts.
Filesystem reads, policy loading and atomic file replacement stay in CLI layers;
source-only library functions do no I/O, JIT, macro or proof execution.

Freeze `tooling::frontend::{AnalysisLimits,AnalysisFailure,CheckedSource,check_source}`.
`CheckedSource` has private validated construction and immutable
`source()/parsed()/checked()/world()` access; retain actual source identity.
Structured parse/check diagnostics and resource/unsupported failures remain
distinct from successful checked snapshots. New editor profiles must apply
tested AST/nominal/inference guards **before** checking or cloning, with explicit
identifier/type/diagnostic/metadata bounds where needed. A caller boolean or
arbitrary unvalidated World cannot promote source to checked status.

Expose checked snapshot → actual documentation metadata without redundant
checking, preserving proof/layout/search limitations. Preserve original CLI
helper wrappers for existing tests/consumers. Pure lint preparation includes
policy/provenance and existing full candidate checking/SemVer gates. Provide a
selected-range preparation API so a quick-fix cannot silently apply document-wide
edits. Recompute findings first, filter allow rules before edit choice, retain
token-group normalization, and reparse/check/rerun/compare the entire candidate.
Explicit fix-all is a different action. Coordinate names early with actions.

## Queries and feature providers

The database owner adds guarded memoized analysis and sound signature-dependent
queries. Actual inferred result/latent types, modes/effects/selectors and
ABI-driving module settings matter even when source headers look unchanged.
Body/comment/span changes may preserve supported semantic signatures; position
queries must still observe the new source revision. Unknown/failed extraction
uses full actual source identity and unavailable status, never a fake stable
header. No blind recursive stripping of map keys named body/span. Preserve old
public queries where needed; new editor consumers use guarded APIs only.

Navigation uses actual `Checked.expression_types`, AST scopes and checked
World/doc facts. Type mode is not current ownership/liveness. Prefer plaintext
markup or escape source/docs. Resolve bindings and shadowing; no bare-name global
or field guesses. Unsupported dispatch/import resolution returns no target.
Validate snapshot/index source identity and always derive current UTF-16 ranges.

Actions use the actual canonical formatter and pure validated lint APIs. Client
diagnostics/fix payloads are not authority. Offer only freshly checked candidate
edits with actual SemVer compatibility, versioned documentChanges and source/
policy provenance. Formatting stays 100 columns/four spaces with zero config.
No speculative projection-arity/arena rewrites or automatic file/client edits.

Providers live in their own new root LSP modules and should expose pure typed
functions over LineIndex/CheckedSource/policy/URI/version, avoiding a shared
mutable dispatcher dependency. Send concrete APIs early. The coordinator owns
server routing, lifecycle, error mapping and initialization capabilities.

## Server integration and trust boundary

Preserve `lsp::serve(BufRead,Write)`. Advertise only implemented capabilities.
Implement correct request/notification/initialize/shutdown/error behavior and
bound document/compiler caches. Closed/old SourceFile IDs must not survive a
database reset; current positions/versions remain separate from stable semantic
hashes. Unsupported source gets explicit analysis-unavailable status rather
than unchecked semantic results. No file URI causes ambient filesystem reads,
host-native build execution, JIT, WASM or proof subprocesses.

Typed signature search is an explicitly documented request over actual open
checked buffers and the existing strict structural query engine. No guessed
cross-file binding, imported workspace graph, audit badge, completion liveness,
PoisonedRecovery/per-function PCG, async cancellation or sub-2ms performance
claim is implied by this batch.

## Delivery

Use fresh separate worktrees from this contract, private Cargo targets, one
build job, zero dev/test debug and no incremental cache. Temporary local wiring
is allowed for testing, then restored before owned-file commits. Dependency
commits may be cherry-picked into private worktrees; final source commits remain
lane-local. The coordinator reviews all APIs/deliveries, exercises real framed
stdio and edit/candidate flows, runs full workspace/strict/native3OS checks, and
publishes a protected-branch PR linked to these issues. Record exact support and
remaining design scope; do not mark the full design complete for a bounded batch.
