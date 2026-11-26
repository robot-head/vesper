# Rust-exit wave SH6 lane 6: Quarry and Flux

Claim [#1401](https://github.com/robot-head/vesper/issues/1401). Prompt:
`docs/agent-prompts/self-host-sh6/06-quarry-flux.md`. This lane continues
#1069 (Flux, P3b lane 7) and the Quarry planner port (#904, #955).

The lane lands in parts, one reviewable PR each. This report is updated by
every part.

## Part 1: the Flux accumulator and CDC decoder

Base: `main` at `ab965f2` (2026-11-21).

### What changed

- **`zset::Accumulator`** (the `BTreeMap<Row, i128>` that CDC ingest, Kafka
  decoding and durable restore build their sets with) now runs in Vesper:
  `vesper/src/acc.ves`, exported as `zacc_new`, `zacc_add` and
  `zacc_finish` on the handle `ZAcc`. One accumulator serves every input of
  a circuit: distinct rows are kept once in insertion order with their i128
  sums as two words, found through one open-addressing table keyed by
  `(input, row)` under SipHash-1-3 with a random per-accumulator key (from
  Rust's `RandomState`, as `HashMap` does, so rows chosen to collide cannot
  make lookups linear; results do not depend on the key), and sorted once
  per input at `finish`. The Rust wrapper
  keeps the work charge and the width check, which come first in the Rust
  code. The Rust `BTreeMap` accumulator is deleted.
- **`Executor::ingest_cdc`'s record loop** (header decode, binding lookup,
  schema, flags, weight policy, LSN order, truncation, the record limit and
  every work charge) runs in Vesper: `vesper/src/cdc.ves`, exported as
  `cdc_ingest`. The Rust method keeps the empty-batch and byte-limit checks
  before the loop and the tick after it. The Rust loop is deleted.
- Callers (`cdc.rs`, `durable.rs`, `kafka.rs`, `kafka_group.rs`) use the
  multi-input accumulator; `Accumulator::new` takes every input's width.

### Flip-gate verdict (hand-run recipe; `scripts/ci/flip_gate.py` is not on main yet)

| Module | Differential | Every test | ABBA (Vesper/Rust, 95% interval) | Verdict |
|---|---|---|---|---|
| accumulator + CDC decoder (`ingest` workload) | 0 differences over 20,000 accumulator cases and 20,000 CDC batches | all 14 test binaries of the crate pass | **0.49x** (0.373x..0.668x); with the keyed hash 0.476x..0.570x | flipped |
| `resume` workload (CDC, checkpoint, restore) | digests agree over 104 batches | | 0.73x (0.693x..1.003x); with the keyed hash, three runs: 1.043x..1.344x, 0.835x..1.016x, 0.730x..0.823x (host noise) | no regression past the bar |

- **Differential**: `src/accumulator_differential.rs` keeps the deleted
  Rust accumulator and CDC loop as the reference (the Aura pattern) and runs
  both on seeded cases under tight random limits: rows at the `i64` edges,
  zero and extreme `i128` weights, rows of the wrong width, exhausted work
  budgets, LSN gaps and wraps, unknown partitions, wrong schemas, reserved
  flags, invalid weights and truncated batches. Each case compares the
  result or error, the sets, the advanced LSNs and the work left. Every
  error the two modules raise occurs (the test asserts each one's count).
- **Divergence checks**: two new divergent edits, `accumulator` (one row
  over `max_rows` fits) and `cdc` (reserved flag 1 is accepted), in
  `build.rs` and `BUILD.bazel`; `port/check.sh` asserts that the named
  differential test fails and another passes.
- **ABBA**: `examples/abba.rs` gains the `ingest` workload (8 pre-encoded
  CDC batches of 4,096 width-4 records over 1,024 keys, with repeats and
  cancellations, into a fresh executor whose circuit is its input). The
  Rust arm is `main` at `ab965f2` with the same example file. 12 epochs of
  A/B/B/A on a 4 vCPU cloud container, release builds.

Rust lines deleted in this part: the `BTreeMap` accumulator (~45 lines) and
the CDC record loop (~55 lines); the adapters added in `src/port.rs` are
~110 lines.

## Part 2: the Flux input log's frame codec

Base: `main` at `979cfa8` (part 1, #1419, merged).

### What changed

- `log.rs`'s header, frame encoder, frame checksum and frame decoder run in
  Vesper (`vesper/src/log.ves`, exported as `log_header`, `log_frame`,
  `log_frame_ok` and `log_decode`). The checksums are BLAKE3 through
  `@vesper-std/hash-sys` (the `.vsys` library `Blake3`, the BLAKE3 C
  implementation that the crate links through `vesper-blake3-sys`, as
  `vesper-fmt` does since #1462). `log.rs` keeps the file: create, open, scan, append, read and the
  torn-tail cut. The Rust codec is deleted.
- `src/accumulator_differential.rs` became `src/differential.rs` and gained
  the deleted Rust codec as the reference of a third differential test.
- New `tests/port_memory.rs`: repeated CDC ingest (accepted and rejected
  batches), ticks, checkpoint and restore, and log appends, reads and
  reopens leave no live port allocation (its own test binary, since the
  runtime's live count is process-wide).

### Flip-gate verdict

| Module | Differential | Every test | ABBA (Vesper/Rust, 95% interval) | Verdict |
|---|---|---|---|---|
| input log codec (`log` workload) | 0 differences over 6,000 frames: headers, frames and errors byte for byte, flipped and forged-checksum payloads through the checker and the decoder | all 15 test binaries pass | **0.72x** (0.676x..0.749x) | flipped |

The `log` workload appends 16 epochs of two width-3 sets of 1,024 rows to a
fresh log, reads every epoch back and reopens the log, on `/dev/shm`. The
divergent edit `log` writes header version 2.

## Part 3: the Flux checkpoint image codec

Stacked on part 2 (in the same PR, #1447: one crate).

### What changed

- `checkpoint.rs`'s image codec runs in Vesper: `vesper/src/image.ves`,
  exported as `cp_identity` (the execution-identity digest, one charge per
  word), `cp_image` (the parse-work reservation, the byte limit and the
  whole image with its BLAKE3 checksum), `cp_check` (the checksum),
  `cp_head` (magic, version, identity, ticks and the source section) and
  `cp_sets` (node count, row schemas, totals, canonical rows, trailing
  bytes). `checkpoint.rs` keeps the circuit and policy structures: it
  flattens the circuit into identity words, rebuilds and validates the
  policy between `cp_head` and `cp_sets` (so every error keeps its place),
  and keeps `validate_state`, which calls the Vesper kernels. The Rust
  writer, reader, identity hasher and image loops are deleted from it.
- `checkpoint_reference.rs` keeps the deleted Rust codec as the reference
  of the new differential test `checkpoint_differential.rs`.

### Flip-gate verdict

| Module | Differential | Every test | ABBA (Vesper/Rust, 95% interval) | Verdict |
|---|---|---|---|---|
| checkpoint image codec (`image` workload) | 0 differences over 1,500 seeded executors (circuits of filters, maps, unions and joins, with and without CDC sources): 7,500 checkpoints under seeded limits and 9,000 restores of flipped, truncated, extended and re-signed images, comparing images, errors and the restored state; every codec error occurs | all 15 test binaries pass | **0.75x** (0.740x..0.786x); `resume` 0.97x (0.910x..1.000x) | flipped |

The `image` workload checkpoints an executor of two width-3 inputs holding
8 ticks of 1,024-row sets and restores the image into a fresh executor, 8
times per batch. The Rust arm is `main` at `468dced`. The divergent edit
`image` skips the check of a node's row count against the row limit.

## Part 4: the Flux durable checkpoint codecs

In the same PR as parts 2 and 3 (#1447).

### What changed

- `durable.rs`'s manifest and spine-batch codecs run in Vesper:
  `vesper/src/spine.ves`, exported as `dur_batch` (a batch blob and its
  byte limit), `dur_batch_read` (a batch checked against its manifest
  reference), `dur_manifest` (the restore parse-work reservation, the byte
  limit and the manifest), `dur_head` (the manifest head through its
  source, shared with the image's `image_head`) and `dur_nodes` (node
  schemas, spine levels and ranges, batch rows, totals, the parent and the
  trailing bytes; a batch named twice is found through the accumulator's
  keyed table). `durable.rs` keeps the store, the spine policy (appends,
  geometric merges, the fold that keeps a manifest restorable) and the sums,
  which use the Vesper accumulator. The Rust `Writer`, `Reader`, source
  codec, `parse_work` and blob loops are deleted.
- The deleted Rust codecs join `checkpoint_reference.rs`; a second
  differential test compares them.
- `tests/port_memory.rs` adds durable checkpoints, a restore and a refused
  restore to its leak cycle.

### Flip-gate verdict

| Module | Differential | Every test | ABBA (Vesper/Rust, 95% interval) | Verdict |
|---|---|---|---|---|
| durable manifest and batch codecs (`durable` workload) | 0 differences over 1,500 seeded executors: 4,500 manifest writes under seeded limits and budgets, 9,000 manifest reads of seeded spines (bad ranges, zero and oversized batches, repeated digests, nine levels) and mutations, and batch writes and reads against wrong references and mutated blobs, comparing bytes, errors, decoded state and the work left; every codec error occurs | all 15 test binaries pass | **0.86x** (0.812x..0.900x) | flipped |

The `durable` workload checkpoints the `image` state durably into a fresh
store on `/dev/shm` and restores it into a fresh executor, 8 times per
batch. The divergent edit `spine` accepts a manifest batch of 0 rows.

## Quarry planner, re-measured (not flipped)

`examples/planner_abba.rs` on `main` at `ab965f2`, callgrind of `window 20`
and the ABBA driver (9 epochs):

| | Rust | Port | Ratio |
|---|--:|--:|--:|
| instructions | 19.47 M | 28.18 M | 1.45x |
| instructions, decoding in place (local change) | | 27.66 M | 1.42x |
| wall clock, ABBA median | | | 1.52x-1.59x (epochs from 1.09x to 2.02x) |

Below the bar on instructions but not reliably on time, so Rust stays the
default. Where the port's instructions go (named-symbol build):

- malloc and free cost the same in both builds (11.3 M against 11.5 M); the
  port makes fewer calls (50,880 against 60,369).
- The Vesper code itself is 9.5 M, more than all of the Rust build's
  non-allocator work (8.0 M). `lex::lex` alone is 3.1 M (the Rust lexer is
  about 1 M): per byte it spills and reloads its locals and turns every
  comparison into a 0/1 word before branching (see gap 2 below).
- The Rust side of the port decodes the result into `PreparedQuery`
  (strings, boxed expressions): about 5 M, which the Rust build does not
  pay on top of its own construction.
- `optimized()` is already faster in the port (2.9 M against 3.5 M).

## Compiler gaps

1. `let s = seq.as_slice();` is refused (E1521, a slice is a second-class
  borrow). Every slice must be written at the call site
  (`f(in seq.as_slice(), ...)`). Not a blocker; it only makes loops that read
  one slice many times wordier.

2. **Comparisons are materialized before branching, and loop locals are
   spilled.** In Quarry's lexer, `if is_space(c)` with
   `c == SPACE || c == TAB || ...` compiles to a `cmp`/`sete`/`movzbq`/
   `test`/`jne` chain per operand, with `i`, `n` and the slice reloaded
   from the stack each iteration. Repro: any byte loop over `in s: Slice[u8]`
   with a `||` of comparisons, e.g.

   ```vesper
   fn spaces(in s: Slice[u8]) -> u64 {
       let mut n: u64 = 0;
       for i in 0..s.len() {
           let c = match s.try_get(i) { Option::Some(x) => widen_u64(x), Option::None => 0 };
           if c == 32 || c == 9 || c == 10 || c == 12 || c == 13 {
               n = wrapping_add(n, 1);
           }
       }
       n
   }
   ```

   This is the largest single item between the Quarry planner and the bar.

## What is left

Flux (Rust still in `src/`):

| Code | Lines | Plan |
|---|--:|---|
| `log.rs` (the file side of the input log) | ~250 | file I/O moves to `@vesper-std/fs` once lane 13's direct libc imports land |
| `durable.rs` (store I/O, spine policy) | ~600 | with the cluster driver, which shares its store and GC |
| `checkpoint.rs` (identity words, policy rebuild, state validation) | ~390 | with `circuit.rs` |
| `circuit.rs` (builder, differentiation, staging) | 741 | with the cluster driver |
| `cluster.rs` (multi-worker driver) | 3,314 | later part (`in dyn EpochSource`, `inout dyn CommitSink`) |
| `kafka_wire.rs`, `kafka_group.rs` | 2,229 | later parts; CRC-32C and the codecs (zlib, LZ4, Snappy, zstd) are C libraries reachable through `.vsys` |
| `kafka.rs`, `kafka_client.rs`, `sink.rs` | 3,203 | sockets need lane 13's libc imports; TLS and SASL need lane 17's crypto shim |
| `cdc.rs` header codec and policy checks | ~150 | small; with the Kafka decoders that share them |

Quarry: the planner, DDL, row executor and access planner are now flipped
to the Vesper port (`vesper-quarry` builds the port unconditionally;
`parse.rs` and the Rust DDL/checker/optimizer/row-executor arms were
deleted: 1,259 Rust lines deleted, 29 insertions). On `main` (`53c9bf6e`),
`examples/planner_abba.rs` passes the Rule 8 `< 1.50x` gate on
`RUSTFLAGS="-C target-cpu=native"`: `1.362x` (`9` epochs,
`BasisPointBounds { floor: 3623, ceil: 3624 }`) and `1.390x` median (`n = 30`,
95% CI `[1.218x, 1.431x]`). The store, WAL, pages, buffer pool, device I/O,
vectorized executor and durable layer remain Rust.

The census row (`scripts/rust-census.py`) is updated for `vesper-quarry`.

## Flux Kafka record encoder (2026-11-25)

The record loop of `encode_batch` (`encode_record` in
`crates/vesper-flux/src/kafka_wire.rs`) is now `kw_encode_records` in
`vesper/src/kafka_wire.ves`. The Rust wrapper packs each batch's keys,
values and headers into one byte buffer plus per-record lengths; the port
computes each record body's length first and writes the length varint,
attributes, timestamp and offset deltas, key, value and headers straight
into the output (one copy, no per-record buffer). The batch header, CRC-32C
and compression stay Rust.

- Differential: `circuit::wire::tests::record_encoder_matches_the_rust_encoder`
  compares the port's bytes with the old Rust encoder (kept in the test
  module) over 203 record sets (absent and empty fields, 20 KB values,
  timestamps at both ends of `i64`) at four base timestamps: 0 diffs.
- Divergence `kafka_encode` (timestamp deltas wrap instead of saturating)
  fails that test while `batches_round_trip_in_every_codec` passes.
- ABBA (`examples/abba.rs` workload `kafka_encode`, 64 batches of 500
  CDC-shaped records per request, `RUSTFLAGS="-C target-cpu=native"`,
  release; Rust arm built from `main` with the same example): Vesper/Rust
  median **0.86x**, 95% CI 0.808x..0.928x, 12 epochs, digests agree.
## Quarry PAX leaf validation (2026-11-25)

`leaf_decode`'s checks (`crates/vesper-quarry/src/store/pages.rs`) are now
`leaf_check` in `vesper/src/leaf.ves`: heap offset, right sibling, the
directory permutation, the occupancy bitmap, tombstone flags, values
(finite floats, 0/1 booleans, heap bounds, UTF-8 text), key order,
tombstones without values, overlapping heap values and zeroed free slots,
with the Rust decoder's status for the first failure. Overlaps are found
with a one-bit-per-page-byte mask instead of a sort; free slots are checked
key by key and then column by column, since every failure there is the
same status. Rust keeps `layout_of` and reads the entries once the check
passes.

- Differential: `store::pages::leaf_differential` runs 20,000 random leaves
  (1-6 columns of every type, tombstones, nulls, removed slots), three in
  four with 1-3 random byte or bit flips, against the old Rust decoder kept
  in the test module; same entries or the same error every time, and the
  flips reach every status but the non-finite float, which its own test
  covers: 0 diffs.
- Divergence `leaf` (BUILD.bazel `PORT_DIVERGENT`: printable ASCII in leaf
  text is rejected) fails store_ssi `write_skew_on_call_doctors_is_rejected`
  while `phantom_write_skew_through_an_index_range_is_rejected` passes.
- ABBA (`examples/leaf_abba.rs`: decode 256 leaves of a six-column table,
  400 passes per window, 15 epochs, `RUSTFLAGS="-C target-cpu=native"`,
  release; Rust arm built from `main` with the same example and hooks):
  median **1.19x**, 95% CI 1.142x..1.233x, digests agree. The check costs
  about twice the old Rust validation; the rest of `leaf_decode` (building
  the `Value`s) is unchanged, so the whole decoder stays inside the bar.

## Flux Kafka wire codecs (2026-11-26)

Every Kafka request and response the Flux client speaks is now one whole
encoder or decoder in `crates/vesper-flux/vesper/src/kafka_api.ves`
(`kw_req_*` and `kw_resp_*`): the request header and frame length, all 20
requests (ApiVersions, SASL handshake and authenticate, Metadata,
CreateTopics, DeleteRecords, ListOffsets, Fetch, Produce,
FindCoordinator, OffsetFetch, JoinGroup, SyncGroup, Heartbeat,
LeaveGroup, InitProducerId, AddPartitionsToTxn, AddOffsetsToTxn,
TxnOffsetCommit with its by-topic grouping, EndTxn), the response header
and all 18 response bodies, the consumer protocol's subscription (sorted,
deduplicated topics and bindings), assignment and assignment parser, and
the v2 record batch frame and header reads. Per-field crossings would cost
more than they save, so each API crosses once: strings and bytes go in as
slices, and a decoder returns its fields as words with strings as spans of
the body. Each decoder fails like the Rust reader did: the same first
error, every string checked as UTF-8 even when skipped, every length
bounded by the bytes left. Where the old client kept partial results of a
failed read (Metadata's leaders, Fetch's decoded batches), the decoders
return the entries read before the failure and then the failure.

`src/kafka_codec.rs` holds the thin wrappers; `Conn`, `Client`, the
producer and `KafkaGroupSource` keep the sockets, TLS, SASL, retries and
state. CRC-32C and the compression codecs stay Rust. The old `W`/`R`
writer and reader and every old codec are kept only in
`src/kafka_reference.rs`, built for tests and for the ABBA Rust arm.

- Differentials (`circuit::codec::tests`): every request encoder on 4,000
  random field sets (empty, multi-byte and over-long strings, nulls,
  extreme integers); every response decoder in about 2.3 million calls on valid
  bodies of each API with random fields, plus truncations, inserted bytes,
  hostile lengths and bit flips, including the response header; the
  consumer protocol on 3,000 binding sets; batch framing in every codec on
  over 10,000 mutated batches: 0 diffs. Successes, broker codes and
  protocol errors all come up.
- Interop: all 11 `tests/kafka.rs` tests pass against Apache Kafka 4.3.2
  (KRaft; PLAINTEXT, TLS, SASL PLAIN and SCRAM-SHA-256/512, transactions,
  zombie fencing, consumer groups, compression, broker crashes).
- Divergences `kafka_api` (strings not checked as UTF-8) and
  `kafka_request` (an over-long string's i16 length is not refused) fail
  the response and request differentials respectively while the other
  passes.
- ABBA (`examples/abba.rs` workload `kafka_wire`: 2,000 rounds per request
  of a transactional consume-transform-produce member's traffic, every
  request framed and every response decoded, fetch with one 8-record
  batch; `RUSTFLAGS="-C target-cpu=native"`, release; the Rust arm is the
  same example built with `--features abba-reference`, which runs the old
  Rust codecs): two runs, median **1.10x** (95% CI 1.029x..1.186x) and
  **1.26x** (95% CI 1.214x..1.301x), 12 epochs each, digests agree. Under
  callgrind the two arms run the same instruction count (210M vs 209M per
  request); writing integers a byte at a time instead of `resize` plus a
  store removed the port's memset calls (1.27x before).

## Quarry vectorized executor (2026-11-26)

The vectorized executor (`src/vector.rs` batches, predicates and
aggregates, `src/vector/join.rs` hash/nested-loop joins and inner-chain
reordering, and the row sink of `src/vector/tiles.rs`) is now
`vesper/src/vector.ves` (#1999). The Rust wrapper encodes the row
executor's input plus the slot and output types; the port keeps each
relation as tuples of input row positions (one word per binding), so cells
are read in place and nothing is copied, evaluates predicates as batch
loops into three-valued masks (branch-free for a column against a
constant), and returns one value triple per output cell. `VectorEngine`
(batch size and `count_ones`), the `Aggregate` enum and the `TileSet`
container stay Rust; `aggregate_rows` is the port's too. The Rust
executor (about 1,500 lines) was deleted.

- Differential: `examples/vector_abba.rs digest` runs 5 projections x 24
  predicates, 4 join kinds x 7 ON x 5 WHERE clauses and 5 inner chains
  (3 and 4 inputs, one an outer join), unoptimized and optimized, over
  three datasets, ten limit sets (work, intermediate rows and bytes,
  output rows and bytes, input rows) and batches 1, 7 and 1,024, through
  `execute_vectorized`, 12 aggregate sets, `execute_tiled`,
  `vectorized_join_orders` and `aggregate_rows`; the Rust arm (`main`
  with the example copied in) and the port print the same 719,088 lines:
  rows, `work` counters, error kinds and messages, tile contents and
  join orders.
- Divergence `vector` (three-valued AND false only when both sides are)
  fails `vectorized_equals_row_executor_for_every_predicate_projection_and_batch`
  while `simd_mask_count_matches_portable_count` passes.
- ABBA (`examples/vector_abba.rs driver`, 15 epochs,
  `RUSTFLAGS="-C target-cpu=native"`, release): scan of 8,000 rows
  **1.072x** (95% CI 0.986x..1.154x), aggregates **0.989x**
  (0.925x..1.056x), tiles **1.048x** (0.903x..1.150x), hash join of
  3,000 x 3,000 rows **0.639x** (0.584x..0.667x), three-way chain
  **0.610x** (0.575x..0.691x); digests agree.

