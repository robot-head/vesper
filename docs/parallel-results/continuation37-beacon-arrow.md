# Continuation: bounded Beacon Arrow IPC (#47)

## Scope and source

Implemented claimed issue [#47](https://github.com/robot-head/vesper/issues/47) on
branch `continuation37/beacon-arrow`, starting at `5e699f8`. Read the issue and
full §22, especially §22.3. Also read the newly refreshed §38 in the coordinator's
`/workspace/vesper/docs/specification.txt`: its untrusted/canonical Grammar,
SIMD, DMA and proof paths are separate integrations, not owned by this change.
Only `crates/vesper-beacon/**`, new `crates/vesper-beacon-arrow/**`, and this report
are changed. Root manifests, lock, workflows, CLI, other crates and Warp are
untouched.

## Delivered behavior

The allocating adapter produces complete standards-compatible Apache Arrow IPC
streams: metadata V5 schema, one record batch including empty batches, then
explicit EOS. This is a versioned custom Beacon schema, `vesper.beacon.spans/1`,
not an OTAP schema/transport implementation. A genuinely different reader,
arrow2, decodes and compares every column in tests and the self-checking example.

Twelve nonnullable primitive columns preserve all source data:

- UInt64: trace_id_hi, trace_id_lo, span_id, parent_span_id, start_unix_ns,
  duration_ns, attribute_value.
- UInt8: trace_flags, schema, status, error_code, attribute_kind.

Both trace ID halves preserve the complete u128. Time/duration remain unsigned
nanoseconds to preserve the full u64 domain rather than narrowing to signed
Arrow timestamp storage. Explicit stable code mappings preserve all three
schemas, success/four error codes including cancellation, and none/item/byte/
redacted attributes. Metadata records the version, mappings and fixed redaction
marker. No secret payload or arbitrary input string is formatted or accepted.

The core remains no_std, safe Rust and dependency-free. It adds only a borrowed
FIFO `records()` iterator and `try_consume_prefix(max_rows, consumer)`. The
consumer sees the actual current ring under its exclusive mutable borrow;
there is no detached commit token that could later apply to a changed prefix.
Removal happens only after the callback returns Ok. Tests confirm partial
inspection followed by error leaves every original record queued and ordered.
A compile-fail doctest demonstrates that the callback cannot mutate the ring.

`export_ring` preflights the requested prefix and commits removal only after
Arrow writing/EOS succeeds. Returned preflight, allocation, or writer errors
leave records available for retry. An injected failure inside the actual Apache
IPC writer (after constructing arrays) exercises this recovery path.
`export_batch` only borrows its independent owned columns, preserving them on
both success and failure.

## Dependencies and exact limits

Direct production dependencies are pinned:

- `arrow-array = "=56.2.0"`
- `arrow-schema = "=56.2.0"`
- `arrow-ipc = { version = "=56.2.0", default-features = false }`
- path dependency on `vesper-beacon`.

Independent reader/example dependency:
`arrow2 = { version = "=0.18.0", default-features = false, features = ["io_ipc"] }`.
Local resolved supporting Arrow versions include arrow-buffer 56.2.1,
arrow-data/select 56.2.0, arrow-format 0.8.1 and flatbuffers 25.12.19.
The coordinator owns the shared lock; no adapter Cargo.lock is committed.

Preflight performs checked arithmetic before constructing Arrow arrays or output:

- Hard row ceiling 65,536; default configured limit 4,096.
- Typed logical attribute bytes: none=0, item/byte counts=8, redacted=10.
  Default attribute budget is 40,960 bytes.
- For n rows, b=ceil(n/8), values V=61*n. The pinned writer emits twelve validity
  bitmaps even for these all-valid columns. With 64-byte relative offsets and
  up to 63 padding bytes per each of 24 buffers, body bound B=V+12*b+1512.
- Intermediate logical reservation: V+B+b+4*65,536. This covers values, encoded
  body, one temporary bitmap and four fixed metadata/control allowances.
  Default intermediate budget is 2 MiB, excluding separately budgeted output.
- Output logical reservation: B+2*65,536+24, including conservative fixed schema/
  batch metadata and framing. Default output budget is 1 MiB. Limits must admit
  this reservation, which is deliberately larger than actual small-batch output.
- Adapter-owned vectors use fallible exact reserve. The output sink reserves
  once, checks every write before extension, and cannot exceed its reservation.

These are logical data reservations, not allocator-capacity, total heap/RSS or
OOM guarantees. Spare capacities, allocator rounding/object overhead and
caller-owned inputs are outside the counters. Arrow allocates internal buffers;
its process abort on OOM is not a recoverable returned error. Fixed metadata
allowances are specific to this pinned writer and schema and must be reviewed
if either changes. Tests check metadata/body lengths against these allowances.
Relative 64-byte buffer offsets do not imply a Vec address is hardware-aligned.
No generic untrusted Arrow verifier, canonical byte uniqueness, zero allocation,
SIMD, zero-copy network send, fiber/evidence propagation, OTAP gRPC/HTTP2,
performance or formal-proof claim is made.

## Validation

All builds use private `/tmp/vesper-beacon-arrow-target`, one build job,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.
Private target was approximately 295 MiB, well below the team disk allowance.

- Adapter `cargo test --manifest-path crates/vesper-beacon-arrow/Cargo.toml --all-targets`:
  eight tests pass. Independent arrow2 compares every column and metadata codes,
  all typed attributes/status/error/cancellation, u128/u64 boundary IDs, opaque
  flags, parent/timing, empty/zero-capacity/partial batches, independently owned
  source batches, FIFO wraparound, validity boundaries (0,1,7,8,9,63,64,65), and
  the 65,536-row hard boundary. Standard descriptors are checked for node lengths,
  null counts, bitmap/value lengths, 64-byte offsets and checked body containment.
  Error tests cover row/attribute/intermediate/output budgets, exact reservation
  boundaries, input preservation, actual writer failure and bounded sink writes.
- Core `cargo test --locked -p vesper-beacon`: 12 runtime tests pass, including
  the new failed-prefix/retry/FIFO case. Original span/histogram tests still pass.
- Core `cargo test --locked -p vesper-beacon --doc`: four compile-fail tests pass,
  including exclusive borrowing during prefix consumption.
- Strict all-target Clippy passes for both adapter and core (`-D warnings`).
- Adapter cargo fmt and focused core rustfmt checks pass; git diff whitespace
  check passes.
- `cargo run --manifest-path crates/vesper-beacon-arrow/Cargo.toml --example bounded_export`:
  passes. It rejects insufficient output budget, retries the retained cancelled
  span and independently verifies every field. Output:
  `independent Arrow reader verified 12 columns, 1 cancelled span, 3528 IPC bytes`.

Validation is local Linux host execution; no cross-platform runner, external
collector or hardware/runtime path was exercised.

## Coordinator registration proposal

Remove the adapter's temporary `[workspace]` isolation and add
`crates/vesper-beacon-arrow` to root members/default-members as desired. Regenerate
the coordinator-owned root lock with the pinned dependencies above. Run focused
core and adapter all-target tests/strict Clippy, and the `bounded_export` example.
No root CLI change is necessary for library/example acceptance. No existing
Arrow root dependency/schema is altered by this branch.
