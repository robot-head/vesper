# Wave six lane 5: the Warp connection lifecycle and 0-RTT

Issue #247. Base: `main` at `07322e3ba47890e939a48967a1ea8f62fdd7aac1`
(a descendant of the required `cda092f`). Code:
`crates/vesper-warp/src/{endpoint,lifecycle,cc,stream,frame,recovery,handshake,lib}.rs`
and `crates/vesper-warp-tls/src/{config,handshake,lib}.rs`. Tests:
`crates/vesper-warp/tests/{lifecycle_endpoint,lifecycle_frames,quinn_interop,pacing_lead}.rs`,
`crates/vesper-warp-tls/tests/zero_rtt.rs`, plus updates to
`handshake_endpoint.rs` and `frames.rs`. Root patch: `Cargo.lock` only
(vesper-warp's new direct dependencies `aes-gcm`, `hkdf`, `sha2`,
`subtle`, `zeroize`, already workspace dependencies at the versions in the
lockfile; dev-dependencies `quinn-proto =0.11.13` and `bytes =1.12.1`).
No BUILD.bazel change: Bazel takes dependencies from Cargo metadata.

## What was missing

After wave five, `WarpEndpoint` completed TLS 1.3 handshakes but had no
Retry, Version Negotiation, NEW_TOKEN, stateless reset, connection-ID
management, path validation or migration. It sent
`disable_active_migration`, dropped a short-header packet coalesced after
long-header packets and every packet that arrived before its keys, used its
own idle timeout, max_ack_delay and datagram size instead of the peer's,
kept Initial and Handshake packets out of BBRv3's flight and pacing,
answered CONNECTION_CLOSE but never freed the slot on a timer, had no
application close, no stream priorities and no 0-RTT.

## What is implemented

### 1. Retry, Version Negotiation, NEW_TOKEN, stateless reset

* `LifecycleConfig { address_validation: AddressValidation::{None,
  Retry}, retry_token_lifetime_ns, new_token_lifetime_ns, stateless_reset,
  active_connection_id_limit, migration, buffered_datagrams }` in
  `EndpointConfig::lifecycle` (default: no Retry, reset on, limit 4,
  migration on, 8 buffered datagrams).
* Address-validation tokens (`lifecycle::TokenKey`, AES-256-GCM from
  `aes-gcm`) bind the client address, the original DCID (Retry) and an
  expiry; Retry and NEW_TOKEN tokens are told apart by a sealed kind byte.
  `set_token_key` shares a key across endpoints.
* Server: under `AddressValidation::Retry`, an Initial without a valid
  token is answered statelessly with a Retry (integrity tag per RFC 9001
  §5.8). A valid Retry token validates the address (no 3× limit) and the
  server sends `original_destination_connection_id` and
  `retry_source_connection_id`. After confirmation the server sends
  NEW_TOKEN; a NEW_TOKEN token skips the Retry next time. Forged, expired or
  misaddressed tokens are refused (counted, Retry again or dropped).
* Client: a Retry is accepted once, only with a valid integrity tag and a
  new SCID; the client restarts its Initial with the token and keeps
  counting packet numbers. Received NEW_TOKEN tokens are cached per server
  name (16 entries, `stored_tokens()`) and used by the next `connect`.
* Version Negotiation: a server answers an unsupported version with VN
  (only for datagrams ≥ 1200 bytes); a client that receives a VN that
  does not list version 1 closes with `ConnectionError::VersionNegotiation`;
  a VN listing its own version, or arriving after any other packet, is
  ignored (RFC 9000 §6.2).
* Stateless reset (`lifecycle::ResetKey`, HMAC-SHA-256): tokens are sent
  for every issued CID (and as the server's transport parameter). A
  short-header datagram for an unknown CID is answered with a reset shorter
  than it (≥ 21 bytes, never in reply to one that could itself be a reset);
  an undecryptable datagram whose last 16 bytes match a peer token closes
  the connection with `ConnectionError::StatelessReset`, draining silently.

### 2. Connection IDs, path validation, migration

* After confirmation both sides issue `active_connection_id_limit - 1`
  NEW_CONNECTION_ID frames with reset tokens and register the CIDs in the
  shard (`alt_cids`). Received NEW_CONNECTION_ID frames are validated
  (limit → CONNECTION_ID_LIMIT_ERROR, a changed CID for a used sequence →
  PROTOCOL_VIOLATION), Retire Prior To retires and answers with
  RETIRE_CONNECTION_ID; received RETIRE_CONNECTION_ID frees ours and issues
  a replacement.
* `migrate(conn, now)` (client): switches to a spare peer CID, retires the
  old one, resets congestion control and RTT for the new path and sends a
  padded PATH_CHALLENGE. Errors: `NoConnectionId`, `MigrationDisabled`
  (either side disabled it).
* Server: a non-probing packet from a new address with the highest packet
  number moves the path, starts validation (3× limit on the new path until
  validated), and falls back to the old path if validation fails.
  PATH_RESPONSE answers every PATH_CHALLENGE on the path it came from.
  `PathValidated`, `PathFailed` and `PeerMigrated` events report it.
* `disable_active_migration` is sent only when
  `LifecycleConfig::migration` is false.

### 3. Coalescing and buffered packets

* Send: Initial, Handshake and 0-RTT packets of one flush are coalesced
  into one datagram (padded to 1200 bytes when it carries a client Initial
  or an ack-eliciting server Initial) and paced as one frame.
* Receive: a 1-RTT or 0-RTT packet after long-header packets in one
  datagram is processed from a copy (`coalesced_received`).
* Packets whose keys are not yet installed are copied into a bounded
  buffer (`buffered_datagrams`, at most 8) and replayed when keys change;
  overflow is counted (`datagrams_buffer_full`) and dropped.

### 4. Peer transport parameters and BBRv3

* The idle timeout is the minimum of both sides' `max_idle_timeout` and at
  least 3×PTO; expiry closes silently with `ConnectionError::IdleTimeout`
  and frees the slot. The peer's `max_ack_delay` feeds RFC 9002 PTO; the
  peer's `max_udp_payload_size` caps our datagrams.
* Initial and Handshake packets are in BBRv3's flight and EDT pacing, with
  their own snapshot buffers (`QuicBbr::on_packet_sent_in`,
  `on_event_in`, `discard_space` when keys are discarded).

### 5. Close, drain, priorities

* `close(conn, error_code, now)`: application CONNECTION_CLOSE (0x1d) in
  1-RTT, or the transport `APPLICATION_ERROR` (0x1c) in long-header spaces
  before 1-RTT keys (RFC 9000 §10.2.3). Received 0x1c/0x1d close with
  `ConnectionError::Peer`.
* Closing (we closed) answers later packets with CONNECTION_CLOSE at most
  once per PTO; draining (the peer closed) is silent. Both end after 3×PTO
  on a lifecycle timer wheel; the slot is freed, `ConnectionEvent::Closed`
  is reported and `connection_error` still answers for the freed slot.
* `stream_set_priority(conn, id, Priority { urgency, incremental })`:
  RFC 9218 order in the send scheduler (lower urgency first;
  non-incremental streams of one urgency one at a time, incremental ones
  round-robin).

### 6. 0-RTT and resumption

* `vesper-warp-tls`: `ClientSetup::with_zero_rtt` and
  `ServerSetup::with_zero_rtt`; `QuicTls::{early_data, is_resumed,
  zero_rtt_key_state, discard_zero_rtt}`, `EarlyData::{NotOffered,
  Offered, Accepted, Rejected}`, `TlsEvent::ZeroRttKeys(ZeroRttKeys)` and
  `TlsEvent::RememberedTransportParameters`.
* Anti-replay: rustls accepts early data only with stateful tickets, which
  it removes from the store on use, so each ticket admits 0-RTT once (RFC
  8446 §8.1 single-use tickets). A configured stateless `ticketer` turns
  0-RTT off. Tested: a replayed ClientHello gets no 0-RTT.
* Endpoint: the client installs 0-RTT keys, applies the remembered
  transport parameters, and sends stream data in 0-RTT packets coalesced
  with its Initial. The server opens 0-RTT (also from coalesced
  datagrams). Accepted: the real parameters raise limits. Rejected: the
  client resets its streams to the server's real parameters, drops 0-RTT
  packets from recovery and BBR (`LossRecovery::reset_space`, fresh
  controller), reports `ZeroRttRejected` and the application resends.

### 7. The seed-246 stall

Root cause: BBRv3's pacing schedule (lane 2's EDT) ran far ahead of the
clock. Under 40% loss and 20% reordering, BBRv3's bandwidth estimate
collapsed (pacing about 2-7 kB/s, delivery-rate samples from two acks over
a lossy second), so each paced packet moved `next_edt` hundreds of
milliseconds forward. With retransmissions and probes it ended about 27 s
ahead. `launch_has_room` requires `next_edt ≤ now + launch_horizon` (1 s),
so `flush` sent nothing. With nothing in flight no ACK arrived to give
BBR a new sample, and recovery had no PTO to arm, so the connection sat
until the clock caught up: 4000 bytes pending, `can_send()` true,
inflight 0 and the launch queue empty.

Fix (`crates/vesper-warp/src/cc.rs`): `QuicBbr` holds its schedule within
`max_lead_ns` (the endpoint passes its launch horizon). While BBR's own
schedule leads the clock by more than that, the adapter paces from the
clock at BBR's current rate, and its own schedule is capped at
`now + max_lead_ns` too. The first version of the fix lacked that second
cap and still stalled about 10 s ahead under the same trace. With the cap,
the original seed-246 trace on `main` completes its data phase in about
6.4 s of simulated time. Regression test: `tests/pacing_lead.rs` (5000
back-to-back packets stay within the lead, and BBR's own schedule is
shown to run past it). BBRv3 itself (lane 2's crate) is unchanged. A
pacing rate that low is BBR's model working as specified under that much
loss; the endpoint just no longer lets it stop sending entirely.

The ignored `handshake_seed_sweep` in `handshake_endpoint.rs` now runs the
40%/20% data phase too (previously handshake only). With 500 seeds
(`SEEDS=500 cargo test --release -p vesper-warp --test handshake_endpoint
handshake_seed_sweep -- --ignored`), 499 data phases complete and 1 ends in
an idle timeout (4 before the handshake-burst change below). None stall.
The idle timeouts were checked: at this loss
rate a run of about seven lost PTO probe round trips (0.64⁷ ≈ 4%) backs
off past the 30 s idle timeout, and the connection closes as RFC 9000
§10.1 requires. The sweep accepts that outcome, but at most 5% of seeds,
and fails on any other.

### Post-quantum ClientHello (found in CI)

The Windows and Bazel builds negotiate X25519MLKEM768 (rustls's
`prefer-post-quantum`, enabled there by feature unification), so the
ClientHello spans two Initial packets and the ServerHello two Initials.
Three fixes followed, all tested locally by building with that feature:

* A coalesced handshake datagram whose pacing slot is within 1 ms leaves
  immediately (RFC 9002 §7.7 burst allowance; the pacing schedule still
  advances), so the second ClientHello Initial is not held a poll interval.
* `vesper-warp-tls`: a resuming client could take its remembered transport
  parameters for the server's when Handshake CRYPTO reached rustls before
  EncryptedExtensions, and close with TRANSPORT_PARAMETER_ERROR. The
  remembered list is now recorded and never taken for the server's while the
  handshake is still running.
* A datagram held for its keys is no longer also counted in `rx_dropped`.

A server answers each token-less Initial datagram with its own Retry (or
VN), as quinn does too, so a split ClientHello can draw two; the client
follows the first and ignores the rest (RFC 9000 §17.2.5.2).

## Interop with quinn-proto

`tests/quinn_interop.rs` drives `quinn-proto 0.11.13` (rustls 0.23.46 with
aws-lc-rs, the same rustls build Warp uses) by hand against a
`WarpEndpoint` over an in-memory link with real UDP addresses as metadata.
It does not use loopback sockets: the endpoint is sans-IO over the SoftNic
model, so the link is a function call. Both directions:

| Feature | Warp client → quinn server | quinn client → Warp server |
| --- | --- | --- |
| TLS 1.3 handshake, ALPN, certificate verification | yes | yes |
| Bidirectional stream echo (100 kB each way) | yes | yes |
| Application CONNECTION_CLOSE (0x1d) with error code | Warp closes, quinn sees `ApplicationClosed(0x42)` | quinn closes, Warp sees `Peer { 0x17 }` |
| Retry with integrity tag and token | quinn sends Retry, Warp follows | Warp sends Retry, quinn follows |
| 0-RTT on a resumed session | Warp sends early stream data, quinn's TLS accepts | quinn sends early data, Warp accepts (`accepted_0rtt()`) |
| NEW_CONNECTION_ID and migration with path validation | Warp migrates onto a quinn CID; quinn validates, Warp validates | Warp issues 3 CIDs, quinn accepts |
| NEW_TOKEN reused by the next connection | quinn (with a token log) issues one, Warp's next connection skips quinn's Retry | Warp issues one, quinn's next connection skips Warp's Retry |
| Version Negotiation | (Warp only speaks v1) | quinn offers draft-29, Warp answers VN, quinn closes with `VersionMismatch` |
| Stateless reset | quinn restarted with the same reset key resets, Warp detects | Warp forgets the connection, quinn sees `Reset` |
| 10% loss, 4 seeds each way | yes | yes |

Not interoperated: 0-RTT rejection (quinn-proto has no switch to reject
early data on one resumed session while completing the handshake), quinn
migrating towards a Warp server (the hand-driven quinn-proto link has no
local-address change), key updates, and stream priorities (local
scheduling only, nothing on the wire). Warp sending VN to a quinn client is
covered; the reverse cannot happen, since Warp only offers version 1.

## Acceptance tests

`tests/lifecycle_endpoint.rs` (SoftNic, both endpoints in-process, with
addresses):

* `a_plain_handshake_issues_connection_ids_and_a_token`
* `retry_validates_the_client_address` (then a NEW_TOKEN skips a second
  Retry)
* `a_forged_or_misaddressed_retry_token_is_refused`
* `version_negotiation_both_ways`
* `a_server_that_lost_state_resets_the_connection`
* `client_migration_is_validated_and_followed`
* `migration_to_a_dead_path_falls_back`
* `an_idle_connection_times_out_on_both_sides`
* `an_application_close_drains_and_frees_both_slots`
* `zero_rtt_data_is_accepted_on_a_resumed_connection`
* `rejected_zero_rtt_resets_streams_and_the_application_resends`
* `handshake_packets_before_their_keys_are_buffered`
* `stream_priorities_order_the_sends`
* `lossy_sweep_of_handshake_opened_connections`: 12 seeds at 10-30% loss
  and reordering, alternating Retry and 0-RTT, two connections each, all
  transfers checked byte for byte.

`tests/lifecycle_frames.rs`: NEW_TOKEN, NEW_CONNECTION_ID,
RETIRE_CONNECTION_ID, PATH_CHALLENGE/RESPONSE and HANDSHAKE_DONE round
trips, Table 3 packet-type permissions, malformed frames as typed errors.
`vesper-warp-tls/tests/zero_rtt.rs`: resumption with 0-RTT keys both
ways, replay refused, clean rejection, unknown ticket fallback, opt-in on
both sides required.

The zero-allocation tests (`endpoint_zero_alloc.rs`, `stream_zero_alloc.rs`,
`zero_alloc.rs`, and the steady-state echo in `handshake_endpoint.rs`)
still pass with the lifecycle code in the path. The lifecycle paths
themselves (Retry, migration, buffering, close) are not claimed
allocation-free: Retry and buffered datagrams copy into preallocated
buffers, but the token cache and connection events allocate.

## Public API (additive)

New: `lifecycle` module (`LifecycleConfig`, `AddressValidation`,
`TokenKey`, `ResetKey`, `PeerAddr`, CID and path types);
`WarpEndpoint::{set_rx_source, set_token_key, set_reset_key,
set_cid_source, stored_tokens, stream_set_priority, close, migrate}`;
`EndpointConfig::lifecycle`; lifecycle counters on `EndpointStats`;
`stream::Priority` and `StreamSet::{set_priority, priority,
queue_new_connection_id, queue_retire_connection_id, queue_new_token,
reset_streams, raise_peer_params}`; `QuicBbr::{with_max_lead_ns,
next_edt_ns, on_packet_sent_in, on_event_in, discard_space}`;
`LossRecovery::{set_max_ack_delay_ns, reset_space}`;
`TransportErrorCode::ConnectionIdLimitError`; the TLS additions above.

Additive but visible to exhaustive matches and struct literals: new
variants `WarpError::{NoConnectionId, MigrationDisabled}`,
`ConnectionError::{Application, IdleTimeout, StatelessReset,
VersionNegotiation}`, `ConnectionEvent::{ZeroRttAccepted,
ZeroRttRejected, Retried, NewToken, PathValidated, PathFailed,
PeerMigrated, Closed}`, new `EndpointConfig` and `EndpointStats` fields
(construct with `..Default::default()`), and a transport parameter change:
`disable_active_migration` is now off by default. For lane 6 (HTTP/3):
`close()` sends an application error code (0x1d), and packets carrying
NEW_CONNECTION_ID are processed, not dropped.

## Decisions

* Addresses are caller-supplied metadata (`set_rx_source`, the `TxTap`
  destination), because UMEM frames carry QUIC datagrams only. With no
  addresses a connection has no path to validate and behaves as before.
* Tokens and reset tokens use `aes-gcm`, `hkdf`'s HMAC and `sha2`, with
  keys from rustls's aws-lc-rs CSPRNG and `subtle` comparisons; the Retry
  integrity tag uses lane 5's `vesper-warp-crypto::retry` (the RFC 9001
  fixed key through `aes-gcm`).
  Nothing is hand-rolled.
* The seed-246 fix is in the adapter (`cc.rs`, this lane's path), not in
  lane 2's BBRv3, so BBR's model and its tests are untouched.
* quinn-proto rather than quinn: a sans-IO driver fits Warp's sans-IO
  endpoint and needs no runtime or sockets.
* Rust ownership here is not evidence that Vesper's source-level typing
  rules (linearity, `[T-Let]`) are implemented; no new linear obligations
  were added.

## Remaining gaps

* 1-RTT packets are not coalesced after long-header packets on send (they
  leave in their own datagram).
* The sequence-0 connection ID stays in the shard table until the
  connection closes, even after it is retired.
* The idle timer restarts only on receive, not also on the first
  ack-eliciting send after a receive (RFC 9000 §10.1).
* No server-initiated migration (`preferred_address`) and no NAT-rebinding
  heuristics beyond RFC 9000 §9.3.
* Hardware: no AF_XDP, hardware EDT, RSS or crypto offload exercised;
  SoftNic is lane 3's software model, which validates the modelled
  protocol only. No performance claims.

## Checks

See the PR description for the exact commands and results on the final
head.
