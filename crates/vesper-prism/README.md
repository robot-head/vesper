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

There is no chunked decoder, trailers, middleware, routing,
HTTP/2, HTTP/3, WebSocket, TLS, I/O, cancellation, allocator integration, SIMD
acceleration, source ownership typing or proof/performance claim.


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
