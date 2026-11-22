# Self-hosting SH5 lane 19: `vesper-sim-session` in Vesper

Part 1 (#1357, merged as `b647460`) ported the datagram link model. Part 2
(#1380, merged as `97de64f`) ported SHA-256 and the WAL frame codec and
recovery scan. Part 3 (#1384, merged as `ce75623`) ported the virtual
message network `net-v1`. Part 4 (#1390, merged as `af7dd90`) ported the
write-ahead log state machine. Part 5 (#1416) made the Vesper SHA-256
faster. Part 6 ports the Warp QUIC wire codec.

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

## Part 3: the virtual message network

Base: `main` at `97de64f`.

### What changed

- The end of `port/src/lib.ves` is `net-v1` from `src/model/net.rs`. The
  Vesper `NetState` makes every decision: message ids, per-link delays,
  partitions, the send-time bounds (message size, in-flight count, the
  inbox reservation), the drop reason at send and at arrival, and the FIFO
  order of each endpoint's inbox and waiting receivers. Its tables are flat
  `ChunkSeq[u64]` words (delay triples, sorted partition pairs, in-flight
  `(id, from, to)` triples, and `(endpoint, value)` pairs for inboxes and
  waiters).
- This is the token-slab plan. The Rust `Network` wrapper keeps only the
  payloads, keyed by message id, so no string crosses the boundary. Send
  passes the payload's byte length, and the wrapper builds the `Message`
  from the id and delivery time that come back. `take_queued` returns
  message id 0 for an empty inbox (ids start at 1), and `crash` reads the
  inbox and waiters before clearing them, so only words and flat seqs cross.
- `Network`'s reference accessors (`endpoint`, `endpoints`, `in_flight`,
  `partitions`) became owned ones on both builds: `in_flight`,
  `in_flight_to`, `inboxes`, `waiting` and `partitions`. The model snapshot
  and the arrival handler were their only users. `Endpoint` is now private
  to the Rust build.
- The differential adds a `net` case: 4,000 pseudo-random sends, arrivals
  (some of unknown ids), receives, waits, timeouts, crashes, partitions and
  heals over four endpoints with tight bounds. After every step it records
  the network's whole state, then checks that a clone compares equal and
  crashes every endpoint of the copy.
- The `net` divergent edit lets an inbox take one message past its bound. It
  fails `bounds_are_checked_before_anything_is_retained` and changes the
  `net` transcript.

### Results

| Check | Rust | Vesper |
|---|---|---|
| Crate tests | all pass | all pass |
| Transcript differential (10 cases) | blessed | **0 differences** |

| Measure (release, best of 5) | Rust | Vesper | Ratio |
|---|---|---|---|
| `net_throughput`: 10^6 operations of the `net` mix | 414 ns/op | 455 ns/op | 1.10x |
| Link (rerun) | 46.5 ns | 65.7 ns | 1.41x |
| SHA-256 (rerun) | 0.82 ns/byte | 13.60 ns/byte | 16.6x |
| WAL scan (rerun) | 347 ns/frame | 4,449 ns/frame | 12.8x |

The `net` benchmark also formats a log line per operation on both builds,
which is shared work, so the network itself costs more than 1.10x. The
Vesper tables are linear: endpoint and partition lookups scan, and removing
from an inbox or the waiter list shifts the rest. That is fine at the
scenario bounds (tens of endpoints and messages) and is the first thing to
change for parity. The reruns of the part 1 and part 2 benchmarks came out
slower than before for both builds, so this machine was noisier than for
those parts.

### More compiler gaps

7. **`eq` on a `Slice` is bytes only.** `a.as_slice().eq(in b.as_slice())`
   on `ChunkSeq[u64]` is E1525 ("'eq' reads and writes bytes"). Comparing
   word sequences is a loop.

## Part 4: the write-ahead log state machine

Base: `main` at `ce75623`.

### What changed

- `WalState` at the end of `port/src/lib.ves` is the `Wal` of
  `src/model/wal.rs`. It owns:
  - the medium's bytes and LSN assignment,
  - the volatile and in-flight regions, kept as one queue of
    `(lsn, frame_len)` pairs whose front `in_flight` entries are the batch,
    with their frames built at append,
  - the record and byte bounds,
  - flush begin, completion and failure (a failed batch is already at the
    front of the volatile region),
  - recovery through the part 2 scan,
  - the crash rules: the persisted prefix is clamped to the batch, and a
    torn tail stays below a whole frame.
- The Rust `Wal` wrapper keeps each live record's payload, keyed by LSN,
  so it can return `WalRecord`s.
- `Wal`'s reference accessors became owned ones on both builds:
  - `durable_bytes` returns a `Cow`.
  - `volatile`, `in_flight` and `begin_flush` return owned values.
  - `view` is now `record(index)`, its only use.
- The differential adds a `wal_ops` case: 3,000 pseudo-random operations,
  each followed by the log's state (the medium's digest and length, the
  durable LSN, the recovered flag, the batch and the volatile records). The
  operations are appends (some too large or past the bounds), flush begins,
  completions and failures, reads, crashes with persisted prefixes and torn
  tails, and recoveries.
- The `log` divergent edit lets a torn tail cover the whole next frame. No
  unit test tears a whole frame, but the `wal_ops` transcript changes.

### Results

| Check | Rust | Vesper |
|---|---|---|
| Crate tests | all pass | all pass |
| Transcript differential (11 cases) | blessed | **0 differences** |

| Measure (release, best of 5) | Rust | Vesper | Ratio |
|---|---|---|---|
| `wal_ops_throughput`: 10^5 operations of the `wal_ops` mix | 742 ns/op | 4,152 ns/op | 5.6x |
| `net_throughput` (rerun) | 396 ns/op | 439 ns/op | 1.11x |
| Link (rerun) | 47.2 ns | 51.4 ns | 1.09x |
| SHA-256 (rerun) | 0.77 ns/byte | 11.91 ns/byte | 15.5x |
| WAL scan (rerun) | 327 ns/frame | 2,892 ns/frame | 8.8x |

The log is bound by SHA-256. Every frame carries a check, the port builds
each frame at append (Rust builds it at flush or crash, so records that
vanish before either are never hashed), and every recovery rescans the
medium. A faster SHA-256 is the first step toward parity. Rust stays the
default.

## Part 5: a faster SHA-256

Base: `main` at `af7dd90`.

`port/src/sha256.ves` now keeps its words as `u32`, using `wrapping_add`
and the total `rotate_right`, where it used to mask them in `u64`. The state
is a `[u32; 8]` value, and the message schedule is a local `[u32; 64]` array
in place of a `ChunkSeq[u64]` behind checked `try_get`/`try_set`. Each block
reads its 16 message words straight from the input slice.

| Measure (release, best of 5) | Rust | Vesper before | Vesper now |
|---|---|---|---|
| SHA-256, 1 MiB | 0.77 ns/byte (SHA-NI) | 11.91 ns/byte | **7.54 ns/byte** |
| SHA-256, 1 MiB, `sha2` with `force-soft` (scalar) | 7.28 ns/byte | | 1.04x of it |
| WAL scan, 10,000 frames | 327 ns/frame | 2,892 ns/frame | 1,944 ns/frame |
| `wal_ops` | 742 ns/op | 4,152 ns/op | 3,001 ns/op |

The port now matches scalar Rust within 4%. This machine's CPU has the SHA
extensions (`sha_ni`), and the Rust build's `sha2` uses them, which is the
whole remaining 10x gap. Closing it needs a SHA-256 intrinsic in Vesper (a
compiler feature, gap 8 below) or a hashing call the port can reach. The
transcript differential still shows 0 differences in all 11 cases.

### More compiler gaps

8. **No access to hardware SHA instructions.** There is no intrinsic for
   x86 `sha256rnds2`/`sha256msg1`/`sha256msg2` or the AArch64 SHA2
   instructions, so a Vesper SHA-256 tops out at scalar speed. This is
   where the WAL and the identity digests miss parity.

## Part 6: the Warp QUIC wire codec

Base: part 5 (#1416).

### What changed

- The end of `port/src/lib.ves` holds the wire layer that `src/model/umem.rs`
  and `src/model/warp.rs` share:
  - `parse_short_header`: the flags byte, the long-header and fixed bits,
    the DCID and the truncated packet number.
  - `parse_ack`: the fixed-width ACK frame. It follows RFC 9000 §19.3, with
    big-endian integers in place of varints, and checks every range for
    underflow.
  - `ack_frame_bytes`, `short_header` and `ack_datagram_len`.
  - The peer's `record_pn`: inclusive ranges kept largest first, merged
    with their neighbours, and capped at `max_ranges`.

  Under `vesper-port` the Rust functions forward to the port. Results cross
  the boundary as flat words: `[code, dcid, largest, delay, lo0, hi0, ...]`
  and range pairs.
- `vesper-warp-umem` (the AF_XDP driver, the software NIC and the UMEM
  auditor) belongs to SH6 lane 14 (#1409). The coordinator and the
  recommended option on the user's card both leave it there. The umem/warp
  models keep calling the Rust crate until that port lands.
- The differential adds a `quic_wire` case: short headers of all 256 first
  bytes, `ack_datagram_len`, and 20,000 encoded ACK datagrams with random
  mutations through `parse_ack`. The mutations are bit flips, cuts,
  extensions, forged range counts and overwritten bytes. The Warp
  transcripts cover `record_pn` through the datagram ACK path.
- The `quic` divergent edit accepts a short header without the fixed bit.

### Results

| Check | Rust | Vesper |
|---|---|---|
| Crate tests | all pass | all pass |
| Transcript differential (12 cases) | blessed | **0 differences** |

| Measure (release, best of 5) | Rust | Vesper | Ratio |
|---|---|---|---|
| `quic_throughput`: encode and parse 10^5 ACK datagrams of 1 to 8 ranges | 390 ns | 529 ns | 1.36x |

Part of the gap is the boundary itself: the ranges go in and out as `Vec`s
of words and are rebuilt as tuples on each side. The codec is a small share
of a Warp session's time, since the evaluator dominates.

## What is left (later parts of #1342)

- Hardware SHA-256 (gap 8), and building WAL frames at flush rather than
  at append.
- The rest of `model/umem.rs` and `model/warp.rs`: the frame adapter and
  journal over the `vesper-warp-umem` driver (its linear owners, location
  table and auditor), the EDT launch queue, the lease timers and the
  flows' event plumbing. The randomness (SplitMix64), the links and the
  wire codec are already in Vesper. The rest is orchestration around the
  driver, so it waits for SH6 lane 14 (#1409) to port `vesper-warp-umem`.
- `model/mod.rs` process epochs, scenario validation, the canonical-JSON
  identity digests and `value.rs`. These need a JSON value model on the
  Vesper side, or the Rust side has to pass canonical strings in.
- The session driver itself, which steps the compiler's reference evaluator,
  waits until the evaluator is reachable from Vesper (after the SH4 swap
  points).
- A Bazel target for the port build. Linux Bazel builds and tests the Rust
  build. The port runs under Cargo in port-harness shard 7.
