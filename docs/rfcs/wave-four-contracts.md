# Fourth-wave (Vesper Warp, §35) integration contracts

Baseline: `a5fa024b8d6f62eaa336337d825a2be2ec8972b4` or a descendant. This is an
ownership and interface agreement so eight lanes can run at once. It is not a
soundness theorem, and it freezes nothing in the existing compiler.

## Dependency rule

Every lane depends only on crates already on `main`. Allowed internal edges:

| Lane crate | May depend on |
| --- | --- |
| `vesper-warp` (1) | `vesper-ffi` |
| `vesper-warp-bbr` (2) | none |
| `vesper-warp-umem` (3) | `vesper-ffi`, `vesper-alloc` |
| `vesper-warp-wheel` (4) | none |
| `vesper-warp-crypto` (5) | none |
| `vesper-warp-shard` (6) | `vesper-simd`, `vesper-alloc` |
| `vesper-warp-tcp` (7) | `vesper-ffi` |
| `vesper-sim-session` (8) | its existing dependencies |

No lane imports another wave-four crate. Cross-lane wiring is the post-wave
integration step in `crates/vesper-warp`, which may then depend on all of them.
Lanes must not edit the read-only crates they depend on.

## Shared vocabulary

Lanes define these types locally with exactly these names and meanings, so
integration writes thin adapters instead of reconciling semantics.

- **Time**: `u64` nanoseconds on a caller-supplied virtual or monotonic clock.
  No lane reads a real clock in library code.
- **Packet numbers**: `u64` with value `< 2^62`. Packet-number spaces are
  `Initial`, `Handshake`, `ApplicationData`.
- **Sizes**: `u32` bytes for packet and window sizes, `u64` for cumulative
  delivered/sent byte counters.
- **Congestion-control events** (lanes 1, 2, 7):
  - `SentPacket { pn: u64, sent_ns: u64, bytes: u32, ack_eliciting: bool, in_flight: bool }`
  - `AckedPacket { pn: u64, sent_ns: u64, bytes: u32 }`
  - `LostPacket { pn: u64, sent_ns: u64, bytes: u32 }`
  - `RttSample { latest_ns: u64, min_ns: u64, smoothed_ns: u64, ack_delay_ns: u64 }`
  - `CongestionEvent::{Acked{..}, Lost{..}, EcnCe{ count: u64 }, PersistentCongestion, AppLimited}`
  - A controller exposes `cwnd_bytes() -> u32`, `pacing_rate_bytes_per_sec() -> u64`
    and `schedule_edt_ns(bytes: u32, now_ns: u64) -> u64`.
  Rate-sample delivery state (delivered, delivered_ns, first_sent_ns,
  is_app_limited) is owned by lane 2 and attached per packet at integration.
- **Packet-protection boundary** (lanes 1, 5): after header parsing, lane 1
  reports `pn_offset: usize` (start of the protected packet-number field) and
  the header length; lane 5 removes header protection in place, returns the
  pn length (1–4) and truncated value, and opens the payload with the header
  as AAD. Lane 1 decodes the full packet number afterwards.
- **UMEM frames and leases** (lanes 3, 4, 8): a frame is identified by
  `chunk_idx: u32`; a lease carries `pkt_num_full`, `edt_departure_ns` and the
  owned TX frame. Lane 4's wheel is generic over the payload, so it never
  names lane 3's types.

## Linearity

Section 35's `linear` values are modelled in Rust as non-Clone, non-Copy,
`#[must_use]` owners with documented consuming exits. A dropped owner is
detected and reported (debug panic or counted leak), never silently recycled.
Failed consuming operations return the owner. This is host-library discipline,
not an implementation of the source typing rules; lane 8 is the only lane that
exercises the Vesper checker, and it records any checker gap as a coordinator
request instead of editing the checker.

## Evidence and claims

- "0 Alloc" requires a counting-allocator test over the warmed-up steady
  state.
- AF_XDP, DPDK, NIC EDT/SO_TXTIME, RSS programming and crypto offload are
  hardware or kernel features. Report them as unavailable unless executed.
- Lean files compile with no `sorry`, `admit` or new axioms, and are run from
  the owning crate's tests under `VESPER_LEAN`. Each report states exactly
  which statement is proved about which model, and that it is not a proof
  about the Rust code.
- Unsafe Rust is forbidden except in lane 3's gated AF_XDP module and lane 6's
  `std::arch` module, each isolated and commented.

## Root and coordinator ownership

Each lane may commit only its own root registration patch (members,
default-members, new `[workspace.dependencies]`, `Cargo.lock`) on its PR
branch. CLI wiring, CI workflows, global docs, `src/lean*.rs`,
`crates/vesper-lean-syntax/**` and `crates/vesper-loom-tuf/**` stay with the
coordinator. Report paths under `docs/parallel-results/warp-*.md` are
exclusive to their lane.
