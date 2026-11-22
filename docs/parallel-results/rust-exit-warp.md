# Rust exit SH6 lane 4: the Warp family

Claim [#1399](https://github.com/robot-head/vesper/issues/1399). Prompt:
`docs/agent-prompts/self-host-sh6/04-warp.md`. Crates: `vesper-warp`,
`vesper-warp-tcp`, `vesper-warp-bbr`, `vesper-warp-wheel`,
`vesper-warp-shard`. None of them is in the compiler's closure
(`cargo tree -p vesper -e normal` lists no Warp crate), so a crate that
passes the flip bar can lose its Rust directly, without the seed pattern.

## Part 1 (base `main` at `ab965f2a`)

**Result.** No crate passes the flip bar yet, so Rust stays the default build
of all five and no Rust is deleted. One port change in `vesper-warp-tcp` cuts
the transfer workload from 1.43x to **1.28x** of Rust's instructions. What
keeps every crate above 1.5x in wall-clock time is native code generation:
the Vesper code is about 8x larger than rustc's, and the TCP transfer path
misses the instruction cache 90 times as often as the Rust build (gap 1).

### Flip-gate verdicts (hand-run recipe, `scripts/ci/flip_gate.py` is not merged yet)

Setup: cloud x86-64 container (4 vCPU, Linux 6.18, Firecracker guest), Rust
1.97 `--release`, ports built with the release `vesper` of the base commit
through `VESPER_PORT_COMPILER` (the harness default, `optimize = true`).
Wall clock is the crate's committed ABBA driver; instructions are callgrind
`Ir`, whole process.

| Crate, workload | Differential | Instructions (Vesper/Rust) | ABBA 95% interval (Vesper/Rust) | Verdict |
|---|---|---|---|---|
| warp-wheel insert/advance/drain | 0 diffs, every test | about 1.9x | 1.770-1.854 | above 1.5x |
| warp-tcp codec | 0 diffs, every test | 0.72x | 0.589-0.791, 0.689-0.932, 0.922-1.298, 1.038-1.454 (four runs) | within 1.5x |
| warp-tcp transfer, before this part | 0 diffs, every test | 1.43x (22.14 M / 15.44 M) | 1.407-1.948, 1.733-2.162, 2.158-2.438 | above 1.5x |
| warp-tcp transfer, after this part | 0 diffs, every test | **1.28x** (19.81 M / 15.44 M) | 1.427-1.678, 1.502-1.902, 1.536-2.339 | above 1.5x |
| BBR ack path (`ack_path_abba abba .. 16 200000`) | 0 diffs, every test | 2.24x (rescore lane) | 1.956-3.289 | above 1.5x |
| warp (QUIC core parts) | 0 diffs, every test | 1.08x-4.16x (P3b lane 4) | not re-run | above 1.5x (recovery) |
| warp-shard | not ported | | | not ported |

The wall clock on this host is noisy: the warp-tcp transfer interval moved
between 1.4x and 2.4x over unchanged binaries. CPU time of 3,000 transfers
in one worker process (no pipe round trips; `getrusage`, three runs each) is
steadier:

| Build | User | System | Page faults |
|---|---|---|---|
| Rust | 0.33-0.38 s | 0.01-0.02 s | 9,806 |
| port, before this part | 0.69-0.72 s | 0.17-0.18 s | 150,593 |
| port, after this part | 0.62-0.66 s | 0.14-0.20 s | 150,586 |

So the port's user time is 1.7-1.9x of Rust's at 1.28x the instructions
(gap 1), and its system time comes from page faults the Rust build does not
take (gap 2).

### What changed in the port (`vesper-warp-tcp`)

1. **The sent queue keeps its sums** (`conn.ves`, `conn_helpers.ves`,
   `conn_transmit.ves`). `pipe` added up the unsacked, unlost entries of the
   whole sent queue, and `try_retransmit` scanned the queue for a lost
   entry, on every `poll_transmit` call: about 1,000 instructions per call
   between them, against 140 for the Rust build's. The `sq_*` helpers, the
   only writers of the queue, now keep two counters in the `Tcb`
   (`sent_pipe`, the bytes `pipe` adds up, and `sent_lost`, the entries
   `try_retransmit` looks for), adjusting them by the old and new value of
   each entry they write, push, pop or clear. `pipe` reads the counter (and
   saturates it to `u32`, as the per-entry saturating sum did), and
   `try_retransmit` skips its scan when `sent_lost` is 0. `poll_transmit`
   went from 2.17 M to 1.00 M instructions on the 8-transfer profile, and
   `try_new_data` from 1.60 M to 0.55 M.
2. **The sent queue is not pre-filled** (`tcb_new`). The constructor pushed
   `max_in_flight_segments` empty entries, touching the whole queue before
   any segment was in flight. Slots are now pushed on first use: they fill
   from 0, and the ring wraps only once all `sent_cap` slots exist, so a new
   slot is always the next one (`sq_push`).

The binding (`port/binding.rs`) is re-blessed for the two new `Tcb` words.
`cargo test -p vesper-warp-tcp --features vesper-port` (67 tests),
`scripts/test-port.sh` (the divergent build still fails each module's
tests) and the Rust build all pass, and both builds return the same digests
on 3,000 transfers.

Tried and dropped: nothing else was committed. Packing `SentSeg`'s five
`bool`s into one word (10 words to 6, the Rust entry's 48 bytes) would make
the queue the Rust build's size and may move the allocator's trim point
(gap 2), but it does not touch the instruction-cache cost (gap 1), so it was
left for a later part.

## Compiler gaps

1. **Native code is about 8x larger than rustc's, and hot paths miss the
   instruction cache.** The warp-tcp port's Vesper functions are 185 KB of
   machine code (`nm -S` over `vesper_tcp_*` and `vesper_function_*`); the
   Rust build's `reference` connection code is 22 KB. On 300 transfers
   (`cachegrind --cache-sim=yes`, 32 KB I1) the port takes 18.76 M I1 misses
   against 0.21 M for the Rust build, at 1.23x the instructions. The largest
   functions are `on_synchronized` (17 KB, 2,979 instructions),
   `tcb_on_segment` (12 KB) and `tcb_poll_transmit` (10.5 KB). Repro: build
   the transfer worker both ways (`crates/vesper-warp-tcp/scripts/bench.sh`
   builds them), then
   `seq 1 300 | sed 's/^/go /' | valgrind --tool=cachegrind --cache-sim=yes <worker> worker transfer`.
   Contributing patterns visible in the disassembly: every `ChunkSeq`
   element access reloads the header, length and data pointer and keeps its
   out-of-range branch; by-value aggregates are copied word by word at
   function entry (gap 3); stack slots are zeroed twice
   (`movq $0x0,(%rsp)` repeated). This is the inliner and code generator
   (SH3/SH4 lanes), not this lane's path.
2. **The allocator returns memory on every transfer in the port's process.**
   With the same allocation sequence (an `LD_PRELOAD` log of every
   `malloc`/`calloc`/`realloc` of 4 KiB or more shows the same calls, except
   the sent queue's 20,504 bytes against the Rust build's 12,288), the port's
   worker makes 8,103 `brk` calls on 3,000 transfers against the Rust
   worker's 533, and takes 150,586 page faults against 9,804 (0.17-0.24 s of
   system time against 0.00-0.03 s). glibc trims the heap top when the free
   top chunk passes its dynamic trim threshold; the larger Vesper entries
   (gap 4) put the port's frees of the harness rings just over it. With
   `GLIBC_TUNABLES=glibc.malloc.trim_threshold=4194304` both builds take
   46.7 k faults, and the user times are 0.54-0.58 s (0.71-0.77 s before
   this part) against 0.32 s.
3. **By-value aggregate parameters are copied at entry.** `on_synchronized`
   takes `seg0: Seg` (a header with its options, about 30 words) and copies
   it to a stack slot word by word before its first statement, then again
   for `let mut seg = seg0`. Already listed as "`in` aggregate parameters
   spilled to a slot at entry" in port-gaps-codegen-redundancy.
4. **Every `bool` field takes a word.** `SentSeg` (4 `u64`, a `u32` and 5
   `bool`s) is 80 bytes in Vesper and 48 in Rust.

## What is left

- warp-tcp: transfer is 1.28x in instructions but 1.5-2.3x in wall clock
  (gaps 1 and 2). `SentSeg` packing is the next port-side lever. The
  harness and simulator (`src/harness.rs`) are still Rust only.
- warp-wheel: 1.77-1.85x; the hot functions (`open_tick`, `place`,
  `next_work_tick`) are the same algorithm as the Rust build, and the extra
  instructions are bounds-checked loads from seven parallel sequences.
- BBR: 2.0-3.3x; see port-parity-bbr and port-gaps-rescore for its gaps.
- warp: the endpoint driver, UMEM, slot tables, application callbacks and TLS
  driving are still Rust (P3b lane 4 left them by the owner's 2026-11-19
  scope); recovery is 4-5x.
- warp-shard: not ported (the SIMD CID lookup through source vectors, §14).
- The census row (`scripts/rust-census.py`) is updated once lane 1's plan
  PR (#1415) is on `main`.
