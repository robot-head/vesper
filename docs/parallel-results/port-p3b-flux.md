# Port wave P3b lane 7: vesper-flux, part 1 (the Z-set kernels and the frontier)

Claim [#1069](https://github.com/robot-head/vesper/issues/1069). Base `main` at
8212ddf (2026-11-18). Plan: `docs/agent-prompts/port-p3b/07-flux.md` (plan PR
#1072).

This is the first part of the lane. It ports the arithmetic every other Flux
module runs on: Z-set consolidation, sums, the `Filter`, `Map` and `Join`
operators of the DBSP circuit, and the frontier hold table. The CDC decoder,
the input log, checkpoint encoding, the Kafka wire codec and group protocol
and the cluster driver follow in later parts (see "Not ported yet"); in this
part they run in Rust on top of the ported kernels, so every test file of the
crate already exercises the port.

## Builds

`vesper-flux` is outside the compiler closure (`cargo tree -i vesper-flux -e
normal,build` does not reach `vesper`) and has no dependents in the
workspace, so it needs no seed. The port first landed as a non-default
`vesper-port` feature next to the Rust build. Because it is faster on every
workload measured below, the owner made it the default and asked for the
ported Rust code to be deleted (2026-11-18), as with `vesper-aura`:

| Build | How | Status |
|---|---|---|
| Vesper | always: `build.rs` compiles the Loom package `vesper/` (library `flux`) and checks `port/binding.rs` | the only build of the kernels |
| Divergent | `--features vesper-port-divergent` (one kernel at a time with `VESPER_FLUX_DIVERGE=<kernel>`) | CI only |

The Rust `BTreeMap` Z-set, its consolidation, sum, filter, map and join
loops and the Rust frontier slot table are deleted. `ZSet` is now the flat
cells the kernels read and write. `Accumulator` stays in Rust for the
modules not ported yet (CDC, Kafka, durable state), which build their sets
with it; those modules stay Rust until their own parts are ported.

## API inventory (re-measured before porting)

| Rust file | Lines | Public API | This part |
|---|--:|---|---|
| `zset.rs` | 116 | `ZSet` (`empty`, `consolidate`, `width`, `len`, `is_empty`, `entries`, `plus`), `Row` | all of it, plus the crate-internal `union` |
| `frontier.rs` | 183 | `Frontier` (`new`, `watermark`, `live_holds`, `acquire`, `fork`, `advance`, `retire`), `FrontierTicket`, `TicketError` | all of it |
| `circuit.rs` | 784 | `CircuitBuilder`, `Circuit` (`batch`, `executor`, counts), `Executor` (`tick`, `materialized`, ...), `Predicate`, `Field`, `NodeId`, `TickInput` | the operators (`apply`: `Filter`, `Map`, `Join`, `Union`, `Exchange`) and every sum (`copy`, `Sum3`, integration); the builder, the differentiation and the staging loop stay Rust |
| `cdc.rs` | 251 | `CdcHeader`, `CdcPolicy`, `SourceBinding`, `WeightPolicy`, `Ingested`, `Executor::{bind_sources, encode_cdc_record, ingest_cdc}` | later part |
| `log.rs` | 433 | `InputLog`, `LogLimits`, `LogRecovery` | later part |
| `checkpoint.rs`, `durable.rs` | 1,347 | `CheckpointLimits`, `Executor::{checkpoint, restore_checkpoint}`, `DurableCheckpointer` and its reports | later part (encoding); the file-backed store stays Rust |
| `kafka_wire.rs`, `kafka_group.rs` | 2,229 | the Kafka protocol codec and the consumer-group state machine (crate-internal) | later parts |
| `cluster.rs` | 3,303 | `run_cluster`, `run_cluster_with`, `ClusterConfig`, `ClusterRun`, faults, rescaling | later part (`in dyn EpochSource`, `inout dyn CommitSink`) |
| `kafka.rs`, `kafka_client.rs`, `sink.rs` | 3,203 | Kafka sources and sinks over rskafka/tokio/rustls/rsasl, `QuarrySink`, `MemorySink` | stay Rust (lane plan), except the pure parts of `kafka.rs` in a later part |

## What was ported

The Loom package `@vesper/flux` (`crates/vesper-flux/vesper`, 1,058 lines, 11
exported functions, export ABI v3):

| File | Contents |
|---|---|
| `lib.ves` | `ZErr` and `FrErr`; the kernels `zs_consolidate`, `zs_union3`, `zs_copy_check`, `zs_filter`, `zs_map`, `zs_join`; the frontier table `frontier_acquire`, `frontier_valid`, `frontier_advance`, `frontier_retire`, `frontier_live_holds` |
| `util.ves` | cell reads, lexicographic row comparison, binary search, the affordable-charge search |
| `sort.ves` | stable bottom-up merge sorts of entry numbers (by row, by key column) and a heap sort of words |
| `wide.ves` | the `i128` accumulator as a two-word value, and the `i64` product check |

**Representation.** The Vesper build keeps a `ZSet` as one flat `Vec<i64>`:
each row's columns, then its weight, rows strictly ascending. Kernels borrow
the cells in place (`in Slice[i64]`, no copy) and return the new set's cells
as one `ChunkSeq[i64]` (one `memcpy` into a `Vec`). The Rust build keeps its
`BTreeMap<Row, i64>`. The crate-internal code that read `ZSet::rows` directly
(`cluster.rs`, `durable.rs`, `checkpoint.rs`, `log.rs`) now goes through two
representation-neutral helpers, `ZSet::from_rows` and `ZSet::iter`, which are
moves and borrows in the Rust build (no extra work). `entries()` must lend
`&Row`, so the Vesper build materializes the rows once, on the first call
(`OnceLock`); the crate's own hot paths use `iter()`. `Debug`, `Clone` and
`PartialEq` print and compare exactly as the derived Rust ones.

**Exact semantics, different algorithm.** The Rust accumulator inserts rows
into a `BTreeMap` one at a time, charging work per insertion and failing on
the first row over `max_rows`. The kernels merge sorted inputs (sums), sort
once (consolidation, `Map`), or index the right input by its key (`Join`,
whose output is already in order), and reproduce the Rust behavior exactly:

- the same work charges, entry by entry (`1 + width` per insertion, the
  `Filter`/`Map` probe charges, the join's product and per-match charges);
- the same `rows` limit, counted over distinct rows in insertion order,
  including rows whose weights later cancel;
- the same first error when several limits fail: when a limit is known to
  fail, the kernel finds the failing entry (the last affordable charge, the
  insertion time of the `(max_rows + 1)`-th distinct row) and reports
  whichever the Rust loop would hit first; a final `i64` overflow comes only
  after every insertion succeeded, and a later row of the wrong width
  (`Schema`) comes before it;
- the `i128` sums (two words) and the `i64` range check of `finish`.

**Frontier.** The hold table is three columns (`gens`, `holds`, `live`) the
port updates in place (`inout ChunkSeq`); the Rust wrapper keeps the
tracker's origin and checks it first, as `validate` does.

**Unsafe code.** None in the port or its adapters (`src/port.rs`); the crate
is `forbid(unsafe_code)` in the Rust build and `deny(unsafe_code)` in the
Vesper build, where only the generated binding (`port/binding.rs`) allows it.

## What stays Rust and why

| Code | File | Why | Unblocked by |
|---|---|---|---|
| Circuit builder, differentiation, staging loop, cluster planner | `circuit.rs` | small control code over the kernels; ported with the cluster driver | part 3 |
| CDC, input log, checkpoint and durable encoding | `cdc.rs`, `log.rs`, `checkpoint.rs`, `durable.rs` | byte codecs, next part | part 2 |
| Kafka wire codec and group protocol | `kafka_wire.rs`, `kafka_group.rs` | next parts; CRC32C as a table or a `.vsys` call, measured both ways | part 2 |
| Cluster driver | `cluster.rs` | `in dyn EpochSource` / `inout dyn CommitSink` work today (F7b); `Rc<dyn GroupOffsetSource>` becomes a borrowed object | part 3 |
| The `Accumulator` used by CDC, Kafka and durable restore | `zset.rs` | it builds sets from decoded records; it goes with those decoders | parts 2 and 3 |
| Compression, BLAKE3, TLS/SASL, the rskafka/tokio client, the file store | `kafka_client.rs`, `durable.rs`, `sink.rs` | lane plan: Rust behind stateless `.vsys` calls or kept outright | not planned |

## Unit tests

`frontier.rs` has the crate's only in-module test that reaches private state
of a ported type (`stale_and_generation_exhaustion_preserve_holds` sets a
slot's generation to `u64::MAX`). It now sets `gens[slot]` instead of
`slots[slot].generation`. The in-module tests of `kafka.rs` and
`kafka_wire.rs` cover code this part does not port and run unchanged.

## Divergence checks

- **Differential test** (`tests/kernels.rs`, new): 4,000 seeded
  consolidations and sums, 1,500 random circuits (filters, maps, unions,
  joins) run for four ticks plus the batch evaluator, and 300 random frontier
  sessions, all under tight random limits with weights at the `i64` edges,
  zero weights, repeated rows, rows of the wrong width and exhausted budgets.
  Every result and error is printed with `Debug` into one FNV-1a digest per
  test, recorded from the Rust build; the Vesper build must match it byte for
  byte. Of the 17,701 circuit values, 6,201 are nonempty sets and the rest
  cover every error the kernels raise (`Overflow`, `rows`, `output`, `work`,
  `join product`). `VESPER_FLUX_KERNELS_DUMP=<dir>` writes every value, so
  two builds can be diffed; this found one ordering bug during the port (an
  `Overflow` reported before a later `Schema`).
- **Divergent kernels** (`build.rs`, `BUILD.bazel`): one wrong line per
  kernel (`union` drops the second set's weights, `filter` makes `AtLeast`
  strict, `consolidate` keeps zero sums, `join` is off by one on the product
  limit, `frontier` publishes 0 for an empty table).
  `crates/vesper-flux/port/check.sh` builds each one and asserts that a named
  differential test fails while another passes; Bazel builds every divergent
  object.

## Tests

All ran locally on Linux x86_64 against both builds before the Rust kernels
were deleted, and again on the Vesper build afterwards: `checkpoints` (12), `cluster` (6), `dbsp` (10),
`durable` (9), `kernels` (3), `lean_model` (2), `live_rescale` (12),
`platform_recovery` (3), `recovery` (8), `rescale` (4), `sources_sinks` (5),
the library's in-module tests (9) and the examples. `kafka` (11) needs a broker
(`VESPER_KAFKA_HOME`) and skips without one; its Rust edges
run over the ported kernels when `.github/workflows/flux-kafka.yml` runs it.

`tests/kernels.rs` checks digests of every kernel's output against goldens
recorded from the deleted Rust build, so the Rust behavior stays pinned.

CI: Bazel builds the port and the divergent objects and runs every test file
on the port; the Linux "Vesper ports" shard 3 runs `port/check.sh`; macOS and
Windows build the crate, which now compiles the port there too.

## Performance

Measured before the Rust kernels were deleted: a script built
`examples/abba.rs` both ways (release) and drives each workload with `vesper-bench`'s ABBA runner (12
measured epochs x A/B/B/A x 4 requests, each arm a worker process); both
builds' digests agree on every batch. Linux x86_64 cloud VM, 4 vCPUs,
2026-11-18; wall clock on these hosts varies by up to 30%, so callgrind
instruction counts (one batch, including process start) are given too.

| Workload | Rust | Vesper | Vesper/Rust wall (95% interval) | Instructions Rust | Vesper | Ratio |
|---|--:|--:|--:|--:|--:|--:|
| `join`: filtered, mapped equijoin, 32 ticks of 64-row deltas over 256 keys | 2,150 µs/tick | 216 µs/tick | 0.10x (0.092-0.111x) | 588.4 M | 71.9 M | 0.12x |
| `example`: the `incremental_join` example, 64 runs | 4.0 µs/tick | 3.4 µs/tick | 0.83x (0.66-1.18x) | 6.79 M | 5.80 M | 0.86x |
| `resume`: `checkpoint_resume`'s shared join over CDC, 16 ticks of 48 records, checkpoint and restore half way | 316 µs/tick | 56 µs/tick | 0.18x (0.173-0.185x) | 55.0 M | 9.0 M | 0.16x |

The gain is the data layout, not the language: 59% of the Rust build's
instructions in `join` are `Accumulator::add` and `BTreeMap` inserts, which
the kernels replace with merges over flat cells. In the Vesper build, 56% of
`join` is `zs_union3` (the integrals), 11% `zs_join`, and about 10% `malloc`,
`free` and `memcpy` of result sequences. On tiny sets (`example`) the two
builds are within noise.

## Compiler gaps found

None blocking. Two notes for the compiler lanes:

1. `mapped[..]` as an `in` argument is a parse error (`expected expression`);
   `in mapped.as_slice()` works. Repro: `fn f(in s: Slice[i64]) -> u64 { s.len() }
   fn g() -> u64 { let q = ChunkSeq::new[i64](); f(in q[..]) }`.
2. Exports come only from the root module, so a package that keeps helpers in
   submodules must declare its exported functions in `lib.ves` (here the
   frontier functions moved into `lib.ves` for that reason).

## Not ported yet (next parts)

- Part 2: `cdc.rs`, `log.rs`, the checkpoint encoding of `checkpoint.rs` and
  `durable.rs`, `kafka_wire.rs` (CRC32C table vs `.vsys`, measured both
  ways), with ABBA on the Kafka wire codec.
- Part 3: `kafka_group.rs` and `cluster.rs` (`in dyn EpochSource`, `inout dyn
  CommitSink`; `Rc<dyn GroupOffsetSource>` in `kafka.rs` becomes a borrowed
  object), the builder and differentiation of `circuit.rs`.
