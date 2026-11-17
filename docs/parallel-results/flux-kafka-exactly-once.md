# Flux: Kafka transactions, consumer groups, TLS/SASL, compression and per-bin live migration (wave nine, lane 20, #597)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Design §34. Continues [Flux Kafka + live rescale](flux-kafka-rescale.md)
(wave eight, lane 18, PR #536). Base: `main` at `5a018bd`. Edited only
`crates/vesper-flux/**`, `lean/FluxDbsp.lean`,
`.github/workflows/flux-kafka.yml`, this report, and the root `Cargo.lock`,
which is in its own commit.

## What was built

### 1. Kafka wire client, idempotent producer and transactions

The library's Kafka data path is now Flux's own wire client
(`src/kafka_wire.rs`, `src/kafka_client.rs`). rskafka 0.6, the client the
prompt asked to keep, is still a dev-dependency, but it could not do the
job. It has no idempotent producer, no transactions, no consumer groups and
no aborted-transaction filtering. It also fails on Kafka 4.x control records
("Unknown control batch record version: 8192"), so it cannot even read a
transactional topic. The new client is built from vetted crates for
everything that is not protocol framing: `crc32c`, `flate2`, `snap`,
`lz4_flex`, `zstd`, `rustls` 0.23 and `rsasl` 2.3.1, all pinned exactly. No
cryptography is hand-written.

* **Protocol.** It speaks request header v1/v2 and flexible (compact,
  tagged) encodings, with the API versions listed in `VERSIONS`, which are
  checked against the broker's ApiVersions. It writes and reads v2 record
  batches with CRC-32C, control batches, and read_committed filtering of
  aborted transactions using the last stable offset.
* **Bounded, typed decoding.** The reader is bounds-checked and every
  decompressor is capped at `max_decoded`. Malformed bytes give
  `Error::Kafka(..)`, never a panic, and a fuzz-style unit test checks this.
* **`KafkaProducer`.** It is idempotent (producer id, epoch, per-partition
  sequence numbers) or transactional (`begin` / `send` / `send_offsets` /
  `commit` / `abort`). InitProducerId on a transactional id fences the dead
  predecessor: the zombie then gets "producer fenced".
* **Delayed requests.** `HeldProduce` keeps a produce request in flight, so
  tests can deliver it late or twice. Kafka 4.3.2 answers a duplicate
  idempotent batch with the original offset; it does not append it again.
* **`KafkaSink`.** With `transactional_id` it runs one transaction per Flux
  epoch: the epoch's records, then a progress TxnOffsetCommit to group
  `<txn>.progress`, then the consumer-group input offsets, then EndTxn.
  * The progress entry is the per-partition next LSN, with metadata
    `vflux/1 e=<epoch> t=<tag>`.
  * A restart calls InitProducerId, which fences the old producer and
    aborts its open transaction, then reads the progress with
    `require_stable`.
  * So whether an epoch is visible no longer depends on an in-doubt window:
    the records and the progress either both commit or both abort.
  * `PublishFault` injects crashes after the records and after the offsets.
* **Reading committed output.** `read_kafka_output` reads with
  read_committed and waits for the LSO. EndTxn returns before the broker
  writes the commit markers, so the LSO lags briefly
  (`KafkaConnection::wait_stable`).

### 2. Consumer groups, TLS/SASL, compression, dead letters

* **`KafkaGroupSource`** (`src/kafka_group.rs`) joins a group with the
  classic consumer protocol, using its own assignor
  `vesper-flux-roundrobin`. It heartbeats, and it rebalances only at Flux
  checkpoints: a member rejoins only once every epoch it cut is published.
  * Its durable index (`VFLUXKG1`) records the span of each partition
    assignment.
  * Input offsets commit inside the sink transaction, through
    `GroupOffsetSource`. A member that finds a committed offset other than
    its span start fails with "assignment lost unpublished epochs" instead
    of reading twice.
  * Scope: per-member Flux state is not moved between group members. The
    cross-member guarantee holds for per-partition pipelines, and static
    membership (`instance_id`) is recommended for shared state.
* **TLS and SASL.** `KafkaSecurity { tls: Some(KafkaTls { ca_pem,
  server_name }), sasl: Some(KafkaSasl { mechanism, .. }) }` gives TLS
  through rustls (aws-lc-rs provider) and SASL PLAIN, SCRAM-SHA-256 or
  SCRAM-SHA-512 through rsasl, via SaslHandshake v1 and
  SaslAuthenticate v1. A bad password gives "sasl authentication failed".
* **Compression.** `KafkaCompression::{None, Gzip, Snappy, Lz4, Zstd}`.
  Snappy uses xerial framing as the Java client does, and raw snappy is
  also decoded.
* **Dead letters.** `KafkaSourceLimits::dead_letter: Option<String>` sends
  a malformed record to a dead-letter topic instead of failing the
  partition. The record is keyed `topic/partition/offset` with the headers
  `vesper-flux-error` and `vesper-flux-source`, and is counted in
  `KafkaSourceStats::dead_lettered`. A gap in the LSN sequence is still an
  error.

### 3. Per-bin live migration, runtime rescale, partial recovery in the window (`src/cluster.rs`)

* **Bins.** Keys hash into `BINS = 256` bins, and a layout gives every bin
  an owner by rendezvous hashing. Going from n to m workers moves only the
  bins whose winner changes. The test shows 3→4 moving about a quarter of
  the bins, where `key % n` moved almost all of them.
* **Megaphone-style migration.** Once the barrier commit is announced:
  * Each old owner sends each new owner (`Migrate`) only the rows of the
    bins moving between those two workers, and drops them.
  * A new owner keeps its own remaining rows, adds what it receives, checks
    that every received row belongs to a bin moving from that sender to
    itself, rebuilds its derived integrals locally, checkpoints and reports.
  * A worker whose bins do not change switches layouts as soon as it has
    snapshotted the barrier. It never waits and writes no new checkpoint;
    the new global manifest reuses its entry from the barrier commit,
    including unaligned channel state. The manifest records the sender
    count of that channel state.
* **Gradual migration.** With `ClusterConfig::migration_bins_per_step = k`,
  a rescale runs as steps at consecutive barriers, each moving at most k
  bins.
  * Bins are grouped by (old owner, new owner), so a step disturbs few
    workers.
  * Every step is its own reconfiguration with its own commit.
  * `LiveMigration` reports `rescale`, `step`, `steps_total`, `bins_moved`
    and `untouched` for each step.
* **Rescale at run time.** `ClusterConfig::rescale_requests:
  Vec<RescaleRequest { at_step, workers }>`.
  * Workers read source epochs only up to a lease: two checkpoint intervals
    past the last commit they know of.
  * At its next commit, the coordinator picks the first barrier at or past
    the lease granted before that commit (and past any scheduled step).
    It records the decision in the global manifest (`VFLUXGM3`, which also
    stores the bin table).
  * So no worker has read past the barrier under the old layout, and the
    decision survives a coordinator failover and a restart.
  * A request stays queued until a commit records it. A request that
    changes nothing is dropped and counted (`ClusterRun::dropped_requests`).
* **Faults inside the window.** A worker or coordinator fault between the
  barrier commit and the new-layout commit no longer escalates to a
  whole-cluster recovery. `Recovery::escalated` became
  `Recovery::in_reconfiguration`.
  * A replaced worker restores the barrier commit. The reconfiguration
    notice is implied for it, so it redoes its part.
  * Peers resend migrated bins from upstream backup, since `Migrate` is now
    kept until a later commit, even by a retired worker. Receivers accept a
    resent part only if it equals the original.
  * A replaced coordinator announces the barrier again, and workers that
    already migrated resend their report.
  * Only a whole-cluster crash in the window still rescales offline.

### 4. Lean (`lean/FluxDbsp.lean`, Lean 4.35.0)

New theorems, each with `#print axioms` audited by `tests/lean_model.rs`:

* `untouched_worker_keeps_state`
* `bin_step_assembles_owner`
* `gradual_steps_preserve_global`, over `reroute` and `migrate`
* `visible_append`
* `aborted_is_invisible`
* `transactional_resume_exactly_once`

They are about the models only. Routings are functions, states are
`α → Int`, and a partition is a list of (committed?, records)
transactions read by a `read_committed` reader. They are not theorems about
the Rust code, the wire protocol or a broker. There is no `sorry`, `admit`
or new axiom.

## Checks

Commands were run with `CARGO_BUILD_JOBS=3 CARGO_PROFILE_DEV_DEBUG=0
CARGO_INCREMENTAL=0`. Results are filled in below as they finish.

* **Broker interop.** Command: `VESPER_KAFKA_HOME=<Apache Kafka 4.3.2,
  Scala 2.13> VESPER_KAFKA_REQUIRE=1 cargo test --locked --release -p
  vesper-flux --test kafka -- --test-threads=2`. The broker is Apache Kafka
  4.3.2 (KRaft, single node) on OpenJDK. The 11 tests are:
  * round trip over TLS with SCRAM-SHA-512 and zstd
  * Flux crash and restart with publish faults
  * broker SIGKILL inside a transaction
  * zombie fencing and delayed or duplicated in-flight produce requests
  * progress never duplicates
  * every TLS and SASL mechanism, plus a wrong password
  * compression interop with rskafka
  * release deletes only committed records
  * malformed records, gaps and index recovery
  * dead-letter topic
  * consumer-group rebalance at checkpoints (two members, four partitions)

  Every test checks that no epoch is lost or duplicated.
* **Flux suite.** `cargo test -p vesper-flux --locked -- --test-threads=2`
  (debug): every target passes. `live_rescale` has 12 tests (about 90 s in
  debug), including:
  * per-bin minimal movement
  * gradual migration equal to a fixed-parallelism run, in both checkpoint
    modes
  * worker, coordinator and whole-cluster faults at every scheduler step of
    single-step and gradual rescales: no offline rescale except for
    whole-cluster crashes
  * runtime requests equal to a fixed-parallelism run
  * worker and coordinator faults at every step around a runtime request
* **Lean.** `VESPER_LEAN=<lean 4.35.0> cargo test -p vesper-flux --test
  lean_model` passes.
* **Workspace.** `cargo fmt --all -- --check`, `cargo +1.100.0 clippy
  --workspace --locked --all-targets -- -D warnings` and `cargo test
  --workspace --locked` are recorded in the PR.

## Decisions

* **Own wire client** instead of rskafka; see §1. rskafka stays as an
  independent second client in the tests.
* **Transactions per epoch, with Flux progress stored as a consumer-group
  offset** inside the same transaction (KIP-98/447 style). This avoids a
  second store and makes "published through epoch e" atomic with the
  records.
* **Rendezvous bins** (256), with the bin table in the global manifest, so
  layouts are pure functions of the schedule and a restore can check that
  every row is on its owner.
* **Leases** for runtime rescales instead of a stop-the-world barrier
  announcement. A worker never blocks on the lease while commits keep
  coming.
* **Format bump.** Global manifests are now `VFLUXGM3`; `VFLUXGM2`
  manifests from older runs are rejected with a typed error, not migrated.

## Remaining gaps

* Consumer groups do not move Flux operator state between group members.
* Transactions use the classic (TV1) protocol; the KIP-890 TV2 protocol is
  not used.
* SASL OAUTHBEARER, GSSAPI and mutual TLS are not implemented.
* The cluster is still the deterministic SimPulse model on one host. Its
  "workers" are tasks, not processes, and the shared schedule stands in for
  a decision carried with the commit notice.
* A whole-cluster crash inside a reconfiguration window still rescales
  offline.
* `zstd` builds the C zstd library through `zstd-sys`. Flux itself has no
  `unsafe`.
