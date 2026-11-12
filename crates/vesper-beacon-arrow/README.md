# Beacon Arrow IPC adapter

This allocating host adapter writes a complete standard Apache Arrow IPC **stream**
(schema message, one record batch including empty batches, EOS). Its custom,
versioned span schema is `vesper.beacon.spans/1`. It is not the OTAP schema or an
OTAP gRPC/HTTP2 implementation. The dependency-free `no_std` Beacon core stays
separate. No zero-allocation, SIMD, fiber/evidence propagation, DMA alignment,
canonical-byte uniqueness, or performance claim is made.

Direct production dependencies are pinned Apache `arrow-array`, `arrow-schema`,
and `arrow-ipc` **56.2.0**. `arrow-ipc` default features are disabled: no IPC
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
