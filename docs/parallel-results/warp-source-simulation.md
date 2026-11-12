# Source XdpNic effect and adversarial WAN simulation

Wave-four lane 8 ([prompt](../agent-prompts/wave-four/08-source-xdpnic-sim.md)), design §35.1 items 1 and 5, §35.2 `linear effect XdpNic`, §35.3 `poll_and_step_quic_rx_one` control flow. Base: `origin/main` at `a5fa024b8d6f62eaa336337d825a2be2ec8972b4` (the required baseline). Branch: `claude/project-thread-53umme`. The commit and PR are in the PR description.

Everything is inside `crates/vesper-sim-session/**` plus this report. There is no new crate, no new dependency and no change to the root `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, CI, the checker, SimPulse or any other crate, so **no root patch is needed**.

This runs checked Vesper source on the **reference CEK evaluator** under SimPulse, against a software model of UMEM frames and WAN links. It is not AF_XDP, DPDK, a NIC, kernel I/O, native execution, Crucible concolic fuzzing (§29) or DPOR. AF_XDP rings, NIC EDT/SO_TXTIME, RSS and crypto offload are **unavailable**: nothing here exercised them. Rust ownership in the host model is not evidence about Vesper's typing rules; the evidence for those is the checker results below.

## Versions

| Item | Version |
| --- | --- |
| Scenario | Version 1 and 2 are unchanged and serialize identically. **Version 3** requires `model.warp`; version 2 rejects a `warp` section, and version 3 rejects a model without one. |
| Model semantics | Version 1/2 scenarios keep `vesper-sim-models-v1(process-epochs-v1,wal-frame-v1,net-v1)`. Version 3 binds `WARP_MODEL_SEMANTICS` = `vesper-sim-models-v2(process-epochs-v1,wal-frame-v1,net-v1,umem-v1,datagram-v1)` into the identity. `net-v1` behaviour is unchanged. |
| Recording | Still version 2. Warp transitions are a new `ModelEvent::Warp(WarpEvent)` variant inside the existing model trace, so v2 recordings parse and replay as before. |
| `SEMANTICS` | Unchanged (`…+sim-session-v2`); the adapter's handling of existing actions did not change. |

## Source binding (§35.2)

`fixtures/warp.ves` declares the §35.2 types and effect verbatim, except that `u16` fields are `u32` (the language has no `u16`) and `ByteSlice` is a copy view struct:

```text
copy struct RxHarvested {}   copy struct TxStaged {}
linear struct UmemFrame[State] { chunk_idx: u32, data_offset: u32, pkt_len: u32 }
linear struct InFlightQuicPkt { pkt_num_full: u64, edt_departure_ns: u64, held_frame: UmemFrame[TxStaged] }
copy struct ByteSlice { chunk_idx: u32, generation: u64, offset: u32, len: u32 }
linear effect XdpNic { harvest_rx_single, view_frame_bytes(in), recycle_to_fill_ring(sink),
                       turn_around_tx_in_place(sink, u32), submit_paced_tx(sink, u64, u64), park_for_ack_or_pto(sink) }
linear effect Wire   { fn split_short_header(bytes: ByteSlice) -> Result[QuicShortHdr, u32]; }
linear effect RxWait { fn wait_rx(timeout_ns: u64) -> u64; }
```

New scenario actions, each checked **structurally** against the checked declaration at load (`scenario::warp_signature`): passing modes (`in`/`sink`), the frame ADT (linear struct, one field-less copy typestate, exactly `chunk_idx/data_offset/pkt_len`), the lease (linear, `held_frame` is the TX frame), the view and header shapes, and that RX and TX frames use distinct typestates. Every NIC role (RX frame, TX frame, lease, view, header) must be the same source type across all bindings of a NIC, and no frame, lease or view type may serve two NICs (chunk indices are per NIC, so a shared type would let one NIC's value alias another's chunk). Generic lease types are checked after substituting their arguments.

| Action | §35.2 operation |
| --- | --- |
| `xdp_harvest {nic}` | `harvest_rx_single() -> Option[UmemFrame[Rx]]` |
| `xdp_view {nic}` | `view_frame_bytes(in UmemFrame[Rx]) -> ByteSlice` |
| `xdp_recycle {nic}` | `recycle_to_fill_ring(sink UmemFrame[Rx])` |
| `xdp_turn_around {nic}` | `turn_around_tx_in_place(sink UmemFrame[Rx], u32) -> UmemFrame[Tx]` |
| `xdp_submit {nic}` | `submit_paced_tx(sink UmemFrame[Tx], u64, u64) -> InFlightQuicPkt` |
| `xdp_park {nic}` | `park_for_ack_or_pto(sink InFlightQuicPkt)` |
| `xdp_split_short_header {nic}` | header split helper (no new syntax) |
| `xdp_wait_rx {nic}` | parks until an RX arrival or the timeout (returns 0) |

A task may use a NIC only if its process owns it. A crash fault on a NIC-owning process is rejected at load (`umem-v1` does not model kernel reclamation of UMEM).

### UMEM model (`umem-v1`, `src/model/umem.rs`)

Every chunk is in exactly one of: fill ring, RX ring, harvested by a task, staged by a task, inside a source-held lease, or parked in the ACK/PTO wheel. These are separate containers. `Umem::check_conservation` recounts all of them after **every** model step (each operation and each timer) and reports a missing, duplicated, out-of-range or lease-inconsistent chunk; the session turns that into `SessionError::Conservation` before committing anything, with the owner retained.

Every consuming call checks the source value against the model: the chunk must be held by the calling task in the expected typestate, with exactly the recorded offset and length. Forged, duplicated or wrong-state values are `SessionError::Model` (`NotHeld`, `Forged`). A `ByteSlice` carries the chunk generation, bumped on every transition, so using a view after its frame was recycled or turned around is `StaleView`. `tx_len` must be 1..=frame bytes minus headroom; packet numbers must be below 2^62; leases are bounded by `max_leases` (a budget error before the lease is retained). A task that finishes while the model still records it as the holder of frames (for example by returning a frame as its output) is a `Conservation` error.

Header splitting parses the materialised datagram bytes: short header form bit clear, fixed bit set, 8-byte DCID, 4-byte truncated packet number. `Err(1)` truncated, `Err(2)` long header, `Err(3)` fixed bit clear. The header also carries the datagram's source address and port so source can see NAT rebinding.

### Datagram links (`datagram-v1`, `src/model/link.rs`) and flows (`src/model/warp.rs`)

A NIC has an ingress and an egress link. Per datagram, in a fixed draw order from a per-link SplitMix64 seeded by the scenario seed and the link name: duplication, Gilbert–Elliott state step and state-dependent loss, independent loss, reordering (extra delay), then a finite **bottleneck queue** with a service rate: full buffer is a modelled tail drop, otherwise departure is `max(at, last departure) + service`, so micro-bursts build queueing delay before they overflow. All probabilities are integer ppm. A duplicate is transmitted as an independent datagram with a fresh id.

Flows emit seeded bulk micro-bursts (back-to-back packets per burst). Each packet's kind (valid, unknown CID, truncated, long header), full packet number (32-bit truncated on the wire), source address and length are recorded. Timed `nat_rebind` events change a flow's source address mid-flow. An arrival needs a fill frame and RX ring space; otherwise it is dropped (`no_fill_frame`, `rx_ring_full`). Budgets: in-flight datagrams (`max_in_flight`) and leases are checked before anything is retained; flow totals, times and packet-number ranges are checked at load.

ACK/PTO: `submit_paced_tx` schedules a launch timer at `max(edt, now)`; the reply enters the egress link only then, so egress queueing follows launch order rather than submit order. Each delivered copy schedules an ACK at its arrival plus `ack_return_ns` and counts against `max_in_flight` until that ACK fires; the PTO is launch plus `pto_ns`. ACK and PTO timers start once the lease is both parked and launched. Whichever fires first retires the lease and returns the frame to the fill ring; the others are recorded as `StaleTimer`. Retransmission and congestion control are not modelled here.

Every choice and transition is a `WarpEvent` (`emitted`, `transmitted`, `rx_arrived`, `rx_dropped`, `harvested`, `recycled`, `turned_around`, `submitted`, `launched`, `parked`, `retired`, `stale_timer`, `rx_waiting`, `rx_woken`, `rx_wait_timed_out`, `rx_wait_stale`, `nat_rebound`). They go through the existing copy-check-commit path: checked against the event budget and, on replay, the recorded model trace before they take effect. View and header reads appear only in the source trace (their responses), since they change nothing.

## Fixtures (§35.3)

`fixtures/warp.ves` has `poll_and_step`, shaped like §35.3: `?` only **before** harvest (`pn_space_left(conns)?`); harvest; split header (recycle on failure); CID lookup (recycle on miss); `decode_packet_number_rfc9000(largest, trunc)` (RFC 9000 §A.3 with 32-bit truncation, no implicit arithmetic); `advance_edt = max(now, prev) + pacing`; `turn_around_tx_in_place(64)`; `submit_paced_tx(next_tx_pn, edt)`; `park_for_ack_or_pto`. `drain` and `serve` are the busy-poll loop (drain the RX ring, then `wait_rx`). It differs from §35.3 in three documented ways: connections are passed by value and returned (see the checker gap below), two named connection tiles stand in for `TiledSeq[QuicConnTile]` (no `tiled(N)` syntax), and the TX packet number is the connection's `next_tx_pn` rather than `full_pn + 1`, so replies to duplicated or reordered datagrams never reuse a packet number. There is no `ConstTime`/`Isa[P]`/AEAD in this slice.

`fixtures/warp_wan.json` is the version 3 scenario: one NIC (64 frames of 2048 bytes, 256 headroom, 32-slot RX ring), a lossy ingress link (GE 2%/25% with 0.1%/50% loss, 0.2% independent loss, 0.5% duplication, 1% reordering by 30 µs, 24-packet bottleneck at 1 Gbit/s), a lossy egress link, two flows (2,400 packets in 16-packet bursts starting at packet number 2^32 − 96, so truncation wraps; 1,200 packets in 40-packet micro-bursts that overflow the buffer), 1% malformed and 1% unknown-CID packets each, and two NAT rebindings.

## Acceptance evidence

`tests/warp.rs` (11 tests) on real checked source, plus 6 new unit tests in `model::link` and `model::umem`.

1. **Bulk ingress with exact conservation.** The full fixture runs 3,600 emitted datagrams (about 3,700 ingress transmissions with duplicates) through burst loss, reordering, duplication, queue overflow and two rebindings. Final accounting is 64 frames, none held by source or leased, no datagram in flight. Independently of the model's own check, the test replays every frame transition from the trace (fill → RX → harvested → recycled, or → staged → leased → parked → retired) and requires each step to be legal and every lease retired once. The source's counters equal what the model delivered: replies = harvested valid datagrams (over 2,000), malformed and unknown-CID counts match, and the decoded largest packet number for each connection equals the largest valid packet number actually harvested, across the 2^32 truncation boundary. Rebinds = 2. GE losses, independent losses, more than 50 queue-overflow drops, duplicates, reorders, ACK and PTO retirements and stale timers all occur; the largest queueing delay is between 20 and 24 service times.
   With 8 frames, a 4-slot RX ring, 30% egress loss and a 5 ms PTO, lost replies pin frames and arrivals are dropped for lack of a fill frame; the trace still conserves all 8 frames.
   A reply paced 1 ms ahead does not occupy a one-packet egress buffer before it launches: a later immediate reply launches and is delivered first. With `max_in_flight: 1`, a delivered reply plus in-flight ingress is a `ModelBudget` error.
2. **Byte-identical replay.** Replaying the full recording reproduces `Recording::to_json()` byte for byte, and the whole report.
3. **A buggy host binding is caught.** With `Session::inject_host_bug(HostBug::DropOnRecycle { occurrence: 1 })` (doc-hidden, test-only) the first recycle forgets its chunk; the step fails with `SessionError::Conservation` ("Missing"), nothing is committed and the recycling task's continuation is retained.
4. **Rejected fixtures.** Appended to the fixture, the existing checker rejects (a) dropping a harvested frame on a branch: `E0302`; (b) `?` while a frame is live: `E0402` "potentially abortable effect 'Fail' may discard live linear value 'frame'" ([T-Let]); (c) using a frame after recycle: `E0303` "use of moved place 'frame'"; and also consuming it on only some paths: `E0309`. None of the three required cases is a checker gap.
5. **Forged, stale and leaked values** at run time: a source-constructed `UmemFrame` literal recycled → `NotHeld`; a view used after recycle → `StaleView`; a task returning a harvested frame as its output → `Conservation`; an oversize `tx_len` → `TxLenOutOfRange`. The lease budget (`max_leases: 1`) refuses the second submit with `ModelBudget`, leaving the staged frame with the task and the owner retained.
6. **Gilbert–Elliott statistics.** Unit test: over 10^6 datagrams for each of three seeds, measured loss, Bad-state share and mean Bad sojourn are within **5% relative** of the stationary values (`πB = g2b/(g2b+b2g)`, loss `πG·lg + πB·lb`; 46,363 ppm for the test chain). Session test: the full fixture's ingress GE loss is within **30% relative** of the configured chain's stationary rate (about 3,700 bursty samples). Independent loss and duplication rates are within 5% over 5·10^5 draws; micro-burst queueing and tail drop, reordering, determinism per seed and clock overflow have exact unit tests.
7. **Changed choices are rejected on replay** (shorter scenario): flipping a recorded GE state, removing a duplicate marker, removing a reorder delay, changing a NAT rebinding target, changing a packet kind and changing an ACK retirement into a PTO each fail with `ModelTraceMismatch` at exactly that index with the owner retained; a truncated trace is rejected; a changed link probability is an identity mismatch (`scenario_sha256`). Re-recording and one-dispatch-at-a-time slicing give identical reports.
8. **Load validation** covers versions, unknown NICs, wrong passing modes, frame structs with missing fields or the wrong mode, oversize flows, process ownership, crash of a NIC process, ppm range, packet numbers at 2^62 and unknown flows in events.
9. **Existing behaviour.** All existing unit, simulation, model and CLI tests pass unchanged, including identity rejection. No real socket, clock, randomness or sleep is used.

## Validation

On the x86_64 Linux cloud host (rustc 1.97.0) with an isolated `CARGO_TARGET_DIR`, `CARGO_INCREMENTAL=0` and debug info off:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test -p vesper-sim-session --locked
cargo test --workspace --locked
cargo run --locked -p vesper-sim-session --bin vesper-sim -- record fixtures/warp.ves fixtures/warp_wan.json rec.json
cargo run --locked -p vesper-sim-session --bin vesper-sim -- replay fixtures/warp.ves fixtures/warp_wan.json rec.json
```

Results are recorded in the PR description. In a debug build the full fixture takes about 30 s per run (record and replay each); the 33 MB recording is not committed.

## Coordinator requests and findings

- **Checker gap (§35.3 signature).** `fn poll_and_step(inout conns: …) -> Result[…] eff [Fail[…]]` cannot use `?` anywhere: `E0402` "potentially abortable effect 'Fail' crosses active inout parameter 'conns'", even before any linear frame exists. §35.3 as written is therefore not admissible today. Either the rule should allow `?` across an `inout` copy parameter whose partial update is acceptable on abort, or §35.3 should pass connection state by value (as the fixture does). Test: `the_checker_rejects_unsound_frame_handling`.
- **No borrowed return types.** §35.2 `view_frame_bytes(in frame) -> ByteSlice` implies the slice borrows the frame. Vesper has no way to tie a returned value's lifetime to an `in` argument, so `ByteSlice` is a copy struct and staleness is caught only dynamically (generation check). A source-level `use of view after recycle` rejection needs a borrow-return rule.
- **No abstract construction.** Source can write `UmemFrame { … }` literals, so frames are forgeable in source; the simulator rejects them at run time. A module-private or opaque constructor would make that a static error.
- **Missing syntax** used by §35: `wire struct`, `tiled(N)`, `TiledSeq`, `ByteArray[N]`, bit operations and `u16`. The fixture avoids them.
- **Suggested global-doc line:** "Source XdpNic and WAN simulation: integrated in `crates/vesper-sim-session` (scenario v3, model semantics `vesper-sim-models-v2`); see `docs/parallel-results/warp-source-simulation.md`."

## Public API changes in this crate

- New modules `model::{link, umem, warp}`; `ModelSpec.warp`; `ModelEvent::Warp`; `ModelSnapshot.warp`; `WARP_MODEL_SEMANTICS`; `scenario::WARP_SCENARIO_VERSION`.
- `Action` gained eight `xdp_*` variants and `Action::warp()`.
- `SessionError::Conservation { task, message }`.
- `Session::inject_host_bug` (doc-hidden, tests only).
- The CLI prints each NIC's final UMEM accounting.

## Remaining gaps

- The packet model is the simulator's own; integration can back it with lane 3's software NIC. No retransmission, congestion control, AEAD, header protection or real QUIC frames.
- ACK and PTO fates are decided at submit from the egress link; ACK-path loss and ACK aggregation are not modelled. Leases retire by timer, not by source.
- One NIC process cannot crash (rejected). Multiple NICs are supported by the model but exercised only by load validation.
- Flow bursts are back-to-back at one instant; there is no per-packet spacing within a burst.
- Evaluator speed bounds the scale: thousands of packets per test, not millions. This is not concolic fuzzing, DPOR or exhaustive search.
