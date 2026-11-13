# Wave-five lane 7: Vesper Chrono (§31)

Claim: #127. Branch `claude/project-thread-1vn5u6`, based on `main` at
`af03122ea74d403f203170926945f9d35786b289`. New crate `crates/vesper-chrono`,
new Lean model `lean/ChronoReplay.lean` with driver
`lean/ChronoReplay/Fixture.lean`.

## What is implemented

| §31 item | Where | Evidence |
| --- | --- | --- |
| 31.2 flight-log wire records | `src/wire.rs` (`vesper-ffi` layouts) | `tests/ring.rs` sizes, offsets, round trips, padding checks |
| 31.3 bounded always-on recorder, 0 Alloc | `src/ring.rs` `FlightRecorder` | `tests/zero_alloc.rs` (counting allocator), `tests/ring.rs` reference model |
| Snapshot epochs over caller state images | `FlightRecorder::capture_snapshot` | `tests/ring.rs`, `tests/replay_simpulse.rs` |
| 31.5 timeline index, O(log N) reverse watchpoint | `src/timeline.rs` | `tests/timeline.rs` against a linear scan, comparisons counted |
| Step back by replay from nearest snapshot | `ReplaySession::step_back`, `state_at` | `tests/replay_simpulse.rs` (every record, snapshot path = full replay = live) |
| 31.1.4 `ObligationLineage` | `src/lineage.rs` | `tests/lineage.rs`, `tests/replay_simpulse.rs`, Lean differential |
| 31.1.5 secret and `@pii` redaction | `src/redact.rs` | `tests/redaction.rs` (bytes absent from the bundle) |
| `.vchrono` bundle, tamper rejection | `src/bundle.rs` | `tests/bundle.rs`, `tests/replay_simpulse.rs` |
| `linear effect ChronoReplay` | `src/replay.rs` `ReplaySession` | `tests/replay_simpulse.rs`, `tests/lineage.rs` |
| SimPulse record and replay | `src/simpulse.rs` | `tests/replay_simpulse.rs` |
| 31.4 Lean replay determinism and lineage completeness | `lean/ChronoReplay.lean` | `tests/lean_model.rs` under `VESPER_LEAN` |

### Public API

`FlightRecorder` (`record_effect`, `record_effect_digest`, `record_inout_write`,
`mint_obligation`, `record_obligation_hop`, `capture_snapshot`, `events`,
`snapshots`), `ChronoTimelineIndex` (`record_inout_write`, `from_writes` with
`IngestOrder`, `find_last_writer_before[_probed]`, `nearest_snapshot`),
`ObligationLineage` (`mint`, `record`, `chain`, `provenance`, `status`,
`open_obligations`, `leaked`, `audit_complete`, `for_truncated_log`),
`RedactionPolicy` (`pii_field`, `secret_region`, `secret_value`), `Bundle`
(`capture`, `export`, `seal`, `open`), `ReplaySession` (`open_bundle`, `seek`,
`reverse_watch_inout`, `query_linear_lineage[_into]`, `provenance`,
`state_at[_from]`, `step_back`, `close`), the `Replayer` trait,
`simpulse::{SimPulseTap, journal_from_bundle, seed_metadata}` and
`Unavailable`.

## Decisions and corrections

- **Wire sizes.** `ChronoEventWireHeader` is 8+8+8+2+2+4 = 32 bytes, as stated.
  The design's `copy struct ObligationHop` (u64, u64, u32, u8) would be 24
  bytes natively with 3 implicit tail padding bytes, which wire layouts
  reject (tested); its wire form `ObligationHopWire` adds an explicit
  `ZeroPad[3]` (24 bytes). `TraceCursor` (u32, u64) has no stated size; its
  wire form is 12 bytes little-endian (it would be 16 natively). All wire
  integers are little-endian byte-aligned, so records sit at any ring offset.
- **Reserved effect ids.** `0xFFF0` inout write, `0xFFF1` obligation hop,
  `0xFFF2` snapshot marker, `0xFFF3` SimPulse scheduler event; user ids at or
  above `0xFFF0` are a typed error. Bit 15 of `op_tag` marks a digest record
  (32-byte BLAKE3 of the outcome), so caller op tags are 15-bit.
- **`ipt_tsc_anchor`** holds the caller-supplied logical time in
  nanoseconds. No TSC/CNTVCT is read (no library code reads a real clock).
- **Ordering** (expansion-37 correction): the recorder assigns consecutive
  Lamport sequences and rejects time going backwards. The timeline index
  rejects non-increasing or duplicate sequences, or with
  `IngestOrder::SortWithReason` stable-sorts first and records an
  `OrderingNote` (reason, moved count). Snapshots must be strictly increasing.
- **Ring.** Records never wrap the end of the byte ring; the tail skips to
  offset zero and the skipped bytes stay used until eviction. Eviction is
  oldest-first by bytes and by record count. Snapshot slots are a separate
  fixed ring of preallocated image buffers.
- **Hop kinds.** Tags 0–3 are the design's (Mint, SinkMove, ChannelHandoff,
  UnpackDischarge); 4 Projection and 5 Leak are added because the lane asks
  for projections and leaks. A mint may name a parent; `provenance` returns
  the ancestor chains first. A truncated log (records evicted) accepts hops of
  pre-window obligations and answers them with `CreationEvicted` instead of
  inventing a chain.
- **Redaction** zeroes `@pii` field ranges (per effect id and op tag), secret
  image regions and every occurrence of registered secret values in payloads,
  images and metadata, records each redaction in the bundle, then scans the
  whole serialized bundle for each secret value and fails with
  `SecretSurvived` if one remains (for example spanning two records). Digest
  records matched by a field rule are zeroed entirely. Replay refuses redacted
  records and redacted snapshot images (`NotReplayable`). Secrets shorter than
  4 bytes are rejected. A digest of a low-entropy outcome can be guessed; that
  is documented, not prevented.
- **Bundle authentication** is keyed BLAKE3 (crate `blake3` 1.8, already in
  the lockfile) over every byte before the MAC, checked before parsing.
  Without the key a modified bundle fails; holders of the key can re-seal, so
  structural validation (consecutive sequences, eviction count, payload sizes,
  hop kinds, padding, snapshot markers, redaction ranges, reserved ids,
  limits, trailing bytes) runs after authentication as well.
- **SimPulse.** The embedder records external I/O outcomes as effect records;
  `SimPulseTap` records the scheduler's journal events. SimPulse exposes the
  committed prefix of a replay journal only through `finish_replay`'s
  `ReplayUnconsumed { remaining }`, which the tap uses.
- **Linear owners.** `ReplaySession` is non-Clone, `#[must_use]`, exits only by
  `close()`; an unclosed drop increments `leaked_sessions()` and panics in
  debug builds. Obligation ids in the recorder are monotonic and hops naming
  unminted ids are rejected. This is host-library discipline; it is not an
  implementation of Vesper's source typing rules for linear values.

## Lean (`lean/ChronoReplay.lean`)

Compiles with Lean 4.24.0, no `sorry`/`admit`/axioms; every theorem's
`#print axioms` is within `propext`, `Quot.sound`, `Classical.choice`
(checked by `tests/lean_model.rs`). Proved, about the models only:

- `chrono_replay_reproduces_live_execution`: for any interaction-tree program
  and live environment, replaying the recorded tape gives the live result,
  consumes exactly the tape and re-records the identical event stream
  (`replay_recorded_prefix`: anything after the tape is returned untouched).
- `replay_ignores_environment`: equal tapes imply equal results.
- `replay_from_snapshot`: folding a partial step function over `pre ++ post`
  equals folding `post` from the state after `pre` (step back from a snapshot).
- `lineage_creation_precedes` / `lineage_complete`: in a log accepted by the
  model checker, every non-mint hop of obligation `i` is preceded by
  `mint i`, so every discharged or leaked obligation has a creation record.
- `reverse_watch_finds_latest_mutation`: for a strictly increasing write list
  `lastBefore` returns a write before `t` with no later write before `t`.

These are not theorems about the Rust code. `tests/lean_model.rs` runs the Lean
lineage checker through `lean/ChronoReplay/Fixture.lean` on 2,000 generated
logs and requires the same accept/reject result and terminal set as the Rust
`ObligationLineage` (371 accepted, 1,629 rejected locally).

## Measured

- Steady state after warm-up: 20,000 rounds of 6 records each plus 313
  snapshots, 0 heap allocations (counting global allocator), with the ring
  overwriting throughout (131,445 records evicted). Debug build, Linux x86-64
  cloud container. No throughput or overhead figure is claimed; the design's
  "<2 percent overhead" is not measured.
- Reverse watchpoint over 50,000 writes: at most 25 sequence comparisons
  (bound 2(⌈log2(n+1)⌉+1) = 34) versus a 50,000-element linear scan, with
  identical answers on 2,000 random queries per size.

## Unavailable (reported by `Unavailable`)

- Intel PT / Arm CoreSight ETM hardware branch tracing: not implemented or
  exercised.
- Native fiber activation and VirtualArena copy-on-write checkpoints
  (userfaultfd, `vm_copy`, `MEM_RESET`): snapshots are caller-provided images.
- Compiled-code interposition on the EvidenceVector: embedders call the
  recorder explicitly.
- `vesper replay` CLI and Lens DAP: integration B (#136).
- Merging several per-shard rings into one bundle: the timeline index can
  ingest sorted-with-reason writes, but bundles hold one recorder.

## Checks

Recorded in the PR description with exact commands and results.
