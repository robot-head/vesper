# Wave five lane 13: Warp QUIC streams, retransmission and flow control

Issue #133. Baseline `af03122ea74d403f203170926945f9d35786b289`.
Code: `crates/vesper-warp/src/stream.rs` (new), `crates/vesper-warp/src/frame.rs`
and `crates/vesper-warp/src/endpoint.rs`. No new dependencies, so there is
no root patch and `Cargo.lock` is unchanged.

## What was missing

The wave-four integration (`warp-integration.md`) reported lost packets to
the `Responder` and never retransmitted stream data. There was no stream
state, no flow control and no stream limits.

## What is implemented

### Frames (`frame.rs`)

RESET_STREAM, STOP_SENDING, MAX_STREAMS (both kinds), DATA_BLOCKED,
STREAM_DATA_BLOCKED and STREAMS_BLOCKED now parse and encode. They are
allowed only in 0-RTT and 1-RTT packets (RFC 9000 Table 3). A stream count
above 2^60 is a FRAME_ENCODING_ERROR on parse and an encode error.
`encode_stream_header` and `encode_crypto_header` write a frame header so
the data can be copied in two pieces from a ring buffer. NEW_TOKEN,
NEW_CONNECTION_ID, RETIRE_CONNECTION_ID and PATH_* stay `Unsupported`.
`TransportErrorCode` gained STREAM_LIMIT_ERROR, STREAM_STATE_ERROR,
FINAL_SIZE_ERROR and CRYPTO_BUFFER_EXCEEDED.

### Stream layer (`stream.rs`)

* `SendBuf`: a fixed ring holding the bytes not yet acknowledged, with
  one bit per byte for "acknowledged" and one for "lost". An acknowledged
  prefix frees ring space. A lost range is queued as bytes, not as a
  packet, and bytes that were already acknowledged are never queued again.
* `RecvBuf`: a fixed ring of exactly the advertised window with a
  "received" bit per byte. It reassembles out-of-order and duplicate data.
  Its memory is the window plus window/8 bytes, however the peer fragments
  the data. Data past the window is a typed error.
* `SendStream` and `RecvStream` implement the RFC 9000 §3.1 and §3.2 states
  (`SendState`, `RecvState`): FIN, RESET_STREAM, STOP_SENDING (which resets
  the sending part with the peer's code, §3.5) and every final-size rule
  (§4.5).
* `StreamSet`, one per connection:
  * Stream IDs by `Role` (§2.1). Peer streams open implicitly, with lower
    IDs opening first. A stream is forgotten once both parts are done
    (§3.3). Its buffers go back to a per-connection pool, and closing a
    peer stream raises our MAX_STREAMS.
  * Flow control: the connection and each stream have credit in both
    directions (§4.1, §4.2). MAX_DATA and MAX_STREAM_DATA are re-advertised
    once half a window has been consumed, and a reset's unread bytes are
    returned to connection credit. MAX_STREAMS limits apply per direction
    (§4.6). DATA_BLOCKED, STREAM_DATA_BLOCKED and STREAMS_BLOCKED are sent
    once per limit value.
  * Peer violations return a `TransportError`: FLOW_CONTROL_ERROR
    (stream or connection credit, including a reset's final size),
    STREAM_LIMIT_ERROR, STREAM_STATE_ERROR (a stream of ours that was never
    opened, the wrong direction on a unidirectional stream),
    FINAL_SIZE_ERROR and CRYPTO_BUFFER_EXCEEDED.
  * One `CryptoStream` per packet-number space (Initial, Handshake and
    application data). Each uses the same `SendBuf` and `RecvBuf` code,
    with no flow control and a buffer bound of at least 4096 bytes (§7.5).
    The endpoint carries only 1-RTT CRYPTO frames, because long headers
    are still dropped. `crypto_mut(space)` and `on_crypto_frame` are the
    hooks for integration A.
  * Packet building with `fill`: control frames with their current values
    first, then 1-RTT CRYPTO, then stream data. Lost ranges go before new
    data, new data stays under both credits, and streams take turns
    round-robin. Each packet's retransmittable frames go into a
    `PacketFrames` record, and `commit(pn, record)` keys it by packet
    number.
  * §13.3 repairs: `on_packet_lost(pn)` requeues lost STREAM and CRYPTO
    ranges. It resends RESET_STREAM until it is acknowledged, and
    STOP_SENDING while the stream is still receiving. MAX_DATA,
    MAX_STREAM_DATA (while in Recv) and MAX_STREAMS are resent with their
    current values. A *_BLOCKED frame is resent only if its limit has not
    changed. `on_packet_acked(pn)` frees buffer space and advances the
    state machines.
  * Bounded bookkeeping. Frame records are capped at `tracked_frames`.
    Past that, the oldest packet's frames are treated as lost, which can
    only cause a spurious retransmission and never loses a byte. Events
    are capped at `event_capacity`, and any past that are counted and
    dropped.

### Endpoint (`endpoint.rs`)

* `EndpointConfig` gained `role`, `streams: StreamConfig` and
  `stream_mode`.
  * `StreamMode::Reliable` is the default. Each connection gets a
    `StreamSet`. The new API is `open_stream`, `stream_write`,
    `stream_finish`, `stream_read`, `stream_reset`,
    `stream_stop_sending`, `poll_stream_event`, `flush`, `streams_mut`
    and `connection_error`. `ConnectionView::streams` shows the state.
  * `StreamMode::Raw` keeps the wave-four behaviour: the `Responder` gets
    STREAM frames, and `send_stream` sends one raw frame.
  * Each mode's API returns `WarpError::StreamMode` when called in the
    other mode.
* Without a handshake, `open_connection` assumes the peer's stream
  transport parameters equal ours. `open_connection_with_params` takes the
  peer's `PeerParams` as TLS would deliver them.
* `poll` calls `flush` for every open reliable connection. `flush` packs
  pending stream frames into fresh, paced, ack-eliciting packets while the
  congestion window, the launch queue and the free transmit frames allow,
  and adds an owed ACK when there is one. A reply to an ack-eliciting
  packet carries pending stream frames alongside its ACK. A PTO probe
  carries stream data when some is pending, and is a PING otherwise.
* The frame budget is exact: datagram size, minus a 4-byte packet number
  header, the owed ACK as it will be encoded, and the tag. If a packet's
  frames end up not being sent, they are requeued. That covers a build
  error, recovery refusing the packet, and a reply that falls back to
  ACK-only.
* RFC 9002 `Acked` and `Lost` events reach the stream layer through
  `resolve_events`. That is the path that turns loss detection into
  retransmission.
* A peer violation, or a malformed stream frame caught by the stream
  layer, closes the connection.
  * The receiver sends CONNECTION_CLOSE (transport variant, the error code
    and the frame type) and enters a closing state (RFC 9000 §10.2.1). In
    that state it drops incoming packets and answers them with
    CONNECTION_CLOSE at most once per PTO base. It does not send probes,
    and its stream API returns `ConnectionClosed`.
  * The peer records `ConnectionError::Peer`, drains and stops sending.
    Frames that fail to parse are still dropped as `DropReason::Frame`, as
    in wave four.

## Evidence

All of the following ran on this host with rustc 1.97.0, in a debug build
unless stated.

* `cargo test -p vesper-warp --locked`: every test passes, including all
  wave-four tests.
* `tests/streams.rs` (9 tests, the stream layer without the endpoint):
  * The round trip of the new frames, the 2^60 limit and Table 3.
  * Send and receive state machines through every state, including a FIN
    alone in its own frame, the stream being forgotten on both sides, and
    MAX_STREAMS being raised and delivered.
  * RESET_STREAM lost and resent, then ResetRecvd and ResetRead.
    STOP_SENDING leading to a reset with the peer's code.
  * A 100 kB transfer with random loss of 1%, 5%, 20% and 40% and full
    reordering, through 8 KiB stream and 12 kB connection windows and a
    4 KiB send ring. It is checked byte for byte, and the frame records are
    empty at the end.
  * Flow control and blocked frames: DATA_BLOCKED and
    STREAM_DATA_BLOCKED are sent, credit is never exceeded, a lost
    MAX_DATA is resent with its current value, and STREAMS_BLOCKED is sent
    at the stream limit.
  * Every peer violation returns its RFC 9000 code.
  * CRYPTO buffers per space, with loss and out-of-order delivery.
  * Evicting frame records causes only spurious retransmission.
* `tests/stream_endpoint.rs` (10 tests, plus one ignored seed sweep, all
  through `WarpEndpoint` over lane 3's software NIC with AEAD, recovery,
  BBRv3 and pacing):
  * A 300 kB echo on a clean link, with no retransmission. Both streams
    close and the server's MAX_STREAMS goes up.
  * A 200 kB echo at **1%, 5% and 20% loss with 5% reordering** in each
    direction. Every byte arrives exactly once and in order in both
    directions, MAX_DATA is never exceeded at any step, retransmission
    actually happened, and there are no transport errors. One run at 20%
    loss dropped 140 datagrams. The client retransmitted 43,551 stream
    bytes and the server 64,796, over 710 ms of simulated time.
  * 4 KiB stream and 6 kB connection windows at 2% loss: a 60 kB echo
    completes, blocked frames are sent and credit is re-advertised.
  * Eight concurrent streams: client bidi and uni, and server uni, at 3%
    loss with reordering. Every stream arrives intact and every stream
    closes.
  * RESET_STREAM and STOP_SENDING across the wire.
  * A raw-mode client that ignores a 4 KiB window gets FLOW_CONTROL_ERROR.
    The server sends CONNECTION_CLOSE, and the client sees
    `Peer { error_code: 0x03, frame_type: Some(0x0e) }`. The same setup
    with stream index 8 against a limit of 8 gets STREAM_LIMIT_ERROR
    (0x04).
  * `sweep_seeds` (ignored, run by hand): 60 seeds × {1%, 5%, 20%, 30%}
    passed in a debug build (36 s) and in a release build.
* `tests/stream_zero_alloc.rs`: a counting global allocator saw **0 heap
  allocations** over 300 warmed-up rounds. Each round wrote 2 KiB, packed
  and paced it, acknowledged it, reassembled it, read it, echoed it on the
  same stream and read the echo. Credit updates and stream events were
  included in the window.
* `tests/endpoint_zero_alloc.rs` (the wave-four test) still passes and
  still sees 0 allocations. What changed: its config now sets
  `stream_mode: StreamMode::Raw`, because it measures the raw
  `send_stream` and `Echo` path that wave four claimed.
  `tests/endpoint.rs` and `tests/review_fixes.rs` set Raw mode for the
  same reason. `tests/support` now opens the server endpoint as
  `Role::Server`. `tests/frames.rs` no longer lists 0x04, 0x05 and 0x12 as
  unsupported.

Commands run: `cargo fmt --all -- --check`, `cargo test -p vesper-warp
--locked`, `cargo clippy -p vesper-warp --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` and `cargo clippy --workspace
--locked --all-targets -- -D warnings`. The results are in the PR.

## Decisions

* Retransmission state is a bitmap over each ring, not a range list. Its
  memory is fixed and it cannot fail when the peer fragments data or
  acknowledges in an odd pattern. The cost is one pass over a few words
  per acknowledged or lost frame.
* Without TLS there are no transport parameters to negotiate. Both
  endpoints assume symmetric limits unless the caller passes `PeerParams`.
* A stream's buffers are allocated the first time a stream slot is used,
  then pooled. The zero-allocation claim covers the warm steady state,
  not the first time a stream is opened.
* A stream is forgotten only once both parts are done, as RFC 9000 §3.3
  requires. The application must finish or reset the sending part of a
  peer's bidirectional stream, and read a reset receiving part, before
  the peer gets that stream credit back.
* Raw mode is kept so that the wave-four tests and their evidence stay
  meaningful. It is also how the tests inject peer violations.

## Not done, or unavailable

* No TLS handshake. Lane 12 builds it, and integration A (#135) feeds its
  CRYPTO data through `crypto_mut` and `on_crypto_frame` and its
  transport parameters through `open_connection_with_params`.
* Initial and Handshake CRYPTO buffers exist but never reach the wire,
  because long-header packets are still dropped.
* Not implemented: stream priorities, other application-level
  scheduling, connection-level application close (CONNECTION_CLOSE 0x1d
  sent by us), draining and closing timers that free the slot by
  themselves, and the frames in the first bullet above.
* Rust ownership in this crate is host code. It is not evidence that the
  Vesper source typing rules for linear stream obligations are
  implemented. This lane owns no Lean file, and nothing here is proved.
