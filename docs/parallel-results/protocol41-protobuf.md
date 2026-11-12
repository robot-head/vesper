# Canonical schema-driven Protobuf — issue #72

Issue: <https://github.com/robot-head/vesper/issues/72>. Dedicated worktree
`/workspace/vesper-worktrees/protocol41-protobuf`, branch `protocol41/protobuf`,
base contract `9eeaecf`. Read the issue, protocol41 contracts and §38. Source
remains the 41-section/Appendix-A snapshot
`8e0f05ae32a6c87a8ce25fb339f66c760fe5d5f3fd3cb05e58236d291fa62207`, modified
`2026-11-12T09:18:30.204Z`; §38 digest
`e86ef66e6ce08aea8f2ba5f661582a96b0a016371890e67973f73490f3b8b293`.

Only the new protobuf module/test/example and this report are committed. A local
`pub mod protobuf;` registration was used for testing and restored before commit.
Grammar lib/README, root registration/lock/CLI, existing codecs, source/core/IR/
ABI/arithmetic/Lean extractor and all external Warp paths remain unchanged.

## Explicit accepted profile

**`vesper.protobuf.explicit-presence/1`** is selected out of band together with a
fixed validated schema. It does not add a wire header, and it is not a claim that
standard Protobuf or all ordinary Protobuf encoders are canonical.

- Supported scalar kinds: uint32, uint64, sint32, sint64, bool, fixed32, fixed64,
  valid UTF-8 string, bytes; nested messages and repeated versions of each.
- Legal field numbers are 1 through 2^29−1, excluding reserved 19000–19999. Only
  the appropriate 0/1/2/5 wire type is accepted. Groups/invalid wire types fail.
- Fields appear by ascending field number. Repeated elements of one field are
  contiguous and retain their exact order. Singular fields cannot repeat;
  unknown fields fail, including in nested messages.
- Every singular field is optional and has **explicit presence**. Absent differs
  from present zero, false, empty bytes/string or empty nested message. No present
  default is silently omitted or coerced into absence. No required/default-value
  schema annotations are supplied.
- Repeated numeric scalars use exactly **one nonempty packed segment**. Alternate
  unpacked encodings, multiple segments and empty segments fail. Zero repeated
  elements is represented only by absence. Repeated strings/bytes/messages use
  one length-delimited occurrence per element, including empty elements.
- Tags, numbers and lengths are canonical unsigned varints. uint32/sint32 ranges
  are checked; bool accepts only 0/1. Zigzag, fixed-width LE and UTF-8 are decoded
  into actual typed logical values; no Unicode normalization is applied.
- Root message and length-delimited extents never exceed `i32::MAX` bytes, even
  when caller budgets are raised. Configured defaults are much smaller.
- All nested lengths are checked against their enclosing byte range and fully
  consumed. `parse_exact` consumes the complete supplied slice, with no ignored
  suffix. Raw Protobuf is not self-framing: truncation at a complete optional
  field boundary can yield a distinct valid message. Use outer length framing
  for a transport boundary/pipeline; no marker can detect every such truncation.

For a fixed schema/profile, accepted bytes retain field presence and repeated
order and re-encode identically **when encoder budgets permit encoding**. Parse
and encode have deliberately different byte budgets: parse can accept bytes above
a smaller configured max_output_bytes; encoding then fails transactionally.
No formal bijection/non-malleability theorem or cryptographic authenticity claim
is supplied. General int32/int64, sfixed, floats, enums, maps, oneofs, extensions,
recursive schemas, proto compiler/source derives, Regions, SIMD and constant-time
parsing are unsupported. Standard protobuf parsers often accept unknowns,
last-wins duplicates, arbitrary order and packed alternatives; this named strict
profile intentionally rejects those normal noncanonical alternatives.

## Public APIs

```rust
pub const PROFILE: &str = "vesper.protobuf.explicit-presence/1";
pub const HARD_DEPTH: usize = 64;
pub const MAX_WIRE_BYTES: usize = i32::MAX as usize;
Kind::{U32,U64,Sint32,Sint64,Bool,Fixed32,Fixed64,String,Bytes,Message(usize)}
FieldSchema { number: u32, kind: Kind, repeated: bool }
MessageSchema { fields: Vec<FieldSchema> }
Schema::validate(&[MessageSchema], root: usize, Limits) -> Result<Schema<'_>>
Message<'a> { fields: Vec<FieldValue<'a>> }
FieldValue<'a> { number: u32, value: Value<'a> }
// Value mirrors Kind, with borrowed String(&str)/Bytes(&[u8]) and owned nested Message.
schema.parse_exact(bytes) -> Result<Message<'_>>
schema.encoded_size(&message) -> Result<usize>
schema.encode_into(&message, output) -> Result<usize>
```

Kind is Copy/Debug/Eq. Recursive Message/Value intentionally lack Clone. A
compile-fail Rustdoc exercises that absence. Schema borrows flat descriptors;
validation checks all field IDs, sorted uniqueness, references, cycles and full
memoized DAG heights, including unreachable definitions. No recursive schema or
message is cloned. Actual string/byte payload views borrow the original input.

`Error { kind: ErrorKind, offset: usize }` is module-local, allocation-free and
implements Display/Error. Wire errors use byte offsets, schema errors may use
message indices, preflight errors use zero. Root CLI schema validation/JSON
policy is coordinator-owned; this library still validates direct typed schemas
and messages and does not rely on CLI checks.

## Bounds and ownership

Limits are per operation. Defaults:

| Field | Default |
| --- | ---: |
| max_schema_messages | 128 |
| max_schema_fields (total) | 1,024 |
| max_fields_per_message | 128 |
| max_depth | 32 (hard 64, zero/>64 rejected) |
| max_input_bytes | 4 MiB |
| max_output_bytes | 4 MiB |
| max_payload_bytes (length-delimited extent) | 1 MiB |
| max_nodes (message containers + field elements) | 65,536 |
| max_metadata_bytes | 8 MiB |
| max_work | 64 Mi logical units |

Schema validation checks counts/work before allocating one temporary metadata
vector. Parsing checks input/depth/nodes before constructing fields; each vector
growth charges cumulative requested capacity bytes and relocation work before
fallible reservation. Declared payload lengths never drive field-vector capacity
allocation. Repeated/aliased definitions or payload slices cannot bypass expanded
node/work accounting. Metadata caps describe **charged/requested Vec capacity
payload**, not exact allocator rounding/overhead, caller allocations or RSS.

Typed encoding borrows schema/message owners, validates every type/order/count/
size/depth/work/output limit, then checks output capacity before the first write.
Every Result failure preserves the entire output and all input owners. Success
preserves suffix bytes. The writer recomputes previously checked bounded nested
sizes without allocation/cloning; depth-weighted work reservations cover those
additional visits. Its unchecked internal sums/casts depend on successful
checked preflight over the same immutable values. Encoding itself allocates no
metadata; this is not a blanket zero-allocation codec claim because schema/parse
metadata allocate. Allocation failure uses try_reserve_exact and a structured
error; actual allocator failure was not fault-injected. Work units are logical
charges, not CPU/time evidence.

## Validation evidence

Used private `/tmp/vesper-protobuf-target` (about 11 MiB), one Cargo job, dev/test
debug zero and incremental off. With the temporary local module registration:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-protobuf-target cargo test -p vesper-grammar --all-targets
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-protobuf-target cargo test -p vesper-grammar --doc
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-protobuf-target cargo clippy -p vesper-grammar --all-targets -- -D warnings
cargo fmt -p vesper-grammar --check
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-protobuf-target cargo run -p vesper-grammar --example protobuf
```

Passed **19 new Protobuf tests + all 20 existing Grammar integration tests**,
four Rustdocs (two new, including nonclone compile-fail), strict all-target Clippy,
formatting and the self-checking example:

```text
vesper.protobuf.explicit-presence/1: 25 canonical bytes; 6 root field elements
Borrowed string, nested message, packed values and transactional failure checks passed
```

Independent standard wire examples were checked against the actual official
[encoding guide](https://protobuf.dev/programming-guides/encoding/) and its
[Markdown source](https://github.com/protocolbuffers/protocolbuffers.github.io/blob/main/content/programming-guides/encoding.md):
`08 96 01` (150), `12 07 74 65 73 74 69 6e 67` (testing),
`1a 03 08 96 01` (nested message), and packed field5 `[1,2,3]`
`2a 03 01 02 03`. An additional independently calculated field4 packed vector
`22 06 03 8e 02 9e a7 05` represents `[3,270,86942]`. Positive int32 values in the
guide have identical VARINT wire bytes to the supported unsigned values; this
does not imply negative int32 support. No external Protobuf runtime or compiler
was required/claimed in these tests.

Other tests cover all numeric singular/packed boundaries and zigzag signed
minima, explicit-default versus absent presence, borrowed UTF-8/raw payload
identity, repeated nonnumeric ordering, every truncation/split of a complete
25-byte message (valid field-boundary prefixes distinguished correctly), every
insufficient output length/suffix conservation, overlong tags/lengths/numbers,
unknowns/duplicates/order/group types, malformed UTF-8, boolean alternates,
numeric/LEN overflow, schema recursion/reference/DAG height failures, each schema
and data budget, typed direct API failures, metadata growth/repeated limits,
10,000 independent division/remainder scalar-reference cases, all 65,536
short-varint candidates, 6,400 actual byte mutations reserialized identically
whenever accepted, 16-level nesting and actual hard-depth-64 roundtrips.

Aether independently reviewed implementation/tests read-only and found no concrete
blocker. Its requested-capacity and encoder-budget caveats are explicit above.
No Lean file or extracted/model proof was added.

## Coordinator integration

Add `pub mod protobuf;` to existing `vesper-grammar/src/lib.rs` and register the
profile/limitations in the coordinator-owned README. The crate already belongs
to the workspace; no Cargo dependency/lock update is needed. CI can execute:
`cargo run -p vesper-grammar --example protobuf`.
