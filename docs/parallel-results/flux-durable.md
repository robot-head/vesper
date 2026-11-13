# Flux: durable distributed checkpoints and recovery (wave six, lane 11, #253)

Design §34 (items 4 and 5 of §34.1). Base `07322e3` (descendant of the required
`cda092f`). Edited only `crates/vesper-flux/**`, `lean/FluxDbsp.lean` and this
report. Read-only dependencies on `vesper-loom-cas` and `vesper-simpulse` are
plain path dependencies of the crate, so the root patch is only the regenerated
`Cargo.lock` entry for `vesper-flux` (no new external crates, no new workspace
members). `crates/vesper-flux/BUILD.bazel` now declares `//:lean` test data and
a regular-file test cwd for the Lean test.

## What was built

### 1. Durable incremental checkpoints in the CAS (`src/durable.rs`)

`DurableCheckpointer::checkpoint(&Executor, &Store, CheckpointLimits)` writes
one *arrangement* blob per original circuit node (its complete canonical
integral) and one *manifest* blob, all under `Domain::Artifact` in the
immutable BLAKE3 store.

- Manifest `VFLUXDM1` v1: execution fingerprint (the existing identity over
  the ordered DAG, limits and CDC policy), tick count, CDC bindings and next
  offsets (same encoding as the in-memory image), per node the arrangement
  digest, width and row count, and an optional parent-manifest digest.
- Arrangement `VFLUXAR1` v1: fingerprint, node index, width, row count and the
  canonical rows (i64 LE cells and weight).
- Creation reserves the exact restore parse work, so an accepted checkpoint
  restores under the same limits; reuse is bound to the destination store and
  skips objects no longer present.
- Incremental: the executor keeps a per-node change counter, bumped when a
  tick's delta for that node's integral is nonzero, and a process-local
  *lineage* replaced on every restore (in-memory or durable). A checkpointer
  reuses the previous arrangement object of every node whose counter did not
  change, as long as the lineage and fingerprint match; otherwise it rewrites
  everything (the store still deduplicates identical bytes). The report says
  how many arrangements were written and reused.
- `DurableCheckpointer::restore(&mut Executor, &Store, &ObjectId, limits)`
  rebinds the checkpointer to the restored manifest, so the chain continues
  incrementally after recovery.

Integrity on restore, in order, all before publication (failures leave the
executor and checkpointer unchanged): object id must be an artifact blob; the
on-disk size is checked against `max_bytes` before reading; the store rehashes
every object (`Error::Storage("corrupt object")`, `"missing object"`); magic,
version and flags; fingerprint in the manifest *and* in every arrangement; the
CDC policy must equal the executor's prebound one; node count, per-node width
and row-count limits; every arrangement's node index, width and row count must
match the manifest, and no object may be named by two nodes; rows must be
canonical (strictly sorted, nonzero); no trailing bytes; then the existing
relational revalidation (`validate_state`) recomputes every non-input node
from its inputs and rejects inconsistent integrals.

Hash identity proves content identity, not provenance: anyone able to write
the store can still write a different *consistent* state. Writes use the
store's own temp-file/fsync/rename protocol; a failed checkpoint can leave
unreferenced immutable blobs (no GC is implemented).

### 2. Multi-worker execution under SimPulse (`src/cluster.rs`)

`run_cluster(&Circuit, &[TickInput], &ClusterConfig, crash_steps, &Store)`
runs N workers (1..=64) and a checkpoint coordinator as SimPulse tasks over a
replayable input log (one `TickInput` per epoch).

- Exchange: each worker reads its source split (rows at positions
  `i % N == worker`), hash-partitions every row by its input's configured key
  column and sends one Z-set delta message per destination per epoch (empty
  messages double as end-of-epoch markers). Links are FIFO with seeded delays.
- Partition check (`ClusterConfig::validate`): keys are propagated through
  filters, column-preserving maps and key-agreeing unions; every equi-join must
  join on its inputs' partition keys, else `Error::Partition`. Linear operators
  distribute over the partition sum and co-partitioned joins pair rows on one
  worker, so the summed worker outputs equal the single-executor outputs.
- Progress: each worker owns a `FrontierTicket` in a cluster `Frontier` and
  advances it after processing an epoch; the coordinator refuses to commit a
  checkpoint the watermark has not passed. Tickets are retired explicitly on
  recovery and at the end (the run checks no hold leaks).

### 3. Coordinated checkpoints and crash recovery

Aligned barriers (Chandy-Lamport specialised to FIFO links and global barrier
epochs): after sending its data for a barrier epoch a worker sends a barrier
on every link; a worker snapshots (durably, incrementally) once barriers from
all N peers arrived and it has processed exactly that epoch, and processes no
later epoch before the snapshot. The coordinator collects N acknowledgements,
writes a global manifest `VFLUXGM1` (fingerprint, worker count, partition
columns, interval, epoch and every worker manifest) to the CAS and commits.
A barrier always follows the last epoch, so completion is a commit.

Outputs are staged per (epoch, worker) and become visible only when a covering
checkpoint commits, so visible output never rolls back (exactly-once output).
A crash at a scheduled step discards every worker's memory, in-flight
messages, staged outputs and pending acknowledgements; recovery reads and
verifies the last global manifest, restores every worker from its durable
manifest (checking its tick count equals the checkpoint epoch), and replays
the input log from the next epoch. This is global rollback recovery.

### 4. Lean (`lean/FluxDbsp.lean`)

Added, about the models only (not the Rust code, byte formats, store,
SimPulse or protocol):

- `run_append`: running a deterministic step function over `xs ++ ys` is
  running `xs` then `ys`, outputs concatenated.
- `restore_rejected_keeps_live`: a rejected image leaves the live state.
- `restore_replay_eq_uninterrupted`: for an honest decoder
  (`dec (enc s) = some s`), restoring the checkpoint taken after `k` inputs
  (from any crashed state) and replaying `drop k` reaches the uninterrupted
  final state, and pre-checkpoint outputs followed by replayed outputs equal
  the uninterrupted outputs.
- `repeated_recovery_eq_uninterrupted`: any number of recoveries from the same
  checkpoint still end in the uninterrupted state.
- `linear_distributes_over_partitions`: an additive, zero-preserving operator
  on Z-sets (modelled as `α → Int`) commutes with summing worker partitions.
- `copartitioned_join_key`: if one key's rows live on exactly one worker of a
  duplicate-free worker list, the sum of per-worker join weights for that key
  equals the join of the summed weights.

All twelve theorems use only `propext`/`Quot.sound` (checked by
`tests/lean_model.rs` when `VESPER_LEAN` is set, and by
`scripts/ci/check-lean-models.sh`). No `sorry`, `admit` or new axioms.

## Tests

- `tests/durable.rs`: restore-then-continue equals uninterrupted at every
  split; incremental write/reuse counts, parent chain, rebinding after
  restore, full rewrite for a foreign executor; tamper rejection (flipped
  arrangement byte, flipped manifest byte, deleted object, correctly hashed
  manifest substituting another node's arrangement, stale same-node
  arrangement, truncated/trailing/empty manifests, wrong domain, wrong
  circuit, byte limit), each leaving the victim executor unchanged; CDC
  offsets survive a durable restore and replay of consumed LSNs is rejected.
- `tests/cluster.rs`: 1–4 workers × 4 seeds × 3 workloads match the single
  executor per epoch (inserts and retractions, join, filter, map, union) with
  incremental reuse observed; same seed gives an identical SimPulse journal,
  different seeds give different interleavings and identical outputs; a crash
  at **every** scheduler step of a 3-worker, 6-epoch run (113 steps) recovers
  to the identical per-epoch output and restores exactly the last checkpoint
  committed before the crash; triple crashes, including crashes during replay;
  non-co-partitioned joins, key-dropping maps and bad configurations are
  rejected; the step budget is a typed limit.
- `tests/lean_model.rs`: hygiene and kernel check with axiom audit.

## Checks

Run on the cloud host (x86_64 Linux) with an isolated target dir:

- `cargo fmt --all -- --check`: clean.
- `VESPER_LEAN=<lean 4.24.0> cargo test -p vesper-flux --locked`: 37 tests
  (36 integration including the Lean check, 1 unit) pass; the crash-at-every-step test takes
  about 5 s in debug.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: see the PR
  for the result on the final head.
- `lean lean/FluxDbsp.lean` with Lean 4.24.0: success, axiom report as above.

## Decisions

- Global rollback recovery rather than per-worker recovery: simple, and with
  aligned barriers it needs no channel-state logging. In-flight messages are
  regenerated by replaying the deterministic source split.
- Barrier epochs are fixed by the input log position (every
  `checkpoint_interval` epochs and after the last one), so every worker
  injects the same barriers without a coordinator broadcast.
- The cluster is a protocol model on one host. SimPulse orders task steps
  deterministically; it does not run workers concurrently or model process
  isolation, partial disk writes or real networks. Crashes are cluster-wide.
- The coordinator's committed-manifest handle is modelled as surviving a
  crash (a durable coordinator record); worker state survives only through
  the CAS.

## Remaining gaps

- No Krabka/Kafka source, Quarry `WriteTxn` or io_uring `O_DIRECT` sink: the
  input log is an in-memory replayable slice and the transactional sink is in
  memory (§34.1 item 4).
- `BarrierToken` is not a linear source-language obligation; Rust ownership of
  `FrontierTicket` is not evidence that the Vesper typing rules are
  implemented.
- No partial (single-worker) recovery, no rescaling to a different worker
  count, no unaligned checkpoints, no coordinator failover, no CAS GC of
  superseded manifests/arrangements.
- Arrangements are whole per-node integrals, not LSM spine levels: a changed
  integral is rewritten in full.
- Exchange routes by one key column per input; joins whose keys are not the
  partition keys are rejected rather than re-shuffled.
- No performance claims: nothing was benchmarked.
