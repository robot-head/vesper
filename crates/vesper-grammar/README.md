# vesper-grammar

Dependency-free, safe scalar host implementations of an explicit §38 subset:
canonical unsigned LEB128, dependent-length frames, validated primitive Arrow
signed-i64 buffers, executable deterministic byte-FIRST grammars, explicit
canonical Protobuf/DER profiles, and actual primitive Arrow IPC stream reading.
This is not a source macro compiler, general LL/PEG/ASN.1 compiler, SIMD
implementation, secret constant-time parser or formal proof.

## Protocol profiles

- `protobuf` implements `vesper.protobuf.explicit-presence/1`: borrowed strings
  and bytes, acyclic typed schemas, explicit singular presence/defaults, ordered
  fields and one nonempty packed segment per repeated numeric field. Unknowns
  and alternate representations are rejected. Standard protobuf itself has no
  universal canonical encoding; maps, oneofs, extensions and float types remain
  unsupported.
- `der` validates minimal supported universal primitive encodings and sequences.
  Tag 17 requires an explicit SET OF schema; ASN.1 SET is unsupported. Typed
  encoding sorts complete child TLVs and all encoder errors preserve output.
  This is not X.509, PKCS#8 or secret-material semantic validation.
- `ipc` reads actual Arrow IPC Schema/RecordBatch FlatBuffer metadata and
  borrowed signed-i64 column bodies. Independent PyArrow fixtures cover V4/V5,
  nullable values and framing. Default relative alignment is eight bytes;
  optional 64-byte suitability is separate. Dictionaries, compression, nested
  fields and custom metadata are explicit unsupported profiles. IPC structural
  validity is not canonical serialization or a byte-uniqueness claim.

Each module documents its byte/depth/work/count/metadata/output budgets. Payload
borrows do not imply allocation-free metadata. See `protobuf`, `der` and `ipc`
examples and the matching `docs/parallel-results/protocol41-*.md` reports.

## Canonical framing and remainder ownership

`framing::decode_canonical_u64_leb128` parses a prefix using at most ten bytes.
It rejects overlong encodings and all >u64 representations, including tenth-byte
continuations. `parse_u64_exact` additionally rejects every trailing byte.
`parse_frame` parses a canonical unsigned-LEB128 byte length and that many opaque
payload bytes. `parse_frame_exact` requires EOF after the payload. These frame
contents have no implicit nesting; nested dependent formats are executable
through grammar `Rule::Frame` below.

```rust
use vesper_grammar::framing::{self, Limits};
let mut out = [0; 16];
let size = framing::encode_frame_into(b"hello", &mut out, Limits::default())?;
assert_eq!(&out[..size], b"\x05hello");
assert_eq!(framing::parse_frame_exact(&out[..size], Limits::default())?, b"hello");
# Ok::<(), vesper_grammar::Error>(())
```

Prefix parsing returns a nonclone `Remainder`. `assert_exact_eof` consumes it on
success; failure returns the identical owner/offset in `TailError`. `next_frame`
transfers it to another frame parse and likewise returns the unchanged owner on
failure. Input and payload bytes are immutable borrows. Rust can still drop,
forget, inspect or independently reparse borrowed bytes: this is an explicit host
API, not Vesper source linear exhaustion. Each operation has its own resource
budget; repeated independent calls do not form a globally metered stream.

The accepted exact unsigned-LEB128 encoding is unique per u64. The exact opaque
frame encoding is unique per payload. Prefix parsing alone makes no whole-input
uniqueness claim. Neither assertion implies cryptographic authenticity. Encoders
check sizes, payload/output/work limits and capacity before the first write;
every Result failure preserves all caller output bytes and every input owner.
Successful writes preserve the output suffix. Framing allocates no storage.

## Actual primitive Arrow validation

`arrow::{FieldNode, BufferDesc}::from_wire` decode exact 16-byte little-endian
signed-i64 descriptor pairs. They are raw DTOs, not validated views.
`I64Column::validate` checks actual borrowed body bytes before creating a column:

- Nonnegative, usize-representable row/null counts, offsets and lengths; checked
  `rows * 8` and half-open ranges. Unknown null counts (`-1`) are unsupported.
- Exact values-buffer length `rows * 8`, complete body bounds and no overlap
  between nonempty validity/value buffers. Empty ranges do not alias.
- A missing bitmap is allowed only with zero null count. Present bitmaps need
  at least `ceil(rows/8)` bytes. Actual LSB-first row bits must agree with the
  declared null count. Padding bytes and bits outside the row count are ignored,
  consistent with this noncanonical Arrow-buffer profile.
- Explicit alignment profile: `ByteCopy` imposes no alignment, `Relative8` and
  `Relative64` check both buffer offsets. `HostAddress64` also checks actual host
  addresses of nonempty buffers. Relative alignment never establishes absolute
  host alignment, and host alignment never proves a Domain-1/DMA capability.
- Body/row/work limits before bitmap iteration. No allocation, typed pointer cast,
  input mutation, offset array, nested child, dictionary or compression support.

`get` exposes only validated i64/null values. `checked_sum(max_work)` uses the
actual bitmap and sequential checked i64 additions; any intermediate overflow is
an error even if later values could cancel it. Empty/all-null columns yield
`None`. Values/bitmap views borrow input; null payload bytes need not encode zero.
This is a primitive body-buffer validator, not a FlatBuffers/Arrow IPC envelope,
message/schema/compression/dictionary identity or DMA validator.

## Executable grammar subset

`Grammar::validate(&rules, root, limits)` borrows caller rules and derives FIRST
and nullable information from their actual structure. Supported rules are:

| Rule | Executed behavior |
| --- | --- |
| `Empty`, `Literal`, `Byte` | Exact byte matching, including empty literals |
| `Uleb128` | Canonical unsigned u64 with captured numeric value |
| `Blob` | Canonical length plus opaque captured payload |
| `Frame(child)` | Canonical length plus exact child parse confined to payload |
| `Sequence` | Ordered references to child rules |
| `Choice` | Select a nonnullable child by disjoint derived byte FIRST sets |

Every rule reference and branch identity is validated, including unreachable
rules. Branch IDs are unique within their choice; a capture identifies both the
choice rule index and branch ID. Cycles are unsupported. Shared acyclic DAG
rules are supported. Their height is checked even when metadata is memoized.

Choice alternatives must be nonnullable and have disjoint FIRST sets. Nullable
rules and sequences are supported outside alternatives, and sequence FIRST sets
skip nullable prefixes. Nullable choice alternatives are rejected explicitly:
there is no FOLLOW-set analysis, ordered backtracking, repetition, recursive
production, left-recursion handling, generic PEG semantics or user-asserted FIRST
metadata. ULEB128/blob/frame FIRST sets conservatively include all 256 byte values.

`parse_prefix` executes the chosen rules and returns bounded capture events plus
a `Remainder`. `parse_exact` checks outer EOF; every nested `Frame` checks its own
EOF before returning. Captures preserve actual rule, chosen branch, input span,
ULEB number and frame/blob payload span. Failed parses publish no partial event
vector and never modify rules/input. This subset does not supply a generic
serializer or assert a bijection for arbitrary grammars.

Validation allocates one fallible bounded metadata vector. Execution allocates
one bounded event vector. Edges, definitions, literal bytes, branch-ID comparisons,
rule visits, byte work, input/frame bytes, output event count and vector payload
bytes have explicit limits. Expanded visits through repeated DAG references each
consume work and event budget; sharing cannot bypass them. Both vectors count
requested payload, not allocator bookkeeping/capacity rounding or process RSS.
Caller allocations for rule definitions are outside these bounds. Recursion has
an additional hard height ceiling of 64; validation/execution use the lower of
this ceiling and the caller limit. No result depends on CPU-time estimates.

`Error { kind, offset }` is allocation-free; input errors use byte offsets,
while definition errors use rule indices. Encoder/resource errors can use zero.

Run the self-checking example:

```sh
cargo run -p vesper-grammar --example grammar
```
