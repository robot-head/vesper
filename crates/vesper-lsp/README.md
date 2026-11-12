# vesper-lsp protocol/document core

Root-free synchronous host support for Content-Length LSP streams and immutable
source snapshots. This crate depends on `vesper-span`, `vesper-serde`, Serde and
serde_json. It performs no filesystem access, compiler execution or dispatch.

`read_frame(BufRead, ProtocolLimits)` returns exactly one body or clean EOF at a
fresh frame boundary. Header lines must end CRLF; byte/line/header-count limits
apply during `fill_buf` scanning and copying. A header buffer supplied by the
caller is inspected only through the remaining limit plus one byte. Exactly one
case-insensitive Content-Length is required; even equal duplicates fail. Lengths
are decimal, checked and bounded before body allocation. Optional Content-Type
supports application/vscode-jsonrpc or application/json and UTF-8/UTF8 charset;
unsupported content encoding fails (identity is allowed). Unknown bounded
headers are ignored. Short headers/bodies fail as truncation; no prefix body is
returned. Framing errors require terminating that stream; no resynchronization
is promised. The caller's BufRead and I/O implementation remain caller-owned.

`parse_envelope` checks UTF-8 and the entire strict JSON structure with
`IndexedJson`, including decoded-equivalent duplicate keys at every nesting
level, before serde_json builds a DOM. Its explicit LSP JSON-RPC2 profile is:

- A single object with exactly `jsonrpc:"2.0"` and recognized envelope keys.
  Batch arrays and extension keys fail. A request/notification has a nonempty
  bounded string method, optional object/array params, and no result/error.
- IDs are signed i32 JSON integer literals or bounded strings. Fraction/exponent
  IDs and null request IDs fail. Notifications omit ID. Responses must include
  ID (null allowed), exactly one result/error and no method/params.
- Error responses have exactly integer i32 code, string message and optional
  data. Standard arbitrary JSON result/params/data values must also be
  representable by serde_json's default number profile; nonfinite numbers fail.

`read_message` combines framing and envelope parsing. An envelope rejection has
consumed exactly that frame, so its next frame boundary is known; a framing
rejection has no such guarantee. Empty bodies fail JSON validation.

`encode_frame` validates the borrowed typed envelope, iteratively bounds payload
nodes/depth/decoded string bytes/work, serializes to an internal byte-limited
buffer, and returns a complete frame whose Content-Length counts UTF-8 bytes.
`write_message` completes that preflight before touching its external writer.
Validation/capacity failures leave external output unchanged. An I/O write or
flush failure may leave partial bytes, requiring termination of that stream.
The reply-byte cap applies to the body; the separately bounded header is added.

Protocol defaults are 2 MiB input/reply bodies, 8 KiB total headers, 1024 bytes per
header line (including CRLF), 32 headers, 64 JSON container levels, 65,536 JSON
nodes including keys, 2 MiB per/aggregate decoded strings, 4 MiB input index
capacity, 32 MiB work units and 256 bytes per method/string ID. `ProtocolLimits`
exposes these as `max_*` fields; `HARD` permits 8 MiB bodies/strings, 16 KiB total
headers, 4096-byte lines, 128 headers, 262,144 nodes, 16 MiB input index, 128 MiB
work and 1024-byte methods/IDs, with depth still 64. Configuration above HARD
fails. Zero caps are accepted and cause operations requiring that resource to
fail. `max_json_index_bytes` applies **only to input IndexedJson** capacity;
output traversal references are bounded by `max_json_nodes`, and output bytes by
`max_reply_bytes`. Logical allocation bounds are not exact allocator/RSS bounds.

## Transactional documents

`DocumentStore::new(StoreLimits)` supplies `open`, `change`, `close`, `get`,
`iter`, `len`, `is_empty`, `total_bytes` and monotone `revision`. URI strings are
opaque, nonempty, byte-bounded labels without control characters. Even file URIs
never trigger filesystem access or normalization. Equality is exact string
identity. Duplicate opens and changes/closes of unknown labels fail; no phantom
entry is created. Initial i32 versions may be negative; changes must strictly
increase. Accepted open/change/close increments the checked u64 store revision.
A returned Document retains its last open/change revision after close.

`ContentChange { range:Option<Range>, range_length:Option<u32>, text:&str }`
borrows its input. Range=None replaces the full source and requires
range_length=None. Incremental ranges use the shared strict UTF-16 LineIndex;
optional range_length must equal removed UTF-16 code units, including CRLF's two
units. Changes apply sequentially against each preceding candidate; each
intermediate candidate must meet size/total/index/work limits. A zero-change
batch may advance the version. Surrogate interiors, reversed/out-of-bounds
ranges and invalid line positions fail. CRLF maps at CR to content-end, rejects
the interior between CR/LF, then starts the next line after LF. Lone CR/LF end a
line; a trailing terminator creates the empty EOF line.

The complete edit batch is staged before one final commit. Every Result failure
preserves all documents, text Arc identity, version, index, total and revision.
The caller's borrowed edit/text data remains owned by the caller. Document text
and its index share an immutable Arc; `text_arc()` retains an old snapshot across
changes/close. No consuming Clone of a store or document is exposed.

Store defaults: 128 KiB/document, 32 documents, 2 MiB total live text, 4096-byte
URI, 64 changes/batch, 16 MiB mutation work units, plus `vesper_span::Limits`
defaults for each index. HARD: 1 MiB/document, 256 documents, 16 MiB live text,
16 KiB URI, 4096 changes, 128 MiB work, and span HARD limits. Configured document
bytes must fit the index source cap; every configuration validates span limits.
Mutation work conservatively charges URI scans/lookups, index scans, removed
UTF-16 scanning and candidate copies/indexing before their corresponding growth.
`get`/`iter` are bounded reads outside that ledger: count and URI hard caps bound
lookup; iteration is lazy. Live-text accounting excludes index/URI metadata,
candidate buffers and caller-retained Arc snapshots. Metadata is separately
bounded by document count and per-index caps. Vec/String capacities use fallible
reserve; bounded Arc allocation follows standard Rust allocator behavior.

Run `cargo test -p vesper-lsp`, `cargo clippy -p vesper-lsp --all-targets -- -D warnings`
and `cargo run -p vesper-lsp --example protocol_documents` after workspace
registration. The example self-checks real framing and a surrogate-pair edit.
This host core adds no language-level linearity/Region guarantee, asynchronous
cancellation, timeout, compiler feature or latency/RSS claim. The §21 daemon,
Salsa/PCG/completion/proof and server-routing work is outside this crate's scope.
