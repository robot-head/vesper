# Vesper Warp timing wheel and EDT launch queue

Wave-four lane 4 (design §35.1 item 3, §35.2 `park_for_ack_or_pto`). Issue #40.
Base: `origin/main` at `a5fa024b8d6f62eaa336337d825a2be2ec8972b4` (the required
baseline itself). Branch: `claude/project-thread-op565j` (the session's
assigned branch; the prompt's suggested name was `wave4/warp-timing-wheel`).

Changed paths: new `crates/vesper-warp-wheel/**`, this report, and the root
registration patch (in its own commit). No internal dependencies and no
external dependencies, including dev-dependencies.

## Root patch

`Cargo.toml`: `crates/vesper-warp-wheel` added to `members` and
`default-members`. `Cargo.lock`: the new package entry only. No new
`[workspace.dependencies]`, no CLI, CI or global-doc edits.

## Public API

- `wheel`: `Wheel<T>` (`new`, `insert`, `cancel`, `deadline`, `advance`,
  `poll`, `next_wake_hint_ns`, `drain`, `len`, `is_empty`, `capacity`,
  `tick_ns`, `now_ns`), `WheelConfig` (`tick_ns`, `slot_bits` 1..=6, `levels`,
  `capacity`, `start_ns`; `with_capacity`), `TimerHandle` (non-`Clone`,
  `#[must_use]`, carries wheel id + slot + `u64` generation), `Expired<T>`,
  `InsertError<T>` / `InsertErrorKind {Full, DeadlineOutOfRange}`,
  `HandleError {Foreign, Stale}`, `AdvanceError {Backwards, OutOfRange}`,
  `WheelConfigError`, `MAX_TIME_NS` (`u64::MAX - 1`), `MAX_CAPACITY`,
  `leaked_timer_entries()`.
- `leases`: `RetransmitLeases<L>` (`new`, `park`, `on_ack`, `rearm`,
  `poll_pto`, `discard_space`, `drain`, `contains`, `len`, `next_wake_hint_ns`),
  `PacketNumberSpace {Initial, Handshake, ApplicationData}`, `LeaseKey`,
  `PtoExpired<L>`, `ParkError<L>` / `ParkErrorKind {PacketNumberOutOfRange,
  Duplicate, Full, DeadlineOutOfRange}`, `LeaseError {NotOutstanding,
  DeadlineOutOfRange}`, `MAX_PACKET_NUMBER` (2^62).
- `edt`: `EdtLaunchQueue<P>` (`new(config, horizon_ns)`, `submit(now, edt,
  packet)`, `poll_release`, `drain`, `last_released_ns`, `next_wake_hint_ns`),
  `Launch<P>`, `SubmitError<P>` / `SubmitErrorKind {BeyondHorizon,
  BehindReleased, ClockBackwards, OutOfRange, Full}`.
- `clock`: `Clock` trait, `VirtualClock` (`starting_at`, `advance_to`,
  `advance_by`), `ClockError`.

## Design decisions

- **Structure.** Varghese–Lauck hierarchical hashed wheel: `levels` rings of
  `2^slot_bits` slots; an entry sits at the level of the highest bit where its
  tick differs from the cursor, and entries beyond `levels * slot_bits` bits go
  to an overflow list. The overflow list keeps a lower bound of its earliest
  tick and is re-placed only when the cursor reaches that block, so a deadline
  2^60 ticks away costs one re-placement, not 2^60/span visits. Per-level `u64`
  occupancy bitmaps let `advance` jump directly to the next tick with work.
- **Exact firing.** `poll(now)` yields exactly the entries with
  `deadline_ns <= now`: when a tick opens, its level-0 slot is merge-sorted in
  place by `(deadline_ns, insertion seq)` onto a ready list, and the head is
  only released once its own deadline is reached. Nothing fires early and
  nothing due is held back to a tick boundary. Order is non-decreasing
  deadline across the whole wheel, FIFO for equal deadlines.
- **Costs.** Insert O(1), except a deadline inside an already-opened tick
  (late or same-tick insert), which is merged into the sorted ready list from
  its tail (O(r) in entries still waiting in opened ticks). Cancel O(1) via
  intrusive doubly linked lists. Advance is amortized O(1) per entry per level
  plus O(k log k) in-place sorting when a tick with k entries opens.
- **Time range.** Ticks are absolute `u64` and never wrap below `MAX_TIME_NS`;
  slot indices wrap on every rotation. `u64::MAX` is reserved so the cursor is
  always representable; larger deadlines or clock values are typed errors.
- **Linear payloads.** Every refusal (`Full`, out of range, duplicate key,
  beyond horizon, behind released, clock backwards) returns the payload. A
  wheel dropped non-empty adds to `leaked_timer_entries()` and panics in debug
  builds unless already unwinding; `drain` empties it explicitly. Handles are
  references, not owners: dropping one leaves the timer armed.
- **Lease index.** Fixed-size open addressing (linear probing,
  backward-shift deletion, load <= 1/2) with a SplitMix64 mix of
  `(space, pn)`. Only our own sent packets are inserted, so a peer's ACKs can
  probe but not shape clusters. Duplicate or unknown ACKs are
  `LeaseError::NotOutstanding`, never success.
- **EDT queue.** Rejects `edt > now + horizon` (model of the `SO_TXTIME`/fq
  horizon) and `edt` earlier than the last released departure (accepting it
  would break non-decreasing release order). An EDT already in the past, but
  not behind the last release, departs on the next poll.

## Acceptance evidence

All tests are deterministic (seeded SplitMix/LCG generators); no real time,
sleeps or threads.

- `tests/model.rs`: the wheel against a `BTreeMap<(deadline, seq), id>`
  reference over 7 shapes × 40 seeds × 2,000 random insert/cancel/advance
  steps. Shapes include 1-bit and 2-bit rings (heavy cascading), single-level
  wheels (everything beyond 2 ticks overflows), deadlines far beyond the span,
  past and same-tick deadlines, large jumps, start times at `2^63` and within
  `2^20`/`2^40` of `u64::MAX`, and capacity exhaustion. Each poll's fired
  sequence must equal the reference exactly; stale handles must be rejected;
  `next_wake_hint_ns` must never exceed the true earliest deadline; the final
  `drain` must return the reference's remaining set. Mutation check: removing
  the per-tick sort, or delaying the overflow re-placement by one tick, makes
  this test fail; suppressing a level's cascade makes it hang.
- `tests/alloc.rs`: thread-local counting global allocator (test-only
  `unsafe impl GlobalAlloc`; the library is `#![forbid(unsafe_code)]`). After
  warm-up, 2,000 steps of wheel insert/cancel/poll with near, mid and overflow
  deadlines, and 3,000 steps of lease park/ACK/PTO plus EDT submit/release,
  perform zero allocations. Passes in debug and `--release`.
- `tests/wheel.rs`: stale handles after cancel, after fire and after slot
  reuse; foreign handles; `Full` and out-of-range return the payload;
  clock-backwards and out-of-range rejection; config validation; a 2^60-tick
  jump with an overflow entry; a drained wheel drops without a leak report,
  while a non-empty drop is counted and panics in debug builds (one test,
  because the counter is process-global).
- `tests/layers.rs`: leases use a linear stand-in that panics if dropped
  unconsumed. ACK removes leases, duplicate/unknown ACKs are errors, PTO fires
  the rest in deadline order, `rearm` moves a deadline, park refusals return
  the lease, and `discard_space` keeps the index consistent with a model set
  under 2,000 churn steps. The EDT queue over 5,000 random steps never
  releases early or out of order, and horizon/order/clock refusals return the
  packet; equal EDTs leave FIFO.

## Commands

```sh
cargo fmt --all -- --check
cargo test -p vesper-warp-wheel                # 17 tests + 1 doctest
cargo test -p vesper-warp-wheel --release
cargo clippy -p vesper-warp-wheel --all-targets -- -D warnings
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
```

Results are recorded in the PR description.

## Limits and gaps

- Software scheduling model only. No kernel timer, `SO_TXTIME`/NIC launch-time
  offload or interrupt-free pacing is used or claimed.
- Rust ownership is not evidence that Vesper's source-level linear typing
  rules (`sink`/`linear` in `park_for_ack_or_pto`) are implemented.
- No Lean model (optional for this lane; no Lean toolchain in this session).
  The model test is a differential test, not a proof.
- The ready-list merge for late/same-tick inserts is O(r), not O(1); this is
  documented rather than hidden. The lease index uses a fixed hash, which is
  adequate because peers cannot insert keys.
- Integration (wiring leases to lane 3's `InFlightQuicPkt` and the EDT queue to
  lane 2's `schedule_edt_ns`) is the post-wave step in `crates/vesper-warp`.
