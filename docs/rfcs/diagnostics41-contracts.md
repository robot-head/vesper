# Causal diagnostic implementation contracts

Base main `4a62f9de7ad00ba215ae3ad733edc4ffe3d23e4d` (PR #86).
The coordinator rechecked open issues and every external PR's actual file list
before claiming this batch. External Warp lanes remain reserved; only their
already-agreed root Cargo/lock registrations overlap. Live specification metadata
remains 2026-11-12T09:18:30.204Z (41 sections/Appendix A). This batch addresses
§21.4's causal multi-span compiler diagnostics, not the full §21/design.

| Issue | Branch | Exclusive source ownership |
| --- | --- | --- |
| [#87](https://github.com/robot-head/vesper/issues/87) | diagnostics41/model | Diagnostic definitions in crates/vesper-core/src/ast.rs; new core diagnostics tests; core serde_json dev-dependency only |
| [#88](https://github.com/robot-head/vesper/issues/88) | diagnostics41/checker | src/check.rs; new tests/diagnostic_causes.rs |
| [#89](https://github.com/robot-head/vesper/issues/89) | diagnostics41/ir | src/ir_check.rs; new tests/ir_diagnostic_causes.rs |
| [#90](https://github.com/robot-head/vesper/issues/90) | diagnostics41/render | New crates/vesper-diagnostics/** |
| [#91](https://github.com/robot-head/vesper/issues/91) | diagnostics41/bounds | src/tooling/frontend.rs and matching diagnostic-bound tests |
| [#92](https://github.com/robot-head/vesper/issues/92) | diagnostics41/acceptance | New tests/diagnostics_cli.rs, scripts/test-diagnostics.sh, scripts/check-diagnostics.py, .github/workflows/diagnostics.yml |
| [#93](https://github.com/robot-head/vesper/issues/93) | diagnostics41/integration | src/main.rs diagnostic dispatcher/imports; src/lsp/session.rs diagnostic conversion/publication capability recording; global Cargo/lock and editor workflow dependency filter; this contract and integration report |

Every lane additionally owns its `docs/parallel-results/diagnostics41-<lane>.md`.
Unlisted production files are read-only. No source work is assigned outside these
claims without first checking existing claims and recording exact new ownership.
A delivered lane stays reserved through integration/validation and closes with
the reviewed PR. Use separate worktrees/private caches, one build job, debug0 and
incremental off. Root's target/parallel-next is reserved for the coordinator.

## Frozen core API and compatibility

Only the diagnostic model changes:

```rust
pub struct DiagnosticLabel { pub span: Span, pub message: String }
// Existing code/message/span fields and Diagnostic::new remain unchanged.
pub related: Vec<DiagnosticLabel> // serde(default, skip_serializing_if=Vec::is_empty)
pub help: Option<String>         // serde(default, skip_serializing_if=Option::is_none)
Diagnostic::with_related(self, span: Span, message: impl Into<String>) -> Self
Diagnostic::with_help(self, message: impl Into<String>) -> Self
```

`span` stays the single primary UTF-8 byte range. Secondary labels are ordered
actual producer evidence, not edits or proof authority. Legacy three-field JSON
is accepted and emits the original shape when metadata is empty. New metadata is
additive only; preserve existing codes/messages/primary spans and compiler
acceptance/rejection. No renumbering errors to resemble the document's examples.
Core type/AST expressions, IR serialization/state tokens, interface/native ABI,
arithmetic, `k_i ⊑ m_i`, and AST-based Lean extraction remain unchanged.

## Producers and bounds

Source checker labels must come from actual binding, argument, loan, consumption
or checkpoint events. A declaration span may be labeled as a declaration, never
as an invented last use. Shadowing and branch-dependent moves require actual
origin evidence. Missing evidence stays absent. Help explains an invariant; it
must not invent a callable abort/commit method or unchecked rewrite.

IR causes use actual FunctionIr parameter/instruction/pattern spans. Static
value-definition indices can supply genuine temporary origins; provenance must
not change abstract-state equality, fixed-point/ownership transfer semantics or
IR schemas. Undefined/internal values may have no source cause.

The frontend guard counts all related labels/help/text and validates every span
before cloning or publishing a failure. Existing diagnostic byte/count budgets
remain aggregate; add a documented finite related-label ceiling. Invalid or
over-budget metadata becomes explicit Resource/unavailable rather than a partial
or falsely checked diagnostic result. Legacy doc/lint independent profiles stay
unchanged. Snapshot construction remains private and successful snapshots carry
no errors.

## Shared renderer and host routing

The root-free vesper-diagnostics adapter depends only on vesper-core,
vesper-span, serde_json and already-locked ariadne0.5. Frozen adapter API: `prepare<'a>(&'a LineIndex, &[core::Diagnostic], Limits)`
returns `Result<Prepared<'a>,Error>`. Prepared is immutable, borrows the exact index
and owns checked span diagnostics. `to_lsp(uri)` returns `Result<Vec<Value>,Error>`;
`terminal(file)` returns `Result<Vec<u8>,Error>`. Limits contains validated
`span:vesper_span::Limits` and `max_name_bytes:usize`. Default source128KiB,
work64MiB/output256KiB/name4096; HARD uses span HARD/name4096. Name labels reject
controls; terminal is deterministic plain output with explicit BYTE indexing.
LSP emits standard relatedInformation URI/range/message and optional `data.help`,
preserving primary code/message, source=vesper and severity1.
No UTF-8/UTF-16 min-clamping, guessed positions, unbounded output or partial frame.

Default editor source is 128 KiB; a bounded CLI profile can use span HARD1MiB.
Direct API limits must validate hard ceilings. Invalid spans/metadata/resources
return explicit failure before cloning/rendering. The adapter validates supplied
data, not semantic causality; only real checker producers establish that origin.
No fixes are minted from labels. The adapter performs no filesystem/compiler/
JIT/WASM/proof I/O. An executable example self-checks real Unicode multi-span
terminal and LSP output.

Root CLI keeps successful behavior and original JSON fields; rich errors add
optional metadata. If source/render metadata is outside the renderer profile,
print a truthful bounded raw coded message/byte location and unavailable reason,
never guessed/clamped squiggles or a suppressed compiler error. Editor conversion
failures publish explicit current-version/hash analysis_unavailable warnings,
not partial or stale errors. Initialization records publishDiagnostics relatedInformation/dataSupport;
optional relatedInformation and data.help are omitted unless negotiated. Existing
unavailable-warning status metadata remains intact. Existing code actions still recompute and verify
actual lint candidates; client labels/fix/proof flags are never authority.

## Delivery and validation

Freeze APIs before dependent writers. Temporary dependency copies/registration
are allowed for private builds, then restored before owned-only commits. The
coordinator integrates and cross-reviews each delivery, tests actual negative
source/CLI/JSON/framed relatedInformation/Unicode/current-edit/recovery flows,
runs full workspace with Lean, strict Clippy and existing source/editor/protocol
acceptance, and observes native Linux/Windows/macOS CI. Publish a protected PR
linked to all seven issues and return to clean main; preserve full branch history.
Record exact supported causes and remaining recovery, completion, automatic
substructural fixes, per-function PCG, proof lenses and latency scope. No full
design completion claim follows from this batch.
