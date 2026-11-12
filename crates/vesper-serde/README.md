# vesper-serde

Bounded scalar Rust host JSON and binary codec slices from design §20.
It has no dependencies and forbids unsafe code. It does not provide source
macros, source Region typing, linear-resource deserialization, SIMD kernels,
float conversion, arbitrary object-graph archives, NDJSON/AoSoA ingestion, or
throughput guarantees. The versioned tree archive below is an explicit host
format, not a generic source-derive archive ABI.

```rust
use vesper_serde::{IndexedJson, Limits};

let json = IndexedJson::parse(br#"{"id":42,"topic":"ready"}"#, Limits::default())?;
let id = json.root().field("id", 1024)?.unwrap().as_u64()?;
assert_eq!(id, 42);
# Ok::<(), vesper_serde::Error>(())
```

`IndexedJson::parse` validates all syntax, Unicode, UTF-8, numbers and nested
unknown fields before publishing any `Node`. Numbers follow JSON grammar and
can be arbitrarily large within the input/work bounds; `as_u64`/`as_i64`
separately check representability. No floating-point parsing is advertised.
Duplicate object keys are rejected by decoded Unicode scalar equality in all
objects. Thus `"id"` and `"\u0069d"` conflict. Unicode normalization and case
folding are not applied. Members in separate objects may reuse keys.

The token tape stores subtree ends, allowing a `Cursor` to skip a previously
validated subtree in constant time. Array cursors stop at their own array end.
Object fields support schema-directed lookup and decoded key comparisons.
Unescaped strings, including non-ASCII UTF-8, borrow immutable input. Escaped
strings decode into explicit caller scratch after a capacity check. Failed
scratch reads preserve every scratch byte and the cursor position. Rust
lifetimes enforce these host borrows; they do not implement Vesper Region modes.

Parsing requests one token-vector allocation, bounded by input length, token
count and `max_index_bytes`. Limits count logical vector payload capacity, not
allocator bookkeeping or process RSS. Strings are never heap-allocated by the
codec. Container recursion has a hard ceiling of 128 in addition to the caller
limit. All input bytes count toward work before UTF-8 validation; consumed bytes
and decoded duplicate-key comparisons add work. A comparison step costs 24
units to cover the bounded escape decoding of two Unicode scalars. Wide objects
use quadratic duplicate checks, stopped by this work allowance. Work units are
a deterministic budget, not CPU instructions or elapsed time. `work_used()` and
`index_capacity_bytes()` expose accounting on success.

`Value`/`encode_into` support null, bool, exact u64/i64, UTF-8 strings, arrays,
objects and validated raw `Node`s. Preflight checks duplicate keys, all limits,
checked sizes and output capacity before any write; errors preserve every output
byte. Raw embeddings also count nested depth/tokens/decoded-string bytes against
the destination limits. Encoding allocates no dynamic storage and uses only
borrowed inputs. Bytes beyond the successful encoded length remain untouched.
There are no consuming operations or partial linear-resource construction.

`SwarFieldMatcher` safely compiles at most seven printable ASCII bytes, rejecting
quote/backslash/control/non-ASCII keys. `matches_window` checks lexical bytes
including the closing quote without over-reading short windows; it does not
validate JSON. `matches_key` operates on validated keys and includes the decoded
fallback for escaped-equivalent spellings. `JsonSizeBound` offers checked integer,
boolean and conservative string ceilings. `Dynamic { static_ceiling }` bounds
only the statically bounded contribution after excluding dynamic payloads; it
provides neither an upper nor a lower bound on total encoded size. The scalar escape-mask reference
carries backslash parity across 64-byte blocks; it is not used as a SIMD proof.

Run the self-checking example with:

```sh
cargo run -p vesper-serde --example bounded_json
```


## Binary codecs (§20.4)

`streamvbyte` implements the original raw, non-delta **1/2/3/4-byte** StreamVByte
u32 layout. The caller supplies the count out-of-band. All `ceil(count/4)`
control bytes come first, followed by the concatenated little-endian data.
Each control byte holds four two-bit `(width - 1)` tags, least significant lane
first. This is not LEB128, delta coding, or the distinct StreamVByte 0/1/2/4
variant. For example `[1,256,65536,16777216]` encodes as:

```text
e4 01 00 01 00 00 01 00 00 00 01
```

`streamvbyte::{encoded_size, max_encoded_size, encode_into, decode_into}` use
checked counts and sizes. Encode writes the minimal widths and zeroes unused
tail control bits. Decode ignores unused tail tags and accepts non-minimal widths
for interoperability, while requiring the exact input frame length. It never
reads padded bytes. Every Result failure preserves all output bytes/elements;
a successful write changes only the returned prefix. The operations allocate
no dynamic storage. `Limits` bound values, encoded bytes and logical work.

```rust
use vesper_serde::streamvbyte::{self, Limits};
let mut bytes = [0; 32];
let len = streamvbyte::encode_into(&[1,256,65536], &mut bytes, Limits::default())?;
let mut output = [0; 3];
streamvbyte::decode_into(&bytes[..len], 3, &mut output, Limits::default())?;
assert_eq!(output, [1,256,65536]);
# Ok::<(), vesper_serde::binary::Error>(())
```

`archive` implements the **VSPA host format**, version 1, tree schema 1. Each
node contains one u32, a UTF-8 label and ordered children. It is not an existing
external archive format, arbitrary object graph, mmap API or source ABI. Empty
archives, cycles, shared children and orphans are rejected. Nodes have a
canonical topological order: root index zero and child index greater than its
parent. Every non-root node has exactly one parent.

The little-endian 32-byte header is:

| Offset | Field |
| --- | --- |
| 0 | Magic `VSPA` (four bytes) |
| 4 | u16 version = 1 |
| 6 | u16 schema = 1 |
| 8 | u32 exact archive byte length |
| 12 | u32 node count, at least one |
| 16 | signed RelPtr32 root, target byte 32 |
| 20 | RelSlice32 nodes: i32 displacement, u32 count |
| 28 | u32 reserved zero |

Exactly `count` 32-byte records start at byte 32:

| Record offset | Field |
| --- | --- |
| 0 | u32 value |
| 4 | RelSlice32 UTF-8 label bytes |
| 12 | RelSlice32 child RelPtr32 entries |
| 20, 24, 28 | reserved u32 zeros |

After the complete node table, each node's payload appears in node-index order:
label bytes, zero padding to four bytes, then its child RelPtr32 entries. Empty
slices have zero displacement/count and no payload. Every nonempty slice must
point to precisely the next canonical payload position; overlaps, aliases,
nonzero padding, unreachable trailing bytes and pointers into metadata fail.
All signed offsets originate at their own displacement field. A child entry
usually has a negative offset because its node precedes the payload. The entire
V1 archive fits within `i32::MAX` bytes, and each range calculation is checked.

`RelPtr32` and `RelSlice32` are explicitly unvalidated wire descriptors. Their
`resolve` methods check field and target ranges, signed arithmetic and logical
alignment; they cannot construct validated node views. Alignment refers to
archive-relative offsets, not host pointer addresses. Safe byte-copy reads allow
the archive itself to reside in an unaligned host subslice.

`archive::Archive::validate` validates actual bytes, exact version/schema,
all node/payload/reference ranges, label UTF-8, reserved/padding bytes and the
complete rooted-tree/depth invariant before publishing an immutable borrowed
`Archive`/`NodeView`. Metadata counts must fit actual node bytes before allocation.
No unsafe typed casts are used. `label_bytes` borrows directly in constant-size
work; `label` safely rechecks UTF-8 in O(label bytes) to produce a str. Child
iteration has constant work per edge. No global O(1) or performance claim is made.

`archive::{encoded_size, encode_into}` borrow `TreeNode` inputs and perform full
schema, checked-size, budget and capacity preflight before writing caller bytes.
Failure preserves the entire output and all caller input owners. Both preflight
and validation use one bounded fallible temporary metadata vector, dropped before
return; no heap metadata is retained by archive views. `validation_bytes(count)`
reports requested vector payload bytes, excluding allocator bookkeeping/capacity
rounding. Validation is iterative, including deep tree checks. Limits cover
archive bytes, nodes, children per node, total edges, depth (root = 1), per-label
and total UTF-8 bytes, temporary metadata and deterministic work. Work units
charge metadata, records, edges, labels and data passes; they are not instruction
or timing counters. Source-region/linear typing, derivation, SIMD, mmap/device
execution and cryptographic authenticity are not implemented.

Both binary modules report `binary::{Error, ErrorKind}` independently of the
unchanged JSON errors. Fixed byte vectors, independent differential reference
encoding, malformed/truncation/mutation tests and owner/buffer-preservation
checks run with `cargo test -p vesper-serde`. The self-checking binary example is:

```sh
cargo run -p vesper-serde --example binary_codecs
```
