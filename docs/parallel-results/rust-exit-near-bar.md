# Rust exit: near-bar library flips

Claim #1873 (part of #1426, #1405, #1399). Re-measures the libraries that
sat just above the 1.50x flip bar after the Wave 23 codegen work, and flips
each one whose Vesper port passes: full function, 0 differential diffs, and
the upper end of the 95% interval of the median Vesper/Rust ratio at or
below 1.50x.

Host: this cloud container (4 cores, shared). Release builds with
`RUSTFLAGS="-C target-cpu=native"`, 30 ABBA epochs per workload, the
distribution-free interval x(10)..x(21) of the median.

## Part 1: ebpf `.BTF.ext` readers and beacon spans

| Crate | Workload | Compiler | Median | 95% interval | Verdict |
|---|---|---|---:|---|---|
| `vesper-ebpf` | `ext` | #1866 (Wave 24) | 1.23x | 1.17..1.24 | pass |
| `vesper-ebpf` | `ext` | `main` `b6611c8` | 1.22x | 1.17..1.22 | pass |
| `vesper-ebpf` | `ext` (repeat) | `main` `b6611c8` | 1.19x | 1.16..1.22 | pass |
| `vesper-beacon` | `spans` | #1866 (Wave 24) | 1.35x | (not printed then) | |
| `vesper-beacon` | `spans` | `main` `b6611c8` | 1.35x | 1.33..1.38 | pass |
| `vesper-ebpf` | `decode` | #1866 (Wave 24) | 1.65x | 1.62..1.70 | fail |
| `vesper-warp-wheel` | insert/advance/drain | #1866 (Wave 24) | 1.54x | 1.52..1.55 | fail |

The census's older `ext` 1.68x and `spans` 1.61x figures were from slower
hosts or older compilers; on this host both have been under the bar since
the part 1 speed-ups of #1441 and the #1426 waves.

Differentials: `tests/ext_port.rs` (`parse_ext`, `local_type` and
`maps_from_btf` against the frozen seed) and every `vesper-ebpf` test file
pass in both builds; `tests/behavior.rs` and every `vesper-beacon` test
file pass in both builds. 0 diffs.

What moved:

- `vesper-ebpf`: `btf_ext::parse_ext`, `local_type` and `maps_from_btf` run
  the Vesper port in every build. Their Rust bodies and helpers (271 lines)
  leave `src/btf_ext.rs`; the frozen copy is
  `seed/src/engine/btf_ext.rs`, which the compiler links until Stage 2.
  The `.BTF.ext` writer (`build`) stays Rust with the BTF builder.
- `vesper-beacon`: `SpanPool`, `TelemetryRing` and the drain iterator run
  the Vesper port in every build. The Rust bodies (227 lines) leave
  `src/spans.rs`; the frozen copy is `seed/src/engine/spans.rs`.
  `tests/abba.rs` now prints the 95% interval and the flip verdict, as the
  eBPF harness does.

Rust lines deleted from the crates' own sources: 498. The seed copies
(1,367 lines) go with the Stage 2 retirement (#1397).

## Still above the bar

- `vesper-ebpf` `decode` (1.65x): one export crossing per instruction and
  the enum match lowering (gaps 3 and 4 of rust-exit-silicon-ebpf.md).
- `vesper-warp-wheel` (1.54x, 1.52..1.55): bounds-checked loads from seven
  parallel sequences in `open_tick`, `place` and `next_work_tick`.

## Part 2: warp-wheel speed-ups (no flip yet)

Measured on main aba7689, same box and settings (release, `-C target-cpu=native`,
30 ABBA epochs), 95% CI of the Vesper/Rust ratio:

| Run | 95% CI | Bar (upper <= 1.50x) |
| --- | --- | --- |
| 1 | [1.469, 1.510] | fail |
| 2 | [1.457, 1.483] | pass |
| 3 | [1.435, 1.480] | pass |

Before these changes the port measured 1.54x [1.52, 1.55]. Two of three runs
now pass, which is not reliable enough to delete the Rust, so Rust stays the
default. 0 diffs; divergence checks pass.

Changes in `crates/vesper-warp-wheel/vesper/src/`:

- The seven parallel entry sequences are one `ChunkSeq[u64]` with stride 8
  (deadline, tick, seq, generation, prev, next, list) and list heads/tails are
  one sequence with stride 2, so each entry touch loads one header.
- `place` finds the level with `leading_zeros_u64` and a 64-entry
  `level_of_bit` table instead of a loop over level spans.
- `next_work_tick` and `open_tick` use wrapping shift helpers (`shl_in`,
  `shr_in`) that need no range check.
- `sort_chain` returns early for single-entry chains.

Remaining cost is codegen (#1426): seq header reloads after stores, the
`try_get` bounds branch, callee-saved register saves, and `poll` returning its
result through memory. Rust lines deleted: 0.

## eBPF wire: bulk-bytes port interface (#2183)

The wire structs (`EthIpv4TcpWire`, `SynDropAlertWire`) were 21.6x Rust:
each byte was a `ChunkSeq` push on the Vesper side, and the sequence was then
copied into a `Vec` across the port. The port interface now has the encoders
write into a buffer the Rust caller lends (`inout Slice[u8]`,
`wire_eth_encode_into`, `wire_alert_encode_into`), and both codecs move whole
big-endian words (`try_read_u64_be` / `try_write_u64_be`): the frame is the
seven words at 0, 8, .., 40 and 46, the alert one word plus the
little-endian rate.

New ABBA workload `wire` (`tests/abba.rs`: 256 frames and 256 alerts encoded
and decoded per call), release, `-C target-cpu=native`, 30 epochs, this
4-core cloud box:

| Interface | Vesper/Rust | 95% interval of the median |
| --- | --- | --- |
| before (per-byte push, seq copy out) | 21.6x | 21.2..21.9 |
| lent buffer, byte stores | 4.53x | 4.42..4.61 |
| lent buffer, word reads/writes | 2.43x | 2.36..2.46 |

0 diffs: `tests/wire_port.rs` compares both builds on 20,000 random frames
and alerts (bytes and fields) and on every buffer length up to one past each
size over 2,000 random byte strings (short buffers are the same typed
error).

Still above the bar, so `wire` stays behind `vesper-port` and Rust stays the
default. Per frame (callgrind, encode + decode) the Vesper bodies are about
355 instructions against Rust's 130, and the crossing adds about 270: what is
left is codegen and the ABI, not the interface.

- Each `try_read_u64_be`/`try_write_u64_be` keeps its own bounds check and
  materialises the `Option` (about 13 instructions a read) after the one
  length check that already covers all seven; the raising `read_u64_be`
  inside `handle Fail[RuntimeError]` was slower still (the handler is not
  folded away, and an effectful helper is not inlined).
- The 21-field struct crosses as 21 words each way (`Abi::put`/`get`, about
  75 instructions); passing the fields as 21 scalar arguments was slower (the
  stack arguments spill). ABI v3 mirrors (#1087) would lend the struct in
  place, but switching `@vesper/ebpf` to `// @export abi 3` changes every
  binding of the crate.

Tried and dropped: `vec![0; n]` for the lent buffer (calloc cost more than
the stack array plus `to_vec`).

The `decode` workload (the instruction codec, already the only build) is
1.68x [1.66, 1.69] and unchanged here: per instruction the crossing costs
`PortInsn` conversion, `Abi::put`, the 11-word `Encoded` result and the
`Vec`, about 100 instructions per `encode` call against Rust's 43, and
`decode_all` returns its instructions through a converted
`ChunkSeq[Insn]`.

Follow-up (#2226): with the bounds facts of #2220, the decoders' word reads
and, after one length guard added to each encoder, the encoders' word stores
compile without per-access checks (straight loads and stores). Re-timed with
a compiler built from #2220: `wire` 2.28..2.55 (median interval), `decode`
1.76..1.77. Both are still above the bar, so wire stays behind `vesper-port`. The
Vesper bodies are now tight; the remaining time is the Rust side of each
crossing (the 21-word struct conversion, the MAC conversion and the result
`Vec`).
