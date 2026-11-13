# Beacon Arrow IPC and OTAP adapter

Two allocating host exporters live here. The `otap` module (below) speaks the
OpenTelemetry Arrow Protocol. The original top-level API writes a complete
standard Apache Arrow IPC **stream** (schema message, one record batch including
empty batches, EOS) with a custom, versioned span schema `vesper.beacon.spans/1`;
that API is unchanged and is not OTAP. The dependency-free `no_std` Beacon core stays
separate. No zero-allocation, SIMD, fiber/evidence propagation, DMA alignment,
canonical-byte uniqueness, or performance claim is made.

Direct production dependencies are pinned Apache `arrow-array`, `arrow-schema`,
and `arrow-ipc` **56.2.0** (`arrow-buffer` 56.2.1), plus `prost` 0.14.6 for the
OTAP protobuf messages and `hmac` 0.12.1 / `sha2` 0.10.9 for PII fingerprints. `arrow-ipc` default features are disabled: no IPC
compression is used. A different implementation, `arrow2` **0.18.0** with `io_ipc`,
is a development dependency for independent decoding and the self-checking
example. Root registration and the shared lock file are coordinator-owned.

`export_batch(&ColumnBatch<N>, limits)` borrows and preserves its source on
success or failure. `export_ring(&mut TelemetryRing<N>, max_rows, limits)` exports
up to the requested prefix and removes it **only after** complete serialization
and EOS succeed. `plan_batch` / `plan_ring` provide checked logical reservations
without allocation. A too-small budget leaves every record queued in its
original order for retry. `max_rows` in the export function selects the prefix;
`limits.max_rows` rejects an oversized selected prefix. No automatic splitting
or dropping occurs. Empty input emits a readable zero-row batch.

The core's `records()` iterator and `try_consume_prefix` callback borrow the
actual current ring. There is no separately committed snapshot or stale prefix
token. Rust's exclusive mutable borrow prevents ring mutation between selection
and successful removal; failed callbacks leave the queue unchanged.

All twelve fields are nonnullable:

| Column | Arrow type | Meaning |
| --- | --- | --- |
| trace_id_hi, trace_id_lo | UInt64 | High/low numeric halves of the full u128 ID |
| span_id, parent_span_id | UInt64 | Full IDs; parent 0 means no parent |
| start_unix_ns, duration_ns | UInt64 | Full unsigned nanoseconds; no i64 timestamp narrowing |
| trace_flags | UInt8 | All eight bits preserved, including opaque reserved bits |
| schema | UInt8 | 0=request, 1=storage_append, 2=background |
| status | UInt8 | 0=ok, 1=error |
| error_code | UInt8 | 0=none, 1=invalid_input, 2=storage, 3=timeout, 4=cancelled |
| attribute_kind | UInt8 | 0=none, 1=item_count, 2=byte_count, 3=redacted |
| attribute_value | UInt64 | Full count value; zero for none/redacted |

Schema metadata stores the version, code mappings, and constant `[REDACTED]`
marker. No secret payload or arbitrary string is accepted or formatted. Enum
mappings are explicit matches, independent of Rust discriminant layout. Arrow
metadata V5, 64-byte **relative buffer offsets**, and no dictionaries are used.
This does not promise that an output Vec's address is 64-byte aligned.

Budgets are checked before any adapter/Arrow value buffers are constructed:

- A hard ceiling of 65,536 selected rows applies even if callers request more.
- Attribute bytes count typed logical payloads: 8 for item/byte counts, 10 for a
  redacted marker, 0 for none. Fixed attribute columns are also counted below.
- For `n` rows and `b=ceil(n/8)`, values occupy `V=61*n` logical bytes. The pinned
  writer emits twelve all-valid bitmaps, and up to 63 padding bytes for each of
  24 value/validity buffers: conservatively `B=V+12*b+1512` body bytes.
- Intermediate reservation is `V+B+b+4*65536`: values plus encoded body, one
  temporary bitmap, and four fixed metadata/control allowances for this fixed
  primitive schema. No user-controlled metadata or dynamic-width data is used.
- Output reservation is `B+2*65536+24`, including fixed schema/batch metadata and
  framing. The budget must admit this conservative reservation, which can be
  substantially larger than actual output, especially for empty/small batches.
- The output Vec reserves once before writing. A bounded sink checks each write
  before extending, so it cannot grow beyond the reserved logical limit.

These are **logical data reservations**, not allocator capacity, total heap,
RSS, or OOM guarantees. Arrow can allocate internal descriptors/buffers; allocator
rounding, spare capacities, object headers and caller-owned ring/batch storage
are outside these logical byte counters. The fixed metadata allowances apply
only to this pinned writer and twelve-column schema; library/schema changes
must revisit them. All returned errors preserve input; process abort on OOM is
not recoverable. `try_reserve_exact` failures in adapter-owned vectors are
reported, while library-returned errors retain their Arrow category.

Run:

```sh
cargo run --manifest-path crates/vesper-beacon-arrow/Cargo.toml --example bounded_export
```

The example first rejects an insufficient budget, retries the retained cancelled
span, then uses arrow2 to verify every decoded field including u64 boundary
values. Tests independently compare all columns/code metadata for success,
every error code, cancellation, every typed attribute, empty/partial batches,
bitmap boundaries and the hard row ceiling. Descriptor tests check lengths,
null counts, 64-byte offsets and body bounds. This is interoperability evidence,
not a general untrusted Arrow validator or a Grammar (§38) implementation.

## OTAP (`vesper_beacon_arrow::otap`)

**Messages.** `otap::proto` carries `BatchArrowRecords`, `ArrowPayload`,
`BatchStatus` and both enums of upstream `arrow_service.proto`
(`opentelemetry.proto.experimental.arrow.v1`), written as `prost` derives with
the upstream field numbers, so no `protoc` runs at build time.

**Encoding.** `TraceEncoder::encode_spans(&[OtapSpan])` produces one
`BatchArrowRecords` with a `SPANS` payload and, when any span has attributes, a
`SPAN_ATTRS` payload. `MetricsEncoder::encode_histograms` produces
`UNIVARIATE_METRICS` + `HISTOGRAM_DATA_POINTS` from `FastLogHistogram`s
(explicit bounds = each Beacon bin's inclusive upper end as `f64`, exact below
2^53; exemplars are not exported). Column names and types are a subset of the
upstream Go schemas (`TracesSchema`, `StatusDT`, `AttrsSchema16`,
`MetricsSchema`, `HistogramDataPointSchema`); a test checks every emitted
column against a transcription of those tables. Ids are absolute, marked
`encoding=plain` in field metadata (upstream defaults to delta encodings).

* **Stream ids.** Each payload type has one long-lived IPC stream named by its
  `schema_id` (`spans:0`, `span_attrs:1`, ...). Its first payload carries the
  schema message; later ones carry only new messages.
* **Delta dictionaries.** String columns (`name`, `status.status_message`,
  attribute `key` and `str`, metric `name`/`unit`) are append-only
  dictionaries. Growth is sent as an IPC dictionary batch with `isDelta=true`
  holding only the new values; an unchanged dictionary sends nothing.
* **Schema reset.** A stream restarts under a new `schema_id` (schema and full
  dictionaries again) when a dictionary would exceed its key width (256 for
  `u8` keys, 65,536 for `u16`) or `EncoderConfig::max_dictionary_entries`, on
  `reset()`, and after any encoding error. A batch that alone overflows a
  dictionary is `EncodeError::DictionaryOverflow`.
* Unsigned Beacon values that do not fit OTLP's signed columns (times above
  `i64::MAX`, counts above `i64::MAX`) are `EncodeError::ValueOutOfRange`.

**Decoding.** `OtapDecoder` keys streams by `schema_id`, replaces a stream of
the same payload type when a new id arrives, and resolves attributes and
data points against their parents. It bounds payload count, stream count,
schema-id length, payload bytes, lifetime stream bytes and rows. Its own IPC
reader validates every message (framing, V5, no compression, a whitelist of
field types, exact node/buffer counts, aligned in-body buffers, bitmap
lengths) before calling `arrow-ipc`, because `arrow-ipc` 56 panics on several
inconsistent inputs; a `catch_unwind` guard remains as defense in depth. A
3,000,000-case mutation run reached it zero times. Any error drops all stream
state.

**Transport.** `BatchTransport::exchange` sends a batch and returns its
`BatchStatus`. `FramedTransport<S: Read + Write>` uses gRPC's length-prefixed
message framing (flag byte, big-endian u32 length, protobuf) over any byte
stream, and `OtapReceiver::serve` is the matching server. This is **not**
HTTP/2 gRPC and does not talk to an OTel collector directly. `TraceExporter`
and `MetricsExporter` reset their encoder after a failed send or a non-OK
status; `TraceExporter::export_ring` removes ring records only after an OK
status.

**PII redaction (§22.4).** `SpanAttributes::insert` accepts only
`vesper_beacon::Telemetrable` values: scalars, `&'static str` vocabulary and
`Redacted`. `Secret<T>`, `Pii<T>` and runtime strings are not `Telemetrable`
(compile-fail doctests). `insert_pii(key, value, &PiiPolicy)` consumes a
`Secret`/`Pii` and stores only the policy's output: `Redact` stores the
constant `[REDACTED]`; `Fingerprint(key)` stores the first 8 bytes of
HMAC-SHA256 as `[REDACTED:hmac-sha256:<16 hex>]`, a deliberate keyed
declassification. Tests search every encoded byte for the raw values and check
that redacted output is byte-identical for different PII inputs.
`lean/BeaconRedaction.lean` proves non-interference for a model of this
boundary and of the dictionary stream (see the report).

Source `Trace` effect propagation (§22.2) remains a compiler step; nothing here
reads the evidence vector or fiber context.

```sh
cargo run -p vesper-beacon-arrow --example otap_export -- /tmp/otap-streams
```

The example exports spans with a redacted e-mail over loopback TCP, checks the
receiver saw no `@`, encodes a histogram, and optionally writes each payload
stream to `<schema_id>.arrows` for another Arrow reader. PyArrow 26.0.1
(Arrow C++) read all four streams, including the delta dictionaries.
