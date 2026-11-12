# Wave four lane 2: BBRv3 congestion control and EDT pacing

Design coverage: §35.1 item 3 (paced, congestion-controlled transmit), §35.3
(`advance_bbrv3_edt` in the receive/turn-around path), §35.4 theorem 2
(`bbrv3_edt_launch_monotonic`), §35.5 `Bbrv3FlowState`. Issue #38.

- Base: `a5fa024b8d6f62eaa336337d825a2be2ec8972b4` (origin/main).
- Branch: `claude/project-thread-6fi0xp`.
- Algorithm source: **draft-ietf-ccwg-bbr-07**, the latest tagged revision in
  the CCWG repository (`ietf-wg-ccwg/draft-ietf-ccwg-bbr`, tag
  `draft-ietf-ccwg-bbr-07`, file `draft-ietf-ccwg-bbr.md`). The pseudocode
  was transcribed function by function; names in the Rust follow the draft.

## What was built

New crate `crates/vesper-warp-bbr` (std only, `#![forbid(unsafe_code)]`, no
internal or external dependencies) and `lean/WarpBbr.lean` with its
executable fixture `lean/WarpBbr/Fixture.lean`.

### Public API

- Contract vocabulary (`docs/rfcs/wave-four-contracts.md`), named exactly:
  `SentPacket`, `AckedPacket`, `LostPacket`, `RttSample`,
  `CongestionEvent::{Acked, Lost, EcnCe, PersistentCongestion, AppLimited}`.
  `CongestionEvent::Acked` carries `&[Delivered]` (packet plus snapshot) and
  an optional `RttSample`; `Lost` carries the packet and its snapshot.
- Lane-owned rate-sample state: `PacketDeliveryState { delivered,
  delivered_ns, first_sent_ns, is_app_limited, tx_in_flight, lost, counted }`,
  returned by `on_packet_sent` and handed back on ACK or loss. Integration
  stores it next to lane 1's sent-packet record.
- `Bbrv3` (alias `Bbrv3FlowState`), `BbrConfig { smss, initial_cwnd,
  has_selective_acks, offload, probe_seed }` with `BbrConfig::quic(smss)`
  (RFC 9002 initial window), `BbrState`, `RateSample`, `BbrError`.
- Inputs: `on_packet_sent` (guarded), `on_probe_packet_sent` (unguarded, for
  PTO probes), `on_ack`, `on_packet_lost`, `on_ecn_ce`,
  `on_persistent_congestion`, `on_app_limited`, `on_spurious_loss_detected`,
  and `on_event(now, CongestionEvent)`.
- Contract outputs: `cwnd_bytes() -> u32`, `pacing_rate_bytes_per_sec() ->
  u64`, `schedule_edt_ns(bytes: u32, now_ns: u64) -> u64`. Also
  `target_cwnd_bytes(mss) -> u64`, `next_edt_ns`, `can_send`,
  `send_quantum_bytes`, and read-only model getters (`state`, `max_bw`, `bw`,
  `min_rtt_ns`, `bdp_bytes`, `inflight_hi/lo`, `bw_lo`, gains, counters).
- Pure functions mirrored in Lean: `pacing::{pacing_delay_ns, edt_step,
  clamp_target_cwnd, send_allowed}`.

### Algorithm coverage (draft -07)

Startup, Drain, ProbeBW DOWN/CRUISE/REFILL/UP, ProbeRTT; full-pipe plateau
detector (3 rounds, 25%); Startup high-loss exit (6 loss events, 2%, in
recovery; any loss without selective ACKs); two-slot `max_bw` filter on
`cycle_count`; `min_rtt` with the 10 s window and ProbeRTT scheduling (200 ms
and one round, `probe_rtt_cwnd`, exit to CRUISE or Startup); delivery-rate
sampling (`OnPacketSent`, `UpdateRateSample`, `GenerateRateSample`,
app-limited bubbles); packet-timed rounds; `extra_acked` (Nichols windowed
max, 1 round in Startup, 10 after); offload budget (QUIC or TCP); long-term
model (`inflight_longterm` = draft's name for `inflight_hi`, raised in UP
with the doubling slope, cut by `HandleInflightTooHigh` with `InflightAtLoss`
and `Beta`); short-term model (`bw_shortterm`/`inflight_shortterm` = `bw_lo`/
`inflight_lo`, Beta cut once per loss round outside probing); precautionary
probing (both halves); Reno-coexistence and randomized 2–3 s probe wait
(seeded SplitMix64); restart from idle; spurious-loss undo; RTO cwnd
(`inflight + SMSS`); pacing rate with 1% margin and the "only raise before
full pipe" rule; send quantum; `SetCwnd`, `BoundCwndForProbeRTT`,
`BoundCwndForModel`, `MinPipeCwnd = 4·SMSS`.

### Decisions and documented deviations

- **Integer arithmetic.** Bytes `u64`, rates bytes/s `u64`, time ns `u64`,
  gains per mille (`2.77 → 2770`, `0.7 → 700`, `2% → 20`). Products in
  `u128`, saturating to `u64::MAX`; counter differences saturate at 0;
  `INFINITY = u64::MAX`. cwnd is clamped to `u32::MAX` (contract window
  type). Cumulative delivered/lost counters use checked addition and reject
  overflow; the CE counter saturates (documented: 2^64 bytes is unreachable).
- **Zero rate / zero RTT.** `pacing_delay_ns(_, 0) = u64::MAX` (infinitely
  slow); the controller floors its pacing rate at 1 B/s so it never paces
  at zero. An unknown `min_rtt` uses `InitialCwnd` for every BDP multiple,
  as the draft says. A zero-length rate interval gives no rate sample.
- **EDT.** `launch = max(now, next_edt)`, `next_edt = launch +sat
  ceil(bytes·10⁹ / pacing_rate)`. The ceiling means pacing never exceeds the
  rate; it is slower by under 1 ns per packet. Any `now` order is accepted.
- **`target_cwnd_bytes` returns `u64`** so that the §35.5 clamp
  `[4·MSS, max(inflight_hi, 4·MSS)]` is exact for every `u32` MSS.
- **Loss recovery.** The contract has no recovery entry/exit events, so the
  controller follows RFC 9002 §7.3.2: the first loss starts recovery
  (`OnEnterFastRecovery`), and an ACK for a packet sent after that point ends
  it (`RestoreCwnd`). The ACK that ends recovery is processed by the model
  with recovery still in effect, then cwnd is restored and re-bounded; this
  keeps `CheckStartupHighLoss` reachable when the loss round and the
  recovery period end on the same ACK, which is common under QUIC rules.
- **ECN.** The draft only requires CE to be treated as congestion. Each new
  CE mark counts as one loss event and as one SMSS of congested data in the
  loss-rate test (`RS.lost` covers lost plus CE bytes), so CE drives the
  short-term model and the Startup exit like loss (RFC 8311 permits an
  identical response). Because marks arrive with ACKs rather than as loss
  events, the probing-phase `HandleInflightTooHigh` also runs on an ACK whose
  rate sample includes new marks (as Linux BBRv3 does on ACK). `EcnCe.count`
  is the increase in the peer's CE count; deliver it before the same ACK's
  `Acked` event.
- **Reno-coexistence rounds** are the draft's `TargetInflight()` in packets
  (bytes divided by SMSS), capped at 63.
- **Pure-ACK packets** (`in_flight: false`) are never counted in flight,
  delivered or lost.
- **Send guard.** `on_packet_sent` refuses an in-flight send when
  `bytes_in_flight >= cwnd` (`BbrError::CwndFull`); PTO probes use
  `on_probe_packet_sent` and are outside the Lean bound.
- **Typed errors, no partial updates.** Time going backwards, packet numbers
  `>= 2^62`, empty in-flight packets, ACK before send, releasing more than is
  in flight, snapshots this controller did not issue, and counter overflow
  are rejected with every check done before any mutation; the property test
  asserts that a rejected call leaves the `Debug` state byte-identical.
- **ACKs that deliver nothing counted** (only pure ACKs) update nothing in
  the model.
- The draft's `C.min_rtt` (lifetime) and `C.srtt` come from `RttSample`;
  pacing is re-initialised from the first non-zero `smoothed_ns` while no
  bandwidth sample exists (draft `InitPacingRate`).

## Lean (`lean/WarpBbr.lean`)

Model id `vesper.warp.bbr.pacing-model/1`. Proved about the Lean
transcription of `src/pacing.rs` (over `Nat` with explicit `u64` saturation),
with only `propext`, `Classical.choice`, `Quot.sound` (10 `#print axioms`
lines, checked by the test), and no `sorry`, `admit`, `native_decide` or new
axioms:

- `bbrv3_edt_launch_monotonic`: `launch ≥ now`, `launch ≥ prev`,
  `next ≥ launch`, `next ≥ prev`, both `≤ u64::MAX`, including saturation.
- `edtRun_ascending`: for any list of `(now, delay)` requests in any order,
  the launch sequence is non-decreasing.
- `pacingDelay_rounding`: for `rate > 0` and `bytes < 2^32`,
  `bytes·10⁹ ≤ delay·rate < bytes·10⁹ + rate` (exact ceiling, never
  saturated); `pacingDelay_zero_rate`.
- `targetCwnd_bounds`: `4·mss ≤ target ≤ max(hi, 4·mss)`.
- `send_respects_cwnd` / `sendTrace_bound`: in a transition system of sends,
  releases and arbitrary window changes, every send the guard accepts with
  `bytes ≤ maxPkt` leaves `inflight < cwnd + maxPkt`. Per the PR #36 review
  correction this is a statement about send transitions only:
  `global_bound_fails` proves there is no global `inflight ≤ cwnd + maxPkt`
  invariant (a window cut breaks it), and `cut_blocks_sends` proves that
  after a cut to `c ≤ inflight` the guard refuses every send.

Not proved: anything about the Rust code (the differential test compares
behaviour on inputs only), the BBRv3 state machine, rate sampling, or
hardware pacing.

`crates/vesper-warp-bbr/tests/lean_model.rs` always audits the source; with
`VESPER_LEAN` set it compiles the model from `lean/` (re-checking every
proof), validates the axiom report, and runs the fixture on 1,642 cases
(edge values and seeded random values, including `0`, `u32::MAX`,
`u64::MAX` and saturation) against `pacing_delay_ns`, `edt_step`,
`clamp_target_cwnd` and `send_allowed`.

## Tests

- 34 unit tests (`src/**`): every state transition (Startup→Drain on
  plateau; Drain→ProbeBW on inflight and on the 3-round limit;
  DOWN→CRUISE→REFILL→UP→DOWN; one-round REFILL; `cycle_count` advance;
  ProbeRTT entry after the 10 s window, cwnd bound, exit after 200 ms plus a
  round to CRUISE, exit to Startup when the pipe was never full); Startup
  high-loss exit with and without selective ACKs and the 6-event minimum;
  loss above threshold in UP (`inflight_hi`, DOWN, undo state) and the exact
  2% boundary; Beta cut of the short-term model while cruising; slope growth
  of `inflight_hi`; precautionary probing; ECN-CE in UP and in CRUISE;
  persistent congestion; spurious-loss undo to Startup and to REFILL;
  app-limited samples and idle restart pacing at `bw·0.99`; `InflightAtLoss`;
  error cases; pure-ACK packets; contract dispatch.
- `tests/simulator.rs` (7 tests) on a deterministic packet-level link
  simulator (`tests/sim/mod.rs`: rate, propagation delay, finite tail-drop
  buffer, seeded random and Gilbert–Elliott burst loss, optional CE marking,
  RFC 9002-style packet/time-threshold loss detection): Startup exits on the
  plateau with no drops in under 0.8 s and `max_bw` within 5% of the link;
  ProbeRTT entered just after 10 s, held 200–400 ms, repeated ~10 s later,
  cwnd capped near BDP/2; steady-state goodput ≥ 93% of the bottleneck with
  mean queue ≤ BDP/2 and max ≤ 1.25 BDP at 2 Mbit/s·100 ms,
  10 Mbit/s·40 ms and 100 Mbit/s·10 ms; shallow buffer and 5% random loss set
  `inflight_hi` (bounded near BDP + buffer, fewer drops later); CE marking
  bounds `inflight_hi` and the queue without drops; seeded burst-loss replay
  is deterministic; an app-limited idle second keeps `max_bw`.
- `tests/properties.rs` (seeded, no external crate): EDT never regresses
  across 320,000 arbitrary `(now, bytes)` interleavings plus 100,000 pure
  steps; pacing equals `bytes·10⁹/rate` within ceiling rounding and a 10,000
  packet burst spans the ideal time within 1 ns per packet; the clamp holds;
  48 random event sequences (invalid inputs included) never panic, keep
  in-flight accounting exact, keep every guarded send under
  `cwnd + packet`, and leave state unchanged on rejection.
- `tests/zero_alloc.rs`: a counting global allocator observes **zero**
  allocations over 200,000 warmed-up steady-state iterations of
  `schedule_edt_ns`, `on_packet_sent`, `on_ack`, `on_packet_lost` and
  `on_ecn_ce`, spanning ProbeBW phases and a ProbeRTT interval. (The test
  harness's allocator is the only `unsafe` code, in the test crate.)

## Checks

Run in this cloud session (Lean 4.24.0 release binary, as pinned by
`lean-toolchain`):

```text
cargo fmt --all -- --check                                         ok
cargo clippy -p vesper-warp-bbr --locked --all-targets -- -D warnings  ok
VESPER_LEAN=lean cargo test -p vesper-warp-bbr                      ok (48 tests)
lean lean/WarpBbr.lean   (from lean/)                               ok, 10 axiom lines, standard axioms only
cargo clippy --workspace --locked --all-targets -- -D warnings      see PR
VESPER_LEAN=lean cargo test --workspace --locked                    see PR
```

## Root patch

One separate commit: `crates/vesper-warp-bbr` added to root `members` and
`default-members`, and the regenerated `Cargo.lock` (one new package entry,
no dependency changes). No new `[workspace.dependencies]`, no CLI, CI or
global-doc edits.

## Not claimed / remaining gaps

- No fairness, coexistence or interoperability claim with Linux BBRv3 or any
  other implementation, and no real-network performance claim: the simulator
  validates the modelled algorithm on an idealised single-flow path.
- No hardware or kernel pacing (`SO_TXTIME`, NIC EDT, fq): EDT timestamps are
  computed only. Not exercised, reported unavailable.
- Rust ownership and these tests are not evidence that Vesper source typing
  rules for §35 are implemented. This crate has no linear obligations of its
  own; `InFlightQuicPkt` and frames belong to lanes 1, 3 and 4.
- Integration (post-wave, `crates/vesper-warp`): map lane 1's sent/acked/lost
  records to these events, store `PacketDeliveryState` per packet, feed
  `schedule_edt_ns` into `submit_paced_tx`, and mirror `cwnd`,
  `bytes_in_flight`, `bw` and `min_rtt` into `QuicConnTile`. The TCP lane
  needs the same adapter with `OffloadModel::Tcp` and its own SACK flag.
- Multi-flow simulation (several BBR flows sharing a bottleneck) and ACK
  decimation/aggregation scenarios are not covered by the simulator.
