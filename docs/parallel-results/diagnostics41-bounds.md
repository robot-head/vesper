# Issue #91: bounded causal diagnostic metadata

Implemented from `b4d1df8846d29afac7133774d00e39362b66deff` on
`diagnostics41/bounds`, after issue #91, `docs/rfcs/diagnostics41-contracts.md`,
specification §21.4 and current frontend/producer/host paths were read. Owned
changes are only `src/tooling/frontend.rs`, `tests/diagnostic_bounds.rs` and this
report. Core model/checker files were copied temporarily for dependency testing,
then restored before the owned-file commit. Cargo/manifests/lock, checker,
renderer, server and legacy doc/lint adapters remain unchanged by this lane.

## Policy and behavior

No `AnalysisLimits` fields are added. The existing `max_diagnostics` remains the
aggregate primary diagnostic count. New public constant
`MAX_RELATED_LABELS_PER_DIAGNOSTIC` is **8**; the checked aggregate ceiling is
`max_diagnostics * 8` (default512, hard2048 under existing validated limits).
Secondary-count overflow or count exhaustion is an explicit Resource failure,
not truncation or a partially published diagnostic vector.

The existing `max_diagnostic_bytes` now measures aggregate logical occupied
metadata and UTF-8 payload bytes across the entire diagnostic slice:

- `size_of::<Diagnostic>()` per diagnostic, including primary span and
  String/Vec/Option controls;
- primary code and message byte lengths;
- `size_of::<DiagnosticLabel>()` per related label, including its span/control;
- every secondary message and optional help byte length.

Arithmetic uses checked multiplication/addition, and every charge observes the
same aggregate byte budget (default65536, hard262144). This counts logical occupied
record/payload bytes, excludes caller-owned spare Vec/String capacity, allocator
rounding and encoded JSON/terminal expansion, and is not an RSS guarantee. Exact
boundaries are computed from native Rust record sizes in tests, without assuming
a padding-free wire/native layout or a particular pointer size.

Every primary and secondary span must satisfy ordered start/end, end within the
exact borrowed source, and both UTF-8 character boundaries. EOF/zero-width spans
are supported. Invalid boundaries, overflows, counts and text/record budget
failures return `AnalysisFailure::Resource` with a fixed bounded reason. The guard
borrows diagnostics, never clones, formats, sorts, clamps or edits supplied
metadata. Actual producer label order, help, code, primary message and span are
preserved on success. It validates resource/position correctness, not causality.

The existing lex/parse/check publication points all call this same guard before
moving diagnostics into `AnalysisFailure`. Any malformed or excessive metadata
therefore returns explicit unavailable Resource instead of a partial/empty error
vector or positive `CheckedSource`. Successful source still returns its actual
private snapshot, including when its diagnostic byte budget is only one byte
and there are no diagnostics. Existing independent CLI doc/lint source profiles
and successful snapshot/source-hash behavior are unchanged.

## Tests and dependency verification

Core metadata dependency: `4ec73e3f030371758f99d7d6030afcd0efa6de25`
(`DiagnosticLabel`, additive related/help fields and builders). Temporary source
copies were used without committing another lane's files. Actual checker cause
metadata uses the final #88 producer commits
`626b27b6ce42954224aedce2d3f51d5b084de12f` and required followup
`9a2c6be63daad3b8b1c083ad281350f7573beb8f`, exact final production checker blob
`e02336f6d6d48a78486775094163b0a4e38bf831`. The three real rich integration tests
were rerun against this final producer after its witness-recovery correction;
the owned tests compare complete
actual raw checker diagnostics with the published frontend failures rather than
reconstructing guessed causes or relying on a fabricated diagnostic flag.

Five new private frontend guard tests cover exact aggregate record/text boundaries,
multibyte UTF-8 byte accounting, all primary/secondary malformed span forms
(reversed, oversized, usizeMAX and character interiors), valid EOF spans,
oversized related messages/help, nine-label refusal/eight-label acceptance,
per-slice aggregate text/count accounting, fixed metadata cost and checked scalar
accounting overflow without allocating gigantic payloads. Original supplied
metadata remains unchanged. These run alongside five existing frontend snapshot,
resource, inference-growth and actual latent-result tests.

Three integration tests in `tests/diagnostic_bounds.rs` use actual rich checker
failures after a Unicode comment: E0303 use-after-move and E0402 aborting checkpoint
with live linear ownership. They preserve every original label/help byte and
actual source cause span; verify exact aggregate budget acceptance and one-byte
underflow becoming Resource with no diagnostics; and verify corrected consuming
source still produces an actual checked snapshot even with a one-byte diagnostic
budget. Causal labels remain actual compiler evidence; no fixes or proof authority
are minted by this guard.

Private target `/tmp/vesper-tooling41-doc-target`, one build job, debug0 and
incremental disabled, baseline Cargo.lock unchanged:

```sh
cargo test -p vesper --locked --offline --lib tooling::frontend
cargo test -p vesper --locked --offline --test diagnostic_bounds
cargo clippy -p vesper --locked --offline --all-targets -- -D warnings
rustfmt --edition 2024 --check src/tooling/frontend.rs tests/diagnostic_bounds.rs
```

All pass: ten focused frontend unit tests and three real diagnostic integration
tests, strict all-target Clippy and formatting. Existing source acceptance,
full checker/IR metadata production, renderer formatting and LSP publication are
separate producer/host lanes; this bounded guard does not claim full §21 recovery,
per-function PCG, automatic fixes, proof lenses or latency guarantees.
