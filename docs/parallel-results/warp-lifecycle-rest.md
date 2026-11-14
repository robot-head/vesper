# Wave seven lane 7: the rest of the Warp connection lifecycle

Issue #330. Base: `main` at `283c8115d2203232d0e9b473cc4bb0312c3a8509`
(a descendant of the required `c025b9a`). Code:
`crates/vesper-warp/src/{endpoint,lifecycle,handshake,cc}.rs` and an
additive API in `crates/vesper-warp-shard/src/{table,error,lib}.rs`.
Tests: `crates/vesper-warp/tests/lifecycle_rest.rs` (new),
`crates/vesper-warp/tests/quinn_interop.rs` (three new cases, one new
assertion, destinations tracked), `crates/vesper-warp-shard/tests/rekey.rs`
(new). No new dependencies, no root patch, no BUILD.bazel change
(`tools/bazel/sync_build_files.py` regenerates nothing).

The wave-six report (`warp-lifecycle.md`) listed four gaps; all four are
closed, plus two RFC 9000 rules found missing on the way.

## What is implemented

### 1. 1-RTT packets coalesced after long-header packets (RFC 9000 §12.2)

* `flush_handshake` now decides before building a datagram whether a
  1-RTT packet should follow its Initial/Handshake packets (1-RTT keys
  installed, and an application-space ACK owed or stream data allowed by
  congestion control and the launch queue). The long-header packets are
  then not `last`, and `append_short` seals one short-header packet at the
  end of the datagram: an owed ACK plus stream frames from the stream
  layer, recorded in recovery and BBR like any 1-RTT packet. A short header
  has no length, so it is always the last packet.
* Padding (§14.1): when the datagram must reach 1200 bytes (a client
  Initial, or an ack-eliciting server Initial), the PADDING goes inside the
  1-RTT packet, before sealing. Zero bytes after a short header would land
  after its AEAD tag, so the old "pad the datagram itself" fallback is used
  only when no 1-RTT packet was added.
* `send_close_code`: with 1-RTT keys and long-header keys both present, the
  long-header CONNECTION_CLOSE(s) and the 1-RTT application close leave in
  one datagram (§10.2.3).
* Counter: `EndpointStats::short_coalesced_sent`.

### 2. Retired connection IDs leave the shard table (§5.1.2)

* New additive API `ConnTable::rekey(handle, new_cid) -> Result<old, RekeyError>`
  in `vesper-warp-shard`: replaces a slot's local CID in both the hash index
  and the tile lanes, keeping the slot, handle and state; never allocates;
  typed errors (`Stale`, `DuplicateCid`, `CidLength`) leave the table
  unchanged. Tested on the scalar and the detected SIMD path, with both
  lookup paths agreeing.
* `Cids` tracks which sequence number keys the table (`table_seq`, 0 at
  first). When the peer retires it, the endpoint first issues a replacement,
  then rekeys the table to the connection's lowest live ID and removes that
  ID from the side list (`alt_cids`). The retired ID (sequence 0 included)
  then routes nowhere: a packet to it draws a stateless reset like any
  unknown CID. If no replacement exists yet, the retired ID keeps routing
  until one is issued (`table_retired`).
* Counter: `table_cids_rekeyed`.

### 3. Idle timer restart on the first ack-eliciting send (§10.1)

* A receive arms `idle_restart_armed`; the first ack-eliciting packet sent
  afterwards (1-RTT, coalesced 1-RTT, Initial/Handshake, 0-RTT) restarts the
  idle deadline from its send time and disarms it. ACK-only packets and
  CONNECTION_CLOSE do not restart it. Counter: `idle_restarts_on_send`.

### 4. Server preferred address and NAT rebinding (§9)

Server (`LifecycleConfig::preferred_address: Option<PreferredAddressConfig { ipv4, ipv6 }>`):

* `accept` generates a fresh connection ID of sequence 1 with its reset
  token and sends `preferred_address` (§18.2; an unset family is all
  zeros). The ID routes from then on and counts against the peer's
  `active_connection_id_limit`, so only `limit - 2` NEW_CONNECTION_IDs
  follow.
* The caller reports the local address each datagram arrived at with
  `WarpEndpoint::set_rx_destination` (the endpoint is sans-IO, like
  `set_rx_source`). A PATH_CHALLENGE that arrives at the preferred address
  is answered from it (§8.2.2: on the path it came in on).
* §9.6.2: after a non-probing packet at the preferred address with the
  largest packet number so far, the server validates the path from there
  (its own PATH_CHALLENGE, sent from the preferred address) and keeps
  sending non-probing packets from its original address until that
  succeeds. Then it sends from the preferred address
  (`ConnectionEvent::MovedToPreferredAddress`).
* The send source is reported through the new `TxTap::on_transmit_via(conn,
  pn, edt, datagram, from, to)`; its default calls `on_transmit_to`, so
  existing taps are unchanged. `from` is `None` for the original address.
* `disable_active_migration` does not stop any of this (§18.2).

Client (`LifecycleConfig::follow_preferred_address`, on by default):

* The server's sequence-1 ID and reset token are recorded from its
  transport parameters, and an address is chosen (the current path's family
  first, a zero address or port meaning "not offered").
* After the handshake is confirmed, the client probes the preferred address
  with a PATH_CHALLENGE sent to the sequence-1 ID, while everything else
  stays on the original path (§9.6.1). On success it moves there, switches
  to the sequence-1 ID, retires the old one, and resets congestion control
  unless only the port changed (§9.4). On failure it stays put
  (`PathFailed`).
* §9: a client discards 1-RTT packets from an address that is neither the
  server's original address, its preferred address, nor one being probed
  (`DropReason::UnknownServerAddress`, counter
  `unknown_server_address_drops`). The original address stays accepted
  because the server keeps sending from it until it has validated the new
  path, and because quinn-proto never changes a connection's source address.

NAT rebinding fixes:

* §8.2.1: a PATH_CHALLENGE must be padded to 1200 bytes "unless the
  anti-amplification limit for the path does not permit" it. The server
  padded unconditionally, so after a NAT rebinding seen in a small packet
  the padded challenge was refused by the 3x limit and no challenge was
  ever sent; the path then stayed unvalidated. Now an over-limit padded
  challenge is retried unpadded (the refused attempt changed nothing: it
  fails before a frame or packet number is taken).
* Tested rules that already held: only the highest-numbered non-probing
  packet moves the path (§9.3), a port-only change keeps the congestion
  controller (§9.4), and rebinding is followed with migration disabled.

### A latent BBR adapter fix found on the way

`QuicBbr::on_event_in` passed an acknowledged packet's recorded send time to
BBRv3 unclamped. A probe leaves at once but is stamped no earlier than the
last paced packet (recovery needs monotone send times), so its ACK could
arrive "before" its stamp and BBRv3 returned `AckedBeforeSent` (a
debug-assert panic). The change in timing from the CID work exposed it in
`client_migration_is_validated_and_followed`. The ACK path now clamps the
time to `now`, as `discard_space` and the TCP adapter already did
(`crates/vesper-warp/src/cc.rs`).

## Interop with quinn-proto

`tests/quinn_interop.rs` was re-run against `quinn-proto =0.11.13` (rustls
0.23 with aws-lc-rs, the same build Warp uses), over the same in-memory
link with UDP addresses as metadata. All 12 wave-six cases still pass; new:

| Case | Result |
| --- | --- |
| Warp client, quinn server with `preferred_address_v4` | Warp probes the preferred address with quinn's sequence-1 ID, quinn answers, Warp moves; a 60 kB echo then goes only to the preferred address; Warp drops nothing from quinn, which keeps sending from its original address |
| quinn client, Warp server with a preferred address | quinn accepts the parameter and the sequence-1 ID plus two NEW_CONNECTION_IDs (no CONNECTION_ID_LIMIT_ERROR), 50 kB echo; quinn-proto does not migrate to a preferred address itself |
| Warp closes before confirmation | one datagram with the Handshake APPLICATION_ERROR and the 1-RTT application close; quinn reports the close |
| CID retirement (assertion added to `warp_client_to_quinn_server`) | quinn retires Warp's sequence-0 ID; Warp's table is rekeyed and the old ID no longer routes |

Not interoperated: 1-RTT *data* coalesced behind the client's Handshake
Finished against quinn (it happens Warp to Warp, below; against quinn the
resumed client's stream had nothing sendable at that moment, and I did not
find out why within this lane); a Warp server moving to its preferred
address with a quinn client (quinn-proto has no client-side preferred
address migration); the idle-timer rule (local timing, nothing on the
wire).

## Acceptance tests

`tests/lifecycle_rest.rs`, SoftNic, both endpoints in-process with
addresses, one test per rule:

* `client_data_rides_behind_its_handshake_finished`: a resumed client's
  request leaves in one `[Initial, Handshake, 1-RTT]` 1200-byte datagram;
  the server processes the coalesced 1-RTT; no client Initial datagram is
  under 1200 bytes.
* `a_close_before_confirmation_coalesces_both_closes`: exactly one datagram,
  `[Handshake, 1-RTT]`; the server sees the peer close.
* `retired_connection_ids_including_sequence_zero_leave_the_table`: two
  migrations; after each, the retired ID (sequence 0 first) routes nowhere
  and the new one routes to the same slot; a third migration still finds a
  spare ID.
* `the_idle_timer_restarts_on_the_first_ack_eliciting_send_after_a_receive`:
  the first send moves the deadline, a second send before any receive does
  not, a receive re-arms it.
* `the_client_moves_to_the_servers_preferred_address`: events on both sides,
  then every client datagram goes to and every server datagram comes from
  the preferred address; the handshake ID left the server's table.
* `the_server_sends_from_its_original_address_until_it_validated_the_preferred_path`:
  while the client has moved and the server has not, only path frames leave
  from the preferred address.
* `a_dead_preferred_address_leaves_the_connection_on_the_original_one`.
* `a_client_may_ignore_the_preferred_address`.
* `a_server_with_migration_disabled_still_moves_to_its_preferred_address`.
* `a_client_discards_packets_from_unknown_server_addresses`: the data
  arrives once the server retransmits from its real address.
* `a_nat_rebinding_keeps_congestion_state_and_only_the_newest_packet_moves_the_path`:
  the older packet from the old port does not move the path back, the
  congestion window is kept, and the (now unpadded) challenge validates the
  new port.

`vesper-warp-shard/tests/rekey.rs`: rekey routing on both kernel paths,
no-op rekey, rekey then remove, and every rejected rekey leaving the table
unchanged.

The zero-allocation tests (`endpoint_zero_alloc.rs`, `stream_zero_alloc.rs`,
`zero_alloc.rs`, the shard's `zero_alloc.rs`) still pass. The new paths are
not claimed allocation-free beyond what they inherit: `append_short` and
`rekey` allocate nothing, but connection events still allocate.

## Public API (additive)

* `vesper-warp-shard`: `ConnTable::rekey`, `RekeyError`.
* `vesper-warp`: `WarpEndpoint::set_rx_destination`; `TxTap::on_transmit_via`
  (defaulted); `lifecycle::PreferredAddressConfig`;
  `LifecycleConfig::{preferred_address, follow_preferred_address}`;
  `EndpointStats::{short_coalesced_sent, table_cids_rekeyed,
  idle_restarts_on_send, preferred_address_migrations,
  unknown_server_address_drops}`.
* Additive but visible to exhaustive matches and struct literals:
  `ConnectionEvent::MovedToPreferredAddress`,
  `DropReason::UnknownServerAddress`, and the new `LifecycleConfig` and
  `EndpointStats` fields (construct with `..Default::default()`; every
  in-tree user already does). No dependent crate (`vesper-prism-h3`,
  `vesper-chorus`, `vesper-vault`) matches these enums exhaustively.

## Decisions

* Local addresses are caller-supplied metadata, like remote ones: UMEM frames
  carry only the QUIC datagram, so the driver says which address a datagram
  arrived at and the tap says which one to send from.
* The table keeps one key per slot and is rekeyed, rather than gaining
  several keys per slot: the tile layout (lane 6) stays as it is, lookups
  stay one probe, and the extra IDs keep their side list.
* A client that moves to the preferred address keeps accepting packets from
  the server's original address. RFC 9000 §9.6.2 requires the server to
  keep sending from there until it has validated the new path, and
  quinn-proto never changes its source address, so dropping them would
  break both.
* Idle restart uses the packet's recorded send time (its EDT for paced
  packets), the time it actually leaves.
* Rust ownership is not evidence that Vesper's source-level typing rules
  are implemented; no new linear obligations were added.

## Review follow-ups

Codex review of the first head raised four points, all fixed with tests:

* A server that moves to a preferred address on a different IP now resets
  congestion state (a port-only change keeps it, as on the client).
* When a PATH_CHALLENGE had to go out unpadded because of the
  amplification limit, the path is validated again with a padded probe
  before it is trusted for full-size datagrams (RFC 9000 §8.2.1);
  `EndpointStats::mtu_revalidations` counts these.
* The idle-restart flag is armed when a packet is received, before the
  lifecycle handlers queue their responses, so a response that is the first
  ack-eliciting send still restarts the timer.
* The unknown-server-address filter also runs on the copied receive path.

## Remaining gaps

* RFC 9000 §19.16's "MAY" (a RETIRE_CONNECTION_ID that names the packet's
  own destination CID) is not detected; such a retirement is honoured.
* A client moves to the preferred address only after confirmation; it does
  not migrate again on its own, and a server's preferred address is used
  once per connection.
* Packets from unknown server addresses are discarded for 1-RTT packets
  (both the zero-copy and the copied receive paths); long-header packets
  are not filtered by address.
* 1-RTT data coalescing was not seen against quinn (see Interop).
* Hardware: no AF_XDP, hardware EDT, RSS or crypto offload exercised.
  SoftNic is lane 3's software model, which validates the modelled protocol
  only. No performance claims.

## Checks

See the PR description for the exact commands and results on the final
head.
