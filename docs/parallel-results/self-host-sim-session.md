# Self-hosting SH5 lane 19: `vesper-sim-session` in Vesper

Part 1 (#1357, merged as `b647460`) ported the datagram link model. Part 2
(below the part 1 sections) ports SHA-256 and the WAL frame codec and
recovery scan.

## Part 1: the datagram link model

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

## Part 2: SHA-256 and the write-ahead log codec

Base: `main` at `b647460`.

### What changed

- `port/src/sha256.ves` is SHA-256 (FIPS 180-4). It holds 32-bit words in
  `u64` and masks them, so every step is total. Under `vesper-port`,
  `program::sha256_hex` forwards to it. That covers every digest the crate
  makes: the source, scenario, bindings and options digests of the
  execution identity, the executable digest, and the durable-log digests in
  model snapshots and events.
- `port/src/wal.ves` is the `wal-frame-v1` codec. `wal_frame` builds a frame
  (`[len u32 LE][lsn u64 LE][8-byte SHA-256 check][payload]`). `wal_scan` is
  the recovery policy. It accepts the longest prefix of complete frames whose
  checks match, whose payloads are UTF-8 and whose LSNs run 1, 2, 3, ..., and
  it reports the first damage. Its UTF-8 check follows Rust's
  `str::from_utf8` (no overlong forms, no surrogates, at most U+10FFFF).
  `WalRecord::frame` and `wal::scan` forward to it. The `Wal` state machine
  (regions, flushes, crash clamping) stays in Rust and calls the codec.
- The binding module moved from `model::link::port` to `crate::port`
  (`src/port.rs`), because the link, the WAL and the digests all share it.
- The transcript differential now also hashes each run's final model
  snapshot, which includes the durable bytes' SHA-256. It also adds a
  `wal_codec` case: frames of every payload length from 0 to 200 bytes,
  which crosses both SHA-256 padding boundaries, then 400 cut mediums, a
  flipped bit in each frame region, out-of-order LSNs, and 8 UTF-8 edge
  cases. The digests were reblessed from the Rust build.
- The divergence check now takes one edit per module, picked with
  `VESPER_SIM_DIVERGE`:
  - `link`: a free buffer slot tail-drops. This changes the 4 Warp
    transcripts and fails the link unit test.
  - `wal`: surrogates are accepted. The Rust wrapper's UTF-8 assertion
    catches it in the `wal_codec` case.

### Results

| Check | Rust | Vesper |
|---|---|---|
| Crate tests | 65 pass | 65 pass |
| Transcript differential (9 cases, now with model snapshots) | blessed | **0 differences** |

| Measure (release, best of 5) | Rust | Vesper | Ratio |
|---|---|---|---|
| SHA-256, 1 MiB | 0.83 ns/byte | 12.06 ns/byte | 14.5x |
| WAL scan, 10,000 frames of 193 bytes | 322 ns/frame | 2,660 ns/frame | 8.3x |
| Link (part 1 benchmark, rerun) | 47.6 ns | 60.3 ns | 1.27x |

The Rust `sha2` crate uses the CPU's SHA extensions where present, so the
gap is mostly hardware against scalar code. The Vesper rounds also read and
write the message schedule through checked `ChunkSeq` accesses, because a
mutable local fixed array was not tried. The scan is dominated by the
per-frame check, which hashes each payload again. Each process hashes its own
executable once for the identity, and through the port that is about 0.6 s
for a 50 MB binary. Rust stays the default build under the parity rule.

### More compiler gaps

4. **Variable shift amounts raise.** `x >> n` and `x << n` with a
   non-constant `n` carry `Fail[ArithOverflow]` even when `n < 32` is known.
   The rotate uses `wrapping_shr_u64` and `wrapping_shl_u64`. Repro:
   `fn rotr(x: u64, n: u64) -> u64 { (x >> n) | (x << wrapping_sub(32, n)) }`
   gives E0401.
5. **Array bounds need a literal.** `if i < ROUNDS { K[i] }` with
   `const ROUNDS: u64 = 64` is E0401. `if i < 64 { K[i] }` is accepted.
6. **Types from a submodule get mangled names in the binding.**
   `ScanResult` in `wal.ves` becomes `vesper__sim_2session__wal__ScanResult`,
   and `scan_result_*` functions do not become methods. Declaring the type in
   the root module avoids this.

## What is left (later parts of #1342)

- `model/net.rs` (`net-v1`). It is mostly ordered maps of message records
  whose payloads are strings. The plan is a token slab: Vesper keeps the ids,
  reservations, partitions and FIFO order, and Rust keeps the payloads.
- The `Wal` state machine itself (part 2 ported only its codec), and a
  faster SHA-256.
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
