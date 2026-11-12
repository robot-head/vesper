# Expansion 37: Beacon

## Source and review coverage

Read `docs/rfcs/expansion-37-contracts.md`, `docs/specification-source.json`, and
complete design sections 8–11 and 21–23 from the refreshed section files. Snapshot
SHA-256: `04c1d5975c031a1dd9be11c3fbfb0c70e973d1c36fefa662ce775cd39dca61c3`;
upstream modified 2026-11-12 08:13:57.039 UTC.

- §§8–10 connect package ceilings, semantic interface variance, and modular
  workspace integration. Preserve existing arithmetic, modal/field premises,
  interface v2, and fiber ABI v2; no changes here. Document scaffolds are not
  evidence of actual sandbox confinement, resolver latency, supply-chain trust,
  or compiler integration.
- §11 requires generated structured Lean ASTs and axiom-hygiene checks. Its
  sample emitter/audit skeleton is not a complete verifier: hard-coded variable
  names/error types and permissive missing audit reports need independent
  treatment. No extractor or proof files changed in this lane.
- §21 introduces incomplete-source recovery, shared compiler/IDE queries and
  signature cutoffs. The ranking/map scaffold does not implement the whole
  parser/query pipeline or establish its latency budgets; no LSP edits here.
- §22's 24-byte context claim conflicts with its 128-bit trace ID, 64-bit span
  ID and flag byte (at least 25 serialized bytes, commonly 32-byte host layout).
  No pinned EvidenceVector propagation or register-copy claim is implemented.
  The §22.2 parser omits explicit version checking; this implementation enforces
  v00 and has an explicit reserved-flags policy.
- §22.5's scalar parser establishes no SIMD/branchless/~3 ns guarantee. Its
  histogram overlaps bins for 8..15 and 16..31, has eight rather than sixteen
  sub-buckets per octave, and clamps large values into bin 255. Two 256-u64
  arrays already occupy 4,096 bytes, before the total. Neither its all-u64
  accuracy nor <2% and 2 KiB promises follow. This library uses 496 bins,
  inclusive full-u64 ranges, a conservative <12.5% error bound, and 7,944 bytes.
- §22.4's example `rfl` theorem requires actual definitions and extraction;
  a Rust marker API is not a compiler noninterference proof. This lane offers
  content-independent explicit redaction and no raw-secret telemetry interface.
- §23 requires real nonce lifecycle, backend taint verification and hardware
  behavior. Its forward SSA sketch needs dominance/order and memory-taint
  treatment before it can prove backend constant time. Secret ownership alone
  cannot establish zeroization or hardware isolation. No Vault code changed.

## Delivered behavior

New standalone `crates/vesper-beacon` is dependency-free, `no_std`, and forbids
unsafe code. It implements strict exact-55-byte v00 W3C traceparent parse/format
with validated nonzero identifiers, lowercase hex, deterministic offset errors,
Display errors, and all flag bytes preserved opaquely (sampled interprets bit0).
Whitespace, future versions, and appended fields/extensions are rejected.

Fixed-capacity span pools mint monotonically increasing local IDs, skip a direct
parent-ID collision, and stop before wraparound. Explicit parent contexts and
caller clocks avoid TLS/evidence changes. Active spans are non-Clone owners;
finish_ok/finish_err consume once on success, return the live owner on queue-full
or end-before-start errors, and cancel explicitly without emitting completion.
Dropped/forgotten owners remain visible as occupied slots. Rust does not enforce
Vesper's total exhaustion, and ID uniqueness is scoped to one pool lifetime.

The bounded FIFO transfers completed records to independently owned fixed-width
column batches, with partial/empty/zero-capacity cases defined. Typed schemas,
error codes and attributes omit arbitrary strings. `Secret<T>` omits Debug and
telemetry conversions; explicit redaction gives a payload-independent marker.
There is an explicit declassification escape hatch, no zeroization guarantee,
and no asserted compiler noninterference theorem.

The histogram partitions every u64 into 496 nonoverlapping inclusive ranges:
exact 0..15, then eight equal-width bins per octave through u64::MAX. A lower
endpoint estimate has conservative error <12.5% for every nonzero value, exact
zero. It occupies 7,944 bytes including exemplars and total. Checked bucket and
total overflow returns an error without partial mutation. No timing, wait-free,
2 KiB, <2%, SIMD, Arrow compatibility or transport claim is made.

## Validation

All commands used `CARGO_TARGET_DIR=/tmp/vesper-beacon-target`,
`CARGO_BUILD_JOBS=1`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.

- `cargo test --manifest-path crates/vesper-beacon/Cargo.toml --all-targets`:
  10 tests passed. Header tests include real bytes, every truncation, invalid
  bytes at every hex position, separators/version/zero IDs, and all 256 flag
  bytes. Lifecycle tests cover explicit ancestry, error attribution, recoverable
  full queues, retry/cancel, abandoned owners, bounded slots, max-ID exhaustion,
  imported-parent collision, FIFO wraparound and independent owned drains.
  Histogram tests cover every bin's bounds/midpoint, contiguous full-u64
  coverage, low-value regression, footprint and transactional counter overflow.
- `cargo test --manifest-path crates/vesper-beacon/Cargo.toml --doc`:
  3 compile-fail tests passed (raw-secret Debug, secret-as-attribute, finish twice).
- `cargo clippy --manifest-path crates/vesper-beacon/Cargo.toml --all-targets -- -D warnings`:
  passed.
- `cargo fmt --manifest-path crates/vesper-beacon/Cargo.toml -- --check`: passed.
- `cargo run --manifest-path crates/vesper-beacon/Cargo.toml --example bounded_trace`:
  passed; output `completed 2 spans; histogram count=1, bytes=7944`.

These are host-library results on the available Linux environment. No native
Pulse execution, compiler effects, hardware/register evidence, formal proof,
Arrow exporter, external service, benchmark, or cross-platform execution was
performed. Private target footprint was under 5 MiB.

## Coordinator integration proposal

Add `crates/vesper-beacon` to root workspace members/default-members; remove its
local `[workspace]` isolation block. No external dependencies are needed. Add a
root path dependency only for CLI integration. Stable CLI entry points are
`parse_traceparent_55(&[u8]) -> Result<W3cSpanContext, TraceError>`, context getters
`trace_id() -> u128`, `span_id() -> u64`, `trace_flags() -> u8`, `sampled() -> bool`,
and `to_traceparent() -> [u8; 55]`. Suggested root command: `vesper traceparent
HEADER [--json]`, with fixed-width lowercase IDs and exact canonical header.
Root manifests, lock, CI, CLI and all existing crates are coordinator-owned.
No leaf Cargo.lock is committed. Runnable crate example is `bounded_trace`.
