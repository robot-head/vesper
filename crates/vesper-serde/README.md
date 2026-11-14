# vesper-serde

Bounded Rust host JSON and binary codec slices from design §20. It has no
runtime dependencies. `unsafe` is denied except in one gated module of
`std::arch` stage 1 kernels (see "Two-stage SIMD parsing" below). It does not
provide source macros, source Region typing, the `eff [Isa[P]]` source effect,
linear-resource deserialization, arbitrary object-graph archives or
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
separately check representability. `as_f64` additionally supports the bounded
finite binary64 profile below; arbitrary raw numbers retain their original role.
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
count and `max_index_bytes`. What `max_index_bytes` leaves after the tape pays
for the stage 1 index at 4 bytes per structural or escape position; every
engine (including recursive descent, which counts the same positions) reports
`AllocationLimit` at the first position that does not fit, and
`index_bytes()` reports tape plus index bytes. Limits count logical vector payload capacity, not
allocator bookkeeping or process RSS. Strings are never heap-allocated by the
codec. Container recursion has a hard ceiling of 128 in addition to the caller
limit. All input bytes count toward work before UTF-8 validation; consumed bytes
and decoded duplicate-key comparisons add work. A comparison step costs 24
units to cover the bounded escape decoding of two Unicode scalars. Wide objects
use quadratic duplicate checks, stopped by this work allowance. Work units are
a deterministic budget, not CPU instructions or elapsed time. `work_used()` and
`index_capacity_bytes()` expose accounting on success.

`Value`/`encode_into` support null, bool, exact u64/i64, finite f64, UTF-8 strings, arrays,
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
boolean, finite binary64 and conservative string ceilings. `Dynamic { static_ceiling }` bounds
only the statically bounded contribution after excluding dynamic payloads; it
provides neither an upper nor a lower bound on total encoded size. The scalar escape-mask reference
carries backslash parity across 64-byte blocks; it is not used as a SIMD proof.

Run the self-checking example with:

```sh
cargo run -p vesper-serde --example bounded_json
```

## Two-stage SIMD parsing (§20.3, §20.5)

Stage 1 (`simd::StructuralIndex::build`) reads 64-byte blocks and produces the
`u32` positions of every operator, opening and closing quote and scalar start
outside strings, the positions of escape-starting backslashes inside strings,
and a UTF-8 verdict. Each path classifies bytes with nibble shuffle tables,
detects odd backslash runs with branchless carry arithmetic, finds string
interiors with a carry-less-multiply prefix XOR and validates UTF-8 with the
Keiser–Lemire lookup tables. Paths: `Isa::Avx512` (AVX-512F/BW, one register
per block, and one VPCLMULQDQ computing four blocks' quote parities),
`Isa::Avx2` and `Isa::Sse42` (x86_64, with PCLMULQDQ), `Isa::Neon` (AArch64,
with PMULL) and `Isa::Scalar`, a byte-at-a-time reference that is always
available. `Isa::detect()` picks AVX2, then NEON, then SSE4.2, then AVX-512
(slower than AVX2 on the one host measured, so opt-in). There is no SVE path:
Rust's SVE intrinsics are unstable; `Isa::sve_reported()` says whether the CPU
has SVE. Every path produces bit-identical per-block masks
(`simd::block_masks`), which the tests check on random and adversarial input.

Stage 2 (`IndexedJson::parse_simd`, `parse_with_isa`, or the reusable
`SimdParser`) validates the document over the structural index with a bounded
explicit stack and builds the same subtree-skipping token tape as
`IndexedJson::parse`. On every input the scalar parser accepts, it produces an
identical tape and `work_used`; it rejects the same inputs with the same error
kind and offset, even when several limits are exceeded at once (it walks in the
scalar parser's order and maps errors through that parser's work frontier).
Nothing is decoded
during stage 2: numbers are converted and escaped strings unescaped only when
`Node`/`Cursor` reads them, and unescaped strings borrow the input. Stage 1
keeps structural and escape positions in one buffer (structurals from the
front, escapes from the back) of at most `min(input, entries allowed by
max_index_bytes)` `u32`s; a document with more is truncated and stage 2 fails
where the scalar parser would. `SimdParser` keeps both buffers between
documents, so steady-state parsing of no larger documents makes no allocation.

`NdjsonBatch::parse` now indexes records with `ndjson::Engine::detect()` (SIMD
when available, sharing one stage 1 buffer across records);
`NdjsonBatch::parse_with(.., Engine::Scalar)` keeps the recursive-descent path.
Batch statistics and `event_columns` projections are identical either way.
`tiles::TelemetryIngestor` decodes the §20.5 `TelemetryPoint` schema from
NDJSON into 16-wide AoSoA tiles (64-byte-aligned hot columns, separate cold
trace ids), transactionally and, once warm, without heap allocation; schema
keys are matched as two 64-bit words (`tiles::SwarKey`).

```rust
use vesper_serde::{IndexedJson, Limits, SimdParser, simd::Isa};

let doc = IndexedJson::parse_simd(br#"{"id":42,"skip":[1,{"a":2}],"t":"x"}"#, Limits::default())?;
assert_eq!(doc.root().field("id", 1024)?.unwrap().as_u64()?, 42);
let mut parser = SimdParser::new();
let t = parser.parse(br#"{"t":"borrowed"}"#, Limits::default(), |d| {
    d.root().field("t", 1024).map(|n| n.map(|n| n.raw()))
})??;
assert_eq!(t, Some("\"borrowed\""));
assert!(Isa::Scalar.is_available());
# Ok::<(), vesper_serde::Error>(())
```

`examples/simd_throughput.rs` measures stage 1, full parsing and tile
ingestion per path; see `docs/parallel-results/serde-simd-json.md` for numbers
measured on one machine (and `serde-json-rest.md` for the AVX-512 path and
float decoding). `lean/SimdJsonEscape.lean` proves, with `bv_decide`, that a
Lean model of the branchless escape kernel satisfies the scalar reference's
recurrence for all 64-bit masks. `lean/SimdJsonQuotes.lean` proves that the
carry-less multiply by all ones equals the prefix-XOR cascade and the per-bit
quote parity recurrence (with the block carry), and that the whitespace,
operator and control classifications (x86 `pshufb` and AArch64 `tbl`
semantics) are exact for all 256 bytes. These are proofs about the Lean models,
not about the compiled Rust code.

## Finite binary64 JSON

`Node::as_f64(FloatLimits)` and transactional `Cursor::read_f64(FloatLimits)`
convert already validated Number nodes to the correctly rounded binary64 with
`number::decode_f64`: Clinger's exact fast path, then Eisel–Lemire over a
generated 128-bit power-of-five table (`tools/gen_pow5.py`, rechecked by a test
with an independent big integer), then `str::parse` when Eisel–Lemire cannot
decide. It reports which path decided. IEEE rounding, subnormals and underflow to correctly signed zero are
supported. Overflow to Infinity returns `Number` at the original token start;
non-number nodes retain `Type`, and empty cursors retain `End`. Every failed
cursor conversion preserves its position for retry or raw forwarding. Existing
integer conversions and raw number spellings remain unchanged.

`FloatLimits` defaults to 4,096 number bytes and 32 Mi logical work units, with
HARD caps of 65,536 bytes and 128 Mi units. Before scalar conversion it prepays
checked `bytes² + 512` work;
invalid byte settings return `InputLimit`, invalid work settings `WorkLimit`.
These units are deterministic admission accounting, not CPU instructions or
elapsed-time bounds. A valid enormous JSON number may remain raw-forwardable
even when finite f64 conversion refuses it.

`Value::F64` rejects all NaN signs/payloads and both Infinities during complete
recursive preflight, before changing any output byte. Finite numbers format in
a stack buffer of 512 bytes; `JsonSizeBound::F64` is `Bounded(512)`. Encoding then
decoding preserves exact binary64 bits, including negative zero. Decimal inputs
may round; this is neither arbitrary-precision decimal equality nor a unique
canonical decimal encoding. The encoder pays two conservative formatting passes
plus output work under the unchanged default work limit, so large float arrays
may require an explicitly larger finite allowance. Encoding/formatting adds no
dynamic allocation; the existing JSON parser still uses its bounded token Vec.

Run the actual self-checking finite corpus and independent Python interoperability:

```sh
cargo build -p vesper-serde --example finite_float_json
python3 scripts/check-float-json.py --binary target/debug/examples/finite_float_json
```

The example emits bounded JSON-lines records containing bit strings and actual
encoded numeric values. Python parses both integer and fractional spellings as
float, rejects non-JSON constants, and compares exact packed binary64 bits. This
is a finite scalar host profile, not Dragonbox/SIMD conversion,
source derives/Region typing, DetFloat arithmetic/backend verification or a
throughput, allocation-free whole-process, RSS or proof guarantee.


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
