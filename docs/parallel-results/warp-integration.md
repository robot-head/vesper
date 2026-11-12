# Wave-four integration: the end-to-end §35.3 Warp path

Issue #45. Code: `crates/vesper-warp/src/endpoint.rs` (the data path) and
`crates/vesper-warp/src/cc.rs` (congestion-control adapters). The crate now
depends on every wave-four lane crate; no lane crate depends back on it.

## What is assembled

`WarpEndpoint::poll_and_step_quic_rx_one` runs one received frame through:

| Step | Lane | Call |
| --- | --- | --- |
| Harvest from the rx ring | 3 | `XdpNic::harvest_rx` |
| Header split to the protected PN boundary | 1 | `HeaderParser::parse` |
| CID lookup in the shard's tiles | 6 | `ConnTable::lookup` or `lookup_cid_simd` (`CidLookup`) |
| Header-protection removal | 5 | `OneRttKeys::open_packet` |
| Full PN decode from the tile's `largest_rx_pn` | 1 | `TruncatedPn::decode` (RFC 9000 A.3), passed to lane 5 as the reconstruct step |
| AEAD open with the full-PN nonce | 5 | `OneRttKeys::open_packet` |
| Reserved-bit check after open, duplicate check | 1 | `packet_number_field`, `ReceivedPackets::check` |
| Frames; ACKs into RFC 9002 recovery | 1 | `scan_payload`, `LossRecovery::on_ack_received` |
| Recovery events into BBRv3 | 1 → 2 | `cc::QuicBbr` |
| Leases resolved by ACK or loss | 3, 4 | `RetransmitLeases::on_ack`, `XdpNic::retire` |
| Reply turned around in the received frame | 3 | `turn_around_tx_in_place` |
| Seal under a linear nonce ticket | 5 | `OneRttKeys::ticket` + `seal` |
| EDT schedule | 2 | `Bbrv3::schedule_edt_ns` |
| Software launch queue until the EDT | 4 | `EdtLaunchQueue` |
| Paced submit, then park until ACK or PTO | 3, 4 | `submit_paced_tx`, `RetransmitLeases::park` |

`WarpEndpoint::poll` releases due launches, harvests completions,
replenishes the fill ring and services expired leases: a lease whose PTO
passed runs the connection's RFC 9002 timer (time-threshold loss, or PTO
with PING probes) and is re-parked at recovery's next deadline unless
recovery resolved it. Connection hot scalars (largest received PN, next
PN, BBR bandwidth and min RTT, cwnd, bytes in flight, next EDT, idle
deadline, peer CID) live in lane 6's tiles and are written back after every
step.

## Adapters

* `cc::QuicBbr`: converts lane 1's event types to lane 2's, attaches lane
  2's `PacketDeliveryState` snapshot per packet, and batches one ACK
  frame's `Acked` events with that ACK's RTT sample into one
  `Bbrv3::on_ack`. Recovery emits ECN-CE and losses before
  acknowledgements, so BBR sees them in the order lane 2 requires.
* `cc::TcpBbr`: lane 7's `CongestionController` over BBRv3. TCP reports
  its RTT sample after the ACK's `Acked` events, so each acknowledgement
  uses the latest sample seen (one ACK late). Partial acknowledgements and
  acknowledgements of segments reported lost are tracked per transmission.
  On RTO TCP marks every segment lost without per-segment events, so the
  adapter reports the outstanding transmissions lost itself. Sends enter
  BBR without its send guard because TCP gates on its own window.

## Deviations from the §35.3 sketch

* The reply uses the connection's next packet number (tile and nonce
  sealer), not `full_pn + 1`, and is as long as its ACK plus the
  application's frames, not a fixed 64 bytes. A reply is sent only for
  ack-eliciting packets.
* ACK-only packets are not paced or put in flight, but they are queued
  behind the connection's paced packets. Sent ahead of them, they made
  the peer acknowledge a higher packet number first, and the
  packet-threshold rule then declared the queued packets lost (found by the
  bulk test).
* The PN length is widened (up to 4 bytes) instead of padding when the
  payload is too short for the header-protection sample, so ACK-only
  packets stay out of flight.
* The shared lease table is keyed by `(space, slot << 40 | pn)`, so a
  connection sends at most 2^40 packets and an endpoint holds at most 2^22
  connections.
* A packet the launch queue refuses is abandoned (and recovery declares it
  lost), never submitted ahead of its EDT. `send_stream` returns
  `LaunchBackpressure` and replies drop their data frames first. An ACK-only
  reply that would need a full queue is deferred: the ACK stays owed and
  `poll` sends it once there is room.
* Recovery events are fed to BBR as each ACK frame is processed, so a
  packet with several ACK frames cannot overflow the event buffer.
* The `Responder` writes into the space left after the short header and
  tag. A reply whose application frames still do not fit beside its ACK is
  sent ACK-only.
* Send keys are updated (RFC 9001 §6) every `key_update_interval` packets
  (2^22 by default, under the AES-GCM confidentiality limit), or on
  `initiate_key_update`.
* Each connection's RFC 9002 loss/PTO deadline is kept in a per-endpoint
  timer wheel and re-armed after every step, so time-threshold loss fires
  on time and not only when a lease's PTO comes due.
* The §35.3 `[T-Let]` rule is kept in host-Rust form: an error is returned
  only after the received frame was recycled or turned around.

## Evidence

`cargo test -p vesper-warp`:

* `tests/endpoint.rs`: echo round trip with RTT, tile, recovery, BBR and
  lease checks; a 300-packet paced bulk transfer through the launch queue
  with no spurious loss; a dropped echo recovered by PTO, probes and
  time-threshold loss; tampered, unknown-CID, long-header and replayed
  packets dropped with their frames recycled; the tile-scan lookup; 40
  connections on one endpoint and closing one; dropping an endpoint with
  work outstanding leaves no wheel leak; UMEM conservation.
* `tests/endpoint_zero_alloc.rs`: a counting global allocator sees 0
  allocations over 300 warmed-up rounds (1200 data packets, echoes and
  ACKs, both endpoints, every step above).
* `tests/review_fixes.rs`: key updates on the interval and on demand,
  launch backpressure with deferred ACKs and no early sends, the ACK-only
  fallback for an oversize echo, and time-threshold loss from the recovery
  timer before any PTO.
* `tests/tcp_bbr.rs`: TCP bulk and lossy, reordering transfers with
  BBRv3 and no adapter errors; partial and late acknowledgements.

## Not done, or unavailable

* No TLS handshake: connections open with caller-supplied 1-RTT secrets.
  Long-header packets are dropped.
* No stream layer: lost stream data is reported to the `Responder`, not
  retransmitted.
* Hardware EDT (`SO_TXTIME`), RSS programming, crypto offload and DPDK are
  unavailable; lane 3's AF_XDP backend is not exercised by these tests.
* Lane 8's source slice keeps its own UMEM model; backing it with lane 3's
  software NIC is left for a follow-up.
