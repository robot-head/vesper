# Bounded editor integration (#79–#85)

This batch implements a bounded synchronous editor slice of specification §21,
using the production parser/checker over unsaved in-memory text. The live document
metadata was rechecked at integration: 2026-11-12T09:18:30.204Z, matching the recorded
41-section/Appendix A snapshot. The specification snapshot validator and its 14
unit tests pass. The full design remains in progress.

## Coordination and boundaries

Issues [#79](https://github.com/robot-head/vesper/issues/79) through
[#85](https://github.com/robot-head/vesper/issues/85) were created before writers
started. The open issue and PR board was checked again during integration; the
external Warp lanes remained unchanged and reserved. Exact file ownership,
branches, budgets and shared APIs are in `docs/rfcs/editor41-contracts.md`.
Each writer used an isolated worktree/private target. Independent cross-reviews
covered protocol, database, navigation, actions and root framing/search; session
review additionally covered lifecycle, immutable revisions and database reset.

Root integration registers `vesper-span` and `vesper-lsp`, extracts pure source
consumers without changing batch CLI contracts, and replaces the minimal server
while preserving `lsp::serve(BufRead, Write)` and `vesper lsp`. Core AST/checker,
Place IR, native ABI, interface and AST-based Lean schemas remain unchanged.
The `k_i ⊑ m_i` linear-exhaustion field restriction and arithmetic semantics remain
in force. There are no new registry dependencies or version changes.
The root-free `vesper-lsp` crate owns transport/document state; semantic server
integration stays in the compiler facade while the checker still shares its World.

## Server profile

The server advertises UTF-16 positions, open/close and incremental synchronization,
checked hover, conservative definitions, document/open-buffer workspace symbols,
canonical formatting, and checked `quickfix` / `source.fixAll.vesper` code actions
when the client negotiates code-action literals and versioned document changes.
Other clients see codeActionProvider=false and explicit unavailable status for
direct action requests; edits are never downgraded to unversioned changes.
Clients advertising hierarchical document symbols receive DocumentSymbol arrays;
other clients receive SymbolInformation arrays. Hover text is plaintext. Type mode
is distinguished from a current ownership obligation. Unsupported field/trait/model
and cross-file resolution never produce guessed definitions. Capability receivers
are distinguished from same-named ordinary global functions.

Formatting uses the actual 100-column/four-space token/comment-preserving formatter.
It is a syntax transformation, not a proof that a buffer typechecks. Code actions
recompute current findings, apply default lint policy and selection, recheck the
entire candidate and compare supported interfaces for SemVer compatibility.
They ignore client diagnostic/fix/verified assertions. Edits include the current
document version and source/candidate/policy provenance. Returned whole-document
replacements contain only selected validated changes; explicit fix-all is separate.
Clients must honor documentChanges versions. No edit is automatically applied.

Initialize must precede feature requests; duplicate initialize and requests after
shutdown fail. Shutdown returns null; exit before shutdown fails the process.
Unknown methods return -32601, malformed parameters -32602 and unavailable checked
analysis -32003 with explicit status. Notifications receive no JSON-RPC response;
rejected changes produce a bounded log message and preserve the prior source,
version and compiler mirror. Invalid envelopes with known body boundaries receive
a null-ID error and the next frame is still processed. Invalid framing terminates
the stream. Response encoding completes before any frame bytes are written;
refused feature output becomes a bounded same-ID -32004 error. Writer I/O failure
terminates the stream because a partial write cannot be repaired.

Strict JSON-RPC uses version 2.0, i32 or bounded string IDs, no batches/extensions,
and object/array parameters when present. In particular, callers omit parameters
for shutdown/exit (an empty object is also accepted); top-level `params:null` is
outside the supported transport profile. Decoded duplicate keys are rejected
before DOM overwrite. Every URI is an opaque exact label: opening a file URI never
reads disk, resolves imports, executes build scripts, WASM, JIT, macros or proofs.

Default bounds: 128 KiB/document, 32 open documents, 2 MiB total live text,
64 changes/batch, 4096-byte URIs, 2 MiB request/reply bodies, 8 KiB total headers,
1024-byte header lines, 32 headers, 64 JSON container levels and 65,536 JSON nodes.
Additional source/AST/type/diagnostic/metadata, provider work and output limits are
reported in the lane reports. Source state commits only after the whole edit batch
validates. Compiler state is rebuilt from current bounded documents on every close
and after 64 accepted open/change revisions, discarding every old SourceFile ID.
No arbitrary old revision remains in the active compiler database indefinitely.
These are logical size/work limits, not hard latency, RSS or termination guarantees.

Diagnostics publish the current version/hash plus a `vesper.status` of `checked`,
`parse_error`, `semantic_error` or `analysis_unavailable`. Parse/semantic diagnostics
use actual compiler spans; unsupported/resource/invalid-span states publish an
explicit warning at the index origin without inventing a semantic cause or checked
success. Position-bearing features always use current text, independently of a
stable semantic signature. Supported signature consumers use actual bounded typed
contracts, inferred latent results and ABI-relevant settings; unavailable extraction
falls back to full source identity. Body edits cannot hide inferred type changes.

## Typed signature-search extension

`vesper/signatureSearch` accepts an object with required `query` and optional `uri`.
The URI restricts search to that exact open buffer; omission searches all open
buffers. Every selected buffer must have current checked analysis, otherwise the
request returns explicit unavailable status instead of a silently incomplete list.
The strict query is the same full structural contract used by `vesper doc --search`:

```json
{
  "query": {
    "parameters": [],
    "result": {"Named": ["u64", []]},
    "result_max_mode": "Copy",
    "effect_ceiling": {"instances": [], "tail": null}
  },
  "uri": "untitled:example"
}
```

The result is `{scope:"open checked buffers", documents:[{uri,source_blake3,
result:{matches:[{function,score}],unsupported_functions:[...]}}],
proof_status:"unverified"}`. Full structural types, passes, selectors, modes and
closed labeled effect instances are compared by the actual `vesper-doc` engine.
Generic/open-row/unresolved contracts remain explicitly unsupported. A successful
empty result does not claim imported workspace coverage or a proof. Queries are
independently depth/node/string guarded before bounded 32 KiB serialization and
strict nested-field parsing, even for an empty workspace. Search caps aggregate
source at 2 MiB, documents at 32 and serialized result at 1 MiB.

## Validation

Local integration validation passed with one Cargo build job, dev/test debug0,
incremental disabled, locked/offline dependencies and the root private target:

- `cargo test --workspace --locked --offline`, with
  `VESPER_LEAN=/home/agent/.elan/bin/lean`: 1,845 tests across 223 suites, zero
  failures/ignored. The final stdio capability-test expansion was then compiled
  and run separately: `cargo test -p vesper --test lsp_cli` passed all five tests.
- `cargo clippy --workspace --locked --offline --all-targets -- -D warnings`:
  passed, including the final five-test stdio source.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `scripts/test-editor.sh`: passed 19 actual framed child sessions, applied and
  rechecked returned candidates, plus both executable span/protocol examples.
- `scripts/test-source-tooling.sh` and `scripts/test-canonical-protocols.sh`:
  passed their existing actual CLI/library acceptance. The optional PMU provider
  remains explicitly unavailable in this environment; no hardware success claim.
- Specification snapshot validation and all 14 snapshot unit tests: passed.

Native CI results are recorded on the linked PR; local checks alone do not
establish Windows/macOS behavior.
The dedicated `.github/workflows/editor.yml` runs real stdio/provider tests,
strict Clippy, acceptance and executable span/protocol examples on Linux, Windows
and macOS. Windows executable selection explicitly prefers the actual `.exe`.

## Remaining design work

This batch does not implement block-local CST reparsing, PoisonedRecovery,
per-function PCG invalidation, obligation-guided completion, imported workspace
resolution, persistent macro caches, background Lean verification/code lenses,
lifetime-free extract-function, causal core-diagnostic migration or the stated
sub-2ms latency target. Span diagnostic DTOs support checked multi-span/fix data;
existing compiler diagnostics still supply their actual single primary span.
No theorem/audited badge, hard deadline or complete §21/design claim is implied.
See the lane reports for exact support and conservative refusal cases.
