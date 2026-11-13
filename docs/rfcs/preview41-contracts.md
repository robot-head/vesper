# Explicit current-source macro preview

Claims #152–155 precede implementation: database snapshot, bounded provider,
actual acceptance, and root integration respectively. Fresh open-board checks
found only our existing macro/formatter/proof stack and Claude wave-five
#121–136. Those domain crates, Warp/sim and native/main CLI hooks remain reserved.
No database/LSP preview overlap was found. Baseline is published PR142
head9ae46082616c87e37a23072a4c651ed2d030c59b, stacked on115/110 and already synced
with mainaf03122. Main84407eb additionally contains wave-five planning docs.
The live design remains41sections/AppendixA, modified2026-11-12T09:18:30.204Z;
§§16.4 and21.3 were reread.

## Ownership

| Claim | Branch | Exclusive writes |
| --- | --- | --- |
| #152 | preview41/database | new database/macro_preview.rs, database.rs only child registration/topdoc distinction, tests/database_macro_preview.rs, database report |
| #153 | preview41/provider | new lsp/macro_preview.rs, tests/macro_preview.rs, provider report |
| #154 | preview41/acceptance | new macro_preview_cli.rs, check/test-macro-preview scripts, macro-preview.yml, fixtures/macro_preview, acceptance report |
| #155 | preview41/integration | lsp/session.rs narrow route/params/current-input association/capability, lsp.rs registration, this contract/integration report and consumed-path filters |

Separate worktrees/private caches, one Cargo job/debug0/incremental0. Root
target/parallel-next remains coordinator-only. Initially3.2GiB free: reuse light
existing caches, no Wasmtime/toolchain install. No AST/IR/checker/native ABI or
normal semantic provider changes. No latency/RSS or per-function checking claim.
All claims stay reserved through reviewed merge. Exact amendments publish before
dependent writers; global root registrations remain coordinated.

## Semantic and execution boundary

This is an explicit experimental `vesper/macroPreview` request on current open
unsaved text. It compiles the complete supported pure Tier1 source through the
actual forge_frontend pipeline: original macro parse, current whole-body hygiene,
identity lowering, full expanded-source checker, actual-body authority graph and
gate, ordinary IR validation. It never manufactures CheckedSource for original
macro text. Ordinary diagnostics/hover/definition continue using guarded_analysis
and remain unavailable on unexpanded macro programs, even after a preview.

No WASI component, Lean, native/interpreter code, repository hook or filesystem
URI is executed/read. The fixed default config is @local/source with pure empty
macro grants/caller ceiling. No client package/policy/graph/World/approval flag
is accepted. This is not source Tier2 or cross-package linking. Caller-origin
antiquotation retains the compiler's ordinary origin semantics.

Whole-input memoization reuses an immutable complete checked expanded artifact
only when actual SourceFile text is unchanged in the current compiler process.
All context is fixed by this profile. This is not raw guest execution caching or
reusing an approval under changed source/graph/policy. Every changed helper body,
definition, argument or comment observes full source, without a header/signature
cutoff. The query must not call legacy parse_file/check_file/signature queries.

## Database API

New public module `database::macro_preview`:

`compile_limits()->forge_frontend::CompileLimits` returns fixed editor limits:
source128KiB, expanded128KiB, maps1024, metadata2MiB, identifier256, work128Mi.
The metadata cap was amended before dependent writers changed it: actual small
helper/linear fixtures exceeded the initial256KiB cumulative syntax preflight
and refused before their authority/checker stages. The new cap matches the
existing compiler default; provider output remains independently capped at1MiB.
`#[salsa::tracked(returns(ref), no_eq)] pub fn preview(db:&dyn salsa::Database,
file:SourceFile)->MacroSnapshot`.

MacroSnapshot has private fields/no constructor/Deserialize. Methods:
`source_identity()->&SourceIdentity`, `source()->Option<&str>`,
`expanded()->Result<&ExpandedChecked,&CompileFailure>`,
`outcome()->Result<Arc<ExpandedChecked>,Arc<CompileFailure>>`.
Retain bounded actual original bytes as Option<Arc<str>> plus actual complete
compiler result. Equality compares source identity and Arc allocation identities,
never deep Worlds. Salsa unchanged-input caching retains the real Arc; changed
input recomputes all compiler guards. Session no-op text changes keep the existing
SourceFile input while advancing document version. A direct caller forcing an
input setter, even with equal bytes, explicitly creates a fresh input revision
and reruns the full compiler; no interned side cache is introduced.
SourceIdentity remains parent-sealed.

Legacy SourceFile can contain oversized strings: refuse through the actual
compiler before copying/hashing such input, keep source=None and opaque identity.
For bounded input, prepay a separate finite1Mi snapshot allowance for source
retention/hash/association before cloning. The compiler retains its128Mi allowance;
there is no reset or unbounded extra traversal. No URI/version in the query:
those are current-session association metadata supplied by the provider.

## Pure bounded provider API

New public module `lsp::macro_preview`:
`Document::new(uri:&str,version:i32,source:&str,snapshot:&MacroSnapshot)
->Result<Document,Error>` with borrowed private fields. Source must match the
snapshot's exact retained bytes (not merely a client-supplied hash); successful
compiler artifact original source/hash must also match. URI is only a label.

`Limits` defaults/HARD: max_source_bytes128KiB, max_uri_bytes4096,
max_output_bytes1MiB, max_work16Mi (HARD32Mi work, other caps same). Zero or
above-HARD limits reject. `render(&Document,Limits)->Result<serde_json::Value,Error>`.
Document association construction is bounded and no World/AST deep clone occurs.
Use a bounded serializer sink before Value materialization; prepay finite
serialization/parse/payload-copy envelopes before allocations. Final encoded
payload <=1MiB leaves room under the existing2MiB JSON-RPC frame cap. Include
current envelope only after complete success. Error enum must distinguish
invalid document/source mismatch/resource/serialization (fixed bounded errors).

Success schema `vesper.editor.macro-preview/1`:
`status: expanded_checked`, `scope: expanded_source_only`, `tier: 1`,
`uri`, `version`, `source_blake3`, `expanded_blake3`, `package`,
`source_bytes`, `expanded_bytes`, `macro_definitions`, `invocations`,
`expanded_source`, `range_unit: utf8_bytes`, actual `source_map`, actual `audits`.
The invocation audit's existing expanded_function scope is preserved. Maps are
byte provenance anchors, not a promise that alpha-renamed lexemes are identical.
Never include an original semantic World, checked_source or approved flag.

Compiler rejection schema uses the same schema/scope/tier/current uri/version/
source_blake3 and `status: rejected`, actual `stage`, bounded `message`,
`expanded_source: null|string` (rejected diagnostic candidate only), actual
`diagnostics`, `diagnostic_source: original|expanded|none`,
`range_unit: utf8_bytes`, `source_map: []`, `audits: []`.
Use original only for Parse-stage diagnostics; expanded only with actual rejected
candidate bytes; otherwise none. Do not invent mapped-original diagnostic spans.
No accepted expanded hash/count/semantic fields on rejection. Provider resource
or association errors return no partially accepted Value.

## Session wire behavior

Request: `vesper/macroPreview`, params exactly
`{"textDocument":{"uri":STRING,"version":OPTIONAL_I32}}`.
Unknown outer/nested fields (including source/policy/graph/approved) reject as
invalid params; version if supplied must equal current server document version.
Stale expected version uses -32801 ContentModified. Unopened URI is existing
invalid params. No server state or diagnostic publications change on preview.

Root obtains the current Document and existing SourceFile mirror, validates
text/version association, calls the tracked query and bounded provider. Reuse
the existing accepted-change, close/reopen and64-revision database reset paths:
no stale SourceFile IDs survive reset, no closed document can be queried. Exact
association errors fail closed. Provider resource/unavailable maps to bounded
-32003 error data, never a partial accepted source result.

Advertise initialize capability
`experimental.vesperMacroPreview = {version:1,tier:1,scope:expanded_source_only}`.
This is an opt-in pure preview, not enabling implicit semantic macro processing
in ordinary check/LSP or an execution capability grant.

## Acceptance

Actual query tests verify unchanged Arc memoization and fresh current source/
artifacts after same-header helper body authority changes, current hashes/maps,
real E0303 duplication/leaks, malformed/unsupported/oversize refusals. Direct
provider tests cover exact source mismatch, arbitrary URI labels, current version
metadata, success audit/byte anchors, truthful failure diagnostic source and
bounded-output refusal before Value materialization.

Actual stdio tests cover open/change/noop, explicit checked-expanded preview and
normal parse_error/hover-unavailable coexistence, both hygiene directions,
run42 only by a separate explicit normal CLI run, unchanged-header effect-body
changes, stale versions, >64 revisions, close/reopen/foreign URI, malformed/
resource/forged-context requests and no filesystem or component execution. Keep
legacy CLI/editor/recovery/macro guards enabled. Native3OS workflow, strict
Clippy/formatting and actual executable acceptance are required; no stubs or
performance claims. The broader design goal remains active.
