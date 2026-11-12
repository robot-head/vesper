# Guarded Salsa analysis and checked signatures — issue #82

Branch `editor41/database`, base `593bbc311a1c0693aa08732b40cec813e2cda436`.
Owned production change: `src/database.rs` only. Uses the separate #81
`tooling::frontend` API; its module/lib registration is not part of this commit.
Follows `docs/rfcs/editor41-contracts.md` and the supported query boundary of §21.

## Queries and immutable responses

The original public `parse_file`, `check_file`, and `diagnostics` queries retain
their signatures and historical behavior. They are legacy batch paths, not the
new editor firewall. New editor consumers must use:

```text
guarded_analysis(db, SourceFile) -> &AnalysisSnapshot
  source_identity() -> &SourceIdentity
    byte_len() -> usize
    blake3() -> Option<[u8; 32]>
  checked() -> Result<&CheckedSource, &AnalysisFailure>
  checked_arc() -> Result<Arc<CheckedSource>, &AnalysisFailure>

signature_snapshot(db, SourceFile) -> &SignatureSnapshot
  availability() -> SignatureAvailability
  digest() -> Option<[u8; 32]>
  normalized_bytes() -> Option<usize>
  fallback_source_identity() -> Option<&SourceIdentity>

signature_inventory(db, SourceFile) -> &SignatureInventory
  availability() -> SignatureAvailability
  digest() -> Option<[u8; 32]>
  declarations() -> &[String]
  fallback_source_identity() -> Option<&SourceIdentity>
```

`SignatureAvailability` is `Complete` or `Unavailable { reason: &'static str }`.
All snapshot/identity/inventory fields and construction are private. A caller
cannot supply a World, hash, or completeness boolean as checked evidence.
`AnalysisFailure` remains the frontend's structured `Parse`/`Semantic`
diagnostics or `Resource`/`Unsupported` reason. `CheckedSource` retains the
actual source and immutable parsed/checked/World values. An Arc accessor permits
retaining a snapshot without deep cloning.

`guarded_analysis` calls `check_source` with the default editor profile directly;
it never reaches `parse_file` or `check_file`. Guards run before the checker and
checked snapshot cloning. The query is memoized within an unchanged source
revision, with `no_eq` propagation after source edits. A range-bearing consumer
must use this current raw-source query and validate the line-index source as
required by the span/navigation lanes. A stable semantic digest never supplies
current positions or versions. Retaining an old Arc does not turn its ranges
into current ranges.

For bounded sources, identity records exact raw byte length and actual BLAKE3,
including every comment/body byte even after failure. Legacy SourceFile permits
arbitrary caller-owned Strings for compatibility. Inputs above the editor byte
limit are refused before hashing or source cloning: their hash is explicitly
absent and a fresh private Arc allocation supplies opaque execution identity.
Equal-length oversized revisions cannot compare equal. There is no integer
nonce or overflow/wrap concern. The coordinator's document store prevents these
oversized inputs in normal server operation.

## Semantic normalization and real equality cutoff

The supported schema is `vesper.editor.checked-signature/1`. Explicit typed DTOs
borrow known header fields from the actual checked World:

* Every function name, generic name/bound/row flag, parameter name/type/pass/
  projection selector, declared result, effect instances/labels/type arguments/
  row tail, and body-presence flag.
* All actual `world.function_results`, including inferred latent arrow types,
  modes and capability identities. Declared headers alone are insufficient.
* Every ADT name/mode/generic/enum flag, ordered variant and field name/type/
  modality/named-payload flag, including compiler prelude types.
* Every effect name/resumption flag/generic and its complete operation headers,
  including the prelude.
* `world.string_capacity`, which changes native string layout even when only a
  function's string literal body changes.

These fields determine the supported declared/effective type modes together
with the checked compiler's fixed type/mode rules. The DTO schema excludes only
actual function expressions and known location fields; it never strips JSON
keys recursively. Semantic field names such as `body` and `span` remain intact.
Ordered fields/selectors/labels are not erased or guessed equivalent.

Known source trait/model/conformance/use/bound syntax and any elaboration
metadata are outside complete header extraction. Residual unresolved actual
function results/parameter/effect/field/operation types, missing results, or
serialization limits also produce unavailable status. The fallback includes
the full actual bounded raw source identity, never a fake stable partial header.
No semantic digest is published for an unavailable result. No claim about
complete trait/Pact interface representation follows from this implementation.

Supported header JSON uses deterministic BTreeMap order and an output writer
that rejects before extending beyond 1 MiB. Metadata is borrowed rather than
deep-cloning declarations/bodies. Header declaration count is capped at 4,096;
residual-type checks are iterative with bounded pending/visited nodes. The
frontend additionally bounds source bytes, AST/types/depth, identifiers,
diagnostics, printed types and metadata before it constructs CheckedSource.
These are logical resource limits, not exact allocator/RSS or wall-clock bounds.

Salsa compares the full bounded normalized DTO bytes for equality; BLAKE3 is a
reported semantic digest rather than the sole equality check. Equal header
results stop propagation to the tracked `signature_inventory` consumer. Failed
extraction uses raw identity, and the inventory retains that identity too.
The inventory contains typed declaration-kind/name labels, not locations or
inferred cross-file bindings. Its public construction is also private.

Some edits to latent capabilities change identities derived by the current
checker from source locations. Those actual result changes conservatively
invalidate signatures. No unproved alpha-equivalence or label erasure is used
to force extra cache hits.

## Verification and integration boundary

Focused checks use a private `/tmp/tooling41-lint-target`, one Cargo job, dev/test
debug information off, incremental compilation off. The agreed #81 frontend
modules/lib registration from commit `14dfbb6a9e1296675aa3a703d57226818699a35f`
were copied temporarily for these checks and removed before this lane's commit. No Cargo/lock/server/CLI production changes belong to
this lane. Integration must include #81 before compiling the new queries.

```text
cargo test -p vesper --test database_signatures
  9 tests passed
cargo test -p vesper --lib database::tests
  1 existing compatibility/memoization test passed
cargo clippy -p vesper --lib --test database_signatures -- -D warnings
rustfmt --edition 2024 --check src/database.rs tests/database_signatures.rs
git diff --check
```

Tests observe actual Salsa `WillExecute` events: a simple body/comment edit
reruns guarded analysis and signature extraction, while signature inventory
does not execute again. The new source hash, retained snapshot source, and later
function spans are checked independently; old retained snapshots stay immutable.
Changing string capacity invalidates the consumer, while same-capacity string
body changes preserve supported semantic equality.

The inferred-result regression keeps identical outer header
`linear effect Ask{fn ask()->i64;} fn make()->fn()->i64 eff[Ask]` and changes only
the body from `{|| Ask::ask()}` to `{|| 0}`. Both pass actual checking; their
declared result types are equal, their inferred `function_results` differ, and
both semantic fingerprint and Salsa consumer invalidate. No declared-header
text or caller-supplied type set stands in for that evidence.

Additional cases cover effect labels/arguments/resumption, parameter pass modes,
projection selectors, ADT modes and fields literally named `body`/`span`, trait
fallback/raw identity changes, parse/semantic failure followed by valid recovery,
and oversized same-length revisions with no hash/equality reuse. `_` as a source
type remains an actual checker error, not an invented top-level inference API.
Hostile inferred tuple doubling, generic duplicating calls, binary nominal DAGs
and recursive nominal dependencies are refused by pre-check guards. They never
use the legacy checker query path.

This is a bounded query/cutoff firewall. It adds no incremental block parser,
per-function PCG, PoisonedRecovery, ownership/liveness completion, macro execution,
cross-file import graph, background proof execution, or sub-2ms timing guarantee.
Database/document count and accepted-revision reset limits remain the root
session owner's responsibility; old SourceFile IDs must not cross resets.
