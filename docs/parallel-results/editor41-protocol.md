# Editor §21 protocol/document core — issue #80

Implemented on `editor41/protocol` from contract
`593bbc311a1c0693aa08732b40cec813e2cda436`; exclusive source is the new
`crates/vesper-lsp/**`. Read issue #80, `docs/rfcs/editor41-contracts.md`, and all
of §21 of the committed specification. The metadata is still modified
2026-11-12T09:18:30.204Z, snapshot
`8e0f05ae32a6c87a8ce25fb339f66c760fe5d5f3fd3cb05e58236d291fa62207`.
The §21 latency/PCG/async-completion design is broader than this host core; this
change makes no claim to implement those portions. Root dispatch, query/features,
compiler and filesystem adapters remain coordinator/other-lane work.

## Delivered APIs

Crate-root protocol exports:

```rust,ignore
read_frame(&mut impl BufRead, ProtocolLimits) -> Result<Option<Vec<u8>>, ProtocolError>
parse_envelope(&[u8], ProtocolLimits) -> Result<Envelope, ProtocolError>
read_message(&mut impl BufRead, ProtocolLimits) -> Result<Option<Envelope>, ProtocolError>
encode_frame(&Envelope, ProtocolLimits) -> Result<Vec<u8>, ProtocolError>
write_message(&mut impl Write, &Envelope, ProtocolLimits) -> Result<(), ProtocolError>
```

`Id::{Integer(i32),String(String)}`; `Envelope::{Request{id,method,params},
Notification{method,params},Response{id,payload}}`; params are Option<Value>,
response ID is Option<Id> (None means null), and
`ResponsePayload::{Result(Value),Error{code:i32,message:String,data:Option<Value>}}`.
Envelope borrows during serialization; no deep Clone is needed.
`ProtocolError.kind:ProtocolErrorKind` is public with explicit header/body,
truncation, encoding, JSON/decoded-duplicate/resource, invalid-envelope/ID,
allocation/work/output and I/O variants. Display and std::error::Error work.

Framing uses bounded `fill_buf` slices and fallible reserves, never read_line or
read_until. CRLF lines, header total/line/count, single case-insensitive decimal
Content-Length and exact body size are enforced while reading. Duplicate lengths
fail even if equal. Unsupported charset/content encoding fails. Header scans
look at most remaining capacity plus one byte, including when BufRead exposes a
huge chunk. Truncated body/header returns no partial body. Clean EOF is only a
fresh boundary; framing failures require terminating the stream.

`parse_envelope` validates all bytes with `IndexedJson` before serde_json DOM
allocation/overwrite; decoded-equivalent duplicate keys anywhere fail, and
unknown malformed subtrees cannot hide syntax errors. Supported JSON-RPC2 is
explicitly strict: one object/version2.0, known envelope/error keys only, i32
integer-literal/string IDs, no null request IDs, null response IDs allowed,
object/array params, exactly one result/error for responses, integer code/string
message error objects. Batches/extensions/fraction/exponent IDs are unsupported.
Payload numbers must fit serde_json's default representable number profile.
A JSON/envelope error after a complete body leaves the next boundary known.

Output validates typed structure/nodes/depth/strings/work before serialization,
uses a bounded private writer, checks header size before header allocation, and
returns exact UTF-8 Content-Length. Validation/capacity failure leaves external
writer bytes untouched. Actual I/O write/flush errors may leave partial stream
bytes and require stream termination.

Document exports:

```rust,ignore
DocumentStore::new(StoreLimits) -> Result<DocumentStore, StoreError>
store.open(uri: &str, version: i32, text: &str) -> Result<&Document, StoreError>
store.change(uri: &str, version: i32, changes: &[ContentChange<'_>]) -> Result<&Document, StoreError>
store.close(uri: &str) -> Result<Document, StoreError>
store.get(uri: &str) -> Option<&Document>
store.iter() -> impl ExactSizeIterator<Item = &Document>
```

Also len/is_empty/total_bytes/revision, Document getters
uri/version/text/text_arc/index/revision, and root reexports Position/Range/
ByteSpan/LineIndex. `ContentChange {range:Option<Range>,range_length:Option<u32>,
text:&str}` borrows caller text. URI labels are bounded opaque strings with no
filesystem authority or normalization. Duplicate open and unknown changes/closes
fail without entries. Changes strictly increase i32 versions; accepted mutations
advance a checked u64 store revision. Full/incremental edits apply sequentially
in a private candidate. Full edits forbid range_length; incremental range_length
counts removed UTF-16 code units, including two for CRLF. Shared span validation
rejects surrogate interiors, bad positions and reversed ranges. CRLF interior is
strict, lone CR/LF terminate lines and trailing terminators create an EOF line.

Every Result failure leaves prior document text Arc identity, index, version,
all documents, total and revision intact. Every intermediate candidate must meet
limits, even if a later edit would shrink it. Retained Arc snapshots remain
immutable across changes/close. No consuming store/document Clone is exposed.

## Fixed limits and accounting

ProtocolLimits defaults/public fields:

- max_body_bytes/max_reply_bytes: 2 MiB each (reply is body only).
- max_header_bytes8192, max_header_line_bytes1024 including CRLF, max_headers32.
- max_json_depth64 container levels, max_json_nodes65536 including object keys.
- max_json_string_bytes/max_json_total_string_bytes: 2 MiB decoded UTF-8 each.
- max_json_index_bytes4 MiB, max_json_work32 MiB.
- max_id_bytes/max_method_bytes256 each.

Protocol HARD: body/reply/strings8 MiB, headers16384/line4096/count128,
nodes262144, input index16 MiB/work128 MiB, ID/method1024, depth64 unchanged.
`max_json_index_bytes` specifically bounds requested/charged input IndexedJson
capacity; output traversal references are bounded by max_json_nodes instead.
Output byte buffers are private and reply-bounded. Input DOM is only built after
byte/node/string/depth/index/work validation. Configurations above hard caps
fail; zeros are permitted and reject operations requiring those resources.

StoreLimits public fields/defaults: max_document_bytes128 KiB,
max_documents32, max_total_bytes2 MiB, max_uri_bytes4096, max_changes64,
max_work16 MiB, index_limits:vesper_span::Limits::default(). HARD caps respectively
1 MiB/256/16 MiB/16384/4096/128 MiB and span HARD. Configurations validate embedded
index limits and require document bytes <= index source bytes. Per-mutation work
charges URI scans/lookups, initial index scans, removed UTF-16 scanning and
candidate source copies/index scans before corresponding growth. get/iter are
reads outside this mutation ledger; count/URI structural hard caps bound lookup,
and iter is lazy. Live text totals exclude candidate buffers, metadata and
caller-retained snapshots. Document counts/per-index caps bound metadata;
Vec/String reserve is fallible, Arc uses standard Rust allocator semantics.
Logical bounds do not prove allocator rounding, process RSS or elapsed time.

## Evidence

Private target `/tmp/vesper-editor41-protocol-target`; each Cargo invocation used
CARGO_BUILD_JOBS=1, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0,
CARGO_INCREMENTAL=0. Span dependency patch d300142 was cherry-picked privately
(local cherry-pick a7bb48b). Temporary standalone leaf workspace wiring was used
only during tests, then restored; no root registration/lock or dependency edits
are included in the delivery.

Commands (all pass):

```text
cargo fmt --manifest-path crates/vesper-lsp/Cargo.toml -- --check
cargo test --manifest-path crates/vesper-lsp/Cargo.toml
cargo clippy --manifest-path crates/vesper-lsp/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path crates/vesper-lsp/Cargo.toml --example protocol_documents
```

16 runtime tests (1 unit +6 document +9 transport), 1 executing doctest, and the
self-checking example pass. Tests exercise every chunk size in a three-frame
stream, every truncation of an actual request frame, exact body boundary, header
storms/huge exposed chunks/total caps, duplicate/conflicting/overflowing lengths,
unsupported encoding, invalid UTF-8, decoded-key equivalence and nested duplicate
JSON, malformed unknown payloads, bad envelope/ID/params/error shapes, node/depth/
string/index/work budgets, exact 64-level container acceptance, exact node/depth
caps, UTF-8 reply-byte lengths, unchanged output on preflight failures, and I/O
failures. Store tests exercise sequential full/incremental UTF-16 edits,
surrogates/CRLF/combining marks, rangeLength, negative/MAX/stale versions,
duplicate/unknown operations, exact URI/document/count/total bounds, oversized
intermediate states, work/change/index failure, zero budget edges, old Arc
ownership, exact unchanged state after a failing later edit, and injected checked
revision overflow. Example output:
`bounded framing and transactional UTF-16 documents: ok`.

Independent Beacon review found no blocker; its input-index/read-ledger scope
clarifications are reflected in API docs and README. Linux tests were executed;
Windows/macOS execution and root stdio/compiler integration remain coordinator
checks. No SIMD, source macro, Region/linear-deserialization, hard timeout,
submillisecond latency, compiler capability or exact RSS claim is added.

## Coordinator registration

The delivered manifest has no standalone [workspace]. Add
`"crates/vesper-lsp"` to root workspace members/default-members and
`vesper-lsp = { path = "crates/vesper-lsp" }` to workspace.dependencies; add
`vesper-lsp = { workspace = true }` to the root package dependencies. The span
lane supplies corresponding span registration/removal of its temporary standalone
workspace. Existing direct new-leaf dependency declarations need no shared root
manifest edits. Regenerate only the coordinator-owned root lockfile.
