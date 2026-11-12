# Beacon bounded host foundation

Dependency-free `no_std`, safe Rust. This is an independently executable host
library for specification §22; it does not integrate compiler effects, Pulse,
Prism, fibers, the evidence vector, Forge schemas, or OTLP/Arrow transport.

`parse_traceparent_55` accepts exactly the v00 field value, with lowercase hex,
three hyphens, and nonzero trace/span IDs. It rejects whitespace, multiple header
values, appended extensions, future versions, and malformed/truncated input.
Errors include byte offsets. Reserved flag bits are accepted and preserved as
opaque values; only bit 0 is interpreted by `sampled()`. No `tracestate` or HTTP
header normalization is provided. `W3cSpanContext::new` validates IDs and
`to_traceparent` produces a canonical `[u8; 55]` without allocating. Neither
scalar parsing nor formatting has a timing or branchlessness guarantee.

`SpanPool<ACTIVE>` has a fixed set of live slots. Its non-Clone `ActiveSpan`
owners have explicit parents and caller-supplied Unix/monotonic timestamps.
Successful `finish_ok`/`finish_err` enqueue one record and consume the owner;
queue-full or backwards-clock errors return that same live owner for retry or
explicit `cancel(queue, end_mono_ns)`. Cancellation emits one
`Error(Cancelled)` record through the same queue and timestamp checks; full
queues and backwards timestamps return the live owner. There is no silent
cancellation path. A cancelled ID is never reused. ID exhaustion never wraps.
A direct imported-parent ID collision is skipped. IDs are only unique within a
pool's lifetime; callers must coordinate trace/ID namespaces between pools and
with remote producers. No entropy, globally unique ID generation, or nesting
validation is supplied.

Rust may drop or forget an owner: `unclosed_count()` continues to count that
occupied slot and further starts can fail with `PoolFull`. There is no
implicit successful completion or recovery/reuse of abandoned slots. Rust
`must_use` warnings and move checking are not Vesper linearity proofs. The pool
must outlive its owners and is not `Sync`; context propagation is explicit.

`TelemetryRing<QUEUE>` is a bounded FIFO with no overwrite-on-full policy.
`drain::<BATCH>()` transfers up to BATCH records into owned fixed-width columns;
empty/zero-sized drains leave the queue unchanged. Columns remain usable after
the ring is destroyed. Drain copies host records and is not a zero-copy Arrow
export. The fixed schema/error/attribute enums accept no arbitrary strings or
secret payloads. `Secret<T>` has no Debug or telemetry conversion. Explicit
`redact()` emits a constant marker independent of payload; `declassify()` is an
explicit escape hatch. This does not prove compiler noninterference, prevent
caller declassification, zeroize payloads, or fence hardware memory.

`FastLogHistogram` accepts every `u64` value (nanoseconds when used for latency):

- 0..=15 have exact singleton bins.
- Each octave 2^e..=2^(e+1)-1 for e=4..=63 has eight equal-width bins.
- Bin intervals are inclusive and available through `bin_range`; the last
  interval ends at `u64::MAX`. There is no clamping.
- The estimate is the lower endpoint. For nonzero v, `(v-estimate)/v < 1/8`
  (a conservative 12.5% bound); zero is exact. It does not promise <2%.
- 496 counters + 496 optional nonzero exemplar IDs + a total occupy 7,944 bytes,
  tested with `size_of`. No 2 KiB or L1 residency claim is made.
- Bucket and total increments are checked before any mutation; overflow leaves
  all counters/exemplars unchanged. A zero exemplar ID is unrepresentable.

Run `cargo run --manifest-path crates/vesper-beacon/Cargo.toml --example bounded_trace`.
The self-checking example parses actual header bytes, completes an error child,
recovers a parent from queue backpressure, attributes explicit cancellation,
drains owned columns, and records a
trace-linked latency exemplar. There are no performance claims or benchmarks.
