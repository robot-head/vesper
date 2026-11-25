# Port wave P1, lane 14: vesper-prism in Vesper

Claim: #911. Branch: `claude/project-thread-h283z4`. Started from baseline
`b69383e` (export ABI v2, #921) and merged `82482a6` (cycle-free port builds,
#919) before the first push.

The lane prompt calls the crate "HTTP/2 framing". vesper-prism is strict
HTTP/1.1 request framing, a pipelined connection state machine, responses,
targets and routing; HTTP/2 is not in the crate. The port covers what the
crate has.

## API inventory (re-measured before porting)

| Module | Public types | Public functions and methods |
|---|---|---|
| `lib.rs` | `Limits`, `Error` (25 variants), `Header`, `RequestHead`, `Request`, `DecodedRequest`, `Decoder` | `RequestHead::headers`; `Decoder::{new, limits, buffered_bytes, buffered_slices, push, end_input, next_request}` |
| `connection.rs` | `RequestId`, `ConnectionLimits`, `Response`, `Inbound`, `RequestEvent`, `SubmitErrorKind`, `SubmitError`, `ConnectionError`, `ConnectionState`, `Abandoned`, `UnsentOutput`, `Shutdown`, `Connection` | `RequestId::sequence`; `Response::{new, header, with_close}`; `Connection::{new, limits, receive, end_input, poll_request, submit, take_abandoned, pending_output, pending_output_len, advance_output, write_to, state, in_flight, retained_outbound_bytes, buffered_input, failure, shutdown}` |
| `response.rs` | `ResponseLimits`, `ResponseContext`, `ConnectionPolicy`, `ResponseError`, `EncodedResponse`, `ResponseBuilder` | `EncodedResponse::{connection_policy, len, is_empty, into_bytes}`; `ResponseBuilder::{new, header, finish}` |
| `target.rs` | `Utf8Policy`, `TargetLimits`, `TargetError`, `Segment`, `QueryEntry`, `DuplicateQueryKey`, `Query`, `OriginTarget`, `Target` | `Segment::{as_bytes, as_str}`; `Query::{entries, all, unique}`; `OriginTarget::{segments, trailing_slash, query}`; `Target::{parse, origin}` |
| `router.rs` | `RouterLimits`, `RouteError`, `RouteId`, `RouteMatch<'a, T>`, `Resolution<'a, T>`, `Router<T>` | `RouteId::index`; `RouteMatch::{param, params}`; `Router::{new, len, is_empty, value, insert, resolve}` |
| `service.rs` | `ParsedRequest`, `Flow`, `Middleware` (trait), `Dispatch<'a, T>`, `ServiceError`, `Service<T>` | `ParsedRequest::{new, request, target, into_parts}`; `Service::{new, router, push, dispatch}`; `method_not_allowed` |

`#[cfg(test)]` unit tests: one, in `connection.rs`
(`never_issued_sequence_is_unknown_and_returns_the_response`). Integration
tests: 54 in `tests/` (connection 15, framing 15, responses 11, routing 13).
Together these are the lane's 55 tests.

Parts that must stay Rust, as measured: the `Middleware` trait-object chain
(F7b), the generic route payload `T`, `io::Write`, and the public types
themselves, which both builds share.

## What was ported

Five Loom packages under `crates/vesper-prism/vesper/`, each built into its own
`vesper build --lib` object with its own binding in `port/`:

| Package | Modules | What it does |
|---|---|---|
| `@vesper/prism-decoder` | `lib.ves`, `syntax.ves` | Limit checks, the incremental CRLF head scan (every byte scanned once), all of `parse_head` (request line, origin-form target grammar, field syntax, Host with Rust std's exact IPv4/IPv6 grammars and the numeric-alias rule, Content-Length, the unsupported framing fields, Connection options), poisoning, the status of the error response for each `Error` |
| `@vesper/prism-response` | `lib.ves` | Field validation, framing (Content-Length/Connection are produced, never accepted), the reason phrases, 1xx/204/304 and HEAD body rules, limits, the encoded bytes |
| `@vesper/prism-target` | `lib.ves` | The single URI decoding policy: syntax classes, percent decoding, `+` handling, UTF-8 policy (std's rules), dot segments, segment/query limits |
| `@vesper/prism-router` | `lib.ves`, `pattern.ves`, `matching.ves` | The trie in flat arenas, pattern parsing and its errors, ambiguity detection, static-beats-parameter precedence, the bounded backtracking match, 405 allow lists with implied HEAD, 404 |
| `@vesper/prism-connection` | `lib.ves` | Request issuing and backpressure, response slots by sequence, the outbound budget and its reserve for the earliest response, close policy (request close, response close, EOF, parse failure), abandonment, the write cursor, state and shutdown ownership lists |

The Rust side of each module is a forwarding adapter in `src/forward/`:

- `decoder.rs` keeps the queued bytes in a `VecDeque` and lends both halves to
  Vesper. It copies out the head fields and the body once Vesper says where
  they end.
- `response.rs` passes the name and value bytes of each field and returns
  Vesper's encoded bytes.
- `target.rs` builds the public `Segment`/`Query` values from the byte ranges
  Vesper returns.
- `router.rs` keeps each route's payload `T`, method and parameter names,
  indexed by the route ids Vesper issues. The match, its captures and the allow
  list come from Vesper.
- `connection.rs` holds bytes only: decoder input, the encoded responses by
  sequence, and the wire queue. Vesper decides validity, budgets, ordering,
  close policy and the write cursor. It returns action plans (`Promote`,
  `Abandon`, `Discard`), and the adapter applies them. This is the largest
  adapter (433 lines), because Rust still owns every `EncodedResponse` and
  `Vec<u8>`.

The public types stay in the crate root, so dependents do not change.
vesper-prism-h3 builds and passes unchanged on the port.

## What stays in Rust, and why

| File | What | Reason, and what unblocks it |
|---|---|---|
| `src/service.rs` | The `Vec<Box<dyn Middleware>>` chain, `Service::dispatch` around it, and `method_not_allowed` | Named in the lane prompt: trait objects need F7b. `dispatch` calls the ported target decoding and router before and after the chain; the file holds no other logic. |
| `src/forward/router.rs` | The route payload `T` and the `&'a T` in `RouteMatch` | Rust generics do not cross the export ABI. Vesper uses route ids as tokens for them, which is the token-slab pattern from lane 1. |
| `src/forward/*.rs` | Owning the byte buffers (`VecDeque<u8>`, `EncodedResponse`, the wire queue) and `write_to(&mut impl io::Write)` | Vesper borrows input as `in Slice[u8]` and returns `ChunkSeq[u8]`. A Vesper-owned ring buffer behind the ABI would copy every received byte once more, and `io::Write` is a Rust trait. |
| `src/connection.rs` | `next_connection_id` (a process-wide `AtomicU64`) | A process-global counter has no Vesper equivalent in this wave. Vesper stores the id and rejects foreign ids itself. |
| `src/forward/*.rs` | `String` conversions for `RequestHead::method/target`, header names and route parameter names | The public types are Rust `String`s. Vesper checks the bytes; the adapter converts already validated ASCII or UTF-8. |
| (dropped) | `Error::AllocationFailed` from `try_reserve` | Vesper aborts on out-of-memory. The variant stays in the public enum, and its status (500) is still mapped. |

There is no unsafe code in the port. `src/lib.rs` changes `forbid(unsafe_code)`
to `deny(unsafe_code)`, so that the five generated bindings in `port/` can opt
in to the `extern "C"` calls. Their `unsafe` blocks are generated code (counts:
connection 31, decoder 23, response 16, router 38, target 16). The hand-written
Rust has none.

## Unit-test treatment

The one `#[cfg(test)]` test was moved, not deleted. It needs a `RequestId` that
was never issued, which the public API cannot build, so it stays a unit test in
`src/connection.rs`. Both implementations provide a `#[cfg(test)]`
`request_id(sequence)` helper, so the test runs against both builds.

## Tests and divergence checks

Both builds pass all 55 tests, along with the doctests. Two new test files run
against both builds:

- `tests/malformed.rs`: 3000 deterministic mutations (xorshift: byte flips,
  insertions, deletions, truncations) of four seed requests, fed whole and one
  byte at a time through `Decoder`, `Target::parse` and a pipelined
  `Connection`. Every outcome is a typed value, never a panic. The outcome log
  is FNV-hashed, and the digest is a constant that both builds must reproduce
  (`16759313125583877486`), so the two builds agree on every mutated input, not
  only on the hand-written cases.
- `tests/divergence.rs`: under Bazel, the deliberately divergent test binaries
  must fail.

There is one divergent build per library (`Port::edit` in `build.rs` under the
`vesper-port-divergent` feature; `edits` with `edit_src` in `BUILD.bazel`):

| Library | Edit | Test that must fail |
|---|---|---|
| decoder | Content-Length may exceed the body limit by one | `framing::exact_limits_accept_and_overruns_poison_without_retaining_excess` |
| response | 205 may carry a body | `responses::body_forbidden_statuses_omit_length_and_never_accept_a_payload` |
| target | `,` instead of `+` decodes as a space | `routing::query_entries_plus_duplicates_and_empty_forms_are_defined` |
| router | a parameter beats a static segment | `routing::static_segments_beat_parameters_independent_of_insertion_order` |
| connection | an out-of-order completion reserves no room | `connection::out_of_order_completions_cannot_starve_the_earliest_response` |

`aspect crate prism test-port` (run from `scripts/test-port-harness.sh` on
macOS and Windows) checks that each divergent build fails exactly that test
while another test in the same file still passes. Bazel runs the same check
on Linux through `:port_divergence_detected`.

## Formatter limits shaped the layout

`vesper fmt` charges 4·N² work units for a file of N lexer tokens, against a
budget of 128 Mi. In practice that caps a file at about 5,700 tokens,
whitespace included. The decoder (8,000 tokens) and the router (7,000) did not
fit in one file, so each became a package with several modules, using the
package support from #919. Every module is at most 4,700 tokens, and all eight
files are canonical under `vesper fmt --check`. #939 has since made the formatter
near-linear and lifted the file cap. The split stays, because the modules are
easier to read that way, and all eight files are still canonical under the new
formatter.

## Benchmarks

vesper-prism has no benchmark, and it is not on the plan's hot-path list, so no
ABBA benchmark is required. As a rough measurement only, here is the
release-mode wall time of `tests/malformed.rs` (3000 mutated inputs, each
decoded whole and byte by byte, plus target parsing and a connection run),
five runs of each release test binary run directly (`--test-threads=1`), on a
4-core Linux x86_64 cloud container:

| Build | Runs (ms) | Median |
|---|---|---|
| Rust | 17.7, 14.6, 15.5, 15.6, 13.4 | 15.5 ms |
| Vesper port | 58.5, 71.4, 70.4, 69.2, 57.5 | 69.2 ms |

The port is about 4.5x slower on this test. That is above the 2x line the plan
uses for hot-path crates, although the rule does not apply here, because the
crate had no benchmark and is not on that list. The likely costs are the
export-boundary copies (every head is copied into a `ChunkSeq` before it is
parsed, and every field crosses back as a sequence), bounds-checked
`try_get` byte reads, and the adapter's action-plan round trips. These have not
been profiled. This is a whole-test timing, not an ABBA benchmark.

## Platforms run

- Local (Linux x86_64 cloud container, Rust 1.97 for tests and 1.100 for
  clippy):
  - `cargo test -p vesper-prism` with and without `--features vesper-port`,
    and `cargo test -p vesper-prism-h3 --features vesper-prism/vesper-port`.
  - The divergence script.
  - `cargo +1.100.0 clippy --all-targets -p vesper-prism -- -D warnings` on
    both builds.
  - `vesper fmt --check` on every module.
- CI: Bazel on Linux, plus macOS (AArch64) and Windows (x86_64 MSVC) through
  `scripts/test-port-harness.sh`. The results are in the PR checks.

## Rust code deleted?

Not yet. The plan deletes the Rust code for this crate once both builds pass
on all four platforms. Because of the measured slowdown above, deletion waits
for the owner's decision.
