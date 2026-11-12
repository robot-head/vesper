# Lossless recovery integration

Claims #95–#100 were opened after checking open issues, PRs and the actual changed
paths of external Warp branches. Each writer used a separate worktree. Source
ownership and APIs are recorded in [the contract](../rfcs/recovery41-contracts.md).
The Google document still has 41 sections and Appendix A, modified
2026-11-12T09:18:30.204Z. This work advances §21.2/21.3.

## Current-source editor boundary

The session mirrors accepted document transactions into both the existing raw
SourceFile and an actual SyntaxDocument/RecoveryFile. Syntax update failure drops
the previous mirror for the accepted new text. Closing a document or reaching 64
accepted revisions recreates the database, every input ID and all remaining
syntax mirrors. Rejected store transactions retain their original state.

The experimental `vesper/recoveryStatus` request reads only an open buffer and
returns its version/hash, syntax inventories and actual update path. Byte ranges
are explicitly syntax-only; the request does not return a checked World, type,
liveness, ownership, completion or proof certificate. URI labels do not load files.

Normal guarded full-source analysis remains the first path and the only authority
for semantic navigation, actions and signature search. Only an original Parse
failure uses recovery syntax errors and the tracked diagnostic-only query over
the actual incremental snapshot. The session verifies the exact current source
and hash before publication. Damaged functions are unavailable in their entirety;
independent complete functions may still publish real checker errors. Repairing
syntax resumes normal full-program checking, including strict linear exhaustion.

Publication retains `parse_error`, with explicit analyzed/unavailable recovery
coverage. The shared renderer validates the aggregate original-byte diagnostics
before client capability filtering. Renderer/span refusal retains the existing
`analysis_unavailable` warning and marks recovery scope unavailable.

## Validation

All production lanes are integrated. The coordinator's full locked workspace run
passed 2,234 tests across 286 suites, with Lean 4.24.0 enabled, zero failures and
zero ignored tests. This includes incoming external Warp work through main
`30216c1`, the final parser boundary fixes and actual CST/Salsa integration.

The final binary also passed the existing source-tooling acceptance (including
192 measured callback calls), 19 editor child sessions, 17 diagnostic CLI checks
and 16 diagnostic stdio sessions, and canonical Protobuf/DER/Arrow/lint/PMU CLI
acceptance. The new recovery acceptance passed 15 framed sessions and three strict
batch repair checks. No timing threshold or hardware-counter success follows from
these checks.

All seven final recovery CLI tests passed after adding current-range and all-four
semantic-gate assertions. Strict all-target workspace Clippy and formatting passed.
Native Linux/Windows/macOS recovery CI is
configured; final PR CI results are tracked separately from these local checks.

## Limits and remaining design

The fast path reparses only an eligible complete nongeneric function body under
its sealed real parser context, replaces a Green subtree and reuses unaffected
syntax. Uncertain edits fully reparse or explicitly refuse. Semantic checking can
still parse/check current complete components or the entire current file. Logical
budgets do not establish an RSS bound, deadline or sub-2ms latency.

There is no new core Incomplete expression, PoisonedRecovery semantic capability,
completion inference, per-function PCG cache, asynchronous proof lens or automatic
macro execution in this slice. Full §21 and the overall specification remain
unfinished. Core AST/IR, arithmetic, interface/native ABI, AST-based Lean emission
and ordinary checker acceptance remain unchanged.
