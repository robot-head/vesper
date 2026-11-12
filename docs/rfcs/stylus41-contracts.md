# Macro and partial-buffer formatting contracts

Claims were published before writers: #111 macro formatter, #112 recovery
formatter, #113 CLI/stdio acceptance, #114 root integration. Baseline is published
PR110 head6faa7ec53284db360864f9bbf3e97f14a7e3aef4. Main remains47a093e while the
external Warp owner awaits their merge decision for #101. Its Rust1.100 lint fix
is not duplicated here. Fresh open issues/actual PR filename checks found no
fmt/LSP action overlap. The live design is unchanged at41sections/AppendixA,
modified2026-11-12T09:18:30.204Z; §39.1 was reread.

## Ownership

| Issue | Branch | Exclusive writes |
| --- | --- | --- |
| #111 | stylus41/macro | fmt/src/lib.rs, new macro_source.rs, tests/macro_source.rs, macro report |
| #112 | stylus41/recovery | new fmt/src/recovery.rs, tests/recovery.rs, recovery report |
| #113 | stylus41/acceptance | new tests/stylus_cli.rs, scripts/check-stylus.py/test-stylus.sh, fixtures/stylus, workflows/stylus.yml, acceptance report |
| #114 | stylus41/integration | fmt_commands.rs, lsp/actions.rs, optional help routing, shared registrations/path filters, this contract and integration report |

Root alone registers `pub mod recovery`/public reexports in fmt lib after the macro
owner's checkpoint; no concurrent edits to that file. Other existing compiler,
syntax, checker, IR, ABI and macro frontend paths are read-only. Changes to these
agreements require a published claim amendment before edits. Worktree/private
cache only, one Cargo job/debug0/incremental0, root target/parallel-next is
coordinator-only. Disk initially4.1GiB free: reuse light private caches; no new
Wasmtime build or toolchain install. No performance/RSS/proof claims.

## Macro formatting

`format_source(&str, Limits)` retains its API and old ordinary behavior. It may
format macros only after actual `vesper_syntax::macro_source::parse` succeeds for
the supported fixed Expr source profile. Unknown payloads remain opaque/errors.
The actual lossless recovery CST may supply tokens for an already separately
validated macro module; its ERROR nodes do not represent semantic acceptance.
Every CST token kind/text/range must match actual lexer/source bytes. No guest,
macro expansion, checking or native execution occurs during formatting.

The old source/delimiter/chain/output/Doc guards remain. All successful output
passes exact ordered non-whitespace token+comment conservation, profile reparse,
and a second byte-identical formatting pass. Comments/string payloads remain
byte-exact; ordinary blank whitespace canonicalizes. Unsupported macro grammar
and lexical/resource errors never return a replacement. Default invalid ordinary
source remains rejecting.

The macro owner extracts a crate-private helper:
`format_with_work(source: &str, limits: Limits, work: &mut Work) -> Result<FormatResult, Error>`.
`Work` remains in lib.rs with current `new/charge` and fields; child recovery module
can access parent-private items. Public format_source creates one Work then calls
the helper. Recovery calls the same helper with its shared Work, never a fresh
per-function budget. No final recovery registration is committed by #111.

## Recovery formatting

New public API in `vesper_fmt::recovery`:
`format_recovery_source(source: &str, limits: Limits) -> Result<RecoveryFormatResult, Error>`.
Root also reexports the function/result/edit types at crate root.

RecoveryFormatResult has private fields and getters `output()->&str`,
`changed()->bool`, `edits()->&[RecoveryEdit]`, `source_hash()->[u8;32]`,
`formatted_declarations()->usize`,
`into_output()->String`. RecoveryEdit has private fields, `span()->Range<usize>`
and `replacement()->&str`. Do not implement caller constructors or Deserialize.
Span/hash identify original source. Edits are sorted, disjoint original-byte
ranges, on UTF8 boundaries, confined to actual complete top-level functions.
The declaration count includes safely formatted/validated functions whose bytes
are already canonical. LSP returns FormatterUnavailable when this count is zero,
and can return empty edits for idempotent safe siblings in a damaged buffer.
No edit can include a gap, comment outside that declaration, damaged body, or
opaque payload. Zero safe changes is successful unchanged with empty edits.

Build actual sealed RecoverySyntax; independently validate eligible complete
top-level function items with the trusted complete-item parser. Never consume
caller descriptors or spans. Global lexer errors reject. Unsupported/opaque
functions remain unchanged; lexical uncertainty or resource errors reject the
entire operation. Whitespace outside complete declarations and every damaged/
opaque gap remain byte-for-byte identical. Whole non-whitespace token/comment
stream and actual declaration kind/name/status inventory must be conserved.
Reparse the actual candidate and run the complete recovery formatting pass again
for byte idempotence; no recursion through the public API or budget reset.

All parsing/source copies, token scans, fragment checks/formatting, edit storage,
output assembly and validation passes use one cumulative Work. Preflight before
clones/reserves, bound output/edit count and parser limits. Underlying parser has
its own finite limits and can conservatively refuse valid large buffers. Limits
do not imply RSS bounds. No checked source, World, diagnostics suppression,
semantic repair, type/linearity or proof authority is constructed.

## CLI and editor behavior

Frozen commands: `vesper fmt FILE [--check] [--json]` formats ordinary/recognized
macro source; `vesper fmt FILE --recovery [--check] [--json]` explicitly allows the
syntax-only complete-function mode. Existing bounded regular UTF8/no-final-
symlink input and successful-only atomic replacement guards apply unchanged.
JSON schema_version1 retains existing fields and adds `mode` equal `source` or
`recovery`, plus `edits` as count in recovery mode. `changed` and check-mode exit
status reflect actual output; --check never writes; all failures preserve input.

LSP textDocument/formatting first uses normal format_source. Only Syntax or
OpaqueMacro errors can try recovery formatting; resource/config/conservation
errors do not trigger a fresh-budget escape. The fallback reserves from a single
bounded formatting-operation allowance before both attempts, rather than reset.
Root reserves two fixed halves of the existing128Mi work allowance:64Mi for
ordinary/macro formatting and64Mi for recovery fallback, never recycling unused
units from a rejected stage. Resource/conservation errors cannot enter fallback.
Use actual current ActionDocument text/index/version/hash. Convert only validated
recovery edit byte spans through the current UTF16 LineIndex. Return proposed
edits; no applyEdit, file/URI I/O, diagnostics clearing or semantic acceptance.
Provenance marks scope complete_declarations for fallback and explicitly excludes
semantic checking. Ordinary valid formatting/provenance stays compatible.

## Acceptance

Actual macro source formats/idempotently conserves comments/string bytes and
still explicitly expands/checks/runs42 via macro41 commands. Unknown/repetition/
nested syntax rejects unchanged. Damaged functions and malicious fake declarations
inside comments/strings/opaque macros never change. Independent complete siblings
format while normal check continues rejecting the damaged program; semantic E0303
or E0302 is never repaired by formatting. Cover CRLF/Unicode byte-to-UTF16 edits,
current revision after changes, stale ActionDocument stamps, bounds before output,
check mode and file rejection preservation. Existing formatter, CLI, editor,
recovery and macro tests remain enabled. Native3OS CI, actual scripts, strict
Clippy/formatting and current dependency-aware protected PR validation are required.
