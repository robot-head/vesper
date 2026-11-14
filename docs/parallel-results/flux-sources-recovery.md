# Flux: durable sources and sinks, partial recovery and rescaling (wave seven, lane 12, #335)

Design §34 (items 4 and 5 of §34.1, continuing the wave-six
[durable checkpoints](flux-durable.md)). Base `283c811` (a descendant of the
required `c025b9a`). Edited only `crates/vesper-flux/**`, `lean/FluxDbsp.lean`
and this report. The crate gains a path dependency on `vesper-quarry`
(read-only, its `durable` and `device` modules as they are on `main`), so the
root patch is only the regenerated `Cargo.lock` entry, in its own commit. No
new external crates and no new workspace members. `BUILD.bazel` gives the
fault-injection test a `long` timeout.

## What was built

### 1. Durable replayable input log (`src/log.rs`)

`InputLog` is an append-only file: a checksummed header (magic `VFLUXIL1`,
input widths) and one frame per epoch (length, payload, BLAKE3 over both).
The epoch number is stored in the frame and is the source offset a sink
commits.

- `append` writes the frame and calls `File::sync_all` before it returns.
- `open` verifies every frame. An incomplete or checksum-failing final frame is
  a torn append: it is cut off durably and reported (`LogRecovery`). A bad
  frame followed by more bytes is `Error::Storage("corrupt log")`.
- `read` re-verifies the checksum and decodes under `LogLimits` (rows per
  frame, frame bytes, columns, epochs); rows must be canonical.
- It is a single-writer local file, not a Krabka/Kafka partition.

`EpochSource` abstracts the input: `InputLog`, `Vec<TickInput>` and slices
implement it.

### 2. Transactional sink into Quarry (`src/sink.rs`)

`CommitSink` publishes a checkpoint's outputs together with a `CommitRecord`
(base-circuit fingerprint, last covered epoch = source offset, worker count,
global manifest handle) atomically, and gives the record back after a crash.
That record is now the only durable "how far did we get" state: whole-cluster
recovery, partial recovery and coordinator failover all read it.

- `QuarrySink<D>` writes one record per output row plus the single commit
  record (updated in place) inside one Quarry `Db::begin` … `Db::commit`
  transaction. Quarry syncs its WAL before `commit` returns, and its recovery
  rolls back transactions without a commit record, so after any crash either
  the old record and none of the new outputs or the new record and all of them
  are visible. `open` rejects two commit records, outputs without a record
  and outputs past the record's offset. A Quarry checkpoint runs every eight
  commits to truncate its log.
- The sink refuses an offset regression, output epochs at or before the
  previous record or past the new one, and a record for another circuit.
- `CrashMedium` says what a device looks like after the holding process
  dies: `FsDevice` (process crash, files as they are), `SimDevice` and
  `&mut SimDevice` (power loss: only synced bytes survive).
- `MemorySink` keeps the same protocol in memory (used by `run_cluster`).

This uses Quarry's durable record store, not its SQL catalog, and no
io_uring or `O_DIRECT` path (the host denies `io_uring_setup`).

### 3. LSM spine arrangements and CAS GC (`src/durable.rs`, `src/cluster.rs`)

Durable checkpoints are now format `VFLUXDM2`: per node, an ordered *spine*
of immutable `VFLUXSB1` batch blobs whose Z-set sum is the integral. Each
batch covers a tick range `(lo, hi]`; ranges of a spine are disjoint and
increasing, and batches are bound to their node, width and range.

- A checkpoint appends one batch holding only the change since the previous
  checkpoint of the same executor history (`current - base`), then merges the
  two newest batches while the older is at most twice the newer's size or the
  spine is longer than `MAX_SPINE_LEVELS` (8); rows that cancel in a merge
  are dropped. Unchanged spines are referenced again. `DurableReport` counts
  batches written, reused, merges and rows written.
- A spine with a batch missing from the store is rewritten as one batch.
- Restore checks magic, version, fingerprint, ranges (within the tick count,
  disjoint), batch headers against the manifest, no object named twice,
  canonical rows, the spine sum's row count, all limits, then the existing
  relational revalidation. Failures leave the executor unchanged.
- The checkpointer keeps a copy of the integrals it last wrote, so memory is
  twice the state; compaction reads the merged batches back from the store.

Garbage collection in the cluster: when a commit supersedes a global
checkpoint, the coordinator deletes the objects the old one references and the
new one does not (global manifest, worker manifests, batches, channel blobs).
At the end of a run, a sweep deletes every Flux object of this circuit (header
magic and fingerprint match) the final commit does not reference: orphans of
failed incarnations, uncommitted manifests, unaligned checkpoints that lost
to a duplicate. The CAS has no delete API, so GC removes object files
directly (clearing the read-only bit first for Windows). It assumes one
cluster per circuit fingerprint writes a store. `collect_garbage: false`
disables it.

### 4. Re-shuffled joins (`src/circuit.rs`)

`Circuit::plan` propagates partition keys (through filters,
column-preserving maps and key-agreeing unions) and inserts an internal
`Op::Exchange { a, key }` before every join input whose key is not its join
key, instead of rejecting the join. On one executor an exchange is the
identity. In the cluster a worker evaluates the differentiated program in
order (`Executor::stage` / `run_stage` / `provide` / `finish_stage`); at an
exchange it scatters its local delta by the owner of each key and waits for
all N parts. Exchange integrals are received state, like inputs, so
relational revalidation treats them like inputs; cluster restore instead
checks that every input and exchange row routes to the worker holding it.
`ClusterConfig::exchanges` reports how many a plan needs.

### 5. Partial recovery, coordinator failover, unaligned checkpoints, rescaling

`run_cluster_with(circuit, source, sink, store, cfg, faults)` runs every
epoch of the source the sink has not committed. `run_cluster` keeps its old
signature (in-memory sink, whole-cluster crashes).

- Coordinator commit in three scheduler steps: write the global manifest
  `VFLUXGM2`, commit the sink transaction, then notify workers and collect
  superseded objects. Acknowledgements carry the worker manifest, the channel
  blob and the worker's outputs since the last commit it knows of.
- `FaultKind::Cluster`: everything volatile is lost; all workers restore from
  the sink's record.
- `FaultKind::Worker(w)`: only that worker's memory and the messages addressed
  to it are lost. It restores from the last commit. Every worker keeps the
  data, shuffle and barrier messages it sent since the last commit it knows
  of (upstream backup) and resends those addressed to the recovering worker.
  Receivers drop duplicates and check that each equals the original, so
  non-deterministic replay is an error. A worker still replaying epochs that a
  newer commit covers restores from that commit when it is notified
  (fast-forward). The failed incarnation's frontier hold is kept until the
  replacement passes it, so the watermark never regresses.
- `FaultKind::Coordinator`: acknowledgements, staged outputs and an
  uncommitted global manifest are lost. The new coordinator reopens the sink
  (`CommitSink::crash`), reads the record and asks every worker to resend its
  acknowledgements.
- `CheckpointMode::Unaligned`: a worker snapshots at the first barrier it sees,
  whatever epoch it reached, and keeps processing. Every message for epochs
  between its snapshot and the barrier epoch is recorded as channel state and
  written as a `VFLUXCH1` blob once it has processed the barrier epoch.
  Restore replays the channel state locally to reach the barrier epoch and
  rejects incomplete or duplicated channel state. A replaced worker may
  acknowledge a different (equally valid) unaligned snapshot; the first one
  wins. In aligned mode duplicates must be identical.
- Rescaling: if the sink's record was written by another worker count or
  other partition columns, the run first restores every old worker (replaying
  channel state), sums the input integrals, routes them to the new workers,
  recomputes every derived integral (gathering each exchange's input
  globally), checkpoints the new workers and commits the rescaled global
  manifest with no outputs; then it continues. Rescaling happens between runs
  at a commit, not live.

Each fault is reported with the protocol phase it hit (`CheckpointPhase`:
idle, barriers injected, snapshotting, all acknowledged, manifest written,
committed but not notified).

## Lean (`lean/FluxDbsp.lean`)

Seven theorems added, about models only (Z-sets as total functions, a
routing function, the deterministic step fold), not about the Rust code, byte
formats, Quarry, the log or the cluster protocol:

- `zsum_append`; `spine_append_delta`: appending `current - sum spine` makes
  the spine sum `current`; `spine_merge_preserves_sum`: merging two adjacent
  batches keeps the sum.
- `restrict_parts_sum`: routing over a duplicate-free worker list containing
  every target loses and duplicates nothing; `rescale_preserves_global`:
  re-routing the sum of any old partition keeps the global sum;
  `exchange_places_rows`: after an exchange every held row routes to its
  holder.
- `channel_replay_reaches_barrier`: the snapshot after `k` inputs, replayed
  over inputs `k..b`, reaches the state after `b` inputs and produces exactly
  the outputs between them.

All 19 theorems use only `propext`/`Quot.sound` (or no axioms). No `sorry`,
`admit` or new axioms; `tests/lean_model.rs` checks them when `VESPER_LEAN`
is set.

## Tests

- `tests/recovery.rs` (circuit with a re-shuffled join, 3 workers, 6 epochs):
  a whole-cluster crash, a single-worker crash and a coordinator failover at
  **every** scheduler step of the fault-free run, each in aligned and
  unaligned mode. Every run must reproduce the single-executor output per
  epoch, restore exactly the last commit before the fault, and leave every
  epoch in the sink exactly once; each sweep must hit all six checkpoint
  phases. Also: repeated single-worker faults on 4 workers (resend observed),
  storms of six mixed faults per run in both modes, unaligned runs that write
  channel blobs and equal the aligned output, GC with and without faults
  (fewer files left, the final commit's objects present), and an invalid
  fault target.
- `tests/rescale.rs` (two joins, the second always re-shuffled): stop after
  3 of 8 epochs and resume on every worker count 1..=5 from every count
  1..=4, aligned and unaligned; all outputs equal the single-worker run and
  the sink holds every epoch once. Rescaling that also changes partition
  columns, with worker, cluster and coordinator faults after it. A finished
  run is a no-op; a sink ahead of its source or of another circuit is
  rejected.
- `tests/sources_sinks.rs`: log round trip, every torn-tail cut of the last
  frame, mid-file corruption, header corruption, limits; the Quarry sink
  crashed after every device operation of a commit (lose-unsynced and
  keep-unsynced), always all or nothing and never losing an acknowledged
  commit; an end-to-end run over a log file, a Quarry sink on `FsDevice` and a
  CAS directory that is killed three ways, reopened as by a new process,
  extended and resumed on more workers; faults of every kind at many steps
  with the Quarry sink on the power-loss device.
- `tests/durable.rs`: updated for spines, plus geometric compaction (40
  incremental checkpoints of a 400-row integral write fewer than 100 rows
  each, spines stay within 8 levels, every checkpoint restores) and malformed
  spines (a batch named twice, a range past the tick count).
- `tests/cluster.rs`: the old suite, with the rejection test replaced by
  re-shuffled joins that match the single executor.

## Checks

Cloud host (x86_64 Linux), isolated target directory, `CARGO_PROFILE_*_DEBUG=0`:

- `cargo fmt --all -- --check`: clean.
- `VESPER_LEAN=<lean 4.24.0 release zip> cargo test -p vesper-flux --locked`:
  54 tests pass (`recovery` takes about 46 s in debug).
- `cargo +1.100.0 clippy -p vesper-flux --locked --all-targets -- -D warnings`:
  clean.
- `cargo test --workspace --locked` and the strict workspace Clippy: see the
  PR for the results on the final head.

## Decisions

- The sink's commit record, not coordinator memory, is the durable recovery
  point, and it commits the source offset and the checkpoint handle with the
  outputs in one Quarry transaction.
- Partial recovery uses upstream backup plus deterministic replay with
  checked deduplication rather than channel logging in aligned mode; peers
  never roll back.
- Unaligned channel state is "every message for epochs between snapshot and
  barrier", because shuffle messages of an epoch are produced after the
  sender's barrier for it. Restore replays it locally, so unaligned
  checkpoints compose with partial recovery and rescaling.
- Rescaling recomputes derived integrals from input integrals instead of
  splitting every node: exchange and join integrals are not partitioned by the
  same key as the inputs.
- The frontier tracker is modelled as a service that survives a coordinator
  failover; a whole-cluster crash rebuilds it.

## Remaining gaps

- No Krabka/Kafka source and no io_uring `O_DIRECT` sink path; the log is a
  local single-writer file, the sink is Quarry's record store, not a SQL table.
- `BarrierToken` and `FrontierTicket` are not linear source-language
  obligations; Rust ownership of `FrontierTicket` or Quarry's `Txn` is not
  evidence that the Vesper typing rules are implemented.
- The cluster is a SimPulse protocol model on one host: no real network,
  process isolation or concurrency. Faults are injected at step boundaries;
  crashes inside a Quarry commit are covered by the sink's device-level test,
  not by the cluster sweep.
- Rescaling is offline (at a commit, between runs), not live.
- GC assumes one cluster per circuit fingerprint per store; after a
  partition-column change, orphans written under the old plan by an earlier
  run are not swept (committed old-plan objects are collected).
- Spine compaction reads batches back from the CAS; the checkpointer keeps a
  full copy of the last checkpointed integrals.
- No performance claims: nothing was benchmarked.
