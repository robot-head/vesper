# Bounded LSP formatting and checked actions (#84)

Implemented pure providers in `src/lsp/actions.rs`, using actual `vesper-fmt`,
`vesper-span` and the extracted source lint/frontend APIs. No filesystem,
subprocess, JIT, macro/proof execution, automatic client applyEdit, dispatcher,
database, root manifest or existing CLI production changes are part of this
commit. Specification §§21 and 39 and the editor ownership contracts were read.
The broad scaffold's partial-buffer/macro formatting, projection/arena rewrites,
proof lenses, zero-GC and latency claims are not implemented or inferred here.

## API and integration

`ActionDocument::new(uri, version:i32, source, &LineIndex)` validates the opaque
URI/source bounds and exact index/text identity. Immutable borrowed text/index
cannot drift during a call. `.stamp()` returns `SourceStamp { version,
source_hash:[u8;32] }`, an actual BLAKE3 digest. Every operation compares an
expected stamp before analysis, and rejects old versions/source identities.
The coordinator supplies the current server document and stamp; client payloads
cannot certify that a source is checked.

`format_document(&document, expected_stamp)` returns `Formatting`, with public
`edits` and source/formatter `provenance`. `Formatting::to_json()` returns the
standard LSP TextEdit array; `workspace_edit_json()` supplies an optional explicit
versioned WorkspaceEdit representation. Formatting has no user options. It uses
the actual canonical 100-column/four-space formatter, which reparses and checks
nontrivia-token/comment conservation and idempotence. Syntax-incomplete or
unsupported opaque syntax returns FormatterUnavailable. Formatting preserves
syntax/token identity; it does not claim semantic checking of a type-invalid
buffer. Empty edit arrays mean already canonical text.

`code_actions(&document, expected_stamp, selection:Range, request:ActionRequest,
&Policy, ConfigProvenance)` returns zero or one typed `CodeAction` or an explicit
`ActionError`. `ActionRequest::QuickFix` invokes freshly recomputed, policy-first
selected-range lint preparation with actual SemVer checking enabled. A nonempty
range intersects real finding spans; a caret uses half-open containment. Other
functions/effects remain unchanged. `FixAllUnusedEffects` separately uses the
bounded editor frontend and explicit whole-document lint preparation. Its title
states the supported unused-capability scope and kind is `source.fixAll.vesper`;
the selected action kind is `quickfix`.

Every offered lint candidate passes complete parse/type/effect/PCG checking,
post-edit lint checking and actual supported-interface SemVer comparison in the
shared pure tooling API. The provider additionally requires a compatible actual
SemVer report and coherent candidate/change metadata. Allowed findings cannot
authorize edits. Unsupported evidence, source/linear errors, unsupported
interface compatibility and unsafe candidates produce no edit and an explicit
LintUnavailable reason. Other remaining lint diagnostics are not called clean.
There are no client diagnostic/fix/verified/proof inputs to this API.

`CodeAction::to_json()` emits actual LSP CodeAction fields and
`edit.documentChanges` with the URI and exact document version. `data` records
source/candidate BLAKE3, selected range, actual policy report/effective-rules hash,
actual SemVer report and applied edit count. ConfigProvenance is a bounded caller
origin label, not filesystem authority or proof that supplied raw policy bytes
were read here; the effective-rules hash is independently computed. Edits use
whole-document ranges derived by LineIndex, so no common-prefix/suffix byte cut
can split UTF-8. The shared selected lint normalizer alone decides the actual
semantic changes; replacing the whole text does not broaden quick-fix selection.
Clients must obey versioned edit semantics. Nothing automatically applies edits.

## Bounds and failures

Sources and offered final replacements are capped at 128 KiB. URI/provenance
path labels are capped at 4096 bytes; explicit policy provenance has at most
64 KiB of config bytes and exactly 64 ASCII hexadecimal BLAKE3 characters.
These direct typed API checks occur before lint processing/metadata publication.
Selection conversion strictly rejects surrogate interiors, CRLF interiors,
reversed/out-of-range spans and invalid source boundaries. One replacement and
one action are returned per request. Formatter, frontend, lint and SemVer retain
their own additional token/node/depth/work/unsupported profiles; these are logical
resource profiles, not elapsed-time/RSS guarantees. Lint failure descriptions are
bounded to 2048 Unicode scalars. No edit is returned on failure.

ActionError distinguishes InvalidDocument, InvalidPolicyProvenance,
StaleDocument, InvalidRange, FormatterUnavailable, LintUnavailable and Invariant.
The coordinator maps failures to truthful LSP errors/analysis-unavailable output.
No disabled action pretends that checking or compatibility succeeded.

## Validation and dependencies

Private `/tmp/vesper-editor41-actions-target`, one build job, zero dev/test debug
information and incremental compilation disabled:

- `cargo test -p vesper --test lsp_actions --locked --offline`: 11 tests pass.
  Tests apply the returned versioned JSON edits through strict UTF-16 conversion,
  independently run the actual parser/checker and interface SemVer comparator,
  and verify selected-function isolation, explicit fix-all, comment/shadowing/
  Unicode preservation, canonical formatting/idempotence, caret exclusive ends,
  stale version/hash/index mismatch, invalid UTF-16/ranges, allow suppression,
  unsupported/linear-error refusal and source/output/provenance boundaries.
- `cargo clippy -p vesper --lib --test lsp_actions --locked --offline -- -D warnings`
  and `cargo fmt --check -p vesper` pass.
- No client-payload authority parameter exists. The root framed integration suite
  owns the actual forged `context.diagnostics`/fix/verified request test; this
  pure provider test suite does not pretend to parse transport context.

Dependencies were privately cherry-picked: Span #79 `d300142` and pure tooling
#81 `14dfbb6`. Root integration must register the Span dependency and
`pub mod actions;` in its existing LSP module. Temporary local manifest/module
wiring was restored before this lane's commit. No dependency source, leaf lock,
root registration or build artifact is included in this lane's changes. No new
Lean theorem/axiom or formal compiler correctness claim is added.
