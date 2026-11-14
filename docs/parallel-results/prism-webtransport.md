# Prism WebTransport, HTTP Datagrams and priorities (wave seven, lane 8)

Issue #331. Base: `origin/main` at `283c8115d2203232d0e9b473cc4bb0312c3a8509`
(a descendant of the required `c025b9a`). Branch:
`claude/project-thread-u4yyw9` (the session's assigned branch). Design
section: §17 (Prism), on §35 (Warp).

Only `crates/vesper-prism-h3/**`, this report and the root patch (three new
`Cargo.lock` packages for test-only dependencies, in its own commit)
changed. `vesper-prism` and `vesper-warp` are used read-only, through APIs
already on `main`; nothing in `vesper-warp` was edited (lane 7 owns it).

## What was built

All in `vesper-prism-h3` (still `#![forbid(unsafe_code)]`):

| Module | Contents |
| --- | --- |
| `capsule` (new) | RFC 9297 §3 Capsule protocol: an incremental, bounded `CapsuleReader`; DATAGRAM, CLOSE_WEBTRANSPORT_SESSION and DRAIN_WEBTRANSPORT_SESSION; unknown capsules skipped without buffering |
| `priority` (new) | RFC 9218: a Structured Field dictionary parser for `priority`, PRIORITY_UPDATE frames (0xf0700/0xf0701), and the scheduler `schedule` |
| `session` (new) | Extended CONNECT sessions on the shared core: WebTransport streams, HTTP Datagrams (QUIC DATAGRAM frames or capsules), held streams, session ends, `SessionEvent`, `SessionError`, `SessionStats` |
| `frame` | New SETTINGS (ENABLE_CONNECT_PROTOCOL, H3_DATAGRAM, the three WebTransport identifiers, NO_RFC7540_PRIORITIES); WEBTRANSPORT_STREAM (0x41) signal and stream type 0x54 |
| `message` | `:protocol` (RFC 9220 §4): CONNECT without it is still refused; with it, `:scheme`, `:path` and `:authority` are required; on another method it is malformed |
| `transport` | Optional methods with defaults (`max_datagram_size`, `send_datagram`, `recv_datagram`, `set_priority`), so other transports are unchanged |
| `warp` | `WarpTransport::set_priority` maps to Warp's existing `stream_set_priority` (Warp itself is unchanged) |
| `connection` | Streaming request bodies, the per-connection memory budget, the priority scheduler in `flush`, interim responses, `Responder::accept_session` and the linear `Session` |
| `client` | Extended CONNECT sessions, streaming request bodies, PRIORITY_UPDATE and interim responses on the test client |
| `prism` | `serve` answers extended CONNECT with 501 and resets streaming requests, so the existing Service bridge is unchanged |

### Extended CONNECT and WebTransport (RFC 9220, draft-ietf-webtrans-http3)

* `H3Config::with_webtransport(max_sessions)` sends `SETTINGS_ENABLE_CONNECT_PROTOCOL=1`,
  `SETTINGS_H3_DATAGRAM=1` (only when the transport reports QUIC datagram
  support), and all three WebTransport identifiers in use by deployed peers
  (`ENABLE_WEBTRANSPORT` 0x2b603742, draft-07 `WEBTRANSPORT_MAX_SESSIONS`
  0xc671706a, current `WT_MAX_SESSIONS` 0x14e9cd29). Any of them on the peer
  is accepted. A boolean setting other than 0 or 1 is `H3_SETTINGS_ERROR`.
* An extended CONNECT arrives as `Event::Connect(ConnectRequest)` with a
  `SessionKind` (`WebTransport` for `:protocol = webtransport`, `Tunnel`
  for any other protocol, which gets HTTP Datagrams and capsules only). The
  request's `Responder` either answers normally (refusal) or is consumed
  by `accept_session`, which sends a 2xx and returns a `Session`.
* `Session` is a linear obligation: non-Clone, non-Copy, `#[must_use]`; its
  only exit is `Session::close(code, reason)` (CLOSE_WEBTRANSPORT_SESSION
  capsule, then FIN). A dropped `Session` is reported to the connection's
  leak sink and the CONNECT stream is reset with H3_INTERNAL_ERROR; the
  test `a_dropped_session_is_detected_and_ends_the_session` covers it. A
  peer FIN, reset or close capsule surfaces as `SessionEvent::Closed`, after
  which `close` returns `SendError::Closed`-style errors without panicking.
* Streams: bidirectional streams with the 0x41 signal and unidirectional
  streams with type 0x54 are bound to their session. Streams that arrive
  before the session is accepted (or before the CONNECT stream itself) are
  held, at most 16 per connection, and released or refused with
  WT_BUFFERED_STREAM_REJECTED; streams for an ended session are refused
  with WT_SESSION_GONE. `open_stream`, `write_stream`, `finish_stream`,
  `read_stream`, `reset_stream` and `stop_stream` work on both ends. Our
  application error codes map to and from the WebTransport range
  (`0x52e4a40fa8db + n + n/0x1e`, skipping reserved values).
* Datagrams (RFC 9297 §2.1): with QUIC datagrams the payload is the quarter
  stream ID plus data in one DATAGRAM frame; without them it falls back to a
  DATAGRAM capsule on the CONNECT stream (`DatagramPath::Capsule`). A
  datagram larger than the path allows is `SessionError::TooLarge`; a
  datagram for an unknown quarter stream ID is dropped and counted, a
  malformed one is H3_DATAGRAM_ERROR. Datagrams that arrive before the
  session is accepted are held (at most 16) and then delivered.
* Capsules are bounded per session (`CapsuleReader::new(max_value)`); a
  capsule over the bound or a malformed one ends the stream with
  H3_MESSAGE_ERROR, an unknown capsule is skipped, DRAIN is reported as
  `SessionEvent::Draining`. A close reason over 1024 bytes or not UTF-8 is
  refused locally (`SendError::BadCloseReason`) and from the peer.

### Priorities (RFC 9218)

* `priority` request headers and PRIORITY_UPDATE frames on the client
  control stream set each stream's urgency and incremental flag; an
  update wins over the header and may arrive before the stream (bounded).
  A PRIORITY_UPDATE naming a non-request stream is H3_ID_ERROR, a push one
  is H3_ID_ERROR (push is never enabled), one on a request stream is
  H3_FRAME_UNEXPECTED, a malformed field value is ignored (defaults).
* `flush` offers queued bytes in urgency order. A non-incremental stream
  runs until it is empty or blocked before the next one; incremental
  streams of one urgency take turns in `incremental_quantum` (16 KiB)
  slices. With `work_conserving = false` (the default) a lower-urgency
  group waits while a higher group made progress this flush and still has
  queued bytes, because Warp's own stream fairness would otherwise undo the
  order; a higher group that is blocked by flow control does not hold the
  others back. `SETTINGS_NO_RFC7540_PRIORITIES = 1` is sent.
* Bytes already handed to the transport are ordered by the transport. A
  new optional `Transport::set_priority` hint (default: ignored) passes
  each stream's priority on; `WarpTransport` maps it to Warp's existing
  `stream_set_priority`. Without the hint, the Warp ordering test passed
  alone but failed when the workspace's unified dependency features
  changed packet timing; with it, the test passes both ways, with small
  stream windows and with windows larger than the responses.

### Streaming request bodies and the memory budget

* With `streaming_request_bodies`, a request with a body is handed out as
  `Event::StreamingRequest` after its headers; the application pulls body
  bytes with `ServerConnection::read_body` (and trailers with
  `take_trailers`). At most `stream_body_window` (64 KiB) is read from the
  transport per stream ahead of the application, so QUIC flow control
  pushes back on the client.
* `max_connection_buffer` (4 MiB) bounds request bytes held across all
  streams. Whole-body requests that cannot fit are not allowed to deadlock
  the connection: the newest one is reset with H3_EXCESSIVE_LOAD and
  counted in `stats().budget_resets`. When the server answers before the
  body ends it sends STOP_SENDING with H3_NO_ERROR (RFC 9114 §4.1).
* On the client, `send_request_streaming`, `send_body` and `finish_body`
  stream an upload; `BufferFull` is the back-pressure signal.

### Interim responses

* `Responder::send_interim(status, fields)` sends a 1xx HEADERS frame (for
  example 103 Early Hints) before the final response; 101 is refused
  (RFC 9114 §4.5) and a non-1xx status is refused. The client reports
  `ClientEvent::Interim` and rejects a 101 from the peer.

## Tests

New integration suites (all in `crates/vesper-prism-h3/tests`):

| Suite | Tests | What it covers |
| --- | --- | --- |
| `webtransport_mem.rs` | 12 | Settings, streams and QUIC datagrams, capsule fallback, tunnel sessions, held streams and their bound, gone sessions, the dropped-session leak, refusals and limits, application error codes, malformed datagrams and capsules, DRAIN and unknown capsules |
| `priority_bodies.rs` | 9 | Urgency order, incremental sharing, PRIORITY_UPDATE overriding headers, bad updates, streaming bodies under a window, early responses and body errors, budget without deadlock, interim responses, transport defaults |
| `warp_webtransport.rs` | 3 | Over a Warp loopback connection pair: a WebTransport session with streams, flow control and capsule datagrams; a streaming upload held back by the server window under 5% simulated loss; an urgent response finishing before a large non-urgent one |
| `quinn_datagrams.rs` | 2 | Over quinn-proto 0.11.13 connections (real QUIC handshake, rustls with aws-lc-rs): plain HTTP/3 requests, and WebTransport datagrams carried in QUIC DATAGRAM frames (RFC 9221), checked through quinn-proto's frame counters |
| `wtransport_interop.rs` | 2 | A WebTransport client whose every byte is produced and parsed by wtransport-proto 0.7.2: its settings, session request (its QPACK), stream headers and datagram codec against our server; our SETTINGS, 200 response, server-opened streams, datagrams and CLOSE_WEBTRANSPORT_SESSION capsule parsed by wtransport-proto; a session ended by FIN |

Unit tests were added in `capsule`, `priority`, `frame`, `message` and
`error`. All earlier `vesper-prism-h3` tests still pass, with two literal
updates (`..Settings::default()` and `protocol: None`) for the new fields.

### Interop peers and versions

Test-only `[dev-dependencies]`, pinned exactly:

* `wtransport-proto = "=0.7.2"`: the protocol crate of the `wtransport`
  WebTransport implementation. It adds `wtransport-proto 0.7.2`,
  `octets 0.3.7` and `httlib-huffman 0.3.4` to `Cargo.lock`.
* `quinn-proto = "=0.11.13"` (features `rustls-aws-lc-rs`) and
  `bytes = "=1.12.1"`: already in `Cargo.lock` through earlier tests.

The wtransport interop is codec-level: wtransport-proto's frames, headers
and capsules run against our server over the in-memory transport, not the
full `wtransport` crate over its own quinn/tokio endpoint. The QUIC side of
datagrams is checked separately against quinn-proto. A full-stack run
against `wtransport` (tokio) would need an out-of-workspace probe and was
not done.

## Checks

Run on the cloud host (Linux x86_64), `CARGO_TARGET_DIR` outside the
checkout and `VLB_CARGO_LOCK` pointing at the root `Cargo.lock`:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo test -p vesper-prism-h3` | pass (all suites, including the five new ones) |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | clean (the only output is the existing `unused dependency serde_json` manifest warning in `vesper-pact`) |
| `cargo test --workspace --locked --no-fail-fast` | 4046 passed, 4 failed, 2 ignored. The 4 failures are root-package Lean tests (`lean4lean42_acceptance`, `oracle42_acceptance`, `oracle42_runner`, `replay42_kernel`) that need a Lean toolchain on `PATH` and `VESPER_LEAN4LEAN42_ROOT`, which this host lacks and CI provides; none touch this crate |

Bazel picks up the new dev-dependencies from `Cargo.lock` through
`crate.from_cargo`; `BUILD.bazel` is unchanged.

## Decisions

* **Datagrams through the `Transport` trait, with defaults.** Warp has no
  QUIC DATAGRAM frames, and lane 7 owns Warp, so over Warp HTTP Datagrams
  use capsules. The QUIC datagram path is real and tested over quinn-proto.
* **All three WebTransport settings.** Peers in the wild still use the
  draft-07 and `ENABLE_WEBTRANSPORT` identifiers; sending all of them costs
  a few bytes and accepting any keeps interop.
* **0x41 is only a signal when WebTransport is offered.** Without it, 0x41
  on a request stream stays an unknown frame type, which keeps earlier
  behaviour and the RFC 9114 grease rules.
* **Non-work-conserving priority by default, plus the transport hint.**
  Ordering our own writes is not enough once bytes sit in the
  transport's buffers, so both are done; `work_conserving = true` turns
  off the waiting.
* **Deadlock relief resets the newest whole body**, so earlier requests,
  which have the most progress, complete.
* **`prism::serve` refuses extended CONNECT with 501.** The `Service` trait
  has no session shape; applications that want WebTransport drive
  `ServerConnection` directly.

## Remaining gaps

* The client's sessions are not linear obligations (the client is a test
  tool); only the server's `Session` is.
* WebTransport bidirectional streams count against
  `max_concurrent_requests` while their first bytes are read.
* Capsules queued on a CONNECT stream go through the same priority
  schedule as responses (CONNECT streams get the default urgency).
* No full-stack interop with a running `wtransport` or `h3` endpoint (see
  above); no browser interop.
* No allocation counting and no performance measurement was done; no
  "0 Alloc" or throughput claim is made.
* Server push remains unsupported and refused.
* Rust ownership of `Session` and `Responder` is host-library discipline,
  not evidence that the Vesper source typing rules are implemented.
