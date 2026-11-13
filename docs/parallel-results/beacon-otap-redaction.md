# Beacon: OTel Arrow transport and PII redaction proofs (wave six, lane 9)

Claim: #251. Base: `07322e3` (`origin/main`, descendant of the required
`cda092f`). Design: §22 of `docs/specification.txt` (Google Doc revision of
2026-11-13 04:46 UTC), mainly §22.3 and §22.4.

## What was delivered

| Item | Where | Status |
| --- | --- | --- |
| OTAP protobuf messages (`BatchArrowRecords`, `ArrowPayload`, `BatchStatus`, payload type and status enums) | `crates/vesper-beacon-arrow/src/otap/proto.rs` | Done, `prost` derives with upstream field numbers |
| OTAP column schemas for `SPANS`, `SPAN_ATTRS`, `UNIVARIATE_METRICS`, `HISTOGRAM_DATA_POINTS` | `src/otap/schema.rs` | Done, a subset of upstream columns, checked against a transcription of the upstream Go schemas |
| Span and histogram encoders with stream ids, schema reset and delta dictionaries | `src/otap/encode.rs` | Done |
| Bounded decoder with a validating IPC reader | `src/otap/decode.rs`, `src/otap/ipc.rs` | Done |
| Transport trait, gRPC-style framed byte-stream transport, receiver loop, exporters, loopback | `src/otap/transport.rs` | Done; not HTTP/2 gRPC (see gaps) |
| §22.4 PII boundary: `Pii<T>`, `Telemetrable`, `Sensitive` | `crates/vesper-beacon/src/redaction.rs` | Done |
| Redaction policies (`Redact`, keyed HMAC-SHA256 `Fingerprint`) and `SpanAttributes` | `src/otap/attributes.rs` | Done |
| Lean non-interference model | `lean/BeaconRedaction.lean`, `lean/BeaconRedaction/Fixture.lean` | Done, no `sorry`/`admit`/axioms, checked from `tests/lean_model.rs` |

### Public API (main items)

- `vesper_beacon::{Pii, Telemetrable, TelemetryValue, Sensitive}`.
- `vesper_beacon_arrow::otap::{TraceEncoder, MetricsEncoder, EncoderConfig,
  EncodeError, OtapSpan, HistogramPoint, span_name, error_message}`.
- `otap::{OtapDecoder, DecodeLimits, DecodeError, DecodedBatch, DecodedSpan,
  DecodedHistogram, DecodedValue}`.
- `otap::{SpanAttributes, AttributeValue, AttributeError, PiiPolicy,
  FingerprintKey, REDACTED_MARKER, FINGERPRINT_PREFIX}`.
- `otap::{BatchTransport, FramedTransport, OtapReceiver, ServeSummary,
  TraceExporter, MetricsExporter, LoopbackTransport, ExportFailure,
  TransportError, read_frame, write_frame}`.
- `otap::proto::*`, `otap::schema::*`.

The original `vesper.beacon.spans/1` Arrow IPC API is unchanged. Existing core
APIs are unchanged; the core gains only `Pii`, the two traits and private
helpers.

## Design decisions

**Streams.** Each payload type owns one `arrow-ipc` `StreamWriter` whose
output is drained after every batch, so each `ArrowPayload.record` holds just
the messages written for that batch. `schema_id` is `<kind>:<generation>`
with one generation counter per encoder, so ids never repeat within an
encoder. Dictionaries are append-only per stream and written with
`DictionaryHandling::Delta`, which emits an `isDelta` dictionary batch with
only the new values. The tests read the IPC message headers of each payload to
check this: first payload = schema + full dictionaries + batch; unchanged
dictionaries = batch only; growth = delta dictionary batches holding exactly
the new entries.

**Reset.** A stream restarts under a new `schema_id` when a dictionary would
exceed `min(key width, max_dictionary_entries)`, on `reset()`, and after any
encode error. Exporters also reset after a failed send or a non-OK status:
the receiver may not have applied that batch's deltas, so the next batch must
not depend on them. The test for this drops one batch and checks the retry
decodes on its own.

**Schema subset.** Beacon emits only the upstream columns it has data for.
`id`/`parent_id` carry `encoding=plain` metadata (absolute ids); upstream
defaults to delta and quasi-delta encodings, and Beacon's decoder rejects id
columns without the plain marker instead of guessing. The span name is a
fixed `vesper.*` string per `SpanSchema`; status uses OTLP codes (1 OK,
2 ERROR) with the error code name as `status_message`; the record's primary
attribute becomes a reserved `vesper.item_count` / `vesper.byte_count` /
`vesper.attribute` attribute. Times and counts above `i64::MAX` are typed
errors (OTLP columns are signed). Histogram buckets are Beacon bins up to the
last nonzero one, with bin upper ends as `f64` bounds (exact below 2^53).

**Untrusted input.** `arrow-ipc` 56 panics on several structurally valid but
inconsistent IPC messages (buffer descriptors outside the body, too few
buffers, misaligned or short offsets buffers, short validity bitmaps,
unsupported type parameters in `fb_to_schema`). Beacon therefore does not use
`StreamDecoder`; its own reader walks the messages and checks framing,
metadata V5, no compression, a field-type whitelist, nesting and field
limits, exact node and buffer counts for the schema, 8-byte aligned buffers
inside the body, element-width multiples and bitmap lengths, before calling
`read_dictionary`/`read_record_batch`. A `catch_unwind` guard
(`DecodeError::DecoderPanic`) stays as defense in depth. It also fixes a
`StreamDecoder` corner: a zero-length body at the very end of a payload (an
empty batch) stays pending there.

**PII boundary.** Only `Telemetrable` values enter `SpanAttributes::insert`;
`Secret`, `Pii` and non-static strings do not implement it (compile-fail
doctests). `insert_pii` consumes the sensitive owner and stores only the
policy's output. Fingerprints use `hmac` + `sha2` (already in the lockfile),
truncated to 64 bits; they are a deliberate declassification and are
guessable for low-entropy values by anyone holding the key. Neither wrapper
zeroizes memory, and `Box::leak` can still turn runtime data into a
`&'static str`.

**Dependencies.** New: `prost =0.14.6` (pulls `prost-derive 0.14.6`; the only
new lockfile entries) and direct use of `arrow-buffer =56.2.1`,
`hmac =0.12.1`, `sha2 =0.10.9`, all already locked. No `tonic`.

## Lean model

`lean/BeaconRedaction.lean` (model id `vesper.beacon.redaction-model/1`)
models attributes labelled public or PII-with-policy, the exporter
`exportAttr`, spans with abstract public fields, and the producer's string
dictionary stream (`encodeBatch` interns strings, emits new ones as a delta,
resets past `cap`). Proved, about the model only:

- `export_attr_noninterference`, `export_span_noninterference`,
  `export_batch_noninterference`: low-equivalent inputs (same keys, labels and
  public values; equal `fp` for fingerprinted values; redacted PII values
  unconstrained) export identically. `redact_only_low` states that without
  fingerprinting nothing about PII values is required.
- `stream_noninterference`: pointwise low-equivalent batch sequences give the
  identical message stream, including deltas, indices and resets.
- `emitted_strings_exported`, `pii_never_emitted`: every delta string is a
  key, public value or the marker of some attribute, so under `redact` a PII
  value that is not also one of those never appears in the stream.

These are not theorems about the Rust code: the model abstracts span fields to
a `Nat`, values to `String`, IPC framing to `Msg` and HMAC to an arbitrary
`fp`. `tests/lean_model.rs` checks hygiene (no `sorry`, `admit`, `axiom`,
`partial`, `unsafe`, extern hooks; every theorem audited), compiles the model
under `VESPER_LEAN`, accepts only `propext`/`Classical.choice`/`Quot.sound`,
and compares the Lean `exportAttr` (via `Fixture.lean`) with the Rust
exporter on 500 generated attribute sets after a real OTAP encode and decode
through one long-lived stream pair.

## Interoperation

- Upstream schema: a test transcribes the column tables of upstream
  `TracesSchema`, `StatusDT`, `AttrsSchema16`, `MetricsSchema` and
  `HistogramDataPointSchema` (otel-arrow `main`, fetched 2026-11-13) and
  checks every emitted column's name, type and order, and that every column
  upstream requires is present.
- Protobuf: field numbers and enum values follow upstream
  `arrow_service.proto` (same fetch).
- Arrow readers: PyArrow 26.0.1 (Arrow C++) read every stream written by the
  `otap_export` example, including delta dictionaries (manual check, not in
  CI). arrow2 0.18 (a test dev-dependency) reads a stream's first payload and
  reports `NotYetImplemented` for delta dictionaries; the test pins that.
- **Not** interoperated: a real OTel collector or the upstream Go/Rust OTAP
  decoders. There is no HTTP/2 gRPC client.

## Checks

Commands run on this branch (Linux x86_64, Rust 1.97, Lean 4.24.0 from the
GitHub release tarball):

```sh
cargo fmt --all -- --check                                            # clean
cargo clippy -p vesper-beacon -p vesper-beacon-arrow --all-targets --locked -- -D warnings  # clean
cargo test -p vesper-beacon -p vesper-beacon-arrow --locked           # all pass
VESPER_LEAN=.../lean cargo test -p vesper-beacon-arrow --test lean_model  # 2 pass
BEACON_FUZZ_CASES=3000000 cargo test --release -p vesper-beacon-arrow --test otap corrupted  # pass, 0 guard hits
cargo run -p vesper-beacon-arrow --example otap_export -- DIR         # self-check pass; PyArrow read DIR
cargo clippy --workspace --locked --all-targets -- -D warnings        # see PR
cargo test --workspace --locked                                       # see PR
```

`tests/otap.rs` covers span round trips with every attribute type, empty
batches, the delta/no-schema message layout, reset on dictionary growth and
single-batch overflow, out-of-range values and row limits, PII absence from
every encoded byte under both policies, byte-identical redacted output for
different PII, keyed fingerprints, attribute key validation, histogram round
trips, a TCP export that commits the ring only after acknowledgement plus a
rejected batch, transport failure with retry on fresh streams, frame errors,
malformed batches, 3,000 byte-mutation cases (the default; raise with
`BEACON_FUZZ_CASES`), protobuf garbage, arrow2 reading, and the upstream
schema comparison.

## Remaining gaps

- No HTTP/2 gRPC (`tonic`) client or server, so no direct collector
  interoperation; the framed transport carries the same messages in gRPC's
  per-message framing over any byte stream. A `tonic` feature is the next step.
- Not emitted: resource and scope columns and their attribute payloads, span
  events and links, `trace_state`, `kind`, dropped counts, histogram
  sum/min/max, exemplars, metric data-point attributes, logs. The decoder
  rejects those payload types with a typed error.
- Ids are plain; upstream delta/quasi-delta id encodings are not decoded.
- Allocating adapter: no zero-allocation, zero-copy, `IORING_OP_SEND_ZC`,
  AoSoA or DMA claim, and no performance measurement.
- Source `Trace` propagation (evidence vector, fiber inheritance) and the
  source checker's `Telemetrable` coherence rule are compiler steps, not part
  of this lane. Rust ownership and trait bounds here are library discipline,
  not the Vesper typing rules.
- `scripts/ci/check-lean-models.sh` (coordinator-owned) does not list the new
  model; the crate test compiles it whenever `VESPER_LEAN` is set.
