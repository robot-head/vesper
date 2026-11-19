# Port P3b lane 4: Warp's QUIC core in Vesper

Claim: #1066 (it lands in several PRs, and the last one closes it). Part
1a (PR #1079) ported the wire modules, part 1b (PR #1108) loss recovery,
part 1c (PR #1116) the stream byte buffers, and part 2a the keyed tokens
of `lifecycle.rs`. Lane prompt:
`docs/agent-prompts/port-p3b/04-warp.md` (plan PR #1072).

## Summary

- `@vesper/warp` (`crates/vesper-warp/vesper/`, 6 modules, about 2,900
  lines) ports Warp's wire layer: `varint`, `packet_number`, `header`,
  `frame` and the receive side of `ack` (`ReceivedPackets`).
- The port is the non-default `vesper-port` feature of `vesper-warp`
  itself, following vesper-warp-tcp. The crate is not compiler-linked, so it
  needs no seed copy. Without the feature, the crate builds exactly the Rust
  code it had before. That code now lives in `src/reference/` and is frozen.
  With the feature, `src/ported/` forwards each function to the Vesper
  object. No Rust code is deleted.
- All 21 test targets pass on both builds. That includes quinn interop, the
  endpoint, handshake, lifecycle and stream suites (which run through the
  ported wire layer), and the zero-alloc tests.
- Speed against Rust (ABBA wall clock, callgrind instructions per batch):
  frames 2.35x / 2.29x, ack 1.86x / 2.03x, and a whole-connection echo
  transfer 1.15x / 1.11x. The port is not at parity, so Rust stays the
  default.
- Later parts added recovery (1b), the stream buffers (1c) and the
  lifecycle tokens (2a, the first `.vsys` imports: AES-256-GCM, HMAC and
  the CSPRNG stay Rust). `cc` stays Rust (see below). The handshake and
  the endpoint come next.

## API inventory

| Module | Ported to Vesper | Stays Rust in both builds |
|---|---|---|
| `varint` | `prefix_len`, `encoded_len`, `decode`, `encode_with_len`, `encode` | `Reader`, `Writer` (thin cursors over the ported functions) |
| `packet_number` | `decode_quic_packet_number_rfc9000`, `decode_with_largest`, `encoded_len`, `encode`, `write_truncated`, `TruncatedPn::read`/`decode` | `PnLen` accessors (const fns over a `u8`) |
| `header` | `HeaderParser::parse`, `ParsedPacket::check_reserved_bits`/`first_byte_mask`/`packet_number_field`, `VersionNegotiation::versions`, `write_short`, `write_short_to_peer`, `write_long`, `write_retry`, `write_version_negotiation` | `HeaderParser::new` and its field layouts, `Span` to slice conversion, `sample`, `dcid`/`scid`/`token` |
| `frame` | `parse_frame`, `allowed`, `Frame::encode`/`frame_type`/`is_ack_eliciting`/`is_probing`, `AckFrame::from_ranges` checks, ACK wire-range iteration, `encode_stream_header`, `encode_crypto_header`, `stream_header_len`, `crypto_header_len` | The `Frames` iterator and `scan_payload` plumbing (they call the ported `parse_frame`), `range_count`/`ranges`/`smallest` |
| `ack` | `ReceivedPackets::new`, `check`, `on_packet_received`, `ack_frame` (delay), `on_ack_sent`, `on_ack_of_ack` | Range storage (`[AckRange; N]`, lent in place), `largest`/`ranges`/`ecn_counts`/`ack_deadline` getters |
| `recovery` | `ack_delay_ns`, `RttEstimator::new`/`update`/`pto_base_ns`/`loss_delay_ns`, `LossRecovery::new`, `on_packet_sent`, `on_ack_received`, `on_timeout`, `pto_time_and_space`, `persistent_congestion_duration_ns`, `set_max_ack_delay_ns`, `set_handshake_status`, `reset_space`, `discard_space`, and the timer it arms | The buffers (three `RecSpace`s, the sent-packet rings and one call's events), lent in place; replaying the events to the caller's sink; the getters |
| `cc` | Nothing yet (see below) | The BBR adapters |
| `stream` | `SendBuf` (`write`, `next_lost`, `take_new`, `copy_out`, `on_ack`, `on_lost`, `discard`, `reset_for_reuse`, `free_space`), `RecvBuf` (`insert`, `read`, `reset_for_reuse`), and the ring bitmaps and ring copies under them | The rings and bitmaps (owned by the wrapper, lent in place); the offset getters; the stream state machines, `CryptoStream`, `StreamSet` (packet building, flow control, the sent-frame record), which call the ported buffers |
| `lifecycle` | `TokenKey::seal`/`open` (the Retry and NEW_TOKEN token layout and every check: length, kind, lifetime, address, ODCID length), `ResetKey::token`, the constant-time reset-token comparison (`token_eq`) | AES-256-GCM, HMAC-SHA-256 and the CSPRNG behind the `.vsys` library `WarpKeys` (`src/ported/lifecycle.rs`, module `keys`); the address encoding (`std::net` types); `LifecycleConfig`, `Cids`, `Path`, `PathProbe` and the preferred-address helpers, which move with the endpoint |
| endpoint, handshake, TLS glue | Not yet (part 2b on) | Everything |

Borrowed slices cross the ABI as spans `(start, len)` into the caller's
buffer, and the Rust side converts them back into slices. That keeps
`Frame<'a>` and `ParsedPacket<'a>` borrowing the datagram with no copies.
In the port build, `AckRange` is the binding's `repr::AckRange`, so a
`ReceivedPackets` range array is lent to Vesper in place.

These const fns are non-const in the port build, because they call foreign
code: `ReceivedPackets::new`/`largest`/`ecn_counts`/`ack_deadline`,
`Frame::is_ack_eliciting`/`is_probing`, `frame::allowed`,
`ParsedPacket::check_reserved_bits`/`first_byte_mask`, and
`varint::prefix_len`/`encoded_len`. No caller in the workspace uses them in a
const context.

## Design

- The Vesper error enums (`VarIntErr`, `PnErr`, `HdrErr`, `FrameErr`,
  `AckErr`) mirror the Rust errors field for field, and checks run in Rust's
  order, so every typed error matches. Malformed input raises `Fail[...]` and
  never traps. Bounds use `try_*` slice builtins throughout, and the package
  has no `unsafe` code. The crate keeps `forbid(unsafe_code)` on the Rust
  build. The port build uses `deny(unsafe_code)`, with one `allow` on the
  generated binding module, so the hand-written forwarding layer has no
  `unsafe` code either, with one exception from part 2a: the `WarpKeys`
  shim (`src/ported/lifecycle.rs`, module `keys`). Its four
  `#[unsafe(no_mangle)] extern "C"` functions receive each `nocapture`
  `CSlice[u8]` as a pointer and a length, and two `unsafe` blocks turn
  those into slices for the call. Each has an `allow(unsafe_code)` and a
  SAFETY note. The Vesper package itself still has no `unsafe` code.
- `.vsys` imports (part 2a). A `.vsys` library is visible only in the root
  module, so the token code is in `lib.ves`. Export ABI v3 refuses handles
  in exported signatures, and the token key lives in the Rust wrapper
  between calls. So the shim does not use a handle: the wrapper lends its
  `Arc<Aes256Gcm>` to the one synchronous call through a per-thread slot
  (vesper-tensor's `WeightFile` pattern). The Vesper code writes the
  layout and the random nonce (an import), and the shim seals or opens the
  token in place. `TokenKey::open` opens a copy, so the caller's bytes stay
  as received.
- Export ABI v3 (`// @export abi 3` in `lib.ves`). `FrameW` is a
  20-variant word enum that converts as `Words<FrameW, 12>`. `RxState` is a
  copy struct lent `inout`.
- Recovery keeps Rust's design: per-space rings of sent-packet records,
  sized at construction, with acknowledged and lost records as tombstones.
  `vesper/src/recovery.ves` indexes the rings itself. The congestion events
  of a call go into a buffer of `capacity + 2` records, and the wrapper
  replays them to the caller's sink afterwards. That keeps the RFC order
  (ECN-CE, losses, persistent congestion, then acknowledgements). The sink
  cannot observe recovery during the call, because `on_ack_received`
  borrows it mutably, so replaying afterwards is equivalent. The `u128`
  RTT arithmetic becomes exact `u64` forms: `(3a + b) / 4` and
  `(7a + b) / 8` split into quotients and remainders, and `9b / 8` is
  `b + b / 8`.
- The stream buffers keep Rust's layout: a byte ring with one bitmap bit
  per byte (acknowledged and lost for sending, received for receiving),
  walked a word at a time. `StreamSet` reads the buffers' offsets through
  their getters now, instead of their fields, so the same Rust
  orchestration runs over either build. `StreamSet` is a pool of streams
  with optional halves, an event queue and a sent-frame record. Porting it
  needs owned collections of structs that hold buffers, which the port
  would have to keep as Rust handles today. It stays Rust for part 2,
  where the endpoint's handles land.
- `cc` stays Rust in this part. Its adapters wrap `vesper-warp-bbr`, whose
  port is one concatenated compilation unit without `use` imports, so it
  cannot be a Loom dependency of `@vesper/warp` without changing that
  crate, which is outside this lane. Making it importable is a small
  follow-up for the BBR crate. The lane can then move the adapters.
- The frame predicates take the frame kind (one word) instead of the whole
  frame. `Frame::frame_type()` is called once per frame on the hot path, and
  passing the kind cut frames from 2.86x to 2.29x instructions.
- The package uses no `ChunkSeq`, `String` or `Box`, so the zero-alloc
  tests pass unchanged. The only allocator imports in the object come from
  the runtime's entry points, and no ported function reaches them.

## Unit tests

- The integration suites (`tests/*.rs`) run unchanged on both builds.
- The unit tests in `src/varint.rs`, `src/packet_number.rs`,
  `src/stream.rs` (the send and receive buffer tests) and
  `src/lifecycle.rs` exercise only the crate's API, so they also run on
  both builds. `src/lifecycle.rs` gained two in part 2a: token round
  trips and every refusal (expired, wrong address, wrong key, a flipped tag
  bit, a truncated token, an empty one), and reset tokens.
- The unit tests in `src/reference/` cover reference internals and run only
  on the Rust build. One example is `bits_find_and_wrap`, a test of the
  reference bitmap type.
- `scripts/test-port-harness.sh` runs both builds through
  `crates/vesper-warp/port/check.sh`.

## Divergence checks

`vesper-port-divergent` with `VESPER_WARP_DIVERGE=<module>` applies one
source edit from `DIVERGENT_EDITS` in `build.rs`. `port/check.sh` checks that
a named test fails and another test of the same target still passes. The
Bazel build has one divergent object per module.

| Module | Edit | Fails | Still passes |
|---|---|---|---|
| varint | 2-byte mask `16383` to `8191` | `varint::tests::rfc_examples` | `packet_number::tests::first_packets_decode` |
| pn | half window `win / 2` to `win / 4` | `packet_number::tests::rfc_a3_example` | `varint::tests::rfc_examples` |
| header | short-header PN offset `dcid_len + 1` to `+ 2` | `headers::configured_dcid_lengths` | `headers::long_headers_roundtrip_and_coalesce` |
| frame | STREAM FIN bit `!= 0` to `== 0` | `frames::stream_without_length_runs_to_end_and_all_type_bits` | `frames::ack_wire_ranges_decode` |
| ack | delayed-ACK threshold `>= 2` to `>= 3` | `ack_tracking::delayed_ack_policy` | `ack_tracking::merges_ranges_and_detects_duplicates` |
| recovery | packet threshold 3 to 4 | `recovery::packet_threshold_then_time_threshold` | `recovery::rtt_estimator_and_ack_delay_rules` |
| stream | an in-order insert no longer advances the readable prefix (`start <= contig` to `<`) | `stream::tests::recv_buf_reassembles_with_wrap` | `stream::tests::send_buf_retransmits_only_unacked_lost_bytes` |
| lifecycle (in `lib.ves`) | a Retry token's ODCID is read one byte early (`token[41..]` to `token[40..]`) | `lifecycle::tests::tokens_round_trip_and_refuse_what_they_must` | `lifecycle::tests::reset_tokens_are_keyed_and_compared_whole` |

## Speed against Rust

`examples/abba.rs`, run by `port/bench.sh`, on Linux x86_64 with release
builds. Each run is 12 A/B/B/A epochs, using the `vesper-bench` estimators.
Callgrind counts are `(Ir(3 batches) - Ir(1 batch)) / 2`, which removes
startup. The frames digests match Rust (`0ec977800e561b4d` for 3 batches).

| Workload | What it does | Rust | Vesper | Wall ratio (95% CI) | Ir Rust | Ir Vesper | Ir ratio |
|---|---|---|---|---|---|---|---|
| frames | encode and parse 1,024 packets of mixed frames (ACK with ranges, STREAM, CRYPTO, flow control, close) | 251 ns/packet | 586 ns/packet | 2.35x (2.33-2.38) | 4.18M | 9.57M | 2.29x |
| ack | 8,192 received packets with gaps, ACK frames built and encoded | 41 ns/packet | 76 ns/packet | 1.86x (1.84-1.88) | 5.54M | 11.23M | 2.03x |
| recovery | 4,096 sends with an ACK every eighth and a loss every fortieth, loss timer polled per send | 59 ns/packet | 222 ns/packet | 5.39x (4.92-5.48) | 1.78M | 7.41M | 4.16x |
| buffers | 1,024 chunks of 1,200 bytes through a 64 KiB send ring (every seventh lost and retransmitted) and a 64 KiB receive ring (neighbours swapped) | 1,249 ns/chunk | 1,908 ns/chunk | 1.51x (1.50-1.55) | 7.03M | 12.22M | 1.74x |
| transfer | 16 echo rounds on a loopback connection pair (endpoint, TLS and crypto stay Rust) | 22.7 µs/round | 26.0 µs/round | 1.15x (1.12-1.17) | 3.13M | 3.78M | 1.21x |

The transfer instruction count is from part 1c, with recovery and the
stream buffers ported too. Part 1a measured 3.46M (1.11x) and part 1b
3.60M (1.15x). Its wall-clock numbers are from part 1a.
Wall-clock runs for part 1b were noisy, because other jobs shared the
machine: recovery measured 3.60x and 6.60x in two later runs. The
callgrind ratio is the reliable figure.

Tokens (part 2a) are measured by the ignored unit test
`lifecycle::tests::measure_tokens` (`cargo test --release --lib --
--ignored --nocapture measure_tokens`, with and without `vesper-port`).
Each iteration seals and opens one Retry token, then derives and compares
one reset token, 100,000 times, best of 7. Two runs each:

| Operation | Rust | Vesper | Wall ratio |
|---|---|---|---|
| Retry token seal + open | 863-890 ns | 951-960 ns | 1.08-1.10x |
| reset token + comparison | 270-279 ns | 262-270 ns | 0.97x |

Callgrind over the whole test (both loops, 7 rounds): 14.29G instructions
for Rust and 15.38G for Vesper (1.08x), about 20,400 and 22,000 per
iteration. AES-GCM and HMAC dominate and are the same Rust code in both
builds. The port adds the copy of the token, the slot and the byte-wise
layout code.

Profile of the frames workload, with functions named: `fr_encode` 17.8%,
`fr_parse_at` 13.9%, `w_vi` 9%, `parse_ack` 8.3%, `vi_put` 4.2% and
`fr_wire_next` 3.1%. The rest is mostly boundary code: `FrameW` word
conversion and `with_slice_mut`.

## Compiler gaps

Recovery profile (callgrind with functions named): `scan` (the timer's
pass over the three spaces) 22%, `rec_on_packet_sent` 15%,
`rec_on_ack_received` 14%, `mark_range` 9%, `detect_lost` 6% and
`sp_set` 5%. The Rust wrappers and the ABI boundary account for about 12%.
Rust's `set_timer` reads two fields of each space in place. The port
copies each 13-word `RecSpace` out of the slice (`try_get`) and moves
struct temporaries through the stack. Gaps 4 and 5 below cover this.

1. **Helpers that take an `inout` slice are not inlined.** `w_vi` (9%) and
   `vi_put` (4%) stay calls inside `fr_encode`, as do the other `w_*`
   writers. Rust inlines all of them. Helpers that take an `in` slice are
   inlined, and so are scalar helpers. Repro, built with
   `vesper build --lib . --lib-name repro`:

   ```vesper
   fn r8(in b: Slice[u8], at: u64) -> u64 {
       match b.try_get(at) {
           Option::Some(v) => widen_u64(v),
           Option::None => 0,
       }
   }

   fn w8(inout out: Slice[u8], at: u64, v: u8) -> u64 {
       out.try_set(at, v);
       wrapping_add(at, 1)
   }

   pub fn get3(in b: Slice[u8]) -> u64 {
       wrapping_add(wrapping_add(r8(in b, 0), r8(in b, 1)), r8(in b, 2))
   }

   pub fn put3(inout out: Slice[u8]) -> u64 {
       let a = w8(inout out, 0, 1);
       let b = w8(inout out, a, 2);
       w8(inout out, b, 3)
   }
   ```

   `vesper_repro_get3` has no calls. `vesper_repro_put3` keeps three calls to
   `w8`. A `Fail` row on `w8` makes no difference. A side note: the nested
   form `w8(inout out, w8(inout out, 0, 1), 2)` is refused, because the
   inner `inout` loan overlaps the outer one.
2. **Word enums cross the export ABI by conversion.** `FrameW` (20
   variants, 12 words) is copied into and out of `Words<FrameW, 12>` on each
   call. That is about 6% in frames. There is no way to lend a Rust enum
   in place, the way copy structs are lent.
3. **No fixed-size arrays.** ACK ranges have to live in Rust (`[AckRange;
   N]`, lent as a slice), and the four SACK-like slots are `b0..b3` fields
   with `if` ladders (see `opts_blk` in warp-tcp). A `[T; N]` value type
   with indexing would let `ReceivedPackets` be one Vesper struct.
4. **Reading a struct element of a slice copies the whole struct.**
   `match spaces.try_get(i) { Option::Some(s) => s.ack_eliciting_in_flight, .. }`
   copies all 13 words of the `RecSpace` to the stack before reading one
   field. There is no projection like `spaces[i].field` that cannot fail.
   This is most of `scan`'s cost.
5. **Copy-struct locals and results live in stack memory.** In `scan`,
   the `PtoOut` and `Scan` values are built in stack slots and copied
   twice on return. Rewriting the locals as scalars did not change the
   instruction count, because the copies come from the `RecSpace` reads
   and the struct result.
6. **No saturating or checked multiply.** `saturating_add` and
   `saturating_sub` exist only for vectors. Recovery writes `sat_mul3`,
   `sat_mul4` and `sat_mul1000` with constant divisors, because dividing
   by a variable needs a `Fail` row. std candidates: `saturating_add_u64`
   (`varint.ves` defines `sat_add`) and `saturating_mul_u64`.
7. **No bitwise not on integers.** `!x` is boolean only, so the bitmap
   code writes `x ^ u64_max()`. An `if` expression also cannot be the left
   operand of a binary operator (`if c { a } else { b } & m` does not
   parse), so it needs a temporary.
8. **`state` cannot be a field name.** It is a keyword. `vesper check`
   says "expected a lower-case identifier", but `vesper fmt` reports only
   `Syntax { offset: N }`. The record field is `status`.
9. **`vesper check` does not read `// @export vsys` directives.** Only
   `vesper build --lib` does, so `vesper check` on the package reports
   `unknown effect 'Ffi'` and `unknown name 'WarpKeys::...'` for code that
   builds. Also, an import named like an export symbol
   (`vesper_warp_token_open` against the exported `token_open`) is a codegen
   error. It is a clear one, but `vesper check` misses it too. The imports
   are named `vesper_warp_keys_*`.

## Platforms

`port/check.sh` runs on the Linux port-harness shards and the
macOS/Windows harness through `scripts/test-port-harness.sh`. The Bazel
targets (`:port`, `port_*_test`, `warp_vesper_divergent_*`) mirror
vesper-warp-tcp. They were written without a local `bazel`, so CI is their
first run.

## Files

- `crates/vesper-warp/vesper/`: `Loom.toml`, `src/{lib,varint,pn,header,frame,ack,recovery,stream}.ves`
- `crates/vesper-warp/src/reference/`: the frozen Rust implementations
- `crates/vesper-warp/src/ported/`: the forwarding layer and the `WarpKeys`
  shim
- `crates/vesper-warp/port/`: `binding.rs` (generated), `check.sh`, `bench.sh`
- `crates/vesper-warp/build.rs`, `examples/abba.rs`, `BUILD.bazel`, `Cargo.toml`
- One line each: `scripts/test-port-harness.sh`, `tests/fixtures/self_host/driver.rs`
