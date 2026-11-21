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

### What is left

- **Fusion part 2**: the rest of the crate is Rust because it evaluates
  through the Rust seeds of `vesper-tensor` (lane 11) and
  `vesper-resonance` (lane 9). Once those flip, `tensor.rs`, `dsp.rs` and
  the witness checker move to the Vesper package and the crate becomes thin
  wrappers.
- **Beacon-arrow**: Arrow IPC framing and buffers, OTAP/OTLP, gRPC on the
  Prism HTTP/2 port with protobuf through the Grammar port, zstd through its
  C library.
- **Migrate**: the C lifter on a Vesper C reader and the Rust lifter on a
  Vesper Rust reader for the subset `vesper migrate` accepts.
- The census row (`scripts/rust-census.py`) is updated once plan PR #1415,
  which adds it, has merged.
