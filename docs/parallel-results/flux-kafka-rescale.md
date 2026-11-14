# Flux: a Kafka source and sink, and live rescaling without a stop (wave eight, lane 18, #467)

Design §34 (items 4 and 5 of §34.1), continuing the wave-seven
[sources, recovery and offline rescaling](flux-sources-recovery.md) lane
(PR #381, which this branch is built on and which merged as `d6696a8`).
Work started on `1094e64` (main after the wave-eight plan) merged with the
#381 branch, and the branch was then merged with main at `d6696a8`. Edited `crates/vesper-flux/**`,
`lean/FluxDbsp.lean`, this report, plus one new workflow file
(`.github/workflows/flux-kafka.yml`, which starts the broker in CI) and the
root `Cargo.lock` patch in its own commit.

## What was built

### 1. Live rescaling (`src/cluster.rs`)

`ClusterConfig::live_rescales` is a schedule of `LiveRescale { after_epoch,
workers }`: from `after_epoch + 1` on, the job runs on `workers` workers.
`after_epoch` must be a barrier epoch, the schedule must increase and every
entry must change the count (typed `Error::Cluster("live rescale schedule")`
/ `Error::Limit("workers")` otherwise). The run keeps one SimPulse task per
worker slot of the largest layout; a slot the current layout does not use is
idle and holds no state.

Protocol at a rescale barrier `b` (from `n` to `m` workers):

1. The `n` old workers finish epoch `b` and checkpoint it normally; the
   coordinator commits `b` under the old layout (outputs, source offset,
   global manifest), exactly as for any barrier. No worker runs `b + 1`
   before it holds new-layout state.
2. The coordinator sends `Reconfigure { b }` to every slot.
3. Each old worker splits its *keyed* integrals (every input integral by its
   partition column and every exchange integral by its key) by the new
   routing and sends each new worker its part (`Migrate`), including empty
   ones as completion markers. Old workers the new layout drops retire
   (frontier hold released, slot idle, but anything peers already sent for
   a later activation is kept).
4. A new worker that has the parts of all `n` old workers sums them,
   recomputes every derived integral (joins, maps, filters, unions) locally
   from those keyed integrals, builds its executor at tick `b + 1`, writes a
   full durable checkpoint and reports it (`Migrated`). It then runs `b + 1`
   at once; data for `b + 1` may already be waiting, because senders route
   epoch `e` by the layout of `e`, which the static schedule fixes.
5. When all `m` new workers reported, the coordinator commits the new
   layout's global manifest at the same `after_epoch = b` with no outputs,
   and garbage-collects the old layout's objects.

Nothing is read back from storage and no task restarts; each worker blocks
only until its own migrated parts arrive. `ClusterRun::migrations` reports
each one (`LiveMigration`: rows moved and total, the new manifest, and the
scheduler steps between the two commits).

Faults: before the barrier commit and after the new-layout commit, worker,
coordinator and whole-cluster faults behave as before (partial recovery from
upstream backups, coordinator resync). Between the two commits a worker or
coordinator fault is *escalated* to a whole-cluster recovery
(`Recovery::escalated`); whole-cluster recovery from a commit whose layout
the schedule has already left (the barrier commit of an interrupted
migration) rescales offline from it first (the wave-seven path,
`ClusterRun::recovery_rescales`), then continues. A run whose source ends at
the barrier leaves the rescale to the next run, which does it offline at
start.

### 2. Kafka source (`src/kafka.rs`, `KafkaSource`)

- Pure-Rust client: `rskafka =0.6.0` (no librdkafka), compression features
  off, plaintext only, with a private single-threaded Tokio runtime per
  `KafkaConnection`.
- Each `CdcPolicy` binding is read from one Kafka partition
  (`KafkaBinding { topic, partition }`). Record values are the existing
  Flux CDC format: the 32-byte `CdcHeader` and `width` LE i64 cells.
- `poll` cuts the next epoch from the records between the end of the last
  epoch and each partition's high watermark (at most
  `max_records_per_epoch` per partition), decodes and validates them, and
  only then appends the end offsets and next LSNs to a durable *epoch index*
  (`VFLUXKX1`: checksummed header with the bindings' fingerprint and initial
  offsets, then one fixed-size BLAKE3-checksummed frame per epoch, fsynced;
  a torn final frame is cut off on open, a bad earlier frame or a
  checksummed frame whose offsets move backwards is
  `Error::Storage("corrupt kafka index")`).
- **Offsets tied to checkpoints**: epoch `e` always denotes the same offset
  ranges, so the commit record's `after_epoch` (written in the same Quarry
  transaction as the outputs and the checkpoint handle) names fixed Kafka
  offsets. `committed(record)` maps a commit to positions; `release(record)`
  deletes Kafka records below them (`DeleteRecords`). Offsets are not
  committed to a Kafka consumer group: the Flux checkpoint is the only
  authority.
- `EpochSource::read` replays an epoch from Kafka (small cache for workers
  reading the same epoch) and checks the replay reaches the indexed LSNs.
- LSNs must be consecutive per binding: a record whose LSN is below the
  expected one is a producer retry and is dropped (`stats().duplicates`);
  gaps, foreign partition ids, wrong schema, reserved flags, zero weights,
  wrong lengths and missing values are typed `Error::Cdc` errors and no
  epoch is cut. Broker failures are `Error::Kafka(..)` with the client's
  message in `KafkaConnection::last_error`.

### 3. Kafka sink (`KafkaSink`)

Guarantee: **exactly-once in the output topic for a single writer per
output partition**, with publication lagging the commit; and, as before,
exactly-once in the Quarry sink.

- `KafkaSink<S: CommitSink>` commits into `S` (normally `QuarrySink`)
  first, then produces only committed epochs, so readers never see output
  that is later rolled back. A publication failure never fails the commit:
  it stays pending until the next commit or `flush()`.
- Rows go to output partition `hash(row[key_column]) % partitions`. Each
  record is a CDC value (so the topic is itself a valid Flux source: LSN
  `0, 1, ...` per partition) with headers for the epoch, the row's sequence
  number in the epoch's partition slice and a circuit tag.
- Produce requests use a client without retries (`BackoffConfig` deadline
  zero), so rskafka never resends a batch after a lost response. After a
  crash, a sink restart or any produce error, the sink reads the last record
  of each output partition (the *fence*) and skips everything at or before
  its `(epoch, seq)`. A tail written by another circuit, or a partition
  emptied by retention (no fence), is a typed error instead of a guess.
- `read_kafka_output` reads a topic back and fails on any LSN gap or
  duplicate, out-of-order `(epoch, seq)` or foreign circuit.

Rskafka 0.6 implements neither Kafka transactions nor idempotent-producer
sequence numbers, so the sink is not a Kafka transactional producer and
`read_committed` consumers gain nothing; the guarantee rests on the fence
and on there being one writer per output partition.

## Lean (`lean/FluxDbsp.lean`)

Three theorems added, about models only (Z-sets as total functions,
`restrict` by a routing function, lists of records), not about the Rust
messages, SimPulse, rskafka or a broker:

- `migration_assembles_owner`: a new worker summing the parts every old
  worker sends it holds exactly the new routing's share of the global state.
- `migration_preserves_global`: the new workers' assembled states sum to the
  old global state (nothing lost or duplicated).
- `fenced_publish_exactly_once`: if a partition holds any prefix of the
  committed record sequence, publishing what follows that prefix yields
  exactly the sequence.

All 22 theorems use only `propext`/`Quot.sound` (or no axioms); no `sorry`,
`admit` or new axioms. `tests/lean_model.rs` checks them when `VESPER_LEAN`
is set (Lean 4.24.0 from the GitHub release zip).

## Tests

- `tests/live_rescale.rs` (two joins, the second re-shuffled, so input and
  exchange state both migrate):
  - every `from` in 1..=4 to every other `to` in 1..=5, aligned and
    unaligned: outputs equal a **fixed-parallelism cluster run** of the same
    job and the single-executor reference, one migration, no restore, two
    commits at the barrier, rows moved, every epoch in the sink once;
  - repeated schedules up and down (`2→4→1→3`, `2→1→5`, `2→1→3→2→4`),
    including a worker that retires and is later reactivated;
  - a whole-cluster crash, a worker crash (also on idle and retired slots)
    and a coordinator failover at **every scheduler step** of a `2→3` and a
    `3→2` live rescale (and of `2→1→3` for workers), both modes: every run
    reproduces the reference with every epoch once; faults inside the
    reconfiguration are escalated and the rescale is redone offline;
  - a run ending inside the reconfiguration resumes offline; invalid
    schedules and fault targets are typed errors.
- `tests/kafka.rs`, against **Apache Kafka 4.3.2** (Scala 2.13, KRaft
  combined broker/controller, single node; OpenJDK 21) started by each test
  on free local ports:
  - round trip with producer retries injected, a 3→2→4 live rescale,
    Quarry + Kafka sinks; the committed positions equal the high watermarks
    and the producer's LSNs;
  - Flux crash and restart: incarnations killed after a bounded number of
    steps or carrying worker, coordinator and cluster faults, while new
    records arrive between incarnations and the job rescales live and
    offline; every incarnation's output equals the reference and the end
    state has every epoch once in Quarry and in the topic;
  - broker SIGKILL and restart: a poll while the broker is down is a typed
    error and cuts nothing; a run whose reads fail commits nothing; after the
    restart the run completes (with a live rescale); with the broker down
    between commit and publication the commits land in Quarry, publication
    stays pending, and a flush after the restart publishes every epoch once;
  - fence: a new sink incarnation republishes nothing; a foreign record at a
    partition tail is refused; `release` deletes exactly the committed
    records and the uncommitted epochs still replay; malformed values, wrong
    schema or partition id, zero weight and LSN gaps are typed errors; a torn
    index tail is cut and an index for other bindings is refused.
  Without a broker every test prints a note and passes, unless
  `VESPER_KAFKA_REQUIRE=1` (set in the workflow).
- Unit tests in `src/kafka.rs` (no broker): every torn cut of the last index
  frame, damaged last vs middle frames, regressing offsets, the epoch limit,
  record decoding, output partitioning, an unreachable broker.
- The wave-seven suites (`recovery`, `rescale`, `cluster`, `durable`,
  `sources_sinks`, ...) pass unchanged.

## Checks

Cloud host (x86_64 Linux, 4 cores), isolated target directories,
`CARGO_PROFILE_*_DEBUG=0`:

- `cargo fmt --all -- --check`: clean.
- `cargo +1.100.0 clippy -p vesper-flux --locked --all-targets -- -D warnings`: clean.
- `VESPER_LEAN=<lean 4.24.0> cargo test -p vesper-flux --locked` (debug, no
  broker): all pass; `live_rescale` takes about 143 s and `recovery` 57 s
  (both get a `long` Bazel timeout).
- `VESPER_KAFKA_HOME=<kafka_2.13-4.3.2> VESPER_KAFKA_REQUIRE=1 cargo test
  -p vesper-flux --release --locked --test kafka`: 6 pass.
- Workspace test and Clippy runs: see the PR for the final head.

## Decisions

- Offsets live in the Flux checkpoint (via the durable epoch index and the
  commit's epoch number), not in a Kafka consumer group: that is what makes
  the offset commit atomic with the outputs, and replay deterministic.
- The Kafka sink is a transactional outbox (commit, then publish with a
  read-back fence) instead of Kafka transactions, which the pure-Rust client
  does not implement; `rdkafka` was avoided because it needs librdkafka
  under Bazel.
- Live migration starts after the barrier commit, and faults inside the
  window escalate to whole-cluster recovery: partial recovery would need the
  migrated state to be recoverable before the new layout is committed.
- New workers rebuild derived integrals from the migrated keyed integrals
  instead of receiving them: joins and projections are not partitioned by
  the same key as their inputs.
- Epoch boundaries are chosen by `poll` from the high watermarks at that
  moment, so they depend on timing; the index makes them durable before use,
  which is what replay needs.

## Remaining gaps

- The cluster is still a SimPulse protocol model on one host. "Live" means
  the schedule is applied inside one running simulation without restore or
  restart; the reconfiguration still pauses each worker until its parts
  arrive (one commit round trip plus the transfer), all key ranges move at
  once (no Megaphone-style per-bin migration), and the schedule is fixed in
  the configuration rather than requested at run time.
- Worker and coordinator faults inside the reconfiguration window cost a
  whole-cluster recovery.
- Kafka: no TLS/SASL, no compression, no consumer groups, no Kafka
  transactions; one writer per output partition is assumed, not enforced;
  a fence lost to retention stops publication. A malformed record in a
  partition blocks the job (a typed error on every poll) until an operator
  intervenes. The Kafka tests run only in the new workflow, not under Bazel.
- `BarrierToken` and `FrontierTicket` are still not linear source-language
  obligations; Rust ownership is not evidence that the Vesper typing rules
  are implemented.
- No performance claims: nothing was benchmarked.
