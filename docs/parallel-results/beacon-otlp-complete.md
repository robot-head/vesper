# Beacon: complete OTLP and OTAP (wave nine, lane 26)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Claim #603. Branch `claude/project-thread-ve1h1c`, based on main at
`5a018bd`. Paths: `crates/vesper-beacon/**`, `crates/vesper-beacon-arrow/**`,
`lean/BeaconRedaction.lean`, this report, and a root patch (`Cargo.lock`
only) in its own commit.

## What was built

**Compression.**

* gRPC message compression. Clients compress with zstd by default
  (`GrpcCompression::{None, Zstd, Gzip}` on `ClientConfig::compression` and
  `OtlpClient::with_compression`). Servers accept zstd and gzip and can
  compress responses (`ServerConfig::response_compression`). Upstream's
  `zstdarrow1`..`zstdarrow10` encodings are zstd frames, so the server reads
  them as `zstd`. tonic bounds decompression by `max_message_bytes`.
* Arrow IPC buffer compression. `EncoderConfig::ipc_compression =
  IpcCompression::Zstd` writes zstd-compressed IPC bodies through
  `arrow-ipc`. On decode, Beacon's own IPC reader decompresses each buffer
  itself, charging `DecodeLimits::max_decompressed_bytes` (64 MiB by
  default), then rebuilds an uncompressed flatbuffer message for
  `arrow-ipc`. This avoids `arrow-ipc`'s unbounded decompression. LZ4 is a
  typed error.

**HPACK headers** (`otap::HeaderDecoder`, `HeaderPolicy::Decode`). The
`headers` field of each `BatchArrowRecords` is decoded in stream order with
one dynamic table per stream, through `loona-hpack` 0.4.3 (maintained fork
of `hpack`). Limits: the dynamic table size (`DEFAULT_HPACK_TABLE_BYTES` =
4096, and a peer's size update above it is an error) and the decoded size
(name + value + 32 per field, which bounds amplification through table
references). Names must be UTF-8. A `catch_unwind` guard turns any library
panic into `DecodeError::BadHeaders`. gRPC servers default to `Decode`;
fields land in `DecodedBatch::header_fields`. The plain decoder still
rejects headers by default, and `KeepRaw` is unchanged.

**Metric kinds.**

* `NumberPoint` (gauge, sum with temporality and monotonicity, int or
  double) goes into `NUMBER_DATA_POINTS` (+ attrs, exemplars, exemplar
  attrs).
* `vesper_beacon::ExponentialHistogram` is new in the `no_std` core: a
  fixed-array (160 buckets) base-2 exponential histogram over `u64`,
  starting at scale 20 and downscaling as the OpenTelemetry SDK does.
  Indexing is exact for scale <= 0 and for powers of two. Positive scales
  use a 63-bit fixed-point log2 (no floats). Every check happens before any
  mutation. `ExpHistogramPoint::from_histogram` exports it into
  `EXP_HISTOGRAM_DATA_POINTS` (+ attrs, exemplars, exemplar attrs), and
  encoding rejects out-of-range scales and inconsistent counts.
* Exemplars (`Exemplar`: time, int or double value, span id, trace id,
  filtered attributes) on all three point kinds.
  `HistogramPoint::with_bin_exemplars` exports each `FastLogHistogram` bin's
  span exemplar. The exemplar `parent_id` uses upstream's quasi-delta
  encoding, keyed on equal type and value.
* `MetricPoint` and `MetricsEncoder::encode_metrics`/`encode_points` take
  every kind at once. Consecutive points with the same `MetricKey` share a
  metric row. `otlp::metric_points_request` builds the matching OTLP
  request (`metric.data` oneof tags 5, 7, 9, 10).
* The decoder returns `numbers`, `histograms` (now with exemplars) and
  `exp_histograms`. It checks each point's kind against its metric row
  (`PointKindMismatch`). Summary (type 5) remains `UnsupportedMetricType`.

**Attribute values.** `AttributeValue` gained `Bytes(&'static [u8])`,
`Slice` and `Map`. `slice`/`map` check depth (8), node count (256), keys
(nonempty, at most `MAX_KEY_BYTES`, unique). Every leaf is a stored value,
so PII inside a map still goes through `PiiPolicy::apply`. Bytes use the
upstream `bytes` column; slices and maps use the `ser` column as CBOR, as
upstream does (`ciborium` 0.2.2). Decoding accepts upstream's
indefinite-length maps and arrays. It bounds recursion at 32, charges every
string and a fixed 16 per node to the decoded-bytes budget, and rejects tags,
undefined, simple values, non-text keys, integers outside `i64` and trailing
bytes. Log bodies take the same values. Attribute equality for delta
encoding follows Go's `carrow.Equal`: maps, slices and empty values never
compare equal.

**TLS and auth.** `ClientTls` (CA, optional server name, optional client
identity) and `ServerTls` (certificate, key, optional client CA for mutual
TLS) configure rustls 0.23 through tonic's `tls-ring` feature.
`BearerToken` accepts only an RFC 6750 `b64token` and has a hidden `Debug`.
A connection sends it on every call, and refuses to send it over plaintext
unless `allow_insecure_auth` is set (`InsecureAuth`). Scheme and TLS
settings must agree (`TlsMismatch`). `ServerConfig::bearer_tokens` makes
both the OTAP and OTLP services answer `UNAUTHENTICATED` without a matching
token. The comparison is constant time (`subtle`) and checks every
configured token.

**Lean.** `lean/BeaconRedaction.lean` adds a model of structured values:
mutual `Val`/`Vals`/`Fields` with labelled leaves (strings and byte strings),
public numeric leaves, slices and maps. `exportVal` models the serialized
token sequence. New theorems:

* `export_val_noninterference`: low-equivalent structured values (same
  shape, keys and labels, equal public leaves, equal fingerprints) export
  identically.
* `struct_attrs_noninterference`: the same, lifted to attribute lists.
* `exported_texts_public`: every text token of an exported value is a map
  key, a public leaf or the marker.

These are about the model only, as the file states.

## Public API added

* `vesper_beacon`: `ExponentialHistogram`, `exp_bucket_index`,
  `EXP_HISTOGRAM_BUCKETS`, `EXP_HISTOGRAM_MAX_SCALE`,
  `EXP_HISTOGRAM_MIN_SCALE`.
* `otap`:
  * Points and exemplars: `NumberValue`, `NumberKind`, `NumberPoint`,
    `Temporality`, `Exemplar`, `ExpBuckets`, `ExpHistogramPoint`,
    `MetricPoint`, `MetricKey`, `to_metric_groups`, `HistogramPoint::exemplars`,
    `HistogramPoint::with_bin_exemplars`.
  * Encoder: `MetricsEncoder::{encode_metrics, encode_points,
    encode_metrics_split}`, `IpcCompression`, `EncoderConfig::ipc_compression`.
  * Attribute values: `AttributeValue::{Bytes, Slice, Map, bytes, slice, map,
    depth, nodes}`, `MAX_VALUE_DEPTH`, `MAX_VALUE_NODES`,
    `AttributeError::{TooDeep, TooManyValues}`.
  * Decoded values: `DecodedValue::{Bytes, Slice, Map, Empty}`,
    `DecodedNumber`, `DecodedExpHistogram`, `DecodedExemplar`,
    `DecodedHistogram::exemplars`, `DecodedBatch::{numbers, exp_histograms,
    header_fields}`.
  * Headers and decode limits: `HeaderPolicy::Decode`, `HeaderDecoder`,
    `HeaderField`, `DEFAULT_HPACK_TABLE_BYTES`,
    `DecodeLimits::max_decompressed_bytes`.
  * Decode errors: `DecodeError::{BadSerializedValue, PointKindMismatch,
    DecompressedTooLarge, BadHeaders}`.
  * Schema: `number_points_schema`, `exp_histogram_points_schema`,
    `exemplars_schema` (+ `_with`) and the `METRIC_TYPE_*`/`TEMPORALITY_*`
    constants.
* `otlp`: `metric_points_request`, and the proto messages `Gauge`, `Sum`,
  `NumberDataPoint`, `ExponentialHistogram`, `ExponentialHistogramDataPoint`,
  `Buckets` and `Exemplar`.
* `grpc`:
  * Configuration types: `GrpcCompression`, `BearerToken`, `ClientTls`,
    `ServerTls`, `ConnectOptions`, `Connection`, `ClientConfig::compression`,
    `ServerConfig::{response_compression, bearer_tokens}`.
  * Entry points: `OtapStream::open`, `GrpcTransport::connect_with`,
    `OtlpClient::{connect_with, new, with_compression}`,
    `BlockingOtlpClient::connect_with`, `serve_otap_tls`, `serve_otlp_tls`.
  * Errors: `GrpcError::{InsecureAuth, InvalidToken, TlsMismatch, Tls}`.

Breaking changes inside the crate:

* `AttributeValue` is no longer `Copy`.
* `ServerConfig` is `Clone`, not `Copy`.
* `ServerConfig::default()` decodes headers.
* Clients compress with zstd by default.
* `HistogramPoint` has an `exemplars` field.

## Checks run

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo +1.100.0 clippy -p vesper-beacon -p vesper-beacon-arrow --all-features --locked --all-targets -- -D warnings` | clean |
| `cargo test -p vesper-beacon --test exponential` | 5 passed (exact index for v < 2^16 at scales 0-3, f64 bounds at every scale, powers of two, downscaling, 5,000 random records) |
| `cargo test -p vesper-beacon-arrow --test otlp_complete` | 9 passed (structured values, upstream CBOR, all metric kinds with exemplars, zstd IPC and its bound, HPACK order/table/random blocks, schemas) |
| `cargo test -p vesper-beacon-arrow --test grpc_secure` | 5 passed (every compression for OTLP and OTAP, TLS + bearer tokens, mutual TLS, insecure configurations) |
| `cargo test -p vesper-beacon-arrow --lib` | 4 passed (incl. `zstdarrowN` normalization) |
| `VESPER_LEAN=<lean 4.35.0> cargo test -p vesper-beacon-arrow --test lean_model` | 3 passed: model compiles, 10 theorems audited (only `propext`, `Quot.sound`), Rust/Lean differential, structured PII leaves absent from the wire |
| `VESPER_OTELCOL=<otelcol-contrib 0.165.0> cargo test -p vesper-beacon-arrow --test collector` | 3 passed (below) |
| `VESPER_LEAN=<lean 4.35.0> cargo test --workspace --locked` | not completed locally: the sandbox ran out of disk (24 GB target) while linking an unrelated crate's tests; CI runs the full workspace |
| `cargo test --locked -p vesper-beacon -p vesper-beacon-arrow --all-features` and `-p vesper --test trace_effects --test wasm_backend` (the root crate's Beacon users) | all passed (16 suites, 146 tests) |

**Collector interop**, OpenTelemetry Collector Contrib **0.165.0**, every
hop at default compression, run once in the cloud sandbox:

1. Beacon → collector. OTAP traces (two batches, one stream), metrics
   (gauge, sum, histogram with bin exemplars, exponential histogram, with
   zstd IPC buffers on top of zstd gRPC) and logs go into the `otelarrow`
   receiver; the same over OTLP/gRPC goes into the `otlp` receiver. The
   file exporter's OTLP-JSON shows every kind, the exemplars, `kvlistValue`,
   `arrayValue` and `bytesValue`, and Beacon's exponential scale. No PII
   appears.
2. Collector `otelarrow` exporter (Go encoder, default `zstd` compression)
   → Beacon's server: 2 spans, 1 histogram, 2 number points, 1 exponential
   histogram and 1 log decoded. That covers structured attributes from Go's
   CBOR, bytes, all exemplars, the exponential buckets and the offset.
3. Both directions again over TLS, with `bearertokenauth` on the
   collector's receivers and exporter and `bearer_tokens` on Beacon's
   server. A wrong token is refused with `UNAUTHENTICATED` and its data
   never reaches the output.

## Decisions

* zstd is the client default because upstream defaults to it, and because
  the lane asked that the collector run with its defaults. Clients can still
  pick gzip or none.
* IPC decompression is Beacon's own, with a budget, instead of
  `arrow-ipc`'s. A small compressed message could otherwise allocate
  without limit.
* `loona-hpack` was chosen over the unmaintained `hpack` 0.3. Its decoder is
  wrapped with size checks and `catch_unwind`, because it predates
  adversarial-input hardening.
* `ciborium` builds a `Value` tree, so a structural pre-scan of the CBOR
  item headers (no allocation) first charges every value, at least
  `size_of::<Value>()` each, and every string byte to the decoded budget.
  Decompressed IPC bytes also count toward `max_stream_bytes`, because
  dictionary deltas retain them (both from review of the PR).
* Bytes inside slices and maps are `&'static [u8]` like strings, so no
  runtime input is stored. Sensitive bytes go through `PiiPolicy`.
* TLS uses rustls with the `ring` provider (tonic `tls-ring`), and the
  bearer-token check happens before any decoding.

## Gaps

* The Go encoder writes a zero `min` of an exponential histogram as null
  (seen with 0.165.0), so `min = 0` does not survive the collector → Beacon
  hop. The collector test asserts what arrives.
* The collector did not send in-band `headers` in these runs, so the HPACK
  decoder is tested only against `loona-hpack`'s encoder and random blocks.
* Beacon's histograms are `u64`, so no negative exponential buckets are
  produced. They are decoded.
* Summary metrics and LZ4 IPC compression are typed errors, not supported.
* The Lean fixture differential still covers flat string attributes. The
  structured-value theorems have a run-time counterpart
  (`structured_pii_leaves_never_reach_the_wire`), not a generated
  differential.
* Collector interop runs only where `VESPER_OTELCOL` is set (not in CI), as
  before.

## Root patch

`Cargo.lock` only, in its own commit. New packages: `loona-hpack`,
`tokio-rustls` (tonic `tls-ring`), `zstd`, `zstd-safe` and `zstd-sys`; the
other new dependencies (`ciborium`, `flatbuffers`, `subtle`, `rustls`,
`flate2`, `rcgen` for tests) were already in the lock file, so only their
users' dependency lists changed.
