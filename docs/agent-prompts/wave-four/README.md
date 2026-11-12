# Fourth parallel implementation wave: Vesper Warp (§35)

Status: **ready to assign**. One thread per lane; do not launch duplicate writers.
Each linked file is a complete standalone prompt. All eight lanes can run
concurrently under the frozen [wave-four contracts](../../rfcs/wave-four-contracts.md):
every lane owns a new crate (or, for lane 8, the existing simulation crate) and
depends only on crates already on `main`. Start from fresh `origin/main` containing
**`a5fa024b8d6f62eaa336337d825a2be2ec8972b4`** or later.

Source: design §35, "Vesper Warp: User-Space Kernel-Bypass QUIC/TCP Networking,
Linear AF_XDP/DPDK UMEM Descriptor Rings, and Verified BBRv3 Loss Recovery"
(roadmap heading `h.t6n43jlxtwy5`). Before this wave nothing of §35 exists on
`main`: there is no QUIC, TCP, UMEM, BBR, timing-wheel or packet-protection code.

| Lane | Copyable prompt | Exclusive boundary | §35 coverage |
| --- | --- | --- | --- |
| 1 | [QUIC packet core and loss recovery](01-quic-packet-core.md) | New `crates/vesper-warp/**`, `lean/WarpPacketNumber.lean` | 35.2 headers, 35.3 PN decode, 35.5 decoder; RFC 9000 frames, RFC 9002 recovery |
| 2 | [BBRv3 and EDT pacing](02-bbrv3.md) | New `crates/vesper-warp-bbr/**`, `lean/WarpBbr.lean` | 35.1(3), 35.4 thm 2, 35.5 `Bbrv3FlowState` |
| 3 | [Linear UMEM frames and AF_XDP rings](03-umem-rings.md) | New `crates/vesper-warp-umem/**`, `lean/WarpUmem.lean` | 35.1(1), 35.2 `XdpNic`/`UmemFrame`, 35.4 thm 1, 35.5 accounting |
| 4 | [Timing wheel and EDT launch queue](04-timing-wheel.md) | New `crates/vesper-warp-wheel/**` | 35.1(3), 35.2 `park_for_ack_or_pto` |
| 5 | [Packet protection and nonce tickets](05-packet-protection.md) | New `crates/vesper-warp-crypto/**` | 35.1(2); RFC 9001 |
| 6 | [Connection tiles, CID lookup, RSS](06-connection-shards.md) | New `crates/vesper-warp-shard/**` | 35.1(4), 35.3 `QuicConnTile`, `lookup_cid_simd` |
| 7 | [RFC 9293 TCP](07-tcp.md) | New `crates/vesper-warp-tcp/**` | §35 "QUIC/TCP" |
| 8 | [Source XdpNic and WAN simulation](08-source-xdpnic-sim.md) | `crates/vesper-sim-session/**` | 35.1(1,5), 35.2 effect, 35.3 control flow |

Each lane commits its own root registration patch (members, default-members,
new workspace dependencies, lockfile) onto its own PR branch, as the user
approved on 2026-11-12. Because every lane touches the root member lists and
`Cargo.lock`, later PRs rebase and regenerate the lockfile after earlier ones
merge; that is expected, not a conflict between lanes' code.

Use a separate checkout and build target per lane. Lean files are checked from
each crate's own tests under `VESPER_LEAN=lean`, so no CI workflow edits are
needed.

## After this wave: integration (not a parallel lane)

Once lanes 1–6 are merged, one follow-up thread assembles the end-to-end
§35.3 path inside `crates/vesper-warp`: software NIC (lane 3) → header split
(lane 1) → CID lookup in a shard (lane 6) → header-protection removal and
AEAD open (lane 5) → PN decode and ACK/loss events (lane 1) → BBRv3 EDT
schedule (lane 2) → turn-around, seal, paced submit and lease parking in the
wheel (lanes 3, 4), with a counting-allocator zero-allocation test and the
contract's event adapters. TCP (lane 7) gets the same BBRv3 adapter. The
source slice (lane 8) can then be backed by lane 3's software NIC. Prism
HTTP/3 (§17), Chorus WAN replication (§25), Crucible concolic fuzzing (§29),
full Vault (§23), DPDK and hardware offloads stay out of scope until those
sections exist.

## Design text quoted from §35

The prompts reference these definitions. The roadmap is the authority; this is
a convenience copy.

```text
wire struct XdpDescWire { umem_addr: u64le, pkt_byte_len: u32le, options: u32le }      // 16 bytes
wire struct QuicShortHdrWire { flags_spin_keyphase: u8, dest_conn_id: ByteArray[8], pkt_num_trunc: u32be } // 13 bytes
copy struct RxHarvested {}   copy struct TxStaged {}
linear struct UmemFrame[State] { chunk_idx: u32, data_offset: u16, pkt_len: u16 }
linear struct InFlightQuicPkt { pkt_num_full: u64, edt_departure_ns: u64, held_frame: UmemFrame[TxStaged] }

linear effect XdpNic {
    fn harvest_rx_single() -> Option[UmemFrame[RxHarvested]];
    fn view_frame_bytes(in frame: UmemFrame[RxHarvested]) -> ByteSlice;
    fn recycle_to_fill_ring(sink frame: UmemFrame[RxHarvested]) -> Unit;
    fn turn_around_tx_in_place(sink rx_frame: UmemFrame[RxHarvested], tx_len: u16) -> UmemFrame[TxStaged];
    fn submit_paced_tx(sink tx_frame: UmemFrame[TxStaged], pkt_num_full: u64, edt_launch_ns: u64) -> InFlightQuicPkt;
    fn park_for_ack_or_pto(sink inflight: InFlightQuicPkt) -> Unit;
}

tiled(16) copy struct QuicConnTile {
    hot largest_rx_pn: u64, hot next_tx_pn: u64, hot bbr_bw_bytes_per_sec: u64, hot bbr_min_rtt_ns: u64,
    hot cwnd_bytes: u32, hot bytes_in_flight: u32, hot next_edt_ns: u64, cold peer_cid: ByteArray[8],
}

fn poll_and_step_quic_rx_one[Alg: AeadCipher, P: IsaProfile](
    inout conns: TiledSeq[QuicConnTile], in rx_keys: SecretSlab, inout tx_sealer: AeadSealer[Alg], now_ns: u64,
) -> Result[bool, QuicError] eff [XdpNic, ConstTime, Isa[P], Fail[QuicError]]
// harvest; split header (recycle on failure); lookup CID (recycle on miss);
// decode_packet_number_rfc9000(largest, trunc, 32); advance_bbrv3_edt;
// turn_around_tx_in_place(64); submit_paced_tx(full_pn + 1, edt); park_for_ack_or_pto.
// [T-Let] forbids `?` while the linear rx_frame is live.
```

§35.4 theorems: `umem_step_conserves_total_frames` (fill + rx + tx count is
preserved by harvest-then-recycle-or-reply) and `bbrv3_edt_launch_monotonic`
(`max(now, prev) + delay` is ≥ both). §35.5 gives the Rust
`decode_quic_packet_number_rfc9000`, `Bbrv3FlowState::{target_cwnd_bytes,
schedule_edt_ns}` and `XdpUmemAccounting::check_conservation` sketches.
