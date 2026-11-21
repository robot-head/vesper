# Self-hosting SH5 lane 19: `vesper-sim-session` in Vesper, part 1

Claim: #1342 (part 1). Base: `main` at `bbda89e`. Prompt:
`docs/agent-prompts/self-host-sh5/19-sim-session.md`.

Rust stays the default build. The port is behind the crate's new
`vesper-port` feature, and nothing depends on it.

## What changed

- `crates/vesper-sim-session/port/` is a Loom package, `@vesper/sim-session`
  (346 lines). It holds the datagram link model `datagram-v1` from
  `src/model/link.rs`: the SplitMix64 generator, the FNV-1a name seeds, the
  Gilbert–Elliott chain with its stationary rates, independent loss, the
  finite bottleneck queue (its departure times are a `ChunkSeq[u64]` with a
  head index, compacted once 256 served entries pile up) and reordering.
  Every function is total. A virtual-clock overflow comes back as an outcome,
  never as a raise.
- `build.rs` and the `vesper-port` and `vesper-port-divergent` features:
  under `vesper-port`, `Link`, `SplitMix64::{next_u64, chance}`, `name_seed`
  and `GilbertElliott::{stationary_bad_ppm, stationary_loss_ppm}` forward to
  the port through the committed binding (`port/binding.rs`, export ABI v2).
  The public API is unchanged. `Link` keeps `Clone`, `PartialEq` and `Debug`
  through `link_state_clone` and `link_state_same`.
- `tests/self_host_sim_session.rs` is the transcript differential. It records
  `pipeline`, `kv_ack_first`, `warp_wan` and `warp_ack`, and the two Warp
  fixtures again under a second seed. For each run it hashes everything the
  run produced except the execution identity: the SimPulse journal, the source
  and model traces, the outputs, the durable bytes, the lost tasks and the
  virtual time. It compares each hash with `fixtures/transcripts.txt`, which
  was blessed from the Rust build. The executable digest in the identity
  differs between any two binaries, so it is left out.
- `port/test-port.sh` runs the port build and the divergence check. It is
  appended to shard 7 of `scripts/test-port-harness.sh`.

The session driver stays in Rust and drives the compiler through its existing
entry points (`parser::parse`, `check::check` and the reference CEK
`Runtime`). So do the scenario JSON, the identity hashing and the other
models.

## Results

| Check | Rust build | Vesper build |
|---|---|---|
| Crate tests (`cargo test -p vesper-sim-session`) | 65 pass | 65 pass |
| Transcript differential, 8 runs | blessed | **0 differences** |
| Divergent port (a free buffer slot tail-drops) | n/a | fails `micro_burst_builds_queue_delay_then_tail_drops`, 4 Warp transcripts differ, and `warp::egress_follows_launch_order...` fails |

### Speed (release, x86_64 Linux, 4 vCPU)

| Measure | Rust | Vesper | Ratio |
|---|---|---|---|
| `Link` alone: duplicate draw + transmit, GE + loss + reorder + queue, 10^6 datagrams, best of 5 (`link_throughput`, ignored test) | 45.0 ns | 52.6 ns | 1.17x |
| Whole `warp_wan` session (3,600 datagrams) | 7.13 s | 6.86 s | about 1.0x (noise) |
| Whole `warp_ack` session | 1.88 s | 1.89 s | 1.0x |

Both builds give the same checksum on the throughput run. A session's time is
almost all the reference evaluator, so the link port does not move it.

## Stage 1 guard

This PR does not touch `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`. It edits nothing in the root crate, so the
verified artifacts cannot change. The bootstrap-stage1 workflow
(`bootstrap/run.sh stage1 --reproduce --outcome` against
`bootstrap/stage1.lock`) runs on the branch before landing.

## Compiler gaps

1. **`while` is not a statement.** `while i < n { .. }` (and `while lo != hi`)
   fails with E0002 "expected RBrace, found Lower". Loops are `for i in a..b`
   or `loop { if .. { break; } .. }`. SH5 lane 4 (#1327) lists
   `while k < n {` as a parser gap. The minimal repro is
   `fn f(n: u64) -> u64 { let mut i = 0; while i < n { i = wrapping_add(i, 1); } i }`.
2. **No `as` casts.** `x as u64` is E0002. Widening is `widen_u64(x)`.
3. **No `insert` or `drain(..k)` on `ChunkSeq`.** The sorted insert shifts
   with a `try_set` loop, and front removal is a head index plus compaction.
   `VecDeque::partition_point` is a hand-written binary search.

## What is left (later parts of #1342)

- `model/net.rs` (`net-v1`) and `model/wal.rs` (`wal-frame-v1`). The WAL
  hashes frames with SHA-256, and `@vesper/hash` has only BLAKE3, so it needs
  a SHA-256 `.vsys` import or a Vesper SHA-256.
- `model/umem.rs` and `model/warp.rs` (3,100 lines, over the
  `vesper-warp-umem` crate).
- `model/mod.rs` process epochs, scenario validation, the canonical-JSON
  identity digests and `value.rs`. These need a JSON value model on the
  Vesper side, or the Rust side has to pass canonical strings in.
- The session driver itself, which steps the compiler's reference evaluator,
  waits until the evaluator is reachable from Vesper (after the SH4 swap
  points).
- A Bazel target for the port build. Linux Bazel builds and tests the Rust
  build. The port runs under Cargo in port-harness shard 7.
