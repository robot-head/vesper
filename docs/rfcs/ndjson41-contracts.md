# Validated NDJSON batches and transactional EventEnvelope columns

**Coordinator review draft: no runtime writers start until root publishes the
reviewed contract.** Public claims210–213 precede writers. Base is float PR204
headf60f11e. Fresh external issue/PR filename checks found no serde overlap;
Claude121–136/main/native,193BUILD and other pending source paths remain reserved.

| Claim | Branch | Exclusive writes |
| --- | --- | --- |
| #210 | ndjson41/integration | this contract, integration report, narrow lib.rs module registrations after publication |
| #211 | ndjson41/batch | new src/ndjson.rs, tests/ndjson.rs, docs/parallel-results/ndjson41-batch.md |
| #212 | ndjson41/columns | new src/event_columns.rs, tests/event_columns.rs, docs/parallel-results/ndjson41-columns.md |
| #213 | ndjson41/acceptance | new tests/ndjson_roundtrip.rs, examples/ndjson_columns.rs, scripts/check-ndjson-columns.py, NDJSON.md profile, acceptance report |

Source/test/example/profile paths above are relative to crates/vesper-serde;
script/report paths are repository-relative. Do not edit existing index, encode,
helper, float, README, allocator, core, CLI, Cargo, BUILD, workflow or wrapper
files. Only coordinator registers `pub mod ndjson; pub mod event_columns;` in
lib.rs. No new dependency. Separate private caches, one Cargo job/debug0/
incremental0; root target is coordinator-only. Claims stay reserved through merge.

## Public batch API and source ownership

The new `vesper_serde::ndjson` module exposes:

```rust
pub struct NdjsonBatch<'a>; // private immutable input/record/index storage
pub struct RecordRef<'d, 'a>; // Copy, immutable reference into this batch
pub struct NdjsonCursor<'d, 'a>; // private batch reference and next ordinal

impl<'a> NdjsonBatch<'a> {
    pub fn parse(input: &'a [u8], limits: BatchLimits) -> Result<Self, Error>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, ordinal: usize) -> Option<RecordRef<'_, 'a>>;
    pub fn cursor(&self) -> NdjsonCursor<'_, 'a>;
    pub fn stats(&self) -> BatchStats;
}
impl<'d, 'a> RecordRef<'d, 'a> {
    pub fn root(self) -> crate::Node<'d, 'a>;
    pub fn ordinal(self) -> usize; // zero-based record ordinal
    pub fn line(self) -> usize; // one-based physical line
    pub fn payload_span(self) -> core::ops::Range<usize>;
    pub fn json_error(self, error: crate::Error) -> Error;
}
impl<'d, 'a> NdjsonCursor<'d, 'a> {
    pub fn position(&self) -> usize; // next record ordinal, not a token index
    pub fn remaining(&self) -> usize;
    pub fn peek(&self) -> Option<RecordRef<'d, 'a>>;
    pub fn read_raw(&mut self) -> Result<RecordRef<'d, 'a>, Error>;
}
pub struct BatchStats {
    pub records: usize,
    pub tokens: usize,
    pub index_bytes: usize,
    pub metadata_bytes: usize,
    pub work: usize,
}
```

RecordRef and cursor views borrow the immutable batch; input strings borrow the
original input. `peek` MUST return the lifetime tied to Batch, not the temporary
`&self`, so a successful typed read can advance cursor while its values continue
borrowing Batch. Batch has no public mutation, append or approval constructor.
No self-referential allocation, borrowed pointer guessing or text-search span
recovery is needed. Payload spans are actual frame byte ranges including JSON
whitespace, excluding LF and a CR paired with that LF. Existing Node errors are
local to that payload; `json_error` relocates their offsets to the original batch.

## Framing and complete validation

Split ONLY at LF bytes. Strip the LF and at most one immediately preceding CR.
Any other CR remains ordinary JSON whitespace or an invalid string control,
according to the existing parser. Escaped `\\n` is string data. Multiline JSON
records are rejected; no stitching or incremental chunk parser is implied.

Empty input is a valid empty batch. A nonempty final record may omit LF. One
final LF/CRLF is allowed without inventing an extra empty record. Every actual
empty or JSON-whitespace-only frame is rejected, including leading, interior or
extra trailing blank lines. No blank-line skipping option in this slice.

Validate EVERY frame with the existing IndexedJson before returning Batch or any
view. Every record may be any JSON value; object/schema requirements belong to
the typed projection stage. Malformed unknown fields, invalid UTF8/escapes and
decoded duplicate keys anywhere prevent batch construction. On failure, release
all temporary indices and metadata; publish no partially validated batch.

## Typed errors and positions

Use these public new types inside `ndjson`, without changing crate-root errors:

```rust
pub enum EventField { Id, Topic, RawMeta }
pub enum ErrorKind {
    InvalidLimits, InputLimit, RecordLimit, TokenLimit, IndexLimit,
    MetadataLimit, WorkLimit, BlankLine, OutputTooSmall, Overflow,
    AllocationFailed, End, MissingField(EventField), Json(crate::ErrorKind),
}
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize, // absolute original input byte offset
    pub record: usize, // zero-based record ordinal; next ordinal for End
    pub line: usize, // one-based physical line
}
```

Derive Copy/Clone/Debug/Eq/PartialEq where possible; Error implements Display/Error
without allocating a message. JSON syntax/type/conversion errors retain the
existing exact local error and add payload_span.start. Missing-field errors
explicitly point to payload start because the field has no source token.
Batch-wide configuration/preflight failures point to offset0/record0/line1.
Per-record framing/quota errors point to that payload start. End points to input
length, next ordinal and the actual physical EOF line (LF count+1). Projection
configuration/count/capacity errors point to current record payload start, or EOF
when none remains. A caller-supplied JSON error outside its record range is
reported as Overflow at payload start; never wrap, clamp or fabricate a range.

All existing parser errors, including quota errors after clamping, may be wrapped
as Json(kind). Component-level quota errors discovered before a parser call use
the corresponding component variant. This avoids guessing which internal parser
capacity comparison caused a failure. Every error identifies its actual record.

## Batch limits and aggregate admission

```rust
pub struct BatchLimits {
    pub max_input_bytes: usize,
    pub max_records: usize,
    pub max_tokens: usize,       // sum of retained record token counts
    pub max_index_bytes: usize,  // sum of record index_capacity_bytes()
    pub max_metadata_bytes: usize, // requested record-vector payload
    pub max_work: usize,         // framing + metadata + ALL parser work
    pub record: crate::Limits,   // independent per-record parser limits
}
```

Defaults/HARD respectively:

| Field | Default | HARD |
| --- | ---: | ---: |
| max_input_bytes | 1MiB | 16MiB |
| max_records | 1024 | 16384 |
| max_tokens | 65536 | 1048576 |
| max_index_bytes | 4MiB | 64MiB |
| max_metadata_bytes | 256KiB | 4MiB |
| max_work | 32Mi | 512Mi |
| record.max_input_bytes | 64KiB | 1MiB |
| record.max_depth | 64 | 128 |
| record.max_tokens | 65536 | 1048576 |
| record.max_string_bytes | 64KiB | 1MiB |
| record.max_total_string_bytes | 64KiB | 1MiB |
| record.max_work | 16Mi | 512Mi |
| record.max_index_bytes | 4MiB | 64MiB |

Expose BatchLimits::HARD and validate(self)->Result<(),Error>. Every field must be
positive and <=HARD, even on empty input. Invalid settings are InvalidLimits.
Use checked arithmetic everywhere; arithmetic failures are Overflow. Independent
caps can refuse a batch before another advertised ceiling is reached.

Before scanning, admit input size and charge three units per input byte for all
framing/blank checks. Count real frames without allocating. Charge32units per
frame plus checked record_count*size_of(private record) before a SINGLE fallible
record-vector reservation. Enforce count and metadata caps before allocating.
The metadata metric is requested payload, not allocator bookkeeping or RSS.

Before EACH IndexedJson::parse, pass per-record Limits whose input, token, index
and work caps are the MINIMUM of record settings and remaining aggregate budgets
(input also bounded by actual frame and total input caps). Keep original depth/
string guards. A parser may not allocate a full fresh index allowance when only
a smaller aggregate remainder exists. On success subtract actual token_count,
index_capacity_bytes and work_used with checked arithmetic; reject any excess.
Frame payloads must fit record.max_input_bytes before parsing. Do not reset work,
tokens or index allowance per line or retry a refusal with a fresh budget.

BatchStats reports those actual retained sums and the framing/metadata work above.
Retaining one bounded index per record and one metadata vector is intentional:
this is a validated batch, not zero-heap or constant-memory streaming. No strings
or typed row vector need be allocated. Rust lifetimes keep all input/index views
valid until their owners may safely drop.

## Cross-lane private cursor seam

Writer211 supplies these crate-private helpers for writer212; no B edits to A:

```rust
impl<'d, 'a> NdjsonCursor<'d, 'a> {
    pub(crate) fn batch(&self) -> &'d NdjsonBatch<'a>;
    pub(crate) fn end_error(&self) -> Error;
    pub(crate) fn current_error(&self, kind: ErrorKind) -> Error;
    pub(crate) fn commit_to(&mut self, end: usize);
}
```

commit_to has an internal monotonic/in-bounds assertion, no returned Result.
Its callers establish end before writing; exclusive mutable cursor borrowing,
immutable Batch and the absence of callbacks keep that invariant throughout both
passes. It is not a public approval flag or a source/proof artifact constructor.
Other helper needs must be published as amendments before dependent edits.

## Fixed EventEnvelope schema and projection

The new `event_columns` module exposes:

```rust
pub struct EventEnvelope<'d, 'a> {
    pub id: u64,
    pub topic: crate::JsonString<'a>,
    pub raw_meta: crate::Node<'d, 'a>,
}
pub struct EventColumns<'out, 'd, 'a> {
    pub ids: &'out mut [u64],
    pub topics: &'out mut [Option<crate::JsonString<'a>>],
    pub raw_meta: &'out mut [Option<crate::Node<'d, 'a>>],
}
pub struct ProjectionLimits { pub max_rows: usize, pub max_work: usize }
impl<'d, 'a> NdjsonCursor<'d, 'a> {
    pub fn read_event(&mut self, limits: ProjectionLimits)
        -> Result<EventEnvelope<'d, 'a>, ndjson::Error>;
}
pub fn project_into<'out, 'd, 'a>(
    cursor: &mut NdjsonCursor<'d, 'a>,
    columns: EventColumns<'out, 'd, 'a>,
    limits: ProjectionLimits,
) -> Result<usize, ndjson::Error>;
```

The batch/input lifetimes match the cursor and column Node lifetimes.
EventEnvelope may be Copy/Clone/Debug. ProjectionLimits defaults{1024,32Mi},
HARD{16384,512Mi}, with
positive validate() and InvalidLimits on bad settings. Validate configuration
before examining records. max_rows bounds the number projected in one call.

Each record must be an object with required decoded keys id/topic/raw_meta.
Unknown fields are allowed after complete JSON validation. id uses existing exact
as_u64 semantics (1e0/1.0 are not integer IDs); topic uses string(); raw_meta may
be any validated JSON value. Check required presence in fixed order Id, Topic,
RawMeta before value conversion; missing fields report MissingField. Then convert
id, then validate topic type. Type/range errors use Json(existing kind) at the
actual value.
Escaped-equivalent keys match; decoded duplicates were already rejected globally.

Scan fields ONCE per schema pass. Before each key's comparisons charge
3*24*(raw_key_content_bytes+1), plus one unit per field and one unit per record.
This bounds the three fixed-key decoded comparisons even for escaped Unicode.
Charge id.raw().len()+1 before numeric conversion and one for topic string view.
No repeated Node.field calls with replenished independent budgets. No user
callbacks or mutable external source may participate in either pass.

read_event validates one row under the finite work cap, then advances exactly one
ordinal only after success. Any Err leaves cursor unchanged. Topics retain a
validated JsonString view: unescaped Unicode borrows input, escaped text decodes
later into explicit caller scratch via the existing API. Do not silently allocate
a String or claim escaped text was already decoded without scratch.

project_into handles ALL remaining cursor records. Before writing, validate
configuration, remaining count, all three output capacities and every row schema;
prepay the declared work for BOTH schema passes with checked arithmetic. It may
hold a single ephemeral row on the stack, but no allocated typed AoS staging.
The immutable second pass performs the same deterministic lookup/conversion under
the prepaid invariant, with no new fallible budget checks, callbacks or returned
Err after the first write. Do not call public read_event repeatedly while writing.
Fill only the first returned count of each initialized slice, preserve every
suffix element, then commit the cursor to its prevalidated end. Every returned
Err preserves ALL columns and cursor. Empty remaining input returns0 after
configuration validation. Mismatched slice lengths are allowed if each fits.
Remaining count above max_rows returns RecordLimit; insufficient column capacity
returns OutputTooSmall. Validate count before capacity and rows in source order.

## Actual validation and honest scope

Writer211 covers empty/final LF/CRLF/bareCR/blank policies, exact global Unicode
and later-record errors, full unknown validation/duplicates, aggregate tokens/
index/work/metadata boundaries, malformed final records, stable source views and
cursor scope. Derive boundaries from successful actual stats where appropriate.

Writer212 covers escaped-equivalent keys, all missing/type/range cases, u64::MAX,
borrowed and scratch-decoded Unicode, mixed raw metadata, late-row failures,
all-column/cursor/suffix preservation and actual two-pass work/capacity limits.
Use initialized sentinel slots; no unsafe or uninitialized outputs.

Writer213 exercises actual batch->columns->encoded records with existing codecs,
an executable self-checking example and independent Python parsing. Use a bounded
corpus/output/process lifetime and a real-binary override; no dependency or build
changes. Include later malformed input preventing every view, and later schema
errors preventing every column write. Existing JSON/binary/float tests remain.

No source derive/Region typing, linear resource construction, actual TiledSeq,
aligned AoSoA allocator, incremental chunks, SIMD/native backend, zero-total-heap,
latency/RSS or proof claim. This slice provides actual host validation and
caller-owned parallel columns; allocator integration is a separate prerequisite.

## Review questions before publication

1. Confirm the proposed numeric caps and fixed framing/schema work units; they
   are logical admission accounting, not timing promises.
2. Confirm public RecordRef::json_error is desirable for callers; it validates
   the supplied local offset and grants no checked/proof status.
3. Confirm common EventField/ErrorKind live in ndjson so A compiles independently
   of B, and root registers both runtime modules only when their files exist.
