# Prism connection codec and routing handoff

Lane 7 of the third wave. Base: `origin/main` at
`1ffd0aec27dfcc24c5564585ae9068255490bf61` (a descendant of the required
`02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6`). Branch:
`claude/project-thread-bozeud` (the session's assigned branch; the prompt's
suggested name was `wave3/prism-connection-routing`).

Only `crates/vesper-prism/**` and this report changed. The existing decoder
(`lib.rs` parsing) and `response.rs` are unchanged apart from module
declarations and crate docs; all 15 framing and 11 response tests pass
unmodified.

## Coordinator patch

None required. `vesper-prism` is already registered in the root workspace
members/default-members, and the new modules add no dependencies, so the root
`Cargo.toml`/`Cargo.lock` are untouched and `cargo test --locked` passes. If the
global docs index Prism capabilities, the coordinator may add one line to
`docs/parallel-work.md` pointing at this report.

## Public API

- `connection`: `Connection`, `ConnectionLimits`, `RequestId` (opaque `Copy`
  association with `sequence()`), `Response` (application-owned, unencoded),
  `Inbound`, `RequestEvent {Request, NeedInput, Backpressure, Failed, NoMoreRequests}`,
  `SubmitError {kind, response}`, `SubmitErrorKind {Foreign, Unknown, Duplicate,
  Stale, Abandoned, OutboundLimit, Encode}`, `ConnectionState {Open, Closing,
  Closed}`, `ConnectionError`, `Abandoned`, `UnsentOutput`, `Shutdown`.
  Methods: `receive` (accepts the fitting prefix, returns its length), `end_input`,
  `poll_request`, `submit`, `take_abandoned`, `pending_output`,
  `pending_output_len`, `advance_output`, `write_to`, `state`, `in_flight`,
  `retained_outbound_bytes`, `buffered_input`, `failure`, `shutdown`.
- `target`: `Target {Asterisk, Origin}`, `OriginTarget` (`segments`,
  `trailing_slash`, `query`), `Segment` (`as_bytes`, `as_str`), `Query`
  (`entries`, `all`, `unique`), `QueryEntry`, `TargetLimits`, `Utf8Policy`,
  `TargetError`, `DuplicateQueryKey`.
- `router`: `Router<T>` (`new`, `insert`, `resolve`, `value`, `len`),
  `RouterLimits`, `RouteId`, `RouteMatch` (`id`, `value`, `head_fallback`,
  `param`, `params`), `Resolution {Matched, MethodNotAllowed, NotFound}`,
  `RouteError`.
- `service`: `ParsedRequest`, `Middleware`, `Flow`, `Service<T>`, `Dispatch`,
  `ServiceError`, `method_not_allowed`.

## Decisions

Connection:
- Responses are encoded at submission (so encode errors return the owner
  immediately) and wait in their request's slot until every earlier response is
  submitted. Only then do they enter the wire queue. A response is moved, never
  re-serialized, so a short write or error cannot reorder, lose or duplicate a
  suffix. `advance_output` rejects overruns without changing state.
- HEAD context and the `Connection` header are derived from the associated
  request; the application cannot pick HEAD framing. Request `Connection: close`
  (any option position, case-insensitive) forces `close` on that response and
  stops issuing. `Response::close` truncates later slots; their ids and any
  already-encoded later responses are returned through `take_abandoned`.
- EOF: complete queued requests still drain; the last outstanding response is
  marked `close` if submitted after EOF was observed. A partial frame at EOF
  produces `Failed(UnexpectedEof)` and closes without an error response.
- Parse errors poison the decoder and are never resynchronized. A failure slot is
  queued after all earlier requests, carrying a minimal `Connection: close`
  response (400 generally; 413 body limit; 431 head/header-count limit; 417
  Expect; 501 transfer-coding/upgrade/CONNECT; 505 version). Later pipelined
  bytes are kept opaque in `Shutdown::unprocessed_input`.
- After the connection stops issuing requests, received bytes are counted in
  `discarded_input` and dropped rather than buffered. Tests check that retained
  plus discarded is exactly the post-close suffix at every split.
- `RequestId` is `Copy`, so runtime rejection of duplicate/stale/foreign ids is
  the actual guard (and is tested), rather than relying on move semantics.
  Connection identity is a process-global counter; the never-issued (`Unknown`)
  case is unit-tested inside the crate because ids cannot be forged externally.

Targets (the single decoder; documented in `target.rs` and the README):
- Split on literal `/` before decoding; reject decoded `/`, `\`, NUL, `.`/`..`
  (including `%2e` forms), empty interior segments. Trailing slash is recorded
  and significant; `/` has no segments.
- `+` literal in paths, space in queries; `%2B` is `+`. Queries split on literal
  `&` and the first `=` before decoding; empty entries/keys rejected; `?k` (no
  value) differs from `?k=`; duplicates are kept in order and `Query::unique`
  errors on them. NUL rejected in keys and values.
- UTF-8 required by default (rejects overlong `%C0%AF`); `Utf8Policy::AllowBytes`
  keeps raw bytes. Hex case-insensitive; escapes of unreserved bytes equal the
  literal byte. Decoded data is never decoded again (`%252e` stays `%2e`), and
  route literals may not contain `%`, so no second decode can sneak in.

Routing:
- Trie with sorted static children and at most one parameter child per node.
  Depth-first, static before parameter, so precedence is insertion-order
  independent and lexicographic by position. Backtracking is bounded by
  `max_match_work`; exceeding it is `RouteError::WorkLimit`, never NotFound.
  Recursion depth is bounded by `max_route_segments`.
- The highest-precedence matching *path* is selected before methods are checked;
  a lower-precedence parameter route is not used to find a different method.
  HEAD falls back to GET at the selected path; 405 allow lists are sorted with
  HEAD implied by GET.
- Exact duplicates, differing parameter names at one shared position,
  duplicate names in a pattern, escapes/dots/empty/control segments in patterns
  and non-token methods are rejected. `insert` validates fully in a dry run
  before mutating, so failures leave no partial nodes.
- `Service` runs a bounded middleware chain and the router over the same
  `ParsedRequest`; a test shows `/%61dmin` cannot bypass an `admin` guard.

## Bounds

Decoder limits (unchanged), `max_in_flight` issued-but-unwritten requests,
`max_outbound_bytes` retained encoded bytes (must be at least
`response.max_total_bytes`; a generated error response under 128 bytes may exceed
it once), `ResponseLimits`, `TargetLimits` (target bytes, segments, query
entries; counted before allocation), `RouterLimits` (routes, pattern segments,
match work) and the service middleware count. Each poll is one bounded decoder
step. Values handed to the caller (requests, abandoned/blocked responses,
shutdown output) are the caller's memory responsibility.

## Checks

All with `CARGO_TARGET_DIR=/home/claude/target-prism CARGO_BUILD_JOBS=2
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0` on
Linux x86_64, rustc/cargo 1.97.0:

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -p vesper-prism --all-targets --locked -- -D warnings`: clean.
- `cargo test -p vesper-prism --locked`: 1 unit, 14 connection, 15 framing
  (existing), 11 response (existing), 13 routing, 3 doctests + 1 compile-fail
  doctest; all pass.

Acceptance coverage: every two-chunk split of a five-request pipeline (GET,
HEAD, POST with a CRLFCRLF body probe, `Connection: close`, and a never-issued
suffix) combined with every write size 1..=48 plus whole-buffer, with reverse
order completion and writes interleaved, compared to exact binary bytes;
single-byte feeding; write failure at every byte offset reconstructing the
exact stream from written bytes plus `Shutdown::unsent`; stale, duplicate,
foreign, unknown and abandoned submissions; application close mid-pipeline;
malformed later requests (duplicate CL, CL+TE, TE, HTTP/1.0, body limit) with a
following smuggled request never issued; clean and partial EOF; encode-error and
outbound-limit rejections returning the owner; HEAD/304/404 body rules; input and
in-flight backpressure; and the route/target cases listed above.

## Remaining gaps and non-claims

- Execution is a real in-process state machine over byte buffers and
  `io::Write`; there are no sockets, timers, idle/slowloris timeouts, lingering
  close or half-close socket handling, and no async scheduler integration.
- Fixed-length only: no chunked bodies/trailers, `100-continue`, HTTP/2/3,
  WebSocket/upgrade, TLS or streaming responses.
- Rust consuming APIs and the non-Clone `EncodedResponse` still permit `Drop`;
  this is not Vesper's source-level linear responder or its exhaustion theorem.
- The target policy rejects rather than normalizes (dots, encoded separators,
  empty segments); routes needing those forms are unsupported. No wildcard or
  catch-all segments, regex or host-based routing. Authority/Host policy remains
  the decoder's restricted subset.
- A response marked keep-alive before EOF or a later parse failure was observed
  may still be followed by a close; the wire header then says keep-alive while
  the transport closes, which HTTP/1.1 permits.
- No fuzzing, performance measurement or cross-target runtime validation was
  performed for this lane.
