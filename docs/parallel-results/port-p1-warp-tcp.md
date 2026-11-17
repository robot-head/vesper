# Port P1 lane 11: vesper-warp-tcp

Claim [#908](https://github.com/robot-head/vesper/issues/908). Baseline
`b69383e` (descendant of the required `d523661`). The crate's sans-IO RFC 9293
endpoint (`seq`, `rtt`, `cc`, `wire`, `conn`) now has a Vesper build that runs
every test unchanged; the Rust implementation is frozen in `src/reference/`
and stays the default build (see "Rust code deleted?").

## API inventory (re-measured before porting)

| Module | Public types | Functions and methods |
| --- | --- | --- |
| `seq` | `Seq`, `SeqMap` | `Seq::{add, diff, lt, le, gt, ge, in_window}`, `SeqMap::{seq, abs}` |
| `rtt` | `RtoConfig`, `RttEstimator` | `RttEstimator::{new, sample, backoff, reset_backoff, rto_ns}`, getters `srtt_ns`, `rttvar_ns`, `min_ns`, `latest_ns`, `has_sample` |
| `cc` | `SentPacket`, `AckedPacket`, `LostPacket`, `RttSample`, `CongestionEvent`, `NewReno`, trait `CongestionController` | `NewReno::{new, set_mss, ssthresh}`; the trait's `on_mss`, `on_sent`, `on_rtt_sample`, `on_event`, `cwnd_bytes`, `pacing_rate_bytes_per_sec`, `schedule_edt_ns` for `NewReno` |
| `wire` | `Flags`, `SackBlock`, `Timestamps`, `TcpOptions`, `WireError`, `Segment`, `HeaderFields`, `IpPair`; 6 constants and `offsets` | `Flags::{contains, with}`, `BitOr`; `TcpOptions::{encoded_len, write, parse, sack}`; `Segment::{parse, seq_len}`; `IpPair::{default_mss, reversed}`; free `write_header`, `sum_bytes`, `fold`, `pseudo_header_sum`, `compute_checksum`, `fill_checksum`, `verify_checksum` |
| `conn` | `State`, `Config`, `TcpError`, `CloseReason`, `Event`, `SendView`, `Released`, `Received`, `Transmit`, `Stats`; `MIN_SEND_MSS` | `State::is_synchronized`; `Connection::{new, with_controller}`; 18 getters (`state` … `ooo_ranges`, `written`, `poll_timeout`); `listen`, `connect`, `write`, `close`, `abort`, `recv_release`, `take_released`, `take_event`, `on_segment`, `on_timeout`, `poll_transmit`; `Received::{chunks, len, is_empty}` |
| `harness` | `Harness`, `Peer`, `LinkConfig`, `ReadPolicy`, `Side`, `Rng` | the simulator (stays Rust, below) |

Unit tests found: 7 `#[cfg(test)]` tests in `seq` (2), `rtt` (1), `cc` (1)
and `wire` (3); `conn` had none. Integration tests: 56 in 7 files (63 with the unit tests, the lane's count).

Parts that must stay Rust (from the lane prompt): `harness.rs` and any
custom-controller adapter.

## What was ported

The Vesper library `tcp` is 17 files in `crates/vesper-warp-tcp/vesper/`
(3,654 lines), one per Rust module and the larger modules split by topic:
`int` (saturating/min/max and handled variable shifts and divisions), `seq`,
`rtt`, `cc`, `wire`, `options`, `segment`, `checksum`, and `conn`,
`conn_user`, `conn_helpers`, `conn_input`, `conn_ack`, `conn_text`,
`conn_timers`, `conn_transmit`, `conn_emit`. `build.rs` (and a `genrule` in
`BUILD.bazel`) concatenates them in a fixed order and compiles the result with
`vesper build --lib`; the binding (`port/binding.rs`) declares 65 exported symbols.

Every function of the five modules is ported, the connection included: the
handshake, RFC 7323 window scaling, timestamps and PAWS, SACK scoreboard and
RFC 6675 recovery, RFC 5961 challenge ACKs, the RFC 6298 timer, zero-window
probing, urgent data, delayed ACKs, FIN/TIME-WAIT and the receive ring.

How the Rust API reaches it (`src/ported/`):

- `Connection<C>` holds the Vesper TCB as an owned handle (`Tcb`) next to the
  Rust `Config`, a mirrored `RttEstimator` and the controller `C`.
- The congestion controller is the one Rust trait call in the loop. The port
  takes `cwnd` as an argument of `tcb_poll_transmit` and records the
  controller calls (`on_mss`, `on_rtt_sample`, `on_event`) as a command plan
  (`CcCmd` sequence) that the Rust side applies after each `on_segment` and
  `on_timeout`. For each new transmission the Rust side asks the controller
  for its EDT and commits it with `tcb_commit_edt`, then calls `on_sent`.
- `NewReno` itself is ported (`newreno_*` in `cc.ves`); the Rust `NewReno`
  forwards to it. The lane prompt asked for an enum of the shipped
  controllers; the crate ships one, so it is a struct.
- `Received` is rebuilt from offsets that the port returns into the segment
  bytes and the receive ring, so it still borrows both.
- Mirrors in `src/ported/types.rs` for the types that hold arrays, slices or
  `&'static str` (`Ips`, `Opts`, `Hdr`, `Parsed`, `TcpFail`).

API notes for the Vesper build: `Seq`'s operations, `Flags::contains/with`,
`State::is_synchronized` and `IpPair::default_mss` are `const fn` in the Rust
build and plain `fn` in the Vesper build (a binding call is not const). No
in-repo caller uses them in a const context.

## What stays in Rust, and why

| File | Reason | Unblocked by |
| --- | --- | --- |
| `src/harness.rs` | Test simulator with a `Box<dyn FnMut>` drop filter; drives the connection through the public API (lane prompt). | `dyn Trait` (F7b) |
| `src/{seq,rtt,cc,wire,conn}.rs` | The public types, shared by both builds so dependents do not change; plain field getters, `IpPair::reversed`, `TcpOptions::sack`, `BitOr`, constants. | — (the plan keeps types in Rust) |
| `src/ported/conn.rs` (`apply_cc`) | Custom-controller adapter: applies the recorded controller calls to the Rust `C: CongestionController`. A user controller exists outside the tests: `vesper-warp`'s `TcpBbr` (`crates/vesper-warp/src/cc.rs`). | a Vesper `dyn Trait` or the BBR port (lane 12) |
| `src/ported/*.rs` | Representation adapters between the public types and the export ABI's plain words. | — |
| `src/reference/**` | The frozen Rust implementation (default build). | deletion decision below |

Unsafe code: none in the port or the adapters (`deny(unsafe_code)` under
`vesper-port`, `forbid` otherwise). The generated binding `port/binding.rs`
has 73 `unsafe` occurrences (the `extern "C"` calls and handle frees), allowed
on that module only, as in the pilot.

## Unit-test treatment

The 7 unit tests were translated to the public API in `tests/units.rs` and run
against both builds; the in-module copies were removed with the
implementation's move to `src/reference/`. One test was added:
`tests/state_machine.rs::third_duplicate_ack_triggers_fast_retransmit`,
because no existing test caught a wrong duplicate-ACK threshold (found by the
`conn` divergence check).

Test counts, both builds, all passing locally (Linux x86_64): alloc 1,
controller 2, ffi_layout 1, fixtures 4, malformed 4, state_machine 36,
transfer 9, units 7, divergence 3 (67).

The counting-allocator test (`tests/alloc.rs`) passes on the Vesper build, but
Vesper's allocations go through libc `malloc`, which the Rust global
allocator does not see. It shows that the Rust side allocates nothing in the
steady state; it does not show that the Vesper side does not.

## Divergence checks

One wrong line per ported module (`DIVERGENT_EDITS` in `build.rs`, the same
`edits` in `BUILD.bazel`), each caught:

| Module | Edit | Caught by |
| --- | --- | --- |
| seq | `sn_ge`: `>= 0` becomes `> 0` | `units::wraparound_ordering` |
| rtt | first sample sets RTTVAR to R, not R/2 | `units::rfc6298_formulas_and_backoff` |
| cc | NewReno's loss floor drops from 2·MSS to 1·MSS | `units::slow_start_then_one_reduction_per_episode` |
| wire | an odd trailing checksum byte is dropped | `fixtures::fixture_checksums_verify_and_recompute`, `fixtures::fixtures_reserialize_byte_for_byte`, `state_machine::abort_sends_reset_and_api_misuse_is_rejected` |
| conn | duplicate-ACK threshold 3 becomes 2 | `state_machine::third_duplicate_ack_triggers_fast_retransmit` |

Each edit was checked alone (`VESPER_TCP_DIVERGE=<module>`) and all together;
in the combined build `units::rfc1071_numeric_example`,
`fixtures::fixture_fields_and_options_decode` and
`state_machine::listen_close_goes_to_closed` still pass, so the failures come
from the edits. Bazel: `:port_divergence_detected` runs the three divergent
test binaries (`tests/divergence.rs`). Cargo (macOS, Windows):
`crates/vesper-warp-tcp/scripts/test-port.sh`, one line in
`scripts/test-port-harness.sh`.

## Benchmarks

`crates/vesper-warp-tcp/examples/abba.rs`, run by
`crates/vesper-warp-tcp/scripts/bench.sh`. The two builds are one crate with
and without a feature, so they cannot share a process: each arm is a worker
process (the example built one way or the other, release profile, Vesper
library compiled with `optimize: true`) that runs one batch per request on
stdin. The driver times every request with `vesper_bench::run_abba` (12
measured epochs plus 1 warmup, A/B/B/A windows), so both arms carry the same
pipe round trip, which is small next to a batch. Each batch returns a digest
of its results and the driver checks that the two builds agree (they did,
over every batch).

- **codec**: per batch, 64 data segments with timestamps and two SACK blocks
  and a 1,460-byte payload: `write_header`, payload copy, `fill_checksum`,
  `verify_checksum`, `Segment::parse`. 32 requests per window.
- **transfer**: per batch, a full harness run (Rust harness driving two
  ported connections): handshake, 64 KiB A→B over a 5 ms ± 2 ms link with 1%
  loss, close both ways. 4 requests per window.

Setup: cloud container, 4 vCPU Intel Xeon @ 2.10 GHz, Linux 6.18, Rust 1.97,
no pinning or isolation; 2026-11-17.

| Workload | Rust (median) | Vesper (median) | Vesper/Rust, median of 12 epochs (min–max) | 2x gate |
| --- | --- | --- | --- | --- |
| codec | 1,185 ns/segment | 15,413 ns/segment | 13.75x (9.74–18.90) | Regression |
| transfer | 225 µs/transfer | 2,260 µs/transfer | 10.30x (7.13–17.65) | Regression |

The gate is `analyze_pairs` at 10,000 bp (candidate at most 2x the
baseline). Where the time goes, from probes during the port: a byte loop in
the Vesper build costs about 4.5 ns/byte against well under 0.1 ns/byte for
Rust's `copy_from_slice`, and bounds-checked `ChunkSeq`/`Slice` access with
`Result` returns dominates the checksum and the payload copies. The crate had
no benchmark before this lane, so the plan's 2x stop rule does not formally
apply; the numbers are the reason for the decision below.

## Platforms run

- Linux x86_64 (cloud container, Rust 1.97 and clippy on 1.100): both builds,
  every test, the divergence checks, the benchmark.
- The dependent `vesper-warp` with the port (`cargo test -p vesper-warp
  --features vesper-warp-tcp/vesper-port --test tcp_bbr`): 3 of 3 pass, so
  its `TcpBbr` controller works through the Rust-side adapter.
- Linux (Bazel on BuildBuddy), macOS AArch64 and Windows x86_64 MSVC (Cargo
  through `scripts/test-port-harness.sh`): in the PR's CI.

## Rust code deleted?

No, not in this PR. Both builds pass every test, so the plan's rule for a
crate the compiler does not link says to delete the Rust code and make the
port the only build. The port is 10–14x slower on both benchmarks, and the
only dependent, `vesper-warp`, would get that slowdown and a build that
compiles the Vesper compiler as a build dependency. This PR therefore keeps
the Rust implementation (frozen, in `src/reference/`) as the default build
and leaves the deletion to the owner's decision. Deleting it later is
mechanical: remove `src/reference/`, the `cfg` switches and the feature, and
make `build.rs` always build the port.

## Other notes

- The lane plan asks for a Loom package (`Loom.toml` plus `src/**/*.ves`).
  The port was written while `vesper build --lib` took one file, so the
  library is plain files that the build concatenates. Package builds landed
  with #919 just before this PR; turning the 17 files into package modules
  needs `use` lists between them and a root module that exports the 65
  functions, and is left as a follow-up. The branch has `main` with #919
  merged and builds through its prebuilt-compiler harness.
- `vesper fmt` rejects a file over 8,192 tokens, which is why the library is
  split; every file is canonically formatted.
- `crates/vesper-warp-tcp/vesper` is skipped in the self-hosted corpus
  (`tests/fixtures/self_host/driver.rs`), as the plan asks, until #880.
