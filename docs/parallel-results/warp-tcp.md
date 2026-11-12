# Warp TCP: sans-IO RFC 9293 endpoint

Wave-four lane 7 ([prompt](../agent-prompts/wave-four/07-tcp.md)) covers the "QUIC/TCP" title and intro of design §35. The base is `origin/main`, which descends from the required `a5fa024`. The branch is `claude/project-thread-fb9hg2`; the commit and PR are in the PR description.

All code is in the new crate `crates/vesper-warp-tcp/**`. The only internal dependency is `vesper-ffi`, and only as a **dev-dependency**: one test checks that the header offsets match a Bridge wire layout. No other wave-four crate is imported. The root patch, in its own commit, only adds `crates/vesper-warp-tcp` to `members` and `default-members` and regenerates `Cargo.lock`. There are no new `[workspace.dependencies]`, no CLI wiring, no CI edits and no Lean files.

What this crate is:
- a library TCP endpoint that the caller drives with byte slices and a virtual clock;
- a deterministic two-endpoint harness that tests it.

What it does not have:
- no kernel, TUN or raw-socket I/O;
- no AF_XDP or NIC offload;
- no claim that it interoperates with Linux or any other real stack. The only peers it has been run against are itself and hand-built segments.

## Public API

| Module | Items |
| --- | --- |
| `seq` | `Seq` holds modular u32 arithmetic: `add`, `diff`, `lt`, `le`, `gt`, `ge`, `in_window`. `SeqMap` lifts wire sequence numbers to u64 stream positions, where 0 is the SYN. |
| `wire` | `Segment::parse` parses in place, with the payload borrowed. `TcpOptions::{parse, write, encoded_len}` covers MSS, window scale, SACK-permitted, SACK (1–4 blocks), timestamps, NOP and EOL; unknown options are skipped. `write_header`. `IpPair::{V4, V6}`. `compute_checksum`, `fill_checksum` and `verify_checksum` work over the pseudo-header. `WireError` holds the typed errors. `offsets::*`. |
| `conn` | `Connection<C: CongestionController = NewReno>`. User calls are `listen`, `connect`, `write`, `close`, `abort` and `recv_release`. The I/O loop uses `on_segment`, `poll_transmit`, `poll_timeout` and `on_timeout`. Results come back through `take_released`, `take_event`, `state` and `stats`. Supporting types: `Config`, `State`, `Event`, `CloseReason`, `TcpError`, `SendView`, `Received`, `Released` and `Transmit`. |
| `cc` | The contract vocabulary: `SentPacket`, `AckedPacket`, `LostPacket`, `RttSample` and `CongestionEvent::{Acked, Lost, EcnCe, PersistentCongestion, AppLimited}`. The `CongestionController` trait has `cwnd_bytes`, `pacing_rate_bytes_per_sec` and `schedule_edt_ns`, plus `on_sent`, `on_event`, `on_rtt_sample` and an optional `on_mss`. `NewReno` implements it. |
| `rtt` | `RtoConfig` and `RttEstimator`, following RFC 6298. |
| `harness` | `Harness`, `Peer`, `LinkConfig` (delay, jitter, loss, duplication, serialization rate, seed), `ReadPolicy`, `DropFilter`, and `Rng` (SplitMix64). |

### Data paths and zero-copy

**Receive.**
- In-order payload comes back as a slice that borrows the caller's segment buffer. The tests check the pointer.
- Out-of-order payload is copied once, into a ring buffer the caller owns, at `offset mod capacity`. When the gap fills, those bytes come back as up to two slices that borrow the ring.
- Delivered bytes count against the receive window until the application calls `recv_release`.
- The window's right edge is `read_offset + capacity`. Because of that, the advertised window never shrinks, and delivered ring bytes cannot be overwritten before they are released.

**Send.**
- The retransmission queue stores only sequence ranges, never payload copies.
- On each `poll_transmit`, the payload is read from the caller's `SendView`. That covers retransmissions too, and the tests check that a retransmission re-reads the caller's buffer.
- It is then copied once into the caller's output frame, for example a UMEM chunk. There is no scatter-gather yet.
- When bytes are cumulatively acknowledged, `take_released` returns a `#[must_use]` `Released { start, end }`. After that the caller may reuse those bytes.

### Congestion-control contract mapping

TCP has no packet numbers, so every transmission gets a fresh, increasing `pn`. A retransmission gets a new `pn`.
- `Acked` is reported once per range, when it is first cumulatively acknowledged or first SACKed.
- `Lost` comes from the dup-ACK or SACK scoreboard.
- An RTO maps to `PersistentCongestion`.
- `ack_delay_ns` is always 0.

`NewReno` follows RFC 5681 slow start and congestion avoidance, with RFC 3465 byte counting (L = 2·SMSS). It reduces the window once per loss episode, and episodes are delimited by send time, as in RFC 9002 §7. It does not use RFC 5681 window inflation, because recovery is driven by RFC 6675 pipe. It does not pace: `schedule_edt_ns` returns `now`. BBRv3 plugs in through the same trait at integration.

## RFC coverage

| Area | Implemented |
| --- | --- |
| RFC 9293 §3.1 format | Header and options parse and serialize; checksums over IPv4 and IPv6 pseudo-headers (RFC 8200 §8.1); malformed option lists, duplicate singleton options and bad data offsets are typed errors. |
| §3.3.2/§3.10 state machine | All eleven states. Simultaneous open and simultaneous close. SYN-RECEIVED returns to LISTEN on RST after a passive open, and becomes CLOSED (refused) after an active one. TIME-WAIT re-ACKs a retransmitted FIN and restarts 2·MSL. A segment arriving in CLOSED gets an RST as RFC 9293 specifies. CLOSE in SYN-RECEIVED is queued until ESTABLISHED. ABORT sends `<SEQ=SND.NXT><CTL=RST>`. |
| §3.4 sequence numbers | Modular comparison everywhere on the wire; u64 positions internally. The ISN is the caller's per-connection base, plus a 4 µs clock. |
| §3.8.6 windows | SND.WL1/WL2 window updates. Receiver SWS avoidance (min(rcv_buf/2, MSS)). Sender SWS avoidance and Nagle (Nagle is optional). An override timer, so a small usable window never stalls. A zero-window probe is one byte of new data on a backed-off persist timer, and it is never counted as loss or as a retry. The window is never shrunk. |
| RFC 7323 | Window scale (both sides must offer it; a shift above 14 is clamped; SYN windows are unscaled). Timestamps (echo, TS.Recent update rule, PAWS rejection; non-RST segments without TSopt are dropped). |
| RFC 2018 / 6675 | SACK-permitted negotiation. The receiver sends up to 4 blocks, or 3 with timestamps, most recent first. The sender scoreboard runs IsLost (DupThresh 3, or more than 2·SMSS SACKed above). Pipe-based sending. A segment retransmitted in the current recovery episode is not re-marked lost (HighRxt). SACK bits are cleared on RTO (RFC 2018 §8). |
| RFC 5681 / 6582 | Without SACK: three duplicate ACKs trigger fast retransmit, partial ACKs retransmit the next hole, and each dup ACK reduces the pipe estimate. |
| RFC 6298 | SRTT/RTTVAR, min/max/initial RTO, clock granularity, exponential backoff. Karn's rule: one timed segment, discarded if it is retransmitted. The timer restarts on new ACKs. The connection aborts after `max_retransmits` (R2). |
| RFC 5961 | RST: the exact sequence number resets, an in-window RST gets a challenge ACK, an out-of-window RST is dropped. A SYN in any synchronized state gets a challenge ACK. ACK validity is checked against `[SND.UNA − MAX.SND.WND, SND.NXT]`. Challenge ACKs are limited to N per second of virtual time. |
| Urgent | The urgent pointer is recorded (`urgent_offset`, one past the urgent data, RFC 6093) and the data is delivered inline. Sending urgent data is not offered. |

## Acceptance evidence

All 59 tests are in `crates/vesper-warp-tcp`. The tests run under debug overflow checks, so any unchecked wire arithmetic would panic.

| Requirement | Test |
| --- | --- |
| Every RFC 9293 transition, including simultaneous open/close and RST in each state | `tests/state_machine.rs` (33 tests). It drives a scripted peer and parses back every reply. It covers CLOSED reset replies, LISTEN (RST, ACK, SYN, close), SYN-SENT (good and bad ACK, the RST rules, SYN retransmission backoff of 200/400/800 ms and then a timeout, data and FIN on the SYN-ACK) and SYN-RECEIVED (ACK, bad ACK, RST back to LISTEN, duplicate SYN, timeout, queued close). It covers both close sequences, CLOSING, FIN+ACK going straight to TIME-WAIT, and TIME-WAIT FIN handling. Simultaneous open followed by simultaneous close (CLOSING to TIME-WAIT to CLOSED) is driven by two real endpoints with crossing segments. |
| RFC 5961 challenge ACKs | `rst_with_exact_sequence_closes_every_synchronized_state`, `rfc5961_in_window_rst_gets_challenge_ack_out_of_window_is_dropped` and `rfc5961_syn_in_synchronized_state_gets_challenge_ack` each run in all 7 synchronized states. `rfc5961_ack_validation` and `rfc5961_challenge_ack_rate_limit` cover the rest. |
| Exact byte stream under loss, reordering, duplication and zero windows | `tests/transfer.rs`. It runs 12 seeds at 3% loss with reordering jitter and 2% duplication. It runs 6 seeds at 8% loss without SACK, timestamps or window scaling. It covers a stalled reader with zero-window probes, a slow reader on a lossy link (lost window updates recovered by probing), bidirectional transfer, and IPv6 with a 64-byte MSS. Every run checks byte equality, a full close to CLOSED and that every send range was released. |
| SACK retransmits only missing ranges | `sack_recovery_retransmits_only_missing_ranges` drops data segments 3, 4 and 9. It asserts exactly 3 retransmissions of exactly 3·SMSS bytes, one recovery episode and no RTO. `newreno_fast_retransmit_without_sack` asserts that two holes recover through fast retransmit and a partial ACK, with no RTO. |
| Checksums match known packets | `tests/fixtures.rs` uses 8 hand-constructed pcap records (see [fixtures/README](../../crates/vesper-warp-tcp/fixtures/README.md)), checksummed by an independent Python RFC 1071 implementation. The RFC 1071 §3 vector is a unit test. |
| Malformed input never panics | `tests/malformed.rs` runs every truncation of a full segment, both with and without a recomputed checksum, and 200k random option lists. It also sends random segments with valid checksums and matching ports to endpoints in CLOSED, LISTEN, SYN-SENT, SYN-RECEIVED, ESTABLISHED, CLOSE-WAIT/FIN-WAIT-2 and TIME-WAIT/LAST-ACK. Those segments have seq/ack near the live values, random flags and options, interleaved transmits and timers, and they reach data delivery (asserted above 10 kB). A hostile MSS of 0 and a window scale of 200 are clamped. |
| Zero heap allocations in steady state | `tests/alloc.rs` uses a counting `#[global_allocator]` over 2000 warmed-up rounds that move more than 8 MiB, with every 97th data segment dropped. The measured path includes emit, checksum, parse, ACK and SACK processing, out-of-order insertion, reassembly, fast retransmit and timers. The result is 0 allocations. The harness itself allocates, and it is not part of this claim. |
| Bridge layout cross-check | `tests/ffi_layout.rs` declares `TcpHeaderWire` with `vesper-ffi` big-endian wire integers, and checks that its offsets and size equal `wire::offsets` and that a fixture header reads back through `view_wire`. |

## Validation

Recorded on base `cca0a16`:

```text
cargo fmt --all -- --check                                              # ok
cargo test -p vesper-warp-tcp                                           # 59 passed
cargo clippy -p vesper-warp-tcp --all-targets --locked -- -D warnings   # ok
cargo clippy --workspace --locked --all-targets -- -D warnings          # ok
cargo test --workspace --locked --no-fail-fast                          # 1367 passed (see note)
```

Notes on the workspace run:
- Lean is not installed in this container. With `VESPER_LEAN=lean`, the four root `tests/lean_ir_ast.rs` tests fail with "configured Lean compiler is missing". Without the variable they skip. This crate has no Lean file; CI provides Lean.
- With `CARGO_TARGET_DIR` outside the checkout, 16 `vesper-loom-build` tests fail with `UnknownFingerprint { missing: ["engine_dependencies"] }`. That is because its `build.rs` looks for `Cargo.lock` by walking up from the target directory. With the in-tree `target/`, as CI uses, `cargo test -p vesper-loom-build --locked` passes. Neither failure involves this lane.

## Decisions

- **One TCB per `Connection`.** LISTEN accepts one SYN and becomes SYN-RECEIVED, like the RFC's TCB model. Listener demultiplexing, backlogs and SYN cookies belong to a socket layer, which is out of scope.
- **ISN.** The caller supplies the RFC 6528 keyed-hash component as `iss_base`, and the crate adds the RFC 9293 4 µs clock. The crate generates no secret and implements no PRF, because the wave forbids hand-rolled cryptography.
- **Data on SYN in LISTEN** is not acknowledged; the peer retransmits it. Data and FIN on a SYN-ACK received in SYN-SENT *are* processed.
- **SACK blocks on data segments** are added only when they fit under the MSS budget. Pure ACKs always carry the maximum number of blocks.
- **Peer MSS below 64** is clamped up to 64 (`MIN_SEND_MSS`), so a hostile option cannot stall the sender with zero-byte segments.
- **Challenge-ACK limit** is per connection. RFC 5961 §7 suggests a system-wide limit, and the socket layer should add one.
- **`Released`** is `#[must_use]`, but it is not a drop-detected linear owner. This lane has no §35 linear obligation such as `UmemFrame`, `InFlightQuicPkt` or `NonceTicket`, and Rust ownership here is not evidence of Vesper source typing.

## Remaining gaps

The following are not implemented:
- keep-alive;
- RFC 5482 user timeout;
- PMTU/PLPMTU discovery;
- ECN (RFC 3168): ECE/CWR are parsed and ignored;
- D-SACK;
- RACK-TLP (RFC 8985) and F-RTO;
- RTT measurement from timestamps (Karn sampling only);
- PAWS 24-day idle invalidation;
- RFC 1337 TIME-WAIT assassination protection, and RFC 6191 reopening from TIME-WAIT;
- TCP-AO;
- RFC 5961 §7 system-wide rate limit;
- sending urgent data;
- LISTEN → SYN-SENT through SEND, which is rejected as `InvalidState`.

Integration work left:
- wiring the BBRv3 adapter (lane 2) through `CongestionController`;
- honouring `Transmit::edt_ns` in a launch queue (lane 4);
- serving segments from UMEM frames (lane 3).

There is no Lean model of this state machine.
