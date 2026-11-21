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
  `(input, row)`, and sorted once per input at `finish`. The Rust wrapper
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
| accumulator + CDC decoder (`ingest` workload) | 0 differences over 20,000 accumulator cases and 20,000 CDC batches | all 14 test binaries of the crate pass | **0.49x** (0.373x..0.668x) | flipped |
| `resume` workload (CDC, checkpoint, restore) | digests agree over 104 batches | | 0.73x (0.693x..1.003x) | no regression |

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

## Compiler gaps

- `let s = seq.as_slice();` is refused (E1521, a slice is a second-class
  borrow). Every slice must be written at the call site
  (`f(in seq.as_slice(), ...)`). Not a blocker; it only makes loops that read
  one slice many times wordier.

## What is left

Flux (Rust still in `src/`):

| Code | Lines | Plan |
|---|--:|---|
| `log.rs` (input log frames) | 430 | next part: frame encode/decode and scan in Vesper over `@vesper-std/hash` (BLAKE3) and `@vesper-std/fs` |
| `checkpoint.rs`, `durable.rs` (checkpoint and spine encoding) | 1,347 | next parts |
| `circuit.rs` (builder, differentiation, staging) | 741 | with the cluster driver |
| `cluster.rs` (multi-worker driver) | 3,314 | later part (`in dyn EpochSource`, `inout dyn CommitSink`) |
| `kafka_wire.rs`, `kafka_group.rs` | 2,229 | later parts; CRC-32C and the codecs (zlib, LZ4, Snappy, zstd) are C libraries reachable through `.vsys` |
| `kafka.rs`, `kafka_client.rs`, `sink.rs` | 3,203 | sockets need lane 13's libc imports; TLS and SASL need lane 17's crypto shim |
| `cdc.rs` header codec and policy checks | ~150 | small; with the Kafka decoders that share them |

Quarry: the planner, DDL and row executor are ported (non-default
`vesper-port`, last measured at 1.80x wall, 1.59x instructions in
`port-parity.md`). The store, WAL, pages, buffer pool, device I/O, the
vectorized executor and the durable layer are Rust. Next: re-measure the
planner on current `main`, then close the gap to 1.5x and flip it.

The census row (`scripts/rust-census.py`) is updated once the census lands
on `main` with plan PR #1415.
