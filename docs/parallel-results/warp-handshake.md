# Wave-five integration A: the TLS handshake inside WarpEndpoint

Issue #135. Base: `main` at `666e4b0` (lanes 12 and 13 merged as PR #138
and PR #143). Code: `crates/vesper-warp/src/handshake.rs` (new),
`crates/vesper-warp/src/endpoint.rs`, `crates/vesper-warp/src/stream.rs`.
Tests: `crates/vesper-warp/tests/handshake_endpoint.rs`. Root patch: one
`Cargo.lock` line (vesper-warp now depends on vesper-warp-tls); no new
external packages.

## What was missing

`WarpEndpoint` only accepted established 1-RTT connections opened with
caller-supplied traffic secrets (`open_connection`), dropped every
long-header packet, and assumed the peer's transport parameters equal its
own. Lane 12 delivered a sans-I/O TLS driver (`QuicTls`) and lane 13 left
per-space CRYPTO buffers and `open_connection_with_params` as hooks.

## What is implemented

### API

* `WarpEndpoint::connect(&ClientSetup, server_name, local_cid,
  original_dcid, now)`: a client connection with a TLS 1.3 handshake. The
  ClientHello leaves immediately.
* `WarpEndpoint::listen(Listener)`: a server accepts client Initials for
  unknown connection IDs. `Listener` holds a lane-12 `ServerSetup` and a
  `CidSource` (default `RandomCids`, rustls's aws-lc-rs CSPRNG).
* `poll_connection_event()` yields `Accepted`, `HandshakeComplete` and
  `HandshakeConfirmed`; `handshake_phase()` and
  `ConnectionView::{phase, tls}` expose the state, ALPN, peer certificates
  and per-space key states.
* `ConnectionView::keys` is now `Option<&OneRttKeys>` (`None` until the
  handshake installs 1-RTT keys). `open_connection` is unchanged.
* New errors: `WarpError::{Tls, Handshaking, KeysUnavailable,
  AmplificationLimited}`, `DropReason::{KeysUnavailable, BadInitial,
  AcceptRefused, Handshake, CoalescedOneRtt}`,
  `ConnectionError::Handshake { error_code }`.

### Packet path

* Received datagrams that start with an Initial or Handshake header are
  copied out of the UMEM frame (which goes straight back to the fill ring)
  and every coalesced long-header packet in them is processed: routing by
  connection ID (and, on a server, by the client's original DCID until
  Initial keys are discarded), header-protection removal and AEAD open with
  the space's lane-5 `Opener`, packet-number decode against the space's own
  largest received number, reserved bits, duplicate check, then frames:
  ACK (RFC 9002 recovery in that space), CRYPTO (lane 13's buffer for the
  space) and CONNECTION_CLOSE. Frame types not allowed in the packet type
  are rejected by lane 1's `scan_payload`.
* In-order CRYPTO bytes go from lane 13's per-space receive buffer to
  `QuicTls::on_crypto_frame`; TLS output goes into lane 13's per-space send
  buffer (with an overflow queue). Lost CRYPTO ranges are requeued from the
  packet records kept per space, so handshake data is retransmitted as
  ranges, like stream data. 1-RTT CRYPTO data (session tickets) uses the
  same route through the stream layer.
* Initial and Handshake packets are built with lane 1's `write_long`,
  sealed with the space's lane-5 `Sealer` under a nonce ticket, recorded in
  the connection's `LossRecovery` for their space and sent from fresh
  frames. Each carries an owed ACK, CRYPTO data (lost ranges first), and
  PING for a probe. Every client Initial and every ack-eliciting server
  Initial is padded to 1200 bytes (RFC 9000 §14.1). A client Initial in a
  smaller datagram, or with an original DCID under 8 bytes, cannot open a
  connection. A tentative server connection is removed again unless its
  first packet authenticates.
* Keys are installed per space from `Event::KeysReady` and discarded on
  `Event::DiscardKeys` (RFC 9001 §4.9): the client discards Initial keys
  when it sends its first Handshake packet, the server when it processes
  one; both discard Handshake keys on confirmation. Discarding a space also
  discards its recovery state (RFC 9002 §6.4).
* The server enforces the 3× anti-amplification limit until a Handshake
  packet validates the client's address (RFC 9000 §8.1), for long-header
  and 1-RTT packets alike, and reports it to recovery
  (`at_amplification_limit`), which then arms no PTO; the client's
  anti-deadlock PTO probe unblocks it. Recovery also gets
  `has_handshake_keys`, `confirmed` and the client's
  `PeerCompletedAddressValidation` (confirmation or a Handshake ACK).
* PTO probes in a long-header space requeue every unacknowledged CRYPTO
  range of that space and carry it (or a PING).
* The peer's transport parameters (authenticated by lane 12, including the
  §7.3 connection-ID checks) replace the stream layer's zero-credit
  defaults through the new `StreamSet::set_peer_params`; its
  `ack_delay_exponent` decodes its ACKs. Stream APIs return
  `WarpError::Handshaking` until then, and 1-RTT data waits for 1-RTT keys.
* The server queues HANDSHAKE_DONE (new `SentFrame::HandshakeDone`,
  resent until acknowledged, and added to every PTO probe while
  unacknowledged); the client confirms on HANDSHAKE_DONE or on an ACK of a
  1-RTT packet (RFC 9001 §4.1.2). Key updates, local or interval-driven,
  wait for confirmation.
* A TLS failure (bad certificate, ALPN mismatch, transport parameters,
  protocol violation) closes the connection with CONNECTION_CLOSE carrying
  lane 12's QUIC error code in every space we hold send keys for.

### Two changes to the existing 1-RTT path

* The short-header packet-number offset and the stream frame budget now
  use the peer's connection-ID length from the tile instead of assuming it
  equals ours (a server's peer CID is whatever the client chose).
* An application-data PTO probe leaves at once instead of being paced
  (RFC 9002 §6.2.4 sends probes regardless of the congestion window). A
  seed sweep at 40% loss showed BBRv3's loss response pushing the pacing
  schedule tens of seconds out; a paced probe then landed past the launch
  horizon, was refused, and the PTO backed off with nothing sent.

## Evidence

All on this host, rustc 1.97.0, debug build unless stated.

* `cargo test -p vesper-warp -p vesper-warp-tls --locked`: every test
  passes, including all wave-four and lane-13 tests and the existing
  zero-allocation tests.
* `tests/handshake_endpoint.rs`, two endpoints over lane 3's `SoftNic`:
  * No caller secrets: the client starts with no 1-RTT keys, stream and
    key-update calls return `Handshaking`, the first datagram is a
    1200-byte Initial; the handshake confirms within 6 wire steps; both
    sides then show ALPN agreed, Initial and Handshake keys discarded,
    1-RTT keys installed, the server's certificate, the client using the
    server's CID, and the negotiated stream parameters; 50 kB up and 70 kB
    down on one bidirectional stream arrive byte for byte; a key update
    after confirmation is accepted by the peer.
  * Lossy, reordering link: 10%, 15%, 20% and 30% loss with 10%
    reordering, handshake then 60 kB each way.
  * 40% loss with 20% reordering, 40 seeds: every handshake completes and
    a 2 kB exchange follows.
  * Each of the first six handshake datagrams (both directions) dropped
    in turn: every case recovers.
  * A client trusting a different CA closes with CRYPTO_ERROR + alert and
    the server receives that code; an ALPN mismatch closes with
    `0x100 + no_application_protocol` on the server and the client sees
    it.
  * Anti-amplification: with every client datagram after the ClientHello
    dropped, the server never sends more than 3 × 1200 bytes.
  * A garbage Initial is dropped (authentication) and leaves no
    connection; a 300-byte Initial is `BadInitial`; Initials without a
    listener and Handshake packets for unknown CIDs are `UnknownCid`.
  * Role and configuration checks for `connect` and `listen`.
  * "0 Alloc" after the handshake: with a counting global allocator on the
    test thread, 300 warm rounds of stream echo over a handshake-opened
    connection allocate nothing (the test wire's own datagram copies are
    excluded; endpoints and the software NIC are counted). The handshake
    itself allocates (rustls) and is not claimed.
* `handshake_seed_sweep` (ignored; `SEEDS=1000`, release): for 1000 seeds,
  a full handshake and a 4 kB exchange each way at 30% loss with 10%
  reordering and at 20% loss with 30% reordering, plus the handshake alone
  at 40% loss with 20% reordering: all pass.

## Not implemented

* Retry, Version Negotiation, 0-RTT, NEW_TOKEN, stateless reset,
  NEW_CONNECTION_ID / connection migration (we send
  `disable_active_migration`), and `preferred_address`.
* Coalescing on send. On receive, a short-header packet after long-header
  packets in one datagram is dropped (`CoalescedOneRtt`; RFC 9000 §12.2
  asks receivers to process it). Undecryptable packets that arrive before
  their keys are dropped, not buffered; retransmission recovers them.
* Initial and Handshake packets are in RFC 9002 recovery (RTT, loss,
  PTO, bytes in flight) but not in BBRv3's flight and not paced, because
  the BBR adapter keys its snapshots by application packet number.
* The peer's `max_idle_timeout` and `max_ack_delay` are not applied (the
  endpoint's own values are used); `max_udp_payload_size` is not used to
  size packets.
* At 40% loss with 20% reordering the data phase after the handshake can
  outlast the sweep's step budget: in seed 246, after a loss burst BBRv3's
  pacing schedule (lane 2) sat about 28 s ahead of the clock. Lane 13's
  own 40% sweep (60 seeds, 5% reordering, caller-supplied keys) passes, so
  it is not established whether handshake-opened connections are more
  exposed to this; it was not investigated further.
* Long-header packets are copied out of their UMEM frame: the zero-copy
  turn-around is the 1-RTT data path only.
