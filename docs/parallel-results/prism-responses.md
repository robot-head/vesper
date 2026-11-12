# Prism fixed-length response handoff

Implemented `crates/vesper-prism/src/response.rs`, exported as `response`, with
new response tests and README documentation. The branch is based on main
`7a88db5`, preserving the coordinator's Host fixes and workspace registration.
No parser implementation, root manifest/lock, runtime or server files changed.

## Validated boundary

`ResponseBuilder::new(status, context, connection, representation_length, limits)`
accepts numeric final statuses 200–599. Known codes use fixed standard reason
phrases; unlisted codes use an empty reason. Callers cannot supply arbitrary
reason text. Informational responses and CONNECT are unsupported; 426 is
rejected because its required Upgrade field cannot be represented in this
non-upgrading subset.

`header(self, name, bytes)` validates ASCII token names and rejects every value
control, including CR/LF, NUL, tab and DEL. Obs-text remains bytes. Header names
emit lowercase; unrelated repeated fields retain order and are never combined.
The encoder exclusively controls Content-Length and Connection. Caller copies,
including case variants, cannot override framing. Transfer-Encoding, Trailer,
TE, Upgrade, Keep-Alive and Proxy-Connection are explicitly rejected as well.

`finish(self, body)` checks the actual representation length against the plan
before producing any output. HEAD validates the actual representation bytes,
emits their length, then suppresses their wire payload. Statuses 204 and 304
require empty content and omit Content-Length conservatively. Status 205 also
requires no content and emits Content-Length: 0. Connection close/keep-alive is
explicit and emitted deterministically without clock-generated fields.

Independent fixed limits cover head size, total header count including generated
fields, representation body bytes and total wire bytes. All size arithmetic is
checked before header copies/output allocation. `usize::MAX` ordinary-body
planning rejects overflow without allocation. HEAD does not bypass the body
limit. The encoder returns no partial wire output on failure.

Builders and completed responses are not Clone. Header addition/finalization
consume the builder; extracting wire bytes consumes the completed response.
Rust Drop still allows unsent disposal and an extracted Vec can be deliberately
copied. This does not prove source-language linear response exhaustion.
ConnectionPolicy does not send or close a socket.

This is framing, not a full response semantic validator. Applications still
supply status-specific fields such as WWW-Authenticate. No chunked/streaming
response, upgrade lifecycle, I/O, routing/middleware, source typing, TLS,
cancellation, zero-allocation, performance or correctness theorem is claimed.

## Validation

```sh
cargo fmt -p vesper-prism -- --check
CARGO_TARGET_DIR=/workspace/vesper/target cargo test -p vesper-prism --locked --jobs 2
CARGO_TARGET_DIR=/workspace/vesper/target cargo clippy -p vesper-prism --all-targets --locked --jobs 2 -- -D warnings
```

11 new integration tests cover exact binary bytes, repeated Set-Cookie fields,
HEAD length/body suppression, 204/304/205 rules, status/reason restrictions,
case-insensitive framing field rejection, header injection/control rejection,
length mismatches, deterministic connection policy, exact independent limits,
header growth and arithmetic overflow. The existing 15 parser tests also pass.
There are two executable doctests and a compile-fail consuming-builder doctest.
Formatting and strict Clippy pass.
# Coordinator integration

Cherry-picked after the parser's Host hardening. All 26 parser/response tests
and three doctests passed in the shared workspace. Strict Clippy and Windows
GNU compilation of all targets passed. No source compiler or server integration
is implied by response encoding.
