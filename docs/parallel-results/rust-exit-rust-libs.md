# Rust-exit SH6 lane 18: replace the Rust-only libraries

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1413. Prompt:
[`docs/agent-prompts/self-host-sh6/18-rust-libs.md`](../agent-prompts/self-host-sh6/18-rust-libs.md).
Crates: `vesper-fusion` (egglog), `vesper-beacon-arrow` (arrow-rs, tonic,
prost) and `vesper-migrate` (lang-c, syn).

## Part 1: `vesper-fusion` runs on a Vesper engine instead of egglog

Base: `main` at `ab965f2`.

| Area | Files |
| --- | --- |
| Engine (Vesper) | `crates/vesper-fusion/vesper/{Loom.toml,src/*.ves}` |
| Binding, build | `crates/vesper-fusion/port/binding.rs`, `build.rs`, `Cargo.toml`, `BUILD.bazel` |
| Rust side | `src/engine.rs` (decodes the engine's words), `src/witness.rs` (the egglog proof producer is gone; the checker is unchanged) |
| Tests | `tests/egglog_differential.rs`; `port/test-port.sh` (one divergence check), its line in `scripts/test-port-harness.sh` |

The engine is the part of egglog 3 the crate runs, in Vesper: it parses the
same program text (`sort`, `constructor` with `:cost`, `rewrite`, `let`),
builds the bindings as e-nodes, saturates for `ITERATIONS` rounds the way
egglog's `(run n)` does (search every rule on the rebuilt graph, apply every
match, rebuild), and extracts with egglog's tree-cost extractor
(Bellman-Ford over the constructors in egglog's breadth-first order from the
root sort, the topological rank that keeps extraction acyclic). Instead of
asking egglog for a proof and converting it, the engine keeps egg-style
explanations: every distinct term is an e-node id, unions build a proof
forest whose edges are justified by a rule application (rule, instantiated
sides, substitution) or by congruence, and `explain(a, b)` walks the forest
path, one step per rule edge and one level deeper per congruence edge. The
steps go to the unchanged Rust `witness::check`, which still rebuilds the
source term from the bindings and replays every step.

`@vesper/egraph` (SH3 lane 8) was not used: its e-nodes have at most two
children and one `i64` of data, its extraction is `AstSize`, and it keeps no
explanations. The fusion rule sets need four children (`RmsMatVec`), string
data, per-constructor costs and a proof of every extraction.

### Flip gate (engine)

Differential (`tests/egglog_differential.rs`, release, x86-64 Linux):
the shapes the crate's tests fuse, long chains, and 2,000 generated tensor
and DSP graphs (`FUSION_DIFF_CASES=1000`), each extracted with and without
saturation by both engines, the saturated ones also fused (explained and
checked).

| Extractions | Same arena and cost | Equal-cost tie | Other |
| --: | --: | --: | --: |
| 4,021 | 3,953 | 68 | **0** |

One generated DSP graph (13 stages, a long run of cascades) is over the
port's e-node budget (2M); egglog runs out of memory on it (above 10 GB), so
it is listed and not compared. All 17,334 witness steps of the fused cases
check. Every crate test passes on the engine (27 tests, plus the Lean model
check), and the divergent build (explanation steps with the wrong direction)
fails `every_shipped_rule_appears_in_a_checked_witness` while
`rule_sets_name_every_rewrite_with_a_lemma` passes.

Every tie is a DSP graph whose saturated class holds two terms of one cost
(a uniform gain folded into one of two identical biquad sections, or
`CScale (GMul g g) c` against `CScale g (CScale g c)`). egglog breaks such
ties in its tables' row order, which is not even the same between its own
proof and non-proof modes (2 of the first 10 ties). The owner accepted
equal-cost results for the e-graph ports on 2026-11-21, and here every term
the port extracts carries a checked witness. No tie occurs on the crate's
own test graphs.

ABBA (`speed_abba`, 11 epochs, egglog, Vesper, Vesper, egglog per epoch).
The egglog arm is egglog's part of the old `fuse` (the unsaturated
extraction, the saturated run with proofs, the `prove` query), without the
old proof-to-witness conversion; the Vesper arm is the unsaturated
extraction and the explained run. The Rust checker both versions run is
left out of both.

| Workload | egglog | Vesper | Ratio (95% interval) |
| --- | --: | --: | --- |
| the crate tests' graphs | 133 ms/case | 0.71 ms/case | 0.005x (0.005x..0.006x) |
| 40 generated tensor graphs, 24 nodes | 81 ms | 0.34 ms | 0.004x (0.004x..0.005x) |
| 40 generated DSP graphs, 12 stages | 109 ms | 3.0 ms | 0.027x (0.026x..0.028x) |

**Verdict: the engine passes the gate** (0 cost differences, ties accepted,
upper interval 0.028x). It is the only build: egglog left the crate's
dependencies (it stays a dev-dependency for the differential).

Rust removed: the egglog engine and its proof producer, 209 lines net in
`src/` (`witness.rs` 303 deleted, `engine.rs` replaces its egglog calls with
the decoder), and the crate no longer links egglog 3 and its eight
`egglog-*` crates (65k lines of Rust).

### Compiler gaps

None blocked the engine. Two small things cost a detour:

- `ChunkSeq` has no `clone`; the package copies with a loop (`copy32`,
  `copy64` in `util.ves`).
- A deep recursion over the proof forest runs on the caller's stack, so
  `fuse` keeps the old 1 GiB proof thread.

## Part 2: the Beacon span stream is written in Vesper

Base: `main` at `3a86df2e`.

| Area | Files |
| --- | --- |
| Writer (Vesper) | `crates/vesper-beacon-arrow/vesper/{Loom.toml,src/*.ves}` |
| Binding, build | `crates/vesper-beacon-arrow/port/binding.rs`, `build.rs`, `Cargo.toml`, `BUILD.bazel` |
| Rust side | `src/lib.rs`: `encode` fills the twelve columns and calls the writer |
| Tests | `tests/span_stream_differential.rs`; `port/test-port.sh` (one divergence check), its line in `scripts/test-port-harness.sh` |
| Shared guard | `crates/vesper-port-harness/{tests,bazel}/closure_guard.rs`: the positive control now also lists this crate's own port object |

`export_batch` and `export_ring` return a `vesper.beacon.spans/1` Arrow IPC
stream. The arrow-rs `StreamWriter` that wrote it is replaced by a Vesper
package: a FlatBuffers builder that lays bytes out exactly as the Rust
`flatbuffers` 25 builder does (downward growth, per-scalar alignment,
default fields left out, vtable reuse, root alignment), the Arrow `Schema`
and `RecordBatch` messages built in `arrow-ipc` 56's field order, and the
stream framing (continuation, 64-byte padded metadata, 64-byte padded
buffers, all-valid bitmaps, EOS). The builder and message modules are the
start of the OTAP writer in later parts. `ExportError::Arrow` became
`ExportError::Ipc`; `span_schema()` still describes the schema with
arrow-schema types (the OTAP modules use them).

### Flip gate (span stream)

Differential (`tests/span_stream_differential.rs`, release,
`BEACON_ARROW_DIFF_CASES=1000`): random rings of 0 to 4,096 rows (ids and
times at the edges of their domain, every schema, status, error code and
attribute kind), each exported as a borrowed batch, a ring prefix and the
rest, plus one 65,536-row ring, against the arrow-rs writer kept in the test
as the oracle.

| Streams | Bytes | Byte-identical | Other |
| --: | --: | --: | --: |
| 3,037 | 259 MB | 3,037 | **0** |

Every crate test passes on the port. The divergent build (every u64 column
shifted by one row) fails
`independent_arrow2_reader_compares_every_column_and_metadata` while
`tests::hard_row_limit_precedes_attribute_walk_and_allocations` passes.

ABBA (`speed_abba`, 21 epochs, arrow-rs, Vesper, Vesper, arrow-rs per
epoch). The arrow-rs arm is the old `export_batch` (plan, then the arrow-rs
encoder) over rows converted before timing; the Vesper arm is
`export_batch`.

| Workload | arrow-rs | Vesper | Ratio (95% interval) |
| --- | --: | --: | --- |
| 64 batches of 8 rows | 10.3 us/batch | 6.9 us/batch | 0.664x (0.654x..0.677x) |
| 32 batches of 256 rows | 17.3 us/batch | 11.8 us/batch | 0.677x (0.672x..0.686x) |
| 8 batches of 4,096 rows | 229 us/batch | 94 us/batch | 0.410x (0.405x..0.413x) |

**Verdict: the span stream passes the gate** (0 differences, upper interval
0.686x). It is the only build. The arrow crates stay dependencies until the
OTAP modules move.

### Compiler gaps (part 2)

None blocked the writer. Wide writes (`try_write_u32_le`) work on a
`ChunkSeq` receiver but not on `as_slice_mut()` of a field (E1521), so the
builder writes into its `ChunkSeq` directly.

## What is left

- **Fusion part 2**: the rest of the crate is Rust because it evaluates
  through the Rust seeds of `vesper-tensor` (lane 11) and
  `vesper-resonance` (lane 9). Once those flip, `tensor.rs`, `dsp.rs` and
  the witness checker move to the Vesper package and the crate becomes thin
  wrappers.
- **Beacon-arrow**, after part 2: the untrusted OTAP IPC reader with zstd
  through its C library, OTAP encode and decode (dictionaries, structs,
  lists), OTLP protobuf through the Grammar port, and gRPC on the Prism
  HTTP/2 port.
- **Migrate**: the C lifter on a Vesper C reader and the Rust lifter on a
  Vesper Rust reader for the subset `vesper migrate` accepts.
- The census row (`scripts/rust-census.py`) is updated once plan PR #1415,
  which adds it, has merged.
