# Flux CDC ingestion and local executor checkpoints (#50)

Read [issue #50](https://github.com/robot-head/vesper/issues/50) and specification
§34.2. Implemented only `crates/vesper-flux/**` and this report on
`continuation37/flux-checkpoints`, based on `5e699f8`. No source simulator,
frontier implementation, other crate, Lean, root manifest/lock, CI or CLI changes.

## CDC wire and transaction boundary

`CdcHeader` explicitly encodes/decodes exactly 32 little-endian bytes:

| Offset | Width | Field |
| --- | --- | --- |
| 0 | 8 | `source_lsn: u64` |
| 8 | 8 | `event_ts_ms: i64` |
| 16 | 8 | `z_weight: i64` |
| 24 | 4 | `partition_id: u32` |
| 28 | 2 | `schema_id: u16` |
| 30 | 2 | `flags: u16` |

A configured record appends exactly `input_width * 8` signed little-endian row
bytes. There is no implicit host struct layout or payload length cast. One
`CdcPolicy` binds sorted unique partition IDs to fixed schema IDs and circuit
input positions; multiple partitions may feed one input. Rows use the existing
all-i64 circuit schemas. Binding occurs once before the first tick; raw `tick`
is disabled afterward, so source offsets and executor steps cannot diverge
through a second ingestion entry point.

The implemented offset policy is deliberately **consecutive integer record
numbers**, starting at each explicit `first_lsn`. It does not implement generic
CDC byte-offset arithmetic, compacted Kafka offsets, gaps, replay deduplication
or connector-specific cursors. Duplicate, missing, backwards or reordered LSNs
within a partition fail; records may interleave different partitions. A record
at `u64::MAX` fails because its next offset cannot fit. The getter reports the
**next required LSN**, not the last observed LSN. Each partition uses either
unit weights (+1/-1 only) or any nonzero signed i64 weight; zero is rejected.
All flag bits are currently reserved and must be zero. Event timestamps may be
negative or out of order: they are labels, never an inferred watermark.

`Executor::encode_cdc_record` validates binding, schema, flags, weight, width and
bounded output length; future LSNs can be prepared. `ingest_cdc` validates the
entire borrowed batch, builds checked exact consolidated input deltas, executes
the existing real differentiated DBSP tick, then commits the tentative source
offsets with its integrals. No fallible operation follows the tick's publication.
Every returned decode, framing, policy, arithmetic or operator error leaves all
live integrals, tick count and offsets unchanged; the input byte owner remains
with its caller. Empty batches fail. Net-zero consolidated batches still commit
all consumed offsets and a successful tick.

CDC hard ceilings are 1024 bindings, 65,536 records and 8 MiB per batch. Record
count is additionally bounded by `Limits::max_input_entries`. Decode and DBSP
evaluation each have their own existing `Limits::max_work` budget, and existing
column/row/product/output bounds remain in force. All weight accumulation stays
checked i128 with final i64 fit under `vesper.arith.checked/1`; no saturation or
wrapping weights were introduced.

## Local checkpoint contract

`Executor::checkpoint(CheckpointLimits)` returns one versioned in-memory image
containing every mutable execution state required to continue:

- Tick count and every original node's complete canonical integral, including
  all input integrals, shared/nested join integrals and output materialization.
- CDC configuration and all committed next-source offsets, in the same image.
- A normalized execution fingerprint and trailing 32-byte BLAKE3 checksum.

The image starts with `VFLUXCP1`, u16 version 1 and zero u16 flags; all integer
fields have explicit little-endian encoding. Counts and host indices use
checked fixed-width representations. The identity hashes the complete ordered
original and differentiated DAG, node depths/widths, input positions/schemas,
join keys, filter kinds/constants, map columns/constants, union edges, selected
output, all query limits, CDC bindings/weight/offset policy and batch limits,
plus explicit execution/arithmetic/row-format version domains. Process-local
builder origins, addresses and frontier IDs are excluded. Equivalent execution
here means an identically reconstructed ordered graph/configuration; this is
not graph-isomorphism or semantic query canonicalization.

The immutable compiled circuit is supplied by the application, not deserialized
from untrusted opcodes. Restore requires its matching fingerprint. A fresh CDC
executor must first bind its expected source policy; images cannot silently
replace that expectation or switch raw/CDC modes. Graph/schema/source-policy
identity is checked independently of the image's checksum.

`restore_checkpoint` checks image size before hashing, checksum, magic/version,
flags, full execution identity, source policy/counts, offset non-regression and
consumed-record/tick bounds, exact node counts, node widths, row/cell budgets,
nonzero weights and strictly increasing unique row keys. Trailing bytes fail.
It then independently recomputes each relational node from the supplied prior
inputs and rejects inconsistent internal integrals. Only after all checks does
it publish integrals, ticks and source offsets together. Every returned error
preserves the existing live executor.

Creation runs the same consistency validation before emitting an image. This is
a conservative boundary: a valid incremental state can be rejected if complete
batch validation exceeds row/product/work budgets. Consequently a successfully
emitted image is not an unconditional promise of restoration under arbitrarily
smaller checkpoint budgets. Checksum hashing is bounded by the image byte cap;
structural parsing/identity/relational validation is charged separately and
shares the smaller of checkpoint and query work budgets. These are structural
bounds, not wall-clock or exact allocator-footprint claims. Host allocation
failure is not converted into a recoverable library error.

Checkpoint hard caps are 64 MiB encoded bytes, 1,000,000 total rows, 4,000,000
cells, 65,536 original/differentiated nodes, 1024 columns, 1024 sources and
16,000,000 charged structural operations. Defaults are 16 MiB, 100,000 rows,
1,000,000 cells, 4096 nodes, 1024 sources and 4,000,000 work units.

No frontier holds, tickets, origins or capabilities are part of the image.
Restoring neither resurrects retired owners nor modifies independent live
frontier trackers. The BLAKE3 checksum detects corruption; it is not a MAC or
signature against a hostile producer who can replace both image and checksum.
These methods do no filesystem IO, durable transaction, Quarry/WAL commit,
barrier alignment, distributed Chandy-Lamport snapshot or end-to-end exactly-once
protocol. The source-offset boundary is local in-memory success only.

## Validation

Private target `/tmp/vesper-flux-checkpoint-target` uses one job, zero debug
information and disabled incremental caching; final target footprint was about
20 MiB. Validation used temporary leaf workspace/profile isolation solely to
avoid root lockfile or other agents' build mutation. That isolation and its leaf
lockfile are removed from the handoff.

Passed on this Linux host:

```sh
CARGO_TARGET_DIR=/tmp/vesper-flux-checkpoint-target CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 cargo test --manifest-path crates/vesper-flux/Cargo.toml --offline
CARGO_TARGET_DIR=/tmp/vesper-flux-checkpoint-target CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 cargo clippy --manifest-path crates/vesper-flux/Cargo.toml \
  --all-targets --offline -- -D warnings
cargo fmt --manifest-path crates/vesper-flux/Cargo.toml -- --check
CARGO_TARGET_DIR=/tmp/vesper-flux-checkpoint-target CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 cargo run --manifest-path crates/vesper-flux/Cargo.toml \
  --example checkpoint_resume --offline
```

There are **22 passing tests**: the 11 foundation tests plus 11 new integration
tests. New evidence includes exact header bytes/signed extrema; malformed row
and header framing; source/schema/partition/flags/weight/LSN policy; byte/record/
work budgets; early differentiated join overflow and late input-integral
overflow with unchanged offsets; every checkpoint truncation; corrupted/versioned
images; deliberately rechecksummed bad counts, widths, zero/duplicate rows,
inconsistent integrals and offsets; same-shape predicate/constant/edge/output/
limit identity changes; all checkpoint budgets; and external frontier ownership.

The main differential resume test checkpoints a real nested/shared join after
three ticks, reconstructs a fresh matching circuit, restores, then compares
every subsequent emitted delta, complete image and materialization to an
uninterrupted executor. Independently integrated deltas also equal the batch
query at every tick, including signed retractions on both inputs.

`checkpoint_resume` additionally launches a **fresh process**, transfers a
bounded 291-byte image over stdin, reconstructs the expected graph/policy there,
restores after two ticks and compares continued retraction deltas/final image
against independently uninterrupted execution. It also checks local resume.
Observed output:

```text
restored 291-byte local image after 2 ticks
fresh-process resume matches uninterrupted shared join/retractions
local resumed execution matches; both committed source offsets are 4
```

## Integration

Added only `blake3 = "1.8"` as a Flux dependency (already used by the root
workspace). Coordinator should update the root lockfile's Flux package dependency
list and run combined checks. No new workspace member is required. Stable example:
`cargo run -p vesper-flux --example checkpoint_resume`. Existing
`incremental_join`, raw executor APIs and Lean model remain available; raw ticks
remain unchanged for executors without a bound CDC policy. No proof, benchmark,
Windows/macOS execution, durability or distributed correctness claim was added.
