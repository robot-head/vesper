# Prism HTTP/1.1 framing handoff

Implemented a bounded, dependency-free host protocol library in new
`crates/vesper-prism/**`, based on design §17's HTTP/1.1 parsing requirement.
No root manifests, compiler/runtime code or external-agent lane files changed.
This is a framing foundation, not the Prism framework or a network server.

## API and ownership

- `Decoder::new(Limits)` validates limits and reserves a bounded ring byte queue.
- `push(&[u8])` is all-or-nothing and bounds current + pipelined buffered bytes.
- `next_request()` returns incomplete (`Ok(None)`), a complete owned
  `DecodedRequest { request, consumed }`, or an explicit fatal error. It removes
  exactly one head + declared body, retaining the following bytes untouched.
- `buffered_slices()` exposes the unconsumed remainder without forcing ring
  compaction. `end_input()` permits draining complete requests and turns a
  truncated head/body into `UnexpectedEof`.
- `RequestHead` preserves method/target case and percent escapes. Header names
  are ASCII lowercase; case-insensitive lookup yields byte slices. Values retain
  non-UTF8 obs-text and interior tabs after edge OWS trim. Bodies remain bytes.
- Every parser/limit error poisons the decoder; later calls cannot silently
  resynchronize to a purported request in the suffix. The caller must close.

The incremental CRLF scanner visits each head byte once, then parses it once.
Waiting for a partial body does not reparse the head. VecDeque consumption avoids
quadratic repeated movement of a large pipelined suffix. Head bytes, header
count, declared body length and total buffered pipeline bytes are explicitly
limited. Construction requires `max_head + max_body <= max_buffered`, with
checked arithmetic. Owned result/head copies are bounded by these limits;
retaining arbitrarily many returned requests is the caller's responsibility.
There is no zero-allocation claim. Unsafe code is forbidden in the crate.

## Deliberate protocol subset

Only HTTP/1.1, origin-form targets and `OPTIONS *` are accepted. A target must
start with one slash; absolute-form, network-path `//`, raw non-ASCII, fragments,
backslashes and invalid percent escapes are rejected. No URI decoding or
canonicalization is performed. CONNECT, including case variants, is explicitly
unsupported. Extension method tokens otherwise retain case.

Exactly one Host is required. Supported authorities are ASCII DNS labels,
canonical dotted IPv4 and bracketed IPv6, optionally followed by a decimal port
within u16. Alternate numeric forms (integer IP, `127.1`, leading-zero IPv4),
userinfo, whitespace/comma lists, percent-encoded hostnames and invalid ports
are rejected. This is deliberately a restricted host syntax, not DNS resolution
or an application authority/authentication policy.

Body framing is absent or exactly one decimal Content-Length. Duplicate lengths
are rejected even if equal; comma lists, signs, interior whitespace and overflow
fail. Transfer-Encoding + Content-Length is ambiguous and always rejected. All
transfer codings, upgrades and expectations are unsupported. Connection options
cannot nominate Host/Content-Length/Transfer-Encoding for removal. Consumers
must still honor Connection: close and apply consistent decoded URI/authority
semantics. Repeated unrelated fields remain separate for application processing.

Whitespace before colon, obsolete folding, bare CR/LF and forbidden controls
fail closed. Valid opaque header obs-text and arbitrary body bytes are accepted.

## Validation

```sh
cargo fmt --manifest-path crates/vesper-prism/Cargo.toml -- --check
CARGO_TARGET_DIR=/workspace/vesper/target cargo test --manifest-path crates/vesper-prism/Cargo.toml --jobs 2
CARGO_TARGET_DIR=/workspace/vesper/target cargo clippy --manifest-path crates/vesper-prism/Cargo.toml --all-targets --jobs 2 -- -D warnings
```

14 integration tests and one executable doctest passed. Tests cover every split
and one-byte feeds of a valid binary-body request, malformed line endings across
every split, exact pipeline remainder, body bytes resembling requests, 1000
iterations of ring wrapping with a queued pipeline, duplicate/obfuscated length
and Host attacks, TE/CL combinations, unsafe connection nominations, header
folding/controls, target/version syntax, exact/over-limit boundaries, EOF and
fatal-state retention. Strict Clippy and formatting passed.

## Coordinator integration

Remove the temporary child `[workspace]` and nested Cargo.lock, register the
crate in root members/default-members and regenerate the shared lock. There
are no dependencies or root CLI changes. See the crate README for usage and
caller responsibilities.

Remaining Prism work includes chunked/trailer handling, responses and linear
response/upgrade lifecycles, routing/middleware, source/runtime integration,
HTTP/2/3, WebSocket, TLS, cancellation, region allocation, SIMD parsing and Lean
security proofs. None is implied by this milestone.

Coordinator registered the crate in both root workspace lists, removed its
standalone workspace/lockfile and reran all 14 integration tests plus the
executable doctest in the shared workspace. Independent review found that
hexadecimal/mixed-base IPv4 aliases could pass DNS validation. The coordinator
now classifies those aliases before strict IPv4 validation, preserves legitimate
names such as `0xcorp.example`, and added a fifteenth test exercising every
transport split and fatal error state. No additional definite framing/bounds
bug was found in that review; no server/runtime consumer is connected yet.
