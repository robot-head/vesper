# Wave four lane 1: QUIC packet core and RFC 9002 loss recovery

Prompt: [01-quic-packet-core.md](../agent-prompts/wave-four/01-quic-packet-core.md).
Issue: #37. Base: `a5fa024b8d6f62eaa336337d825a2be2ec8972b4` (origin/main).
The branch is `claude/project-thread-jt2x7l`, and the PR carries the commit SHAs.

## What was delivered

`crates/vesper-warp` is a new sans-IO crate with `#![forbid(unsafe_code)]`. It
has no sockets, no crypto and no congestion controller, and its only internal
dependency is `vesper-ffi`, used read-only. `lean/WarpPacketNumber.lean` and
`lean/WarpPacketNumber/Fixture.lean` hold the packet-number model.

| Module | Covers |
| --- | --- |
| `varint` | RFC 9000 §16 encode/decode; non-minimal encodings accepted on decode; typed truncation/overflow errors; `Reader`/`Writer` cursors |
| `packet_number` | A.2 shortest-length encoding (`encoded_len`, `encode`), A.3 decoding as §35.5 `decode_quic_packet_number_rfc9000(largest, truncated, nbits)` plus `decode_with_largest(Option<u64>, ..)` for an empty space; `pn_nbits ∉ {8,16,24,32}`, over-wide truncated values, `largest ≥ 2^62` and results `≥ 2^62` are typed errors |
| `header` | `HeaderParser::new(dcid_len)` compiles the §35.2 `QuicShortHdrWire` (`u8`, `ByteArray[n]`, `u32be`; 13 bytes for n = 8) and the long-header prefix through `vesper-ffi`; RFC 9000 §17.2 Initial/0-RTT/Handshake (token, Length, coalescing via `packet_end`), Retry (token + 16-byte integrity tag) and Version Negotiation; serializers for short/long/Retry/VN |
| `frame` | Zero-copy parse/encode of PADDING (runs), PING, ACK/ACK_ECN (validated ranges, lazily decoded without allocation), CRYPTO, STREAM 0x08–0x0f, MAX_DATA, MAX_STREAM_DATA, CONNECTION_CLOSE 0x1c/0x1d, HANDSHAKE_DONE; Table 3 permissions per packet type; `scan_payload` gives ack-eliciting/in-flight |
| `ack` | `ReceivedPackets<N>`: fixed-array range set (no heap), duplicate detection, bounded ACK frames (≤ N ranges), ECN counts, ack-delay encoding, RFC 9000 §13.2 immediate/delayed ACK deadline, ack-of-ack pruning |
| `recovery` | RFC 9002 `RttEstimator` (§5), `LossRecovery` (A.5 sent, A.7 ACK, A.8 timer/PTO, A.9 timeout, A.10 packet and time thresholds, §6.4 space discard), §7.6 persistent congestion, ECN-CE increases; `CongestionEvent` sink with lost-before-acked order |

### Receive order and the packet-protection boundary

The coordinator's correction from the PR #36 review is followed. On receive:

1. `HeaderParser::parse(datagram)` reads only the clear header bytes. It
   returns a `ProtectedHeader` made of plain offsets that borrow nothing:
   `pn_offset`, the 16-byte `sample()` range, `first_byte_mask()` (0x1f for
   short headers, 0x0f for long), the DCID/SCID/token spans and `packet_end`.
   Because nothing is borrowed, lane 5 can take `&mut` on the buffer and
   remove header protection in place.
2. Lane 5 removes header protection.
3. `ProtectedHeader::packet_number_field(&packet)` reads the unprotected
   first byte. It rejects nonzero reserved bits and returns the `TruncatedPn`
   (length 1–4 and value), the key phase, `header_len` (the AEAD AAD is
   `packet[..header_len]`) and the payload span.
4. `TruncatedPn::decode(largest)` reconstructs the full packet number.
5. Lane 5's AEAD open takes the full packet number, since RFC 9001 §5.3 builds
   the nonce from it.
6. `frame::scan_payload` / `frame::frames` run on the opened payload.

The 4-byte `pkt_num_trunc` field of `QuicShortHdrWire` is the protected
window. The unprotected first byte says how many of its bytes, 1 to 4, are the
packet number; the rest belong to the payload. Parsing requires
`pn_offset + 4 + 16 ≤ packet_end`, so the sample is always in bounds.

### Wave-four vocabulary

`SentPacket`, `AckedPacket`, `LostPacket`, `RttSample` and
`CongestionEvent::{Acked{packet}, Lost{packet}, EcnCe{count}, PersistentCongestion, AppLimited}`
are defined locally with the contract's names. Time is caller-supplied `u64`
nanoseconds, and no clock is read. Packet numbers are `u64 < 2^62`, and
`PnSpace` is `Initial | Handshake | ApplicationData`. Acked and Lost events are
emitted only for in-flight packets. `EcnCe.count` is the increase in the
peer-reported CE count. `AppLimited` belongs to the shared vocabulary, but the
sender emits it, not recovery. The controller methods (`cwnd_bytes`, ...)
belong to lane 2.

## Lean: what is proved

`lean/WarpPacketNumber.lean` (`vesper.warp.packet-number.a3-model/1`) defines
`decode expected truncated nbits : Option Nat` over `Nat`. It follows the
A.3 branch structure, with the two signed comparisons rewritten so that no
subtraction truncates:

- `candidate <= expected - hwin` becomes `candidate + hwin ≤ expected`.
- `candidate < 2^62 - win` becomes `candidate + win < 2^62`.

The Rust code uses the same forms. Proved, using only core Lean (`omega`,
`simp`, `decide`, no `sorry`, no axioms beyond `propext`, `Classical.choice`
and `Quot.sound`):

- `decode_congruent`: `decode e t n = some r → r % 2^n = t`.
- `decode_bound`: `decode e t n = some r → r < 2^62`.
- `decode_window`: for valid `n`, `e ≤ 2^62`, `e < p + 2^n/2`,
  `p ≤ e + 2^n/2` and `p < 2^62`, `decode e (p % 2^n) n = some p`.
- `decode_rejects_invalid_bits`.
- `rfc_example`: `0xa82f30ea` with 16-bit `0x9b32` gives `0xa82f9b32`.

These are theorems about the Lean model, **not about the Rust code**.
`tests/lean_model.rs` checks the Rust decoder against the executable model,
and only on concrete inputs. It runs under `VESPER_LEAN`, covers about 3,300
boundary, random and invalid cases, and audits the axiom report and the source
(no `sorry`/`admit`/`native_decide`/`partial`/`axiom`). Without `VESPER_LEAN`
it prints a skip.

## Tests (`cargo test -p vesper-warp`)

- RFC vectors: §16 varint examples (8, 4, 2 and 1 byte, plus the non-minimal
  `0x4025`), the A.2 examples (`0xac5c02` needs 16 bits and `0xace8fe` needs
  24) and the A.3 example.
- Packet-number properties: 200k random (largest, true pn) windows checked for
  recovery, congruence and the bound, plus agreement with a signed
  i128 transcription of A.3. Boundary values include 0, window edges and
  2^62 − 1. An encode → decode round trip runs over 100k cases where the peer's
  largest lies anywhere in [largest_acked, pn).
- Headers: the 13-byte layout; HP removal simulated with an XOR mask, then
  decode. Also covered: configured DCID lengths 0, 1, 8 and 20; long headers
  with coalescing; a Retry; a VN with 30-byte CIDs; reserved bits; the fixed
  bit; unsupported versions; Length overrun; sample shortfall.
- Frames: round trip and byte-identical re-encode for every supported frame;
  all eight STREAM type bits; ACK wire-range arithmetic and malformed ranges;
  the full Table 3 matrix over 4 packet types; unknown, unsupported and
  non-minimal types; offset overflow; empty payload.
- Truncation and fuzzing: every truncation point of every header and frame
  sample is a typed error. The only exceptions are length-less VN and Retry
  packets, which legitimately shorten, and the Retry integrity tag catches the
  Retry case. 200k random or mutated header inputs and 300k frame inputs ran
  with no panics, and successfully parsed frames re-encode.
- ACK tracking: merging, duplicates, a 120-packet set model, the range bound
  with a floor, ack-of-ack pruning, delayed-ACK policy, ECN and ack delay.
- Recovery traces:
  - Ten packets 1 ms apart, ACK of pn 9 at 50 ms: pn 0–6 are lost by packet
    threshold, then pn 7 and 8 by time threshold at exactly
    `sent + 46.125 ms`.
  - Time threshold alone.
  - PTO of `333 + 4·166.5 + 25` ms, then ×2 and ×4 backoff, reset on ACK.
  - Application-space PTO waits for confirmation; the anti-deadlock Initial
    probe; the amplification-limit cancel.
  - Persistent congestion is detected over 380 ms ≥ 165 ms, not detected over
    100 ms, and not detected when split by an acknowledged packet (it is still
    detected when one side alone spans long enough).
  - RTT rules: first sample, ack-delay subtraction guard, Initial ignoring
    delay, `max_ack_delay` cap, u128 arithmetic for extreme samples.
  - Typed errors: ACK of an unsent packet (no state change), non-increasing
    pn, clock going backwards, pn ≥ 2^62, space discarded, tracking bound.
- Zero allocation (`tests/zero_alloc.rs`): a counting global allocator, with a
  live self-check, observes zero allocations over 19,000 warmed-up iterations.
  Each iteration does header write+parse, the PN field, PN decode, a frame scan
  (ACK + STREAM), receive tracking, `on_packet_sent`, `on_ack_received` with
  events, and ACK generation+encode. The counting allocator needs `unsafe impl
  GlobalAlloc` in that test file only; the library stays `forbid(unsafe_code)`.

## Checks

See the PR description for the exact commands and results:
`cargo fmt --all -- --check`, `cargo clippy --workspace --locked --all-targets
-- -D warnings`, `VESPER_LEAN=lean cargo test --workspace --locked`, and
`lean lean/WarpPacketNumber.lean` with Lean 4.24.0 (`lean-toolchain`).

## Root patch

This lane's root patch is its own commit. It adds `crates/vesper-warp` to
`members` and `default-members` and adds the `vesper-warp` entry to
`Cargo.lock`. No new workspace dependencies, no CLI wiring, no CI edits.

## Decisions

- **Offsets instead of borrows.** `ProtectedHeader` stores offsets, not slices,
  so in-place header-protection removal can borrow the buffer mutably between
  the two parsing steps.
- **Bounded tracking with conservative eviction.** ACK tracking evicts the
  lowest range when full and reports anything at or below it as `TooOld`, so a
  duplicate is never accepted. Recovery tracking is bounded per space
  (`max_tracked_per_space`, default 1024) with preallocated `VecDeque`s, and
  overflow is a typed error.
- **Strict short-header parsing.** Only QUIC v1 is accepted. The fixed bit must
  be set (no grease-bit negotiation). A frame type that is not minimally
  encoded is a PROTOCOL_VIOLATION, as RFC 9000 §12.4 permits.
- **Errors map to transport codes.** `transport_code()` maps errors to
  RFC 9000 §20.1 codes (FRAME_ENCODING_ERROR or PROTOCOL_VIOLATION).

## Remaining gaps (explicitly not done)

- **No real cryptography or network peers.** There is no packet protection,
  and no interoperability with real QUIC peers is claimed. No real network
  traffic was exercised, and none of the hardware or kernel features (AF_XDP,
  NIC EDT, offload) are involved here.
- **Unsupported frames.** RESET_STREAM, STOP_SENDING, NEW_TOKEN, MAX_STREAMS,
  the `*_BLOCKED` frames, NEW_CONNECTION_ID, RETIRE_CONNECTION_ID and
  PATH_CHALLENGE/RESPONSE return `FrameError::Unsupported`. Stream and
  flow-control state machines, connection-ID management and datagram-level
  rules (such as the 1200-byte client Initial minimum) are not implemented.
- **Version handling is minimal.** Unsupported versions are an error. The
  parser does not hand back the CIDs needed to answer with Version
  Negotiation, and VN packets can be written but are not validated against
  the original packet.
- **Partial congestion signals.** Persistent congestion is evaluated per space
  and per loss batch. ECN validation (RFC 9000 §13.4.2) is not done, and only
  CE increases are reported. Spurious-loss undo is not modelled.
- **No source-level claims.** Rust ownership here is not evidence that
  Vesper's source typing rules are implemented. This lane has no §35 linear
  owners: `InFlightQuicPkt` and `UmemFrame` belong to lanes 3 and 4.
