# Prism HTTP/3 and QPACK over Warp (wave six, lane 6)

Issue #248. Base: `origin/main` at `07322e3ba47890e939a48967a1ea8f62fdd7aac1`
(a descendant of the required `cda092f`). Branch:
`claude/project-thread-5ra5ft` (the session's assigned branch; the prompt
suggested `wave6/prism-http3`). Design section: §17 (Prism), on §35 (Warp).

Only `crates/vesper-prism-h3/**`, this report and the root registration
patch changed. `vesper-prism` and `vesper-warp` are used read-only, through
APIs already on `main`.

## What was built

A new crate, `vesper-prism-h3` (no `unsafe`, `#![forbid(unsafe_code)]`):

| Module | Contents |
| --- | --- |
| `qpack` | RFC 9204: prefixed integers and string literals, the HPACK Huffman code, the static table, the dynamic table with bounded capacity, a single-pass `Encoder` and a `Decoder` with their instruction streams, Required Insert Count encoding, blocked streams |
| `frame` | RFC 9114 §7 frames and §6.2 stream types: an incremental `FrameReader` (DATA streamed, unknown and grease types skipped without buffering, other payloads bounded before they are retained), SETTINGS, GOAWAY, MAX_PUSH_ID, CANCEL_PUSH |
| `message` | §4.1.2–§4.3 validation of request, response and trailer fields |
| `transport` | The `Transport` trait: the slice of a QUIC connection HTTP/3 needs |
| `connection` | `ServerConnection` over any `Transport`, and the shared core both roles use |
| `client` | `ClientConnection`, a small client with whole bodies, used by the tests |
| `prism` | `answer` (one dispatch path for HTTP/1.1 and HTTP/3), `to_prism`/`from_prism`, `serve` |
| `warp` | `WarpTransport`: `Transport` for one Warp endpoint connection |

### QPACK

* **Static table** (99 entries) generated from RFC 9204's Markdown source and
  the **Huffman code** from the `h2` 0.4.12 crate's table, both by
  `tests/fixtures/gen_tables.py`. The unit tests check that the code is
  canonical and complete (Kraft equality) and decode with per-length
  canonical tables built at compile time. EOS in a string, padding over 7
  bits and padding that is not all ones are errors (RFC 7541 §5.2).
* **Decoder**: encoder-stream instructions are accepted byte by byte (a
  partial instruction waits, bounded to `4 × capacity + 64` bytes); Set
  Dynamic Table Capacity above our SETTINGS, an entry larger than the
  capacity, bad static and evicted dynamic references are
  `QPACK_ENCODER_STREAM_ERROR`. Field sections check the Encoded Insert
  Count with the RFC's algorithm, the Base sign rule, references at or past
  the Required Insert Count, and (permitted by §2.2.1) a Required Insert
  Count larger than the references need. A section that needs inserts not
  yet received is copied and held, up to `SETTINGS_QPACK_BLOCKED_STREAMS`
  streams and a byte budget; exceeding either is
  `QPACK_DECOMPRESSION_FAILED`. Decoded sections with dynamic references
  are acknowledged; Insert Count Increments carry the rest; abandoned
  streams send Stream Cancellation. A section larger than
  `SETTINGS_MAX_FIELD_SECTION_SIZE` is a stream error (the stream is
  cancelled), not a connection error. Errors are sticky.
* **Encoder**: Base is the Insert Count at the start of a section; entries
  inserted for it are referenced post-base. Exact static matches are
  indexed; dynamic entries are referenced if acknowledged, or if this stream
  may block within the peer's blocked-stream budget (a stream that is
  already blocking may block again). Otherwise the encoder inserts the field
  for later sections without referencing it. An insertion never evicts an
  entry that is unacknowledged or referenced by an unacknowledged section
  (§2.1.1); entries over 3/4 of the capacity are sent as literals. Fields
  marked `sensitive` are never inserted and carry the N bit. Section
  Acknowledgment for nothing, a zero Insert Count Increment and one past the
  inserts are `QPACK_DECODER_STREAM_ERROR`. Huffman is used when shorter.

### HTTP/3

* Each side opens its control stream (SETTINGS first, with one grease
  setting) and its QPACK encoder and decoder streams once the transport can
  open streams. QPACK instructions are flushed before the request streams
  that depend on them.
* Peer unidirectional streams: one control, one encoder and one decoder
  stream are allowed (a second of any is `H3_STREAM_CREATION_ERROR`); a push
  stream is `H3_STREAM_CREATION_ERROR` (push is never enabled: no
  MAX_PUSH_ID is sent); unknown and grease types get STOP_SENDING
  (`H3_STREAM_CREATION_ERROR`) and are discarded; closing or resetting a
  critical stream is `H3_CLOSED_CRITICAL_STREAM`.
* Control stream: the first frame must be SETTINGS (`H3_MISSING_SETTINGS`);
  a second SETTINGS, DATA, HEADERS or PUSH_PROMISE is `H3_FRAME_UNEXPECTED`;
  HTTP/2 frame types and settings are errors; duplicate settings are
  `H3_SETTINGS_ERROR`; GOAWAY may not increase and must name a client
  bidirectional stream (from a server); CANCEL_PUSH past MAX_PUSH_ID and a
  decreasing MAX_PUSH_ID are `H3_ID_ERROR`. Unknown frames are ignored.
* Request streams: HEADERS, DATA\*, optional trailing HEADERS, with unknown
  frames anywhere. DATA before HEADERS, frames after trailers, and
  control-stream frames are `H3_FRAME_UNEXPECTED`; a frame truncated by FIN
  is `H3_FRAME_ERROR`. Malformed messages (pseudo-header order, duplicates,
  unknown pseudo-headers including `:protocol`, uppercase names,
  connection-specific fields, TE other than `trailers`, `:authority`/`host`
  disagreement, content-length mismatch) reset the stream with
  `H3_MESSAGE_ERROR`; bodies over the limit with `H3_EXCESSIVE_LOAD`
  (checked before buffering); FIN without HEADERS with
  `H3_REQUEST_INCOMPLETE`; requests over the concurrency limit or past our
  GOAWAY with `H3_REQUEST_REJECTED`. A QPACK-blocked request stream is not
  read further, so its bytes stay in QUIC flow control (RFC 9204 §2.1.2).
* `ServerConnection::shutdown` sends GOAWAY with the next unprocessed request
  stream ID; requests already received complete. The client reports GOAWAY
  and fails requests at or above its ID with `H3_REQUEST_REJECTED`.

### The §17.2 response obligation

Each complete request is handed out with a `Responder<HeadersPending>`: not
`Clone`, `#[must_use]`, with consuming exits only. `respond` sends a whole
response (all or nothing: on error the responder comes back with the
error); `send_headers` moves to `Responder<StreamingBody>`, which offers
`send_data` and the consuming `finish` (optionally with trailers); `reset`
from either state abandons the stream with a code. Every exit returns a
`ResponseSent` receipt. Content-length is computed and checked; 204/304
carry no body; HEAD responses send the representation's length and no
body; informational statuses and caller-supplied pseudo or content-length
fields are refused.

A dropped responder is detected, never silently released: its `Drop`
records the stream, and the connection's next poll resets it with
`H3_INTERNAL_ERROR` and counts it in `ConnectionStats::leaked_responders`
(tested). If the peer resets the stream or sends STOP_SENDING after the
request was handed out, the application gets `Event::ResponseAbandoned` and
the responder refuses further sends with `SendError::StreamGone`; it can
still be discharged with `reset`.

This is host-library discipline (Rust ownership plus a runtime check). It is
**not** the Vesper source typing rules of §17.2 (Theorem 2, `[T-Let]`):
nothing here is checked by the Vesper compiler.

### Prism over HTTP/3 on Warp

`prism::answer(service, handler, request, limits)` decodes the target once,
runs the middleware chain and the router (`Service::dispatch`) and calls the
handler; target errors are 400, unmatched paths 404, wrong methods 405 with
`Allow`, router failures 500. `prism::serve` polls a `ServerConnection`,
turns each HTTP/3 request into a `vesper_prism::Request` (`:path` is the
target, `:authority` becomes `host`), calls `answer`, and converts the
`vesper_prism::connection::Response` back (names lowercased, HTTP/1.1
connection-management fields and content-length dropped; `close` becomes a
GOAWAY). The HTTP/1.1 path (`vesper_prism::connection::Connection`) calls the
same `answer`, so one handler answers both protocols.

`warp::WarpTransport` maps the Warp stream API (`open_stream`,
`stream_write/finish/read/reset/stop_sending`) onto `Transport`. Streams the
peer opened are found by probing the next expected peer stream ID of each
kind (Warp opens peer streams implicitly and in order), not through Warp's
bounded stream-event queue, which drops events when full.

## Evidence

All on this host (rustc 1.97.0, debug build, Linux x86_64).

* `cargo test -p vesper-prism-h3 --locked`: 48 tests pass (14 unit,
  22 `h3_mem`, 2 `qpack_qifs`, 3 `qpack_rfc`, 5 `robustness`, 2
  `warp_prism`). Seven `h3_mem` tests come from the PR review: FIN behind a
  QPACK-blocked section (server and client), `Responder::reset` refusing
  a responder from another connection, critical streams waiting for
  stream credit, `respond` reserving HEADERS plus body before it encodes
  anything, 304 keeping the representation's `content-length`, and a
  lower second GOAWAY rejecting the requests it newly excludes. Each one
  fails with its fix reverted.
* **RFC 9204 Appendix B** (`tests/qpack_rfc.rs`): every exchange (B.1–B.5)
  replayed byte for byte against the decoder, including the encoder stream
  fed one byte at a time, the blocked section of B.2, the Section
  Acknowledgment `84`, the Insert Count Increment `01`, the Duplicate, the
  Stream Cancellation `48` and the eviction in B.5 (table sizes 106, 160,
  217, 215). Our encoder reproduces B.2's field section `0381 10 11` and
  instruction `3fbd01`, and its acknowledgment bookkeeping follows the
  example.
* **RFC 7541** Appendix C.1 integers and the C.4/C.6 Huffman strings.
* **QIFs interop corpus** (`tests/qpack_qifs.rs`; source and hashes in
  `tests/fixtures/README.md`): every file under `encoded/qpack-05/` for the
  `netbsd`, `fb-req` and `fb-resp` QIFs from six encoders (f5, ls-qpack,
  nghttp3, proxygen, qthingey, quinn) at every published table size,
  blocked-stream limit and acknowledgment mode decodes to the original
  field lists: 210 of 210 files with `VESPER_QIFS_DIR` set; the committed
  `netbsd` subset (70 files) runs in CI. The corpus is QPACK draft 05,
  whose table started at the maximum capacity; RFC 9204 starts at zero, so
  the harness prepends one Set Dynamic Table Capacity instruction. No other
  adaptation was needed.
* **Round trip under delayed acknowledgments** (`robustness.rs`): our
  encoder's output for the `netbsd` QIF decodes to the original with seven
  (capacity, blocked-streams) configurations from (0, 0) to (4096, 100),
  sections overtaking encoder-stream bytes, and decoder feedback delivered
  late in random batches; every section is acknowledged at the end.
* **Fuzz-style input** (`robustness.rs`, `h3_mem.rs`): 3,000 rounds of
  random and mutated (bit flip, truncation, insertion, byte replacement)
  encoder streams and field sections; 2,000 rounds of random decoder
  instructions; 5,000 random frame streams in random chunks (buffering
  stays bounded); 800 servers given random control, unknown and request
  streams. No panic; every failure is a typed error, and a failed
  connection was closed with that error's code. `h3_mem.rs` checks each
  named connection and stream error above individually.
* **End to end over Warp** (`tests/warp_prism.rs`): two Warp endpoints on the
  software NIC with a TLS 1.3 handshake (ALPN `h3`, the Warp test PKI), the
  HTTP/3 client and server on top, and a Prism `Service` with a middleware
  guard and four routes. Nine requests (GET, path parameters with
  percent-decoding, POST with a binary body, middleware 401 and pass,
  405, 404, a rejected `%2e%2e` target, HEAD) give the same status and
  body over HTTP/3 as through `vesper_prism::connection::Connection` over
  HTTP/1.1. A second test sends six 60 KB POST bodies over a link that drops
  5% of datagrams; all six echo back intact.
* **Interop with the `h3` crate over `quinn`** (`interop/`, run by hand; it
  is outside the workspace so quinn, h3 and tokio stay out of the lockfile
  and CI): the `h3` 0.0.8 client with `h3-quinn` 0.0.10 over `quinn` 0.11.9
  (quinn-proto 0.11.19, rustls 0.23.46 with aws-lc-rs, tokio 1.54.1, MTU
  discovery off) on loopback UDP, against `prism::serve` on a Warp server
  whose software NIC is bridged to a UDP socket. The QUIC and TLS handshake
  completed, SETTINGS and QPACK streams were exchanged, and three GET
  requests returned 200 with the right JSON and text bodies and
  content-length. Warp dropped two of quinn's packets as
  `Frame(Unsupported(0x18))` (NEW_CONNECTION_ID, which Warp does not
  implement); quinn retransmitted and the exchange completed.

Workspace checks (`cargo fmt --all -- --check`, `cargo test --workspace
--locked`, `cargo clippy --workspace --locked --all-targets -- -D warnings`):
see the PR description for the results on the pushed head.

No "0 Alloc" claim is made: the request path allocates (field vectors,
bodies, per-stream buffers) and no counting-allocator test was written. No
performance was measured.

## Root patch

One commit: `crates/vesper-prism-h3` in root `members` and `default-members`
(before `experiments/native-pulse`), its `Cargo.lock` entry (no new external
packages), and `crates/vesper-prism-h3/BUILD.bazel` from
`tools/bazel/sync_build_files.py`. Tests read fixtures only from inside the
crate directory, so the default Bazel test data suffices.

## Decisions

* **Sans-I/O over a `Transport` trait** rather than code written against
  `WarpEndpoint` directly. The same connection runs over Warp, over the
  in-memory pair the protocol tests use, and could run over another QUIC
  stack. Warp is reached only through `WarpTransport`.
* **Whole request bodies.** `vesper_prism::Request` carries a whole body, so
  the server buffers request bodies up to `max_body_bytes` and hands out
  complete requests. Responses can stream (`send_data`).
* **QPACK strictness.** A Required Insert Count larger than the references
  need is refused (RFC 9204 §2.2.1 permits it); none of the six corpus
  encoders produced one.
* **Speculative inserts.** When a section may not block, the encoder still
  inserts new fields (unreferenced) so later sections can use them once
  acknowledged. Without this a peer advertising zero blocked streams would
  never benefit from the table.
* **A dropped responder resets the stream** with `H3_INTERNAL_ERROR` on the
  next poll instead of panicking in debug builds, so tests can exercise the
  leak path and a production server never hangs a client.
* **Peer-stream discovery by probing** in `WarpTransport`, because Warp's
  stream events are lossy by design.
* **CONNECT** is refused with `H3_REQUEST_REJECTED`; **extended CONNECT**
  (`:protocol`) is malformed (`H3_MESSAGE_ERROR`) because RFC 9220's
  `SETTINGS_ENABLE_CONNECT_PROTOCOL` is never sent.

## Out of scope and remaining gaps

* **Extended CONNECT and WebTransport are out of scope** (as assigned), and
  so are server push (never enabled), HTTP Datagrams (RFC 9297), priorities
  (RFC 9218) and 0-RTT. Interim 1xx responses are skipped by the client and
  cannot be sent by the server.
* **No application CONNECTION_CLOSE in Warp.** `WarpTransport::close`
  records the HTTP/3 error code (`PeerStreams::closed_with`) and frees the
  connection with `WarpEndpoint::close_connection`; the peer only sees an
  idle timeout. Sending the code needs an additive Warp API (lane 5 owns
  `vesper-warp`).
* **Warp drops packets carrying NEW_CONNECTION_ID** (seen in the quinn
  interop run). quinn retransmits and the exchange completes, but every such
  packet is lost to Warp. A Warp change, outside this lane.
* **Reading STOP_SENDING.** The `Transport` trait has no STOP_SENDING event;
  the server learns of it when a write on that stream fails, which is when
  it matters (`Event::ResponseAbandoned`).
* **Streaming request bodies**, request body backpressure beyond QUIC flow
  control, and a per-connection memory budget across all streams.
* **The interop probe is not in CI**, by design (dependencies); it was run on
  this host with the versions above. The QIFs corpus is QPACK draft 05; no
  newer offline-interop corpus was available.
* The Rust typestate is not the Vesper source typing rule (see above).
