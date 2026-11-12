# Actual IR diagnostic causes (#89)

Added optional related source evidence and invariant help to existing E0410
Place-IR ownership errors. The original primary codes, message strings, spans,
error conditions and deduplication order remain unchanged. E0411 state-token
checking is untouched. No IR lowering/schema/serialization, abstract-state
fields, ownership transfers, joins, queue or convergence conditions changed.
Only the IR checker, a new matching test file and this report belong to this
lane. Core diagnostic dependency: #87 `4ec73e3f030371758f99d7d6030afcd0efa6de25`.
The issue/contracts and specification §21.4 were read.

## Supported evidence

A complete per-function diagnostic-only index records unique instruction ValueId
definitions and unique parameter/Bind-pattern slot origins. Source-renamed local
slots remain distinct, so a shadowed binding cannot be mistaken for an earlier
same-spelled declaration. Parameter labels say **source span for this IR
parameter**: compiler-generated capture parameters can point at a capture context
rather than a physical parameter declaration. Binding labels refer to the actual
pattern span; temporary labels refer to the actual defining instruction span.

These origins support related information for live linear places/temporaries at
abort frames and returns, mutable temporary loans, shared/borrowed parameter
violations, moved places, overlapping loans, linear aggregate projection/
destructuring/wildcard errors, overwrites and inconsistent CFG ownership. Loan
conflicts identify the actual IR read that requests the conflicting access plus
the accessed slot origin where available. Recovery may retain a loan flag after
an invalid read request; the related caption therefore does not claim that a
valid borrow completed. A wildcard's temporary origin is the whole
input aggregate, not an invented individual field origin.

Repeated non-copy consumption can additionally identify an earlier owned consume
in the **same straight-line block visit**. The witness is recorded only when the
value was owned and not already consumed. Invalid borrowed move attempts do not
become completed-consumption witnesses. Non-sink argument loan release is not an
owned consumption witness. Witnesses reset before every visited block and are
never propagated into predecessor/join states. CFG join errors use static
binding/definition evidence, never a guessed last assignment/move on a branch.

Undefined values/places and convergence errors remain unadorned. Empty/reversed
or internal `0..0` origin spans are omitted. Duplicate value definitions or slot
origins are ambiguous and omitted, even if a later duplicate is unreachable.
Block/phi parameters have no unique instruction origin and are not assigned a
made-up location. Duplicate labels and labels identical to the primary range
are omitted. Missing evidence stays missing; no guessed source search occurs.
Help states the ownership/permission invariant and never invents a callable
abort/commit operation, rewrite, fix, proof or current-liveness certificate.

## Boundaries and bounds

Origins and per-block history live in Flow, entirely outside SlotState,
ValueState, State equality and the fixed-point inputs. A source audit verified
these state declarations/equality fields are byte-identical to the baseline,
and all original Flow error-message literals occur in the same order. This is
implementation evidence, not a formal proof of the compiler.

Static indexing allows 65,536 charged parameter/block/instruction/pattern work
units per function, 1 MiB aggregate slot-name copies and 1024 bytes per name.
Pattern traversal is iterative; child pushes are charged before growth. On any
index budget failure the entire index is discarded, so a prefix cannot falsely
claim uniqueness before an unseen duplicate. The original ownership validation
still runs and reports its original errors. The same-block witness map contains
only IDs with completed unique source indexing and is bounded by that index.
Each diagnostic publishes at most three distinct nonempty related labels and
one bounded static invariant help string. These are logical metadata/work
bounds, not allocator RSS or elapsed-time guarantees.

FunctionIr does not contain the original source buffer. The producer uses exact
IR parameter/instruction/pattern spans; it does not certify that arbitrarily
forged external IR metadata corresponds to source. The renderer/frontend must
still validate every primary/related range against the exact source and apply
their aggregate publication limits. This change does not make malformed IR a
new safe input format or add a structural IR validator.

## Validation

Private prior cache `/tmp/vesper-editor41-actions-target`, one build job, zero
dev/test debug information and incremental compilation disabled; root's cache
was not used:

- `cargo test -p vesper --test ir_diagnostic_causes --locked --offline`: 11 pass.
  Actual source/checker/generated IR tests cover Unicode abort-frame temporary
  definitions and nested closure/handler temporary causes. Valid source IR is
  then deliberately mutated for duplicate owned consumption, invalid borrowed
  repeated attempts, mutable loan overlap/reuse/abort, invalid shared-read recovery
  without a false completed-borrow caption, real parameter and shadowed binding
  origins, branch joins, loop revisits, undefined/internal/
  duplicate definitions and all-or-nothing provenance budget fallback.
- `cargo test -p vesper --lib --locked --offline`: 30 existing units pass.
- Existing `language`, `syntax_and_ir`, `effect_optimization` and
  `checked_arithmetic` suites: 134 pass (107 + 4 + 5 + 18). They preserve the
  existing PCG/control/effect/state-token/arithmetic acceptance and rejection
  behavior, including old E0410 negative sources.
- `cargo clippy -p vesper --lib --test ir_diagnostic_causes --locked --offline
  -- -D warnings`, owned-file rustfmt and `git diff --check` pass.

The tests preserve expected primary messages/ranges and check real source slices
for every emitted E0410 related label. The borrowed-consume regression explicitly
rejects a false prior-completed-consumption label. Temporary local lock resolution
for the core model dev-dependency was restored before the owned commit. No root
Cargo/lock, compiler core/checker, renderer, CLI/LSP or other lane file is included.
No new Lean theorem/axiom, automated substructural repair, recovery/completion,
per-function PCG or latency guarantee is claimed.
