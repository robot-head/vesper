# Macro and partial-buffer formatting integration — issues #111–114

The formatter previously rejected all macro-looking source and damaged editor
buffers. Default `vesper fmt FILE` now accepts the separately validated fixed
expression-macro profile. The explicit `vesper fmt FILE --recovery` mode formats
only independently complete top-level functions and preserves damaged/opaque
source regions byte-for-byte. It uses the existing successful-only atomic file
replacement path; `--check` proposes no file mutation.

The formatter never expands macros, runs components, or creates checked source.
Recognized macro modules are validated by the actual macro source parser before
their lossless recovery CST provides token locations. Exact ordered token and
comment conservation, reparsing and byte idempotence remain success conditions.
Unknown macro syntax stays opaque. Recovery additionally uses sealed original
syntax plus the trusted complete-item parser, with lexical top-level delimiter
qualification before any edit; diagnostic recovery alone cannot authorize an
edit inside a damaged body.

The CLI keeps JSON schema_version1 and adds `mode: source|recovery`; recovery
reports the edit count. Existing regular-file, UTF8, size, final-symlink,
permission, staging and observed-source-drift guards remain enabled. Default
ordinary invalid-source formatting rejects; recovery is an explicit CLI choice.

Editor formatting first tries normal/recognized-macro formatting. Only syntax
or opaque errors may enter the complete-declaration fallback; resource,
conservation or idempotence errors cannot retry. Two fixed64Mi work-unit stages
reserve a128Mi upper bound for the entire operation. Recovery itself shares one
budget across source/parser copies, all fragments, output/edit storage,
conservation, inventory comparison and the second formatting pass.

Fallback edit ranges come from the current private recovery result and must
match the ActionDocument source hash. The current LineIndex converts byte spans
to UTF16 ranges. Proposals retain the current version/hash association and mark
their provenance as syntax-only complete-declaration formatting. With no safe
declarations formatting stays unavailable; canonical safe siblings can return
empty edits. No URI/file I/O, applyEdit, diagnostics suppression or semantic
repair occurs in the editor adapter. Normal checking still rejects damaged
source and real linear leaks.

## Coordination and verification

Claims #111–114 were created and checked for comments before writers. The
published stylus41-contracts.md froze API, CLI, schema and exclusive paths.
Implementations used separate worktrees/light private caches. Fresh actual
external PR101 filenames contained only Warp/TCP/Cargo/report paths; none were
edited here. The live document remains at41sections/AppendixA, modified
2026-11-12T09:18:30.204Z, and §39.1 was reread. This branch initially depends on
published PR110 head6faa7ec because it consumes the new actual macro parser.

After the final boundary guard, the combined formatter suite passed 31 tests,
including the private-construction compile-fail case, with no ignores. Integrated
root suites passed 87 tests across formatting, actions, actual CLI/stdio, recovery
and source macro checking. Independent actual acceptance passed 36 commands and
three stdio sessions, including an explicitly expanded source run returning 42;
all seven Rust CLI tests also passed against the final guarded binary. Strict
workspace all-target Clippy and formatting pass on Rust 1.99.1. Existing source
tooling (192 callbacks), editor (19 sessions), diagnostics (17 CLI/16 stdio),
recovery (15 stdio/3 batch), and macro (115 commands/11 explicit runs) acceptance
all still pass.

The independent review caught the diagnostic parser's nested
function resynchronization before publication; the formatter adds its own
top-level qualifier and regression coverage rather than treating every Complete
recovery descriptor as edit permission. Native3OS workflow tests real formatting,
CLI/stdio and current dependency integration. Full workspace Rust1.100 CI remains
subject to external PR101's already-owned Warp lint fix; local/selected checks
are not represented as a passing full workspace gate.

## Remaining scope

This covers known fixed Expr macro source and complete top-level functions in
otherwise damaged buffers. Unknown macro payloads, ambiguous missing-delimiter
boundaries and incomplete declarations are not rewritten. Recovery formatting
does not establish semantic completeness, incremental minimal rechecking,
macro-generated lint provenance, formal conservation proofs or latency/RSS
guarantees. Those design requirements remain part of the active implementation
goal.
