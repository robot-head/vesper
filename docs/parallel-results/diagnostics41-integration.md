# Causal compiler diagnostics (#87–#93)

This batch implements actual causal multi-span diagnostics from specification
§21.4. The live document metadata was rechecked and still matches the recorded
41-section/Appendix A revision, modified 2026-11-12T09:18:30.204Z. The full design
remains in progress.

The coordinator checked open issues and every external PR's actual file list
before creating seven exclusive claims. External Warp work remains reserved;
its shared Cargo/lock registration patches remain allowed. Ownership, frozen
APIs and validation contracts are in `docs/rfcs/diagnostics41-contracts.md`.
Every writer used a separate worktree/private cache. Cross-reviews covered core
metadata, source and IR causes, frontend bounds and shared rendering.

## Source and IR evidence

Core Diagnostic retains code/message/span and its original constructor. Ordered
`related: Vec<DiagnosticLabel {span,message}>` and `help: Option<String>` are
optional, serde-defaulted and omitted when empty/None. Old three-field JSON
roundtrips unchanged; rich compiler failures add metadata without renumbering
codes or changing original messages/primary UTF-8 byte ranges. These fields
carry evidence, not edits or proof authority. Core types/expressions, World/IR,
interface/native ABI, arithmetic, linear-exhaustion rules and AST-based Lean
extraction remain unchanged.

The source checker adds actual declaration, admissible move, requested argument
access and checkpoint/handler-initializer evidence for supported ownership,
aliasing, branch and resumption failures. Each binding keeps at most four
bounded transfer witnesses (64 path components/1024 total path bytes); each
diagnostic adds at most eight labels. Witnesses do not include failed repeated,
borrowed, invalid-projection/field, conflicting-loan or forbidden linear-field
transfers. Existing recovery still marks failed attempts exactly as before.
Shadowed declarations never inherit another binding's history. Common incoming
history is excluded from branch-specific captions. Rejected handler initializers
and requested loans use neutral source captions, without claiming successful
initialization/borrowing. Help explains invariants; it invents no abort/commit
method or unchecked rewrite.

Place-IR diagnostics add actual instruction definitions, unique binding/parameter
source ranges and same-block owned-consumption witnesses. Static origin indexing
is bounded and discards all optional origin data if incomplete; duplicate,
phi/internal/zero/unknown origins stay absent. Provenance is outside abstract
State equality/transfer/join behavior and IR serialization. Borrow releases and
failed borrowed consumptions do not become owned-consumption witnesses. Loan
captions describe requested IR access, including recovery states. The original
E0410 primaries and compiler decisions remain unchanged. This is bounded source
provenance, not a complete execution trace or ownership proof.

## Guards and shared rendering

The guarded frontend validates every primary/secondary span, label count and
UTF-8 byte payload plus fixed diagnostic/label record metadata before publishing
a failure. Existing max_diagnostic_bytes is aggregate, with eight secondary
labels per diagnostic and checked max_diagnostics×8 aggregate count. Oversize,
overflow or malformed metadata becomes Resource; no partial/empty diagnostics
or positive World replaces failure. No AnalysisLimits field or legacy doc/lint
profile changes.

The new root-free vesper-diagnostics crate validates supplied core metadata
against the exact immutable LineIndex, stages checked span DTOs and emits:

- Standard LSP relatedInformation with actual URI/UTF-16 range/message, original
  primary range/code/message, severity1/source=vesper and optional data.help.
- Complete private terminal output using Ariadne explicitly in BYTE mode, with
  genuine labels/help, stable ordering and colors disabled.

Counts, text, source, endpoint work, staging metadata and escaped output size
are bounded before publication. Names are opaque labels and grant no filesystem
access. The adapter performs no compiler/JIT/WASM/proof execution and creates no
fixes. It validates data; causality comes from actual producer events. Per-lane
reports document limits and conservative peak refusals. These are logical work/
size profiles, not RSS, hard timing, sanitization or termination guarantees.

Ariadne's extra Unicode line separators are explicitly unsupported for terminal
rendering because the shared source index treats only CR/LF as line terminators;
LSP remains exact. Empty source/zero-width EOF and CRLF/lone-CR cases are tested.
A display-only trailing blank addresses the actual final empty EOF line after a
terminator without changing original bytes, source identity or validated offsets.

## CLI and editor integration

Native source commands keep their successful behavior and raw compiler JSON
contract; optional metadata is emitted only when present. Terminal rendering
uses the bounded HARD source profile, checking the 1 MiB source ceiling before
cloning into an Arc. If presentation is unavailable, stderr reports that reason
and original coded messages with explicit raw UTF-8 byte extents, never clamped
or guessed squiggles. This fallback is bounded to 128 displayed errors, 128 code
characters and 4096 message characters each, with an explicit remaining count.
Compilation remains rejected. JSON mode retains the original raw diagnostic
serialization behavior rather than pretending rendered coordinates exist.

LSP converts actual current Parse/Semantic failures through the shared renderer.
Invalid primary or secondary coordinates become one current analysis_unavailable
warning, with no retained partial errors. Other metadata/output failures are
also explicit unavailable status. Source version/hash and strict transactional
edits/cache resets from the editor batch remain intact. A corrected source clears
old errors/causes. Initialization records the client publication capabilities:
relatedInformation and optional data.help are sent only with their respective
relatedInformation/dataSupport flags. Absent/false clients receive the original
primary presentation; existing unavailable-warning status markers remain. All
metadata is validated before optional fields are filtered. Client diagnostic/related/fix/verified payloads never supply
compiler provenance or authorize actions. Existing checked lint actions remain
separate and recompute their candidates.

## Validation

Local integration passed with one build job, dev/test debug0, incremental off,
locked/offline dependencies and the coordinator's private target:

- Full `cargo test --workspace --locked --offline` with actual
  `VESPER_LEAN=/home/agent/.elan/bin/lean`: 1,910 tests across 231 suites, zero
  failures/ignored, before the final publication-capability test expansion.
- After publication negotiation: root library35, legacy CLI7, framed editor5
  and all eight final diagnostics CLI/stdio tests passed separately.
- Strict `cargo clippy --workspace --locked --offline --all-targets -- -D warnings`
  passed on final production/test sources; formatting and diff checks passed.
- `scripts/test-diagnostics.sh`: 17 actual CLI checks, 16 framed stdio sessions,
  and the executable shared renderer example passed. It checks real source
  slices/current UTF-16 positions/hash/version, shadowing, recovery, independent
  publication capabilities, rejected client evidence, resource refusal and exact
  raw fallback for unsupported terminal line separators.
- Existing `scripts/test-editor.sh` (19 child sessions and span/protocol examples),
  `scripts/test-source-tooling.sh` and `scripts/test-canonical-protocols.sh` passed.
  Optional PMU evidence remains explicitly unavailable here; no hardware claim.
- Specification snapshot validation and all 14 snapshot unit tests passed.

The dedicated native Linux/Windows/macOS workflow exercises model/renderer,
producer/bounds and real CLI/framed stdio acceptance, strict Clippy and the
causal_diagnostics example. Native results are recorded on the linked PR;
local success alone does not establish Windows/macOS behavior. Existing source/
editor/protocol workflows and full workspace/Lean checks remain required.

## Remaining design scope

This batch does not implement PoisonedRecovery, localized CST reparsing,
per-function PCG invalidation, obligation-guided completion, lifetime-free extract
function, general substructural quick fixes, background proof lenses, macro
integration or the stated sub-2ms target. Missing/ambiguous optional provenance
stays absent. Typed span fix DTOs and previously checked lint actions are not
promoted to arbitrary verified compiler rewrites. No full §21/design completion
or formal all-path causal guarantee follows from these diagnostics.
