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
  `@vesper-std/hash-sys` (the `.vsys` library `Blake3`, whose Rust side
  `packages/vesper-std/hash/sys/shim.rs` the crate links, as `vesper-fmt`
  does). `log.rs` keeps the file: create, open, scan, append, read and the
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
