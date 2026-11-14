# Beacon: tonic gRPC transport and OpenTelemetry Collector interop (wave seven, lane 1)

Claim #324. Branch `claude/project-thread-ey6nt5`, based on main at
`283c8115d2203232d0e9b473cc4bb0312c3a8509`. Paths: `crates/vesper-beacon/**`,
`crates/vesper-beacon-arrow/**`, this report, and a root patch (Cargo.lock
only) in its own commit.

## What was built

**OTAP columns (`otap::encode`, `otap::decode`, `otap::schema`, `otap::model`).**

* Grouped input: `ResourceGroup<T>{resource, scopes: Vec<ScopeGroup<T>>}`.
  `Resource` carries attributes, schema URL and dropped count. `Scope` carries
  name, version, attributes, dropped count and schema URL. Spans, histogram
  points and logs are all grouped this way. The flat `encode_spans` /
  `encode_histograms` / `encode_logs` entry points wrap one default
  resource and scope.
* Traces:
  * Resource and scope structs, with `RESOURCE_ATTRS` and `SCOPE_ATTRS`.
  * `kind`, and `trace_state` (validated against the W3C grammar by
    `TraceState::parse`).
  * Dropped attribute, event and link counts.
  * `SPAN_EVENTS` and `SPAN_EVENT_ATTRS`.
  * `SPAN_LINKS` (trace id, span id, trace state, flags) and
    `SPAN_LINK_ATTRS`.
* Metrics:
  * `FastLogHistogram` now tracks `sum` (u128), `min` and `max`. Its size
    grows from 7,944 to 7,984 bytes; the `size_of` test was updated.
  * Histogram points export `sum`, `min` and `max`.
  * Per-point attributes go in `HISTOGRAM_DP_ATTRS`.
  * Consecutive points with the same name and unit share one metric row.
* Logs: `LOGS` and `LOG_ATTRS`, carrying:
  * time and observed time;
  * trace and span id, and flags;
  * severity number and text;
  * event name;
  * a body as the upstream `AnyValue` struct (str, int, double and bool
    columns);
  * the dropped count.
* All attribute sets use the same `Attributes` type and the same PII
  boundary. This covers resource, scope, span, event, link, data-point and
  log attributes, and also log bodies (`LogRecord::set_body_pii`).
* **Transport id encodings.** The upstream Go decoder (`otel-arrow`
  `go/pkg/otel/...`, read from the module source fetched through
  proxy.golang.org) **ignores the `encoding` field metadata** and always
  applies transport decoding. Wave six's absolute ids marked
  `encoding=plain` would therefore be misread by a real collector. The
  default is now `IdEncoding::Transport`, which emits:
  * **Delta ids** for the main record `id` columns, `resource.id`,
    `scope.id`, and the event, link and data-point `id` columns.
  * **Row-wise delta `parent_id`** for data points.
  * **Quasi-delta `parent_id`** for:
    * attributes: delta while the key and value equal the previous row's,
      with Go `==` semantics, so NaN never repeats;
    * events: delta while the name repeats;
    * links: delta while the trace id repeats.

  Resources and scopes start where the accumulated id changes, as in Go.
  `IdEncoding::Plain` is still available and still decodes.
* **Dictionaries.** The new string columns use append-only delta
  dictionaries:
  * resource and scope schema URLs;
  * scope name and version;
  * trace state;
  * event name;
  * log severity text, event name and body.

  The decoder's own IPC validator now also accepts dictionaries over binary,
  int, float, fixed-size binary, timestamp and duration values. Upstream Go
  sends these, so the decoder must read them.
* **Splitting.** The `encode_split` methods bisect a group list until every
  batch fits three limits:
  * the row limit;
  * the 16-bit attribute `parent_id` space (`AttributeRowLimit`);
  * related-payload row limits and dictionary key widths.

  Capacity errors are checked in a pure planning phase before any state
  changes. They no longer reset the streams. Other encode errors still reset.
  If a split fails after writing earlier pieces, those pieces are discarded
  and the streams reset, so the next batch starts with a fresh schema.
* **Decoder.** It builds `DecodedResource`, `DecodedScope`, `DecodedSpan`
  (with events and links), `DecodedHistogram` and `DecodedLog`. New typed
  errors:
  * `MixedSignals`;
  * `BadIdEncoding`;
  * `IdOverflow`;
  * `DanglingParent` (names the column);
  * the existing missing-main-payload error.

  Copying an attribute group again when the same resource or scope id is
  placed twice (possible with plain ids) is charged against
  `max_decoded_string_bytes` before the copy. Without that charge, a small
  batch could multiply memory on the server, which decodes before admission
  control.

**OTLP (`otlp`).** `traces_request`, `metrics_request` and `logs_request`
build `ExportTraceServiceRequest` / `ExportMetricsServiceRequest` /
`ExportLogsServiceRequest` from the same grouped data and exported attributes.
`otlp::proto` is hand-written `prost` with field numbers checked against
opentelemetry-proto: common, resource, trace, histogram metrics and logs.

**gRPC (`grpc`, feature `grpc`, default on).** The transport is tonic 0.14.9
over plaintext HTTP/2. The service stubs are written by hand on tonic's
codegen API, so the build needs no `protoc` and no build script.

* **OTAP services.** They use the upstream names:
  * `/opentelemetry.proto.experimental.arrow.v1.ArrowTracesService/ArrowTraces`
  * `.../ArrowMetricsService/ArrowMetrics`
  * `.../ArrowLogsService/ArrowLogs`

  Each is bidirectional streaming, `BatchArrowRecords` → `BatchStatus`.
* **OTAP client.**
  * `OtapStream` pipelines batches with at most `max_in_flight`
    unacknowledged. `send` waits for acknowledgements while the window is
    full; this is the client side of back-pressure. It rejects duplicate
    and unexpected batch ids, and applies a status timeout.
  * `GrpcTransport` is a blocking `BatchTransport`, so the existing
    `TraceExporter` and `MetricsExporter` work unchanged. It reopens the
    stream after any failure or non-OK status; the exporter resets its
    encoder on the same events, so schema ids start fresh on both sides.
* **OTAP server.** `OtapService` and `serve_otap` give each stream its own
  `OtapDecoder`, decode, then admit into a bounded queue:
  * `INVALID_ARGUMENT` for a rejected batch; the stream stays usable.
  * `RESOURCE_EXHAUSTED` when the queue stays full for `admission_timeout`.
    This is the server side of back-pressure, and `is_retryable()` is true
    for it.
  * `UNAVAILABLE` when the consumer is gone.

  Batches are decoded before admission, so a shed batch leaves the stream's
  dictionaries in step.
* **OTLP.**
  * `OtlpClient` and `BlockingOtlpClient` call `TraceService/Export`,
    `MetricsService/Export` and `LogsService/Export`.
  * `OtlpService` and `serve_otlp` receive them, with the same admission
    control; a full queue answers with gRPC status `RESOURCE_EXHAUSTED`.
* **Message size.** Both directions default to a 16 MiB message limit.
* **Unsupported.** No TLS, compression, authentication or `headers`
  (HPACK) support.

## Tests

* `tests/otap_columns.rs` (10 tests):
  * a full trace round trip in both id encodings;
  * exact transport delta and quasi-delta values;
  * decoding upstream-style sorted quasi-delta input and kind/duration
    dictionaries;
  * dictionary deltas and reset for the new columns;
  * histogram sum, min, max, data-point attributes and shared metric rows;
  * logs with bodies;
  * an automatic split at 80,000 attribute rows;
  * PII across resource, scope, event, link, data-point and log attributes
    and log bodies (raw values absent from every encoded byte);
  * the trace state grammar;
  * malformed related payloads: dangling parents, missing main payload,
    mixed signals, id overflow and bad encoding metadata all give typed
    errors.
* `tests/otap.rs` (21 tests): updated for the intended format changes:
  * resource and scope dictionaries in the message sequence;
  * capacity errors no longer reset;
  * the expanded upstream schema transcription (8 schemas).
* `tests/grpc.rs` (8 tests, real HTTP/2 on 127.0.0.1, runs in CI):
  * OTAP traces (3 batches on one stream, `max_in_flight` 2), metrics and
    logs;
  * `INVALID_ARGUMENT`, then a good batch on the same stream;
  * a one-slot queue: the window wait, `RESOURCE_EXHAUSTED`, then recovery
    on the same stream;
  * the blocking `GrpcTransport` under `TraceExporter`, including
    `export_ring`;
  * an unreachable peer and an invalid endpoint;
  * OTLP round trips for all three signals;
  * OTLP load shedding;
  * the blocking OTLP client.
* `tests/collector.rs` (2 tests, gated on `VESPER_OTELCOL`): generates a
  collector config and checks two paths:
  * Beacon OTAP and OTLP into the collector's `otelarrow` and `otlp`
    receivers, read back through the `file` exporter's OTLP-JSON.
  * OTLP into the collector, then the collector's `otelarrow` exporter (the
    Go encoder), then Beacon's `serve_otap`.

  Without the variable both tests print a note and pass.

## Collector interop

Peer: OpenTelemetry Collector Contrib **v0.165.0**
(`otel/opentelemetry-collector-contrib:0.165.0-amd64` from Docker Hub; the
binary layer's sha256 was verified against the manifest). It bundles the
`otelarrow` receiver and exporter.

**Status: not yet run.** The binary was extracted on the cloud host, but the
session's safety classifier blocked executing a downloaded binary without
the owner's explicit approval, and that approval has not been given yet. The
run command is:

```sh
VESPER_OTELCOL=/path/to/otelcol-contrib cargo test -p vesper-beacon-arrow --test collector -- --nocapture
```

Until it runs, Beacon's compatibility with the Go implementation rests on
three things:

* reading the upstream Go encoder and decoder source;
* the transport-encoding tests above;
* decoding upstream-style quasi-delta input.

None of these is a run against the real peer. Go's handling of Arrow *delta*
dictionary batches in particular is unverified until the collector runs.

## Checks

Toolchains: stable 1.97 for builds and tests; 1.100.0 for clippy, matching
the Bazel CI.

* `cargo fmt --all -- --check`: see the PR.
* `cargo test -p vesper-beacon -p vesper-beacon-arrow --locked`: all pass.
  * vesper-beacon: 2 unit tests and 10 behavior tests.
  * vesper-beacon-arrow: 3 unit tests, plus collector 2 (skipped), grpc 8,
    interoperability 5, lean_model 2, otap 21, otap_columns 10, and 8
    doctests.
* `cargo +1.100.0 clippy -p vesper-beacon -p vesper-beacon-arrow --locked --all-targets -- -D warnings`
  and `cargo test --workspace --locked`: results recorded in the PR
  description.
* `python3 tools/bazel/sync_build_files.py --check`: clean (no new crate).

## Root patch

`Cargo.lock` only (+290 lines). It adds tonic 0.14.9, tonic-prost 0.14.9,
tokio 1.54.1, arrow-select 56.2.0 and their transitive dependencies (hyper
1.x, h2 0.4, tower, ...). Root `Cargo.toml` is unchanged, because the pins
live in the crate manifest.

## Decisions

* **Transport id encoding by default**, because the Go decoder does not read
  the `encoding` metadata. `Plain` is kept for readers that want absolute ids.
* **`grpc` is a default feature**, so the loopback test runs in CI without
  feature flags; this follows the vesper-forge precedent. Without the
  feature the crate still encodes, decodes and frames OTAP and OTLP.
* **Hand-written tonic stubs and prost messages** instead of `tonic-build`:
  no `protoc` and no build script, which matches how wave six wrote the
  OTAP messages.
* **Server-side shedding by admission timeout**, answered per batch. The
  collector's `otelarrow` receiver behaves the same way under admission
  control.

## Remaining gaps

* The collector run (above) is waiting on the owner's approval.
* OTLP metrics cover histograms only. There are no gauges, sums or
  exponential histograms, because Beacon produces none.
* There are no exemplars.
* There is no TLS, compression, auth or HPACK `headers`.
* Attribute values are scalar and string only. Upstream map, slice and
  bytes values are typed as such in the schema but not produced, and are
  rejected on decode.
* The Lean model (`lean/BeaconRedaction.lean`) was not extended. Its
  non-interference proof covers the earlier attribute boundary model, not
  the new attribute sets, and is not a theorem about the Rust code.
