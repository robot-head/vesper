# Prism HTTP/1.1 framing milestones

This is a safe, dependency-free Rust host library for a bounded part of expanded
specification §17. It is not the Prism server or its source-language APIs.

```rust
use vesper_prism::{Decoder, Limits};
let mut decoder = Decoder::new(Limits::default())?;
decoder.push(b"POST /events HTTP/1.1\r\nHost: example.test\r\nContent-Length: 3\r\n\r\na")?;
assert!(decoder.next_request()?.is_none());
decoder.push(b"bc")?;
let decoded = decoder.next_request()?.unwrap();
assert_eq!(decoded.request.body, b"abc");
# Ok::<(), vesper_prism::Error>(())
```

`push` retains either the whole supplied slice or none of it. The caller drains
`next_request` before feeding more bytes when the queue approaches its configured
limit. `Ok(None)` means incomplete input, or an empty drained queue. After
`end_input`, complete queued requests can drain, while a partial head/body returns
`UnexpectedEof`. A successful `DecodedRequest` owns its head/body and reports the
exact number of wire bytes consumed. `buffered_slices` exposes the untouched
pipelined remainder as up to two ring-buffer slices.

Every parse or limit error poisons the decoder. The connection must close; do not
resynchronize by scanning queued bytes for a different request. Headers are
parsed only once, after a single incremental CRLF scan. Waiting for a body does
not reparse a head, and the ring queue avoids moving the pipelined suffix after
each request. Logical queued bytes, head bytes, header count and declared body
length all have explicit fixed limits. Construction reserves the queue storage;
owned heads/bodies incur additional bounded allocations. A caller retaining
arbitrarily many returned requests remains responsible for its own memory use.

Accepted requests use HTTP/1.1, an ASCII token method, and an origin-form target
beginning with one `/`; `OPTIONS *` is also supported. Methods and targets retain
exact case/percent escapes. Targets reject malformed escapes, fragments,
backslashes, raw non-ASCII bytes and network-path forms beginning with `//`.
Host is required exactly once and supports ASCII DNS labels, canonical dotted
IPv4 addresses and bracketed IPv6 addresses, with an optional decimal port at
most 65535. Alternate numeric-host forms such as `127.1`, integer IP addresses
leading-zero IPv4 components and hexadecimal/mixed-base aliases are rejected. This
is a deliberately restricted subset of HTTP host/target syntax, not a general
URI parser or hostname canonicalizer. Routing/authentication code must define
its own consistent URI decoding and authority policy; `%00` and `%2f`, for
example, remain encoded here.

Header names are ASCII lowercase in the result, and lookup is case insensitive.
Values preserve bytes, including obs-text, after trimming edge spaces/tabs.
Whitespace before a colon, obsolete folding, bare LF/CR and forbidden controls
are rejected. Unknown repeated headers remain separate; field-specific
application semantics are not inferred.

Framing permits no body or exactly one decimal `Content-Length`. Duplicate
Content-Length fields are rejected even if equal. Comma lists, signs, embedded
whitespace and overflow are rejected. Any Transfer-Encoding is unsupported;
Transfer-Encoding combined with Content-Length is an ambiguous-framing error.
CONNECT (including case variants), Upgrade/Connection: upgrade and Expect are
explicitly unsupported. Connection options cannot nominate Host, Content-Length
or Transfer-Encoding for removal. A consumer must still honor `Connection:
close`; this decoder does not manage socket lifetime.

There is no chunked decoder, trailers, HTTP/2, HTTP/3, WebSocket, TLS, I/O,
cancellation, allocator integration, SIMD acceleration, source ownership typing
or proof/performance claim. The connection, target and routing layers below are
built on this decoder without changing its behavior.


## Fixed-length responses

The `response` module encodes a bounded deterministic HTTP/1.1 final response:

```rust
use vesper_prism::response::{
    ConnectionPolicy, ResponseBuilder, ResponseContext, ResponseLimits,
};
let response = ResponseBuilder::new(
    200, ResponseContext::Ordinary, ConnectionPolicy::Close,
    3, ResponseLimits::default(),
)?
.header("Content-Type", b"application/octet-stream")?
.finish(&[0, 128, 255])?;
let wire = response.into_bytes();
# Ok::<(), vesper_prism::response::ResponseError>(())
```

The expected representation length must match the actual byte slice passed to
`finish`; mismatches return no output. `ResponseContext::Head` verifies those
same representation bytes and emits their Content-Length while suppressing the
wire payload. Statuses 204 and 304 require an empty representation and omit
Content-Length; 205 also forbids content and emits Content-Length: 0. Status is
numeric, final, and in 200–599. Known statuses use fixed standard reason phrases;
other accepted codes use an empty phrase. Informational responses, CONNECT and
426 Upgrade Required are unsupported.

Ordinary fields preserve insertion order and repetitions and emit lowercase
ASCII token names. Values remain bytes, including obs-text; all controls,
including tabs, CR and LF, are rejected. The encoder alone produces
Content-Length and Connection. Caller-supplied framing/connection fields,
transfer coding/trailer fields and Upgrade are rejected case insensitively.
No Date or other clock-dependent field is synthesized. The application remains
responsible for status-specific semantics outside framing, such as required
WWW-Authenticate fields on a 401 response.

`ResponseLimits` independently bounds head bytes, header count (including fields
generated by the encoder), representation bytes and total wire bytes. Arithmetic
and limits are checked before retaining header copies or allocating output.
HEAD representations remain subject to the body limit even though they do not
count toward wire payload bytes. This is an owned fixed-length encoder with
bounded allocations, not a streaming or chunked response API.

`header` and `finish` consume the builder, and `into_bytes` consumes the completed
response. Neither type is Clone. Rust still permits dropping them without
sending, and bytes can be deliberately copied after extraction. These ownership
choices prevent accidental builder reuse; they do not establish Vesper's linear
response-exhaustion theorem. ConnectionPolicy is explicit metadata plus a wire
header; the transport owner must enact it. No socket is sent to or closed here.


## Connection state machine

`connection::Connection` composes the decoder and response encoder without a
socket. The transport owner calls `receive` (which accepts only the prefix that
fits the bounded input queue and returns its length), `poll_request`, `submit`
and `pending_output`/`advance_output` (or `write_to` for one `io::Write` call).

```rust
use vesper_prism::connection::{Connection, ConnectionLimits, RequestEvent, Response};
let mut connection = Connection::new(ConnectionLimits::default())?;
connection.receive(b"HEAD /a HTTP/1.1\r\nHost: x\r\n\r\n")?;
let RequestEvent::Request(inbound) = connection.poll_request() else { unreachable!() };
connection.submit(inbound.id, Response::new(200, b"body".to_vec())).unwrap();
assert_eq!(connection.pending_output(),
    b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\n");
# Ok::<(), vesper_prism::connection::ConnectionError>(())
```

Each issued request gets an opaque `RequestId`. Responses may be submitted in
any order; they enter the wire queue strictly in request order. Foreign,
never-issued, duplicate, already-written and abandoned ids are rejected, as are
encode errors and submissions that would exceed `max_outbound_bytes`; every
rejection returns the `Response` and leaves the wire unchanged. HEAD context and
the Connection header come from the associated request, so applications never
choose HEAD framing themselves. Short writes advance only by the reported count;
overruns are rejected.

Close policy: `Connection: close` on a request makes it the last one issued and
forces `Connection: close` on its response. `Response::close` ends the
connection after that response; already-issued later requests are abandoned and
any already-submitted later responses are returned by `take_abandoned`. After
`end_input`, complete pipelined requests still drain and the final response is
marked close if not yet submitted. A parse error poisons the decoder and is
never resynchronized: earlier responses are written in order, then a minimal
400/413/417/431/501/505 `Connection: close` response (none for EOF inside a
frame). Bytes received after the connection stops issuing requests are counted
and discarded. `shutdown` consumes the connection and returns unsent wire bytes
with their written offsets, blocked out-of-order responses, awaiting ids,
abandoned owners, unprocessed input and the failure.

Bounds: decoder limits, `max_in_flight` issued-but-unpromoted requests (polling
returns `Backpressure`), and `max_outbound_bytes` of retained encoded output,
which must admit one maximal response; a generated parse-error response (under
128 bytes) may exceed it once. Rust `Drop` still permits discarding a
`Connection`, `Response` or `EncodedResponse`; this is not a linear responder.

## Targets and routing

`target::Target::parse` is the only percent decoder. It splits the path on
literal `/` before decoding and rejects encoded `/`, `\` and NUL, `.`/`..`
segments (also when encoded), empty interior segments and invalid UTF-8 (unless
`Utf8Policy::AllowBytes`). A trailing slash is recorded and significant. `+` is
literal in paths and a space in queries. Queries split on literal `&`/first `=`
before decoding; empty entries and empty keys are rejected, a key with no `=` has
no value, duplicates are kept in order, and `Query::unique` reports duplicates
as an error. Decoding happens once: `%252e` is the data `%2e`.

`router::Router` matches decoded segments against static and `:param` patterns.
A static segment beats a parameter at the first differing position regardless of
insertion order, with bounded backtracking (`max_match_work`). The highest
precedence matching path is selected before its methods are consulted (405 lists
methods sorted, with HEAD implied by GET; HEAD falls back to GET). Duplicate
routes, conflicting parameter names at one position, escapes in literal
patterns and malformed patterns are rejected without changing the router.
`service::Service` runs a bounded middleware chain and the router over the same
`ParsedRequest`, so guards and routes cannot disagree about what a path meant.
