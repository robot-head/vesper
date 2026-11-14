# Source XdpNic simulation on the Warp software NIC

Wave-five lane 14 ([prompt](../agent-prompts/wave-five/14-sim-softnic.md)), claim issue #134, design §35.1–35.3 (Vesper Warp source slice). Base: `origin/main` at `af03122ea74d403f203170926945f9d35786b289` (the required baseline). Branch: `claude/project-thread-2yft1n`. The commit and PR are in the PR description.

Everything is inside `crates/vesper-sim-session/**` plus this report. The root patch is one line of `Cargo.lock` (the crate's new path dependency on `vesper-warp-umem`), in its own commit; the root `Cargo.toml` is unchanged because a path dependency needs no `[workspace.dependencies]` entry. `vesper-warp-umem` is used read-only, without its `af-xdp` feature.

This still runs checked Vesper source on the **reference CEK evaluator** under SimPulse. It is not AF_XDP hardware, kernel I/O or native execution. The software NIC validates the modelled ring protocol only; nothing here says anything about NIC zero-copy, DMA isolation, launch-time offload or line rate. Rust ownership in the adapter is host-library discipline and **not evidence that the Vesper source typing rules for `linear` are implemented**; the evidence for those is the checker results below.

## What changed

### Frames on the software NIC (`umem-softnic-v1`)

`src/model/umem.rs` no longer has a private frame model. Each NIC is a real `vesper_warp_umem::SoftXsk` (the `Xsk` driver over a `SoftNic` in scripted mode) with its four rings, location table and leak ledger. For every source-held `UmemFrame[State]` value the adapter holds the matching Rust owner (`UmemFrame<RxHarvested>`, `UmemFrame<TxStaged>` or `InFlightQuicPkt`), so source operations drive exactly the frame accounting the Rust path uses:

| Source / model step | Software NIC call |
| --- | --- |
| a datagram arrives | `SoftNic::deliver`: the device takes one fill buffer, writes the bytes into the UMEM, publishes an RX descriptor (or drops: no buffer, RX ring full) |
| `harvest_rx_single` | `XdpNic::harvest_rx` (the adapter checks the chunk is the one it expected at the head of the RX ring) |
| `view_frame_bytes`, `split_short_header` | `XdpNic::view_frame_bytes`; the header is parsed from the **real UMEM bytes** |
| `recycle_to_fill_ring` | `XdpNic::recycle_to_fill_ring` |
| `turn_around_tx_in_place` | `XdpNic::turn_around_tx_in_place` (same chunk, same offset; uncovered bytes zeroed) |
| `submit_paced_tx` | the staged owner enters the simulator's EDT launch queue (lease state `Paced`) |
| launch time `max(edt, now)` | `XdpNic::submit_paced_tx` posts the TX descriptor; two `SoftNic::step`s consume it and post its completion; `harvest_completions` makes the chunk `InFlight` under the lease |
| ACK, source-observed ACK or PTO | `XdpNic::retire` (the chunk is freed), then `Xsk::replenish_fill` |

The device takes a fill buffer only when a datagram arrives (`fill_batch = 0`), so the fill ring stays FIFO and chunk choice is reproducible. The fill, TX and completion rings are sized to hold every frame, so they never refuse one.

**Conservation after every step.** `Umem::check_conservation` runs after every operation and timer. It first runs the crate's exact `UmemAuditor::audit` (every chunk in exactly one of eleven locations, the driver's collapsed kernel sides equal the device's rings, the multiset of chunks is exact, nothing leaked), then cross-checks the adapter's own record: every chunk the adapter attributes to a task or lease is where the driver's table says, owners name their chunk, the adapter's RX queue matches the device's RX ring, and the driver's application-held count equals the adapter's. A failure is `SessionError::Conservation` and nothing is committed. A dropped Rust owner is never released: the driver quarantines the chunk and the audit fails with `Leaked`.

**Copy-check-commit with an uncloneable NIC.** The session steps the model by cloning it, applying the step to the clone, checking events against the budget and the recorded trace, and only then committing. The driver and its linear owners cannot be cloned, so every copy of the model shares one NIC through `SharedUmem`, which journals every mutating call. The session commits the journal when it commits a step and, when a step fails, rolls back by replaying the committed journal into a fresh driver. Rollback only happens on failure. The session test with a broken host binding checks that after the failed step the frame is again harvested by the task and the audit passes.

**Typed errors.** Forged frames (`NotHeld`, `Forged`), stale views (`StaleView`, by per-chunk generation), out-of-range `tx_len` (`TxLenOutOfRange`) and packet numbers (`PacketNumberOutOfRange`), and the lease budget (`LeaseLimit`, a `ModelBudget` error) are unchanged. A call the adapter validated but the driver refuses is a new `UmemError::Driver`, reported as a conservation failure (the adapter and the NIC disagree).

### Datagram ACK path (`ack-path-v1`)

The wave-four model retired leases by timer at egress arrival plus `ack_return_ns`; ACKs could not be lost or aggregated. That **timer ACK path** is still the default. A NIC may now declare an `ack_path`:

```json
"ack_path": { "link": "ack", "ack_every": 2, "max_ack_delay_ns": 25000, "max_ranges": 8 }
```

- Each delivered reply copy reaches the flow's **peer** at its egress arrival time (`PeerReceived`). The peer records received packet numbers as ranges and sends an ACK after `ack_every` ack-eliciting packets, or `max_ack_delay_ns` after the first unacknowledged one (**aggregation**).
- An ACK frame carries the peer's `max_ranges` most recent ranges (RFC 9000 §19.3 fields: largest, delay, range count, first range, then gap/length pairs, as fixed-width big-endian integers rather than varints). The ACK datagram uses the flow's packet-number space and connection ID.
- It crosses `ack_path.link` back to the NIC with that link's Gilbert–Elliott and independent loss, duplication, reordering and bottleneck queue (**ACK-path loss**), and lands in the RX ring like any other datagram (it needs a fill frame and RX ring space).
- **Source-observed retirement.** The new action `xdp_on_ack` binds `fn on_ack(bytes: ByteSlice) -> Result[AckSummary, u32]`. The model parses the ACK frame from the frame's UMEM bytes and retires, through `XdpNic::retire`, every parked, launched lease of that connection whose packet number it covers. A covered lease that source still holds is marked and retires when parked. Codes: 1–3 header errors as for `split_short_header`, 4 not an ACK frame, 5 truncated or underflowing ranges. On a datagram ACK path the model never retires a lease on its own except by PTO; a lost ACK is covered by a later ACK that still carries its range, or the PTO fires.
- Validation: the link exists; `ack_every` 1..=1024; `max_ack_delay_ns` > 0; `max_ranges` 1..=64; the largest ACK datagram fits a frame after the headroom; flows on the NIC have distinct connection IDs. The ACK pn is checked below 2^62 when the ACK is sent.

The signature of `on_ack` is checked structurally (an `in` view of the NIC's view type, `Result[<copy struct { largest_acked: u64, retired: u64 }>, u32]`).

### Versions

| Item | Version |
| --- | --- |
| Model semantics (scenario v3) | `vesper-sim-models-v3(process-epochs-v1,wal-frame-v1,net-v1,umem-softnic-v1,datagram-v1,ack-path-v1)` (`WARP_MODEL_SEMANTICS`). |
| Old recordings | Recordings that name the wave-four `vesper-sim-models-v2(…,umem-v1,datagram-v1)` (exported as `RETIRED_WARP_MODEL_SEMANTICS`) are **rejected** with `SessionError::Identity` on field `model_semantics` (test `recordings_of_the_retired_frame_model_are_rejected`). They would in any case fail the executable digest. |
| Scenario | Still version 3. `ack_path` is optional and omitted from serialization when absent, so existing v3 scenarios load and hash exactly as before. |
| Recording format, `SEMANTICS` | Unchanged (version 2; `…+sim-session-v2`). Version 1 and 2 scenarios keep `vesper-sim-models-v1` and are unaffected. |
| Trace | New `WarpEvent`s `PeerReceived`, `AckSent`, `AckObserved`; `Launched` gains `peer_at_ns` (omitted when empty); `LeaseTimer::SourceAck`. |
| Snapshot | `Accounting` gains `free`, `launched` and `leaked`; `NicSnapshot` gains `softnic` (device and driver counters). |

## Expectation changes, each explained

- `a_host_binding_that_drops_a_recycled_frame_is_caught`: the broken binding now drops the Rust owner instead of forgetting a chunk number, so the failure is the software NIC's `Leaked { count: 1, first_chunk: Some(_) }` rather than the private model's `Missing`. The test now also checks the rollback.
- `forged_stale_and_leaked_frames_are_model_errors`, case `leak`: a task that returns its harvested frame is still a `Conservation` error ("holding"); its owner is now dropped and the message carries the software NIC audit's `Leaked { count: 1, … }`; the snapshot reports `leaked: 1`.
- NIC geometry is the software NIC's: frame sizes are powers of two in 2048..=32768 with at least 64 payload bytes after the headroom, and the RX ring is a power of two of at most `frames` entries (was any size). Every existing fixture already satisfied this; no scenario changed.
- Nothing else. The full fixture reproduces the same frame choices as before (FIFO fill order on both models), and all eleven wave-four Warp tests pass with their original assertions.

## Acceptance evidence

`tests/warp.rs` (12 tests) and `tests/warp_ack.rs` (5 tests) on real checked source, plus 5 unit tests in `model::umem` and the existing `model::link` tests.

1. **Existing tests on the new backend.** All wave-four Warp tests pass: the full fixture (3,600 emitted datagrams through burst loss, reordering, duplication, queue overflow and two NAT rebindings) conserves all 64 frames in the trace replay and the final audit, source counters match what was delivered, the largest decoded packet number per connection matches across the 2^32 truncation boundary, Gilbert–Elliott statistics, launch-order egress, the in-flight and lease budgets, load validation, and changed-choice replay rejection.
2. **The real NIC did the I/O.** For the full fixture the software NIC's own counters equal the model trace: `rx_delivered` = `RxArrived`, driver `rx_harvested` = `Harvested`, `rx_dropped_no_buffer`/`rx_dropped_ring_full` = `RxDropped` by reason, `tx_submitted` = `tx_consumed` = `completions_posted` = driver `completions` = `Launched`, driver `retired` = `Retired`, `leaks_detected` = 0.
3. **Conservation, including a leaking source program.** The audit runs after every step. A source task that returns a harvested frame fails with the software NIC's `Leaked`; a host binding that drops a recycled owner fails with `Leaked` before anything is committed, and the rolled-back NIC audits clean.
4. **ACK aggregation and ACK-path loss** (`fixtures/warp_ack.ves` appended to `warp.ves`, scenario `fixtures/warp_ack.json`: 960 data datagrams, ACK link with GE 3%/30% and 2%/60% loss, 3% independent loss, 1% duplication, 2% reordering). Measured on this host with seed 3535: 721 replies, 720 peer arrivals, **370 ACKs sent** (aggregation, with some sent by the delay timer), **54 ACK datagrams lost**, **714 leases retired by source-observed ACKs and 7 by PTO**, none by the timer path; source's own count of retired leases equals the model's, and every data datagram of a known connection was first refused by `on_ack` with code 4. For at least half of the lost ACKs (asserted) a later delivered ACK of the same flow still carries the lost ACK's largest packet number. With the ACK link at 100% loss, peers still send ACKs, source observes none, and every lease retires by PTO. Frames are conserved through every path.
5. **Deterministic replay.** The full wave-four fixture and the ACK-path fixture each replay byte-identically (`Recording::to_json()` and the whole report). Old-model recordings are rejected by version.
6. **Rollback.** A unit test journals a harvest and a turn-around, rolls back, and finds the datagram back on the RX ring and harvestable again with the same descriptor.

## Validation

On the x86_64 Linux cloud host (rustc 1.97.0), isolated `CARGO_TARGET_DIR`, `CARGO_INCREMENTAL=0`, debug info off:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test -p vesper-sim-session --locked
cargo test --workspace --locked
```

Results are in the PR description. In a debug build the full wave-four fixture takes about 35 s per run on the software NIC (about 30 s on the private model); the ACK fixture about 10 s.

## Findings for the coordinator

- **Checker gap (§35.3 signature), still present.** `fn poll_and_step(inout conns: …) -> Result[…] eff [Fail[…]]` cannot use `?` anywhere: `E0402` "potentially abortable effect 'Fail' crosses active inout parameter 'conns'", even before any linear frame exists. The fixture passes connections by value. Test: `the_checker_rejects_unsound_frame_handling` (unchanged, still passing). The checker was not edited.
- The other wave-four findings still apply: no borrowed return types (views are checked dynamically by generation), no abstract construction (source can write `UmemFrame { … }` literals; the adapter rejects them at run time as `NotHeld`), and §35 syntax that does not exist yet (`wire struct`, `tiled(N)`, `ByteArray[N]`, bit operations, `u16`). There is no numeric widening in source, so `AckSummary.retired` is `u64`.
- **Suggested global-doc line:** "Source XdpNic simulation runs on the `vesper-warp-umem` software NIC (`umem-softnic-v1`) with an optional lossy, aggregated datagram ACK path (`ack-path-v1`); see `docs/parallel-results/sim-softnic.md`."

## Public API changes in this crate

- `model::umem`: `Umem` is now the adapter over `SoftXsk` (not `Clone`); new `SharedUmem` (journal, `commit`, `rollback`), `UmemParams`, `AckSummary`, `AckFrame`, `AckOutcome`, `parse_ack`, `ack_frame_bytes`, `ERR_NOT_ACK`, `ERR_BAD_ACK`; `UmemError::Driver`; `ConservationError` now wraps the software NIC's error (`Nic`) plus adapter disagreements; `Accounting` gains `free`, `launched`, `leaked`; `Datagram` gains `flow`.
- `model::warp`: `AckPathSpec`, `NicSpec.ack_path`, `SoftNicCounters`, `NicSnapshot.softnic`, `WarpOp::OnAck`, `LeaseTimer::SourceAck`, new `WarpEvent` variants, `MAX_ACK_RANGES`, `MAX_ACK_EVERY`, `ack_datagram_len`; `MAX_FRAME_BYTES` is now the software NIC's 32 KiB.
- `scenario::Action::XdpOnAck`; `RETIRED_WARP_MODEL_SEMANTICS`.

## Remaining gaps

- Transmission on the device is instantaneous: a launched frame is consumed and completed at its launch instant, so a source-observed ACK never meets a lease whose completion is still pending (the driver's `AwaitingCompletion` path is reported as a disagreement if it ever occurs).
- The EDT launch queue is the simulator's, not lane 4's timing wheel; NIC launch-time offload is not modelled.
- No retransmission, congestion control, AEAD, header protection or real QUIC frames beyond the fixed-width ACK frame; source does not write reply bytes.
- A NIC-owning process still cannot crash (rejected at load).
- Evaluator speed bounds the scale: thousands of packets per test.
