# Port P3b lane 4, part 1: Warp's pure QUIC modules in Vesper

Claim: #1066 (this is part 1; the last part closes it). Lane prompt:
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
- Part 2 adds recovery and cc, reusing the warp-wheel and BBR ports, and
  then stream. The endpoint waits on opaque handles (lane 3, #1065).

## API inventory

| Module | Ported to Vesper | Stays Rust in both builds |
|---|---|---|
| `varint` | `prefix_len`, `encoded_len`, `decode`, `encode_with_len`, `encode` | `Reader`, `Writer` (thin cursors over the ported functions) |
| `packet_number` | `decode_quic_packet_number_rfc9000`, `decode_with_largest`, `encoded_len`, `encode`, `write_truncated`, `TruncatedPn::read`/`decode` | `PnLen` accessors (const fns over a `u8`) |
| `header` | `HeaderParser::parse`, `ParsedPacket::check_reserved_bits`/`first_byte_mask`/`packet_number_field`, `VersionNegotiation::versions`, `write_short`, `write_short_to_peer`, `write_long`, `write_retry`, `write_version_negotiation` | `HeaderParser::new` and its field layouts, `Span` to slice conversion, `sample`, `dcid`/`scid`/`token` |
| `frame` | `parse_frame`, `allowed`, `Frame::encode`/`frame_type`/`is_ack_eliciting`/`is_probing`, `AckFrame::from_ranges` checks, ACK wire-range iteration, `encode_stream_header`, `encode_crypto_header`, `stream_header_len`, `crypto_header_len` | The `Frames` iterator and `scan_payload` plumbing (they call the ported `parse_frame`), `range_count`/`ranges`/`smallest` |
| `ack` | `ReceivedPackets::new`, `check`, `on_packet_received`, `ack_frame` (delay), `on_ack_sent`, `on_ack_of_ack` | Range storage (`[AckRange; N]`, lent in place), `largest`/`ranges`/`ecn_counts`/`ack_deadline` getters |
| recovery, cc, stream, endpoint, handshake, lifecycle, TLS glue | Not yet (later parts) | Everything |

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
  `unsafe` code either.
- Export ABI v3 (`// @export abi 3` in `lib.ves`). `FrameW` is a
  20-variant word enum that converts as `Words<FrameW, 12>`. `RxState` is a
  copy struct lent `inout`.
- The frame predicates take the frame kind (one word) instead of the whole
  frame. `Frame::frame_type()` is called once per frame on the hot path, and
  passing the kind cut frames from 2.86x to 2.29x instructions.
- The package uses no `ChunkSeq`, `String` or `Box`, so the zero-alloc
  tests pass unchanged. The only allocator imports in the object come from
  the runtime's entry points, and no ported function reaches them.

## Unit tests

- The integration suites (`tests/*.rs`) run unchanged on both builds.
- The unit tests in `src/varint.rs` and `src/packet_number.rs` exercise only
  the public API, so they also run on both builds.
- The unit tests in `src/reference/` cover reference internals and run only
  on the Rust build.
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

## Speed against Rust

`examples/abba.rs`, run by `port/bench.sh`, on Linux x86_64 with release
builds. Each run is 12 A/B/B/A epochs, using the `vesper-bench` estimators.
Callgrind counts are `(Ir(3 batches) - Ir(1 batch)) / 2`, which removes
startup. The frames digests match Rust (`0ec977800e561b4d` for 3 batches).

| Workload | What it does | Rust | Vesper | Wall ratio (95% CI) | Ir Rust | Ir Vesper | Ir ratio |
|---|---|---|---|---|---|---|---|
| frames | encode and parse 1,024 packets of mixed frames (ACK with ranges, STREAM, CRYPTO, flow control, close) | 251 ns/packet | 586 ns/packet | 2.35x (2.33-2.38) | 4.18M | 9.57M | 2.29x |
| ack | 8,192 received packets with gaps, ACK frames built and encoded | 41 ns/packet | 76 ns/packet | 1.86x (1.84-1.88) | 5.54M | 11.23M | 2.03x |
| transfer | 16 echo rounds on a loopback connection pair (endpoint, TLS and crypto stay Rust) | 22.7 µs/round | 26.0 µs/round | 1.15x (1.12-1.17) | 3.13M | 3.46M | 1.11x |

Profile of the frames workload, with functions named: `fr_encode` 17.8%,
`fr_parse_at` 13.9%, `w_vi` 9%, `parse_ack` 8.3%, `vi_put` 4.2% and
`fr_wire_next` 3.1%. The rest is mostly boundary code: `FrameW` word
conversion and `with_slice_mut`.

## Compiler gaps

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
4. **std candidate: `saturating_add_u64`.** `varint.ves` defines `sat_add`.

## Platforms

`port/check.sh` runs on the Linux port-harness shards and the
macOS/Windows harness through `scripts/test-port-harness.sh`. The Bazel
targets (`:port`, `port_*_test`, `warp_vesper_divergent_*`) mirror
vesper-warp-tcp. They were written without a local `bazel`, so CI is their
first run.

## Files

- `crates/vesper-warp/vesper/`: `Loom.toml`, `src/{lib,varint,pn,header,frame,ack}.ves`
- `crates/vesper-warp/src/reference/`: the frozen Rust implementations
- `crates/vesper-warp/src/ported/`: the forwarding layer
- `crates/vesper-warp/port/`: `binding.rs` (generated), `check.sh`, `bench.sh`
- `crates/vesper-warp/build.rs`, `examples/abba.rs`, `BUILD.bazel`, `Cargo.toml`
- One line each: `scripts/test-port-harness.sh`, `tests/fixtures/self_host/driver.rs`
