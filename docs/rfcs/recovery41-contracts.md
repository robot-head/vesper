# Lossless editor recovery contracts

Baseline clean main `6d61fd855e5c733e6b92ea74e5d3c09b3fe68c46`: PR94 plus incoming
external Warp PR53/57. The combined baseline passed 1,947 tests/238 suites with
Lean, strict workspace Clippy and formatting. Open claims and every updated
external PR's actual files were rechecked before the six claims below. External
Warp paths/docs stay reserved; their shared Cargo/lock additions remain allowed.
Live spec metadata remains 2026-11-12T09:18:30.204Z, 41 sections/Appendix A.
This batch advances §21.2/21.3, without claiming full semantic recovery or latency.

| Issue | Branch | Exclusive production ownership |
| --- | --- | --- |
| [#95](https://github.com/robot-head/vesper/issues/95) | recovery41/model | New syntax recovery.rs, model tests, syntax Cargo.toml existing blake3 dependency |
| [#96](https://github.com/robot-head/vesper/issues/96) | recovery41/parser | Existing syntax parser.rs/lexer.rs, new private parser child helpers, parser recovery tests |
| [#97](https://github.com/robot-head/vesper/issues/97) | recovery41/scoped | New tooling/recovery.rs, frontend.rs guard helper visibility/reuse, recovery_analysis tests |
| [#98](https://github.com/robot-head/vesper/issues/98) | recovery41/incremental | New syntax incremental.rs/tests, existing database.rs and database_recovery tests |
| [#99](https://github.com/robot-head/vesper/issues/99) | recovery41/acceptance | New recovery_cli.rs, test-recovery.sh/check-recovery.py/recovery.yml |
| [#100](https://github.com/robot-head/vesper/issues/100) | recovery41/integration | LSP session recovery state/query/publication/status routing, lib/module/Cargo/lock registration, consumed-source workflow filters, this contract/integration report |

Each lane owns docs/parallel-results/recovery41-<lane>.md. Unlisted production
files are read-only except explicitly agreed integration fixes. New assignments
require fresh claim/overlap checks. Delivered lanes remain reserved until reviewed
merge. Separate worktrees/private caches, one Cargo job, debug0/inc0; root's
target/parallel-next stays coordinator-only. Temporary dependency wiring is restored
before owned-only commits. Freeze concrete APIs before dependent writers.

## Authority boundary

Keep existing Parse/parse/parse_with_limits, core AST expressions/types, World/IR,
interface/native ABI and normal checker behavior unchanged. There is NO new core
ExprKind hole/PoisonedRecovery state in this batch. Preserve linear exhaustion,
k_i⊑m_i, arithmetic and AST-based Lean extraction. Actual syntax-complete source
uses the existing full guarded_analysis/check_source path. Recovered syntax cannot
mint CheckedSource, expose a World, certify a signature, authorize semantic hover/
definition/actions/search or execute JIT/WASM/Lean. No blanket E030x filtering.

Every published byte remains original text. No wrapper function, padded prefix,
empty body, Unit/wildcard placeholder or guessed signature makes the original
program checked. Damaged functions are semantically unavailable in their entirety;
real independent complete siblings may still produce actual normal diagnostics.
Repairing syntax restores strict full-program exhaustion checks.

## AST-free syntax model

RecoverySyntax has private validated construction and immutable source Arc/hash,
actual GreenNode, actual syntax diagnostics and bounded declaration/site inventory.
It stores/exposes NO core AST or World. Public methods source/source_hash/syntax/
diagnostics/declarations/sites are read-only; syntax completeness is never a
semantic success claim. An internal assembler is available only inside syntax
crate for trusted parser/incremental implementations and validates source/CST text,
all ranges, metadata/count/work bounds before publication.

Declarations describe syntactic kind, actual name/header/body/raw-green-body
ranges and Complete/Recovered/Opaque status. Prefer name spans/borrowed source
rather than unvalidated name copies. The parser records completeness per production;
do not infer it merely from global diagnostic overlap (a missing delimiter can
point at the next real sibling token). Recovery sites describe missing member/
delimiter, unexpected tokens and opaque lexical regions, with actual spans and
optional receiver root/projection TOKEN syntax. They carry no resolved place,
type, ownership, liveness or completion recommendation. Missing tokens are zero-width
metadata, never inserted text. Site ownership may refer to the preceding damaged
production even when an expected-token cursor is at a following sibling anchor.

Model limits default: parse source128KiB/tokens16,384/diagnostics64/depth48,
sites256/declarations256/metadata1MiB/work16MiB/identifier256B. HARD source1MiB,
tokens65,536/diagnostics128/depth128/sites4096/declarations4096/metadata16MiB/
work128MiB/identifier1024B. Validate direct configs and source bytes before clones;
finite counters reject overflow. Explain logical staging versus parser allocations
and do not imply RSS/deadline bounds. Keep captured parser context internal,
bounded and source-bound (row_names/bound_values/no_struct); no public forged
context constructor.

## Actual parsing and fragments

New parser::parse_recovery(source, recovery::Limits) returns Result<RecoverySyntax,
recovery::Error>. Editor mode adds BLOCK/ERROR_NODE/INCOMPLETE CST kinds while
keeping old batch APIs/valid behavior compatible. Tokens skipped on errors remain
inside ErrorNode; tx. retains real receiver/dot. Lexer/parser synchronize with
context-aware statement/item prefixes and always progress, stop at an anchor or
exhaust a budget. Do not consume the next fn LowerIdent as a missing member.
Methods inside effect/impl contexts differ from top-level siblings; ambiguous
boundaries stay opaque. Strings/comments/opaque macro payloads never provide
anchors; unterminated lexical tails can remain opaque. No hostile nesting or
unknown context gets promoted to complete syntax.

The parser also supplies internal real-full-source token-range helpers:

1. Complete item ranges produce actual AST Module/items with original absolute
   spans, model-use/trait metadata included or explicitly rejected. Lex once where
   possible; bound total range/token/parse work and reject per-range diagnostics.
   No text padding/wrapping or silent source-offset reconstruction.
2. A known complete top-level nongeneric function body can be parsed to a BLOCK
   GreenNode under its sealed real header context. It must consume the exact
   full-source token range and have no diagnostics/uncertainty. Full fallback on
   context/header/delimiter/lexical changes. This is not arbitrary context-free
   block reparsing. No AST from an old revision is retained by syntax snapshots.

Model/parser owners agree exact helper/descriptor/internal-builder types early.

## Scoped diagnostic-only analysis

New tooling::recovery returns private immutable RecoveryDiagnostics: actual source
identity, original-span diagnostics, analyzed-function coverage and explicit
unavailable ranges/reasons. No checked-program boolean/World/certificate. Normal
syntax-complete input stays on existing full checker; recovered input uses only
complete original ASTs obtained from the real token-range helper.

Initial context is conservative: known complete unique headers, nongeneric acyclic
user ADTs/effects, supported nongeneric functions with closed types/effects.
Exclude ordinary global-function dependencies, imports/traits/models/impls/use-models,
closures/handlers/higher-order/residual inference. Unknown or opaque headers cannot
silently satisfy dependency closure or justify a guessed missing-name error.
Validate uniqueness/reserved names/dependencies and AST/nominal/type/metadata bounds
before cloning/checking; reuse existing guarded frontend helpers. Bound aggregate
context/component cloning/work/function count and causal diagnostic metadata. Any
unsupported component is explicitly unavailable, not an error-free accepted program.

## Real CST reuse and Salsa inputs

SyntaxDocument owns current Arc<RecoverySyntax>; open/update accept actual text.
An update may derive a UTF8/CRLF-safe minimal changed byte range. Fast path only
inside a known complete top-level nongeneric body with unchanged header and sealed
context, complete real fragment and safe lexical/delimiter boundaries. Replace the
actual Green subtree, reuse untouched siblings and relocate only known syntax-side
metadata/red-node ranges. No old AST spans exist in this model. Full recovery
fallback for uncertainty; explicit refusal on budgets. Failed updates retain old
state, which root MUST discard rather than publish against newer document text.

Test fast results against full recovery oracle including diagnostics/tree/sites/
declarations; separately assert actual pointer/subtree reuse. Expose truthful update
kind. Do not full-parse merely to pretend a fast path or claim whole semantic checking
became incremental/sub-2ms.

New Salsa RecoveryFile input carries sealed Arc<RecoverySyntax> (an identity wrapper
is acceptable if needed for bounded Salsa equality). A tracked recovery diagnostic
query consumes that actual incremental snapshot via scoped lane. Existing SourceFile,
guarded_analysis/signature and legacy queries stay compatible. Root updates new IDs
only after accepted document transactions, validates source identity, clears/resets
all recovery maps/IDs on close and every64 accepted revisions. No disconnected demo
query or stale snapshot can supply current diagnostics.

## Host integration and tests

LSP semantic features remain gated by original-source CheckedSource. For original
Parse failures publish current real recovery syntax diagnostics plus guarded scoped
errors and explicit coverage/unavailability. Overall vesper.status remains parse_error;
vesper.recovery can explain scope. Valid syntax and other resource/unsupported states
keep normal behavior. Use shared validated renderer and client publication capabilities.
A bounded experimental vesper/recoveryStatus request may expose open-buffer syntax-only
source/version/hash/inventory/actual update path; no World/types/liveness/proof. URI
labels never load files or execute macros/builds/proofs.

Acceptance covers tx./calls/braces/sibling resilience, context/comment/string/macro
spoofing, genuine complete-sibling leaks versus damaged-function unavailability,
repair restoring strict exhaustion, current Unicode/CRLF positions/hash/version,
body reuse and real fallbacks, stale/invalid transactions and resource refusal.
Run old/new workspace/Lean/strict Clippy and source/editor/diagnostics/protocol
acceptance, observe native3OS CI, publish protected PR linked claims and return to
clean main, accounting for independent external merges. Remaining semantic
PoisonedRecovery, full recovery inference/completion, per-function PCG, macros/proof
lenses and latency guarantees stay explicit.
