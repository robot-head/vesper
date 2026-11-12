# Strict bounded DER profile (#73)

Implemented claimed [issue #73](https://github.com/robot-head/vesper/issues/73)
on `protocol41/der`, from `9eeaecf`. Read the issue,
`docs/rfcs/protocol41-contracts.md`, complete §38 and current Grammar APIs.
Only new `src/der.rs`, matching tests/example and this report are committed.
The coordinator-owned Grammar lib/README and all root files remain unchanged;
a temporary `pub mod der;` used for validation is restored before handoff.

## Supported semantics

This is actual strict DER TLV parsing, canonical typed encoding and recursive
re-encoding over borrowed input, not a trusted opaque DER wrapper. Supported:

- BOOLEAN: exactly one content octet, false=00, true=ff.
- Signed INTEGER: nonempty, arbitrary bounded minimal two's-complement bytes.
  Redundant 00/ff sign extension is rejected. `as_i64` is a separate checked
  convenience; larger canonical integers remain valid borrowed values.
- BIT STRING: one unused-bit-count octet (0..7), empty requires count0, and low
  unused bits in the last payload octet must be zero. Bit length is preserved.
- Primitive OCTET STRING, zero-length NULL, and OID with minimal terminated
  unsigned base128 subidentifiers. Every subidentifier, including the first
  combined pair, must fit u64. This explicitly excludes larger legal ASN.1 OIDs.
- SEQUENCE with either an exact ordered schema field list, homogeneous
  SequenceOf, or Any's recursively supported dynamic sequence contents.
- Explicitly typed SET OF, compared/sorted lexicographically by **complete
  canonical child DER encodings**, retaining duplicate values as a multiset.

ASN.1 SET is not implemented. Universal tag17 is never automatically interpreted
as SET OF: Any rejects it with SetSchemaRequired, including nested occurrences.
A caller must supply Schema::SetOf(child). SET tag-order rules are not conflated
with SET OF octet ordering. No optional/default/choice/tagged schema compiler is
included. Context/application/private tags and unimplemented universal/time/
string types return UnsupportedTag, not accepted opaque payloads.

Tag/length parsing rejects EOC, wrong constructed bits, indefinite lengths,
nonminimal long tags/lengths, overflow and truncation. Child TLVs are confined to
the constructed payload extent. Fixed schemas enforce arity and child types;
exact parse requires complete outer input consumption. Prefix parse returns the
exact consumed extent and a document borrowing only that prefix. It does not
validate or consume the caller's trailing bytes, and claims no linear tail owner.

## Ownership, budgets and transactionality

Borrowed Schema/Value APIs are validated before cloning or allocation (the
implementation never recursively clones them). Shared schema/value DAGs charge
every expanded visit and respect node/work/depth caps. Parse validation is a
first pass without metadata allocation; its exact second-pass work requirement
and full node-record allocation are admitted before allocating the flat tape.
Payload bytes remain borrowed; typed Node views cannot be forged externally.

Typed encode preflights all primitive semantics, schema compatibility, expanded
node count, lengths, work, metadata, intermediate and output allowances plus the
caller output capacity. Only then does it allocate private encoded children.
For SET OF, the conservative sort work charge is `n*(sum_child_sizes+n)` before
allocation, covering bounded insertion sorting. Sort mutates only private child
buffer ordering. A private parent output is constructed after children; all
Result failures leave supplied output bytes unchanged. Successful copies preserve
the output suffix. `reencode_into` reconstructs canonical tags/lengths recursively
from validated nodes, rather than merely copying input, and reapplies depth,
value, node, work, metadata, intermediate and output limits before allocation.
No new schema is walked during reencoding an already typed Document.

`EncodingPlan.intermediate_bytes` sums complete encoded sizes of every expanded
value node. It conservatively bounds simultaneously retained child buffers plus
parent output; no per-child encoded buffer is uncounted. Metadata counts two Vec
controls plus two usize per encoded node (64 bytes/node on this 64-bit host).
Parser metadata is `node_count*size_of<Record>`; Document.metadata_bytes reports
requested payload (48 bytes/node on this host). Allocator rounding/spare capacity,
stack/object overhead and caller-owned inputs are outside these logical counters.
All internal Vec allocations use fallible exact reserve before bounded pushes.
This is not a total RSS, process-abort cleanup, zero-allocation or constant-time
claim.

Limits and defaults (hard ceiling in parentheses):

| Field | Default (ceiling) |
| --- | --- |
| max_input_bytes | 1 MiB (16 MiB) |
| max_value_bytes | 1 MiB (16 MiB) |
| max_nodes | 4096 (16384) |
| max_schema_nodes | 1024 (4096) |
| max_depth | 32 (64), applies to schema and value traversal |
| max_work | 16 Mi units (128 Mi units) |
| max_metadata_bytes | 512 KiB (4 MiB) |
| max_output_bytes | 1 MiB (16 MiB) |
| max_intermediate_bytes | 8 MiB (64 MiB) |

Above-ceiling configurations return InvalidLimits. Zero budgets are meaningful
and reject operations needing those resources. Typed encoding also bounds total
encoded bytes by max_input_bytes, so direct value inputs cannot bypass byte
policy. Work units count bounded visits/bytes and conservative sorting/copies,
not elapsed time or an instruction-level cost proof. A valid value can fail a
conservative budget and can be retried with a larger permitted budget.

## Stable API / registration

Module-local `Error { kind: ErrorKind, offset: usize }` has Display/Error. Input
errors use byte offsets; typed/resource errors may use zero.

- `parse_exact(input, &schema, limits) -> Result<Document>`.
- `parse_prefix(input, &schema, limits) -> Result<(Document, usize)>`.
- `encoding_plan(&value, &schema, limits) -> Result<EncodingPlan>`;
  fields encoded_bytes, nodes, metadata_bytes, intermediate_bytes.
- `encoded_len(&value, &schema, limits) -> Result<usize>`.
- `encode_into(&value, &schema, &mut output, limits) -> Result<usize>`.
- `reencode_into(&document, &mut output, limits) -> Result<usize>`.
- Document: root(), nodes(), node_count(), raw(), metadata_bytes().
- Node: kind(), raw(), content(), children(), boolean(), integer_bytes(),
  as_i64(), bit_string(), octets(), oid_bytes(). Primitive accessors borrow input.

Schema variants: Any, Boolean, Integer, BitString, OctetString, Null,
ObjectIdentifier, Sequence(&[Schema]), SequenceOf(&Schema), SetOf(&Schema).
Value variants borrow payload bytes, with Boolean(bool),
BitString { unused_bits, bytes }, Sequence(&[Value]), SetOf(&[Value]). INTEGER and
OID take already canonical content bytes that are checked again before encoding.
Schema/Value intentionally have no recursive Clone/Debug implementations.

Coordinator adds `pub mod der;` to Grammar lib and updates shared README/summaries.
No dependencies, root manifest/lock changes or standalone crate registration are
needed. Root CLI can use Any normally and explicit SetOf(&Any) for --set-of;
that option does not infer nested SET OF schemas or validate general ASN.1 SET.

## Actual validation evidence

Private target `/tmp/vesper-protocol41-der-target`, one job, dev/test debug0,
incremental off. Target footprint approximately 11 MiB. Commands passed:

- `cargo test --locked -p vesper-grammar --test der`: 15 focused tests.
- `cargo test --locked -p vesper-grammar --all-targets`: 35 tests total,
  including the existing 20 Grammar/framing/Arrow tests.
- `cargo test --locked -p vesper-grammar --doc`: two existing doctests pass.
- `cargo clippy --locked -p vesper-grammar --all-targets -- -D warnings`.
- Focused rustfmt checks and git diff whitespace check.
- `cargo run --locked -p vesper-grammar --example der` prints:
  `DER: OpenSSL fixture matched (30 bytes, 7 nodes); typed SET OF sorted canonically (8 bytes)`.

Independent fixture evidence: OpenSSL 3.5.7 (9 Jun2026) generated a real 30-byte
SEQUENCE using `openssl asn1parse -genconf ... -out ... -noout`, then independently
decoded its Boolean, INTEGER -129, BIT STRING, octets, NULL and rsaEncryption OID.
The exact reproducible config and emitted bytes are embedded in the test; CI
needs no OpenSSL runtime. Hand-checked X.690 primitive forms and the RFC3279§2.3.1
rsaEncryption AlgorithmIdentifier bytes are also compared against typed encoding.
This does not claim algorithm/key/certificate semantic validation.

Tests cover signed i64 boundaries and larger integers, every Boolean value,
65,536 unused-bit/padding combinations, every tag octet, lengths0..260 and every
truncation plus long127/128/255/256/65535/65536 boundaries, OID termination/minimality/
u64 overflow, unsupported strings/times/contexts, EOC/constructed bits, nested
bounds, fixed arity, prefix/exact distinction, payload pointer borrowing, all24
SET OF input permutations, nested/duplicate/unsorted SET OF, schema/value shared
DAG budgets, exact work/metadata boundaries, all undersized output lengths and
transactional failures. A deterministic mutation corpus reencodes every accepted
candidate byte-identically with the actual recursive TLV encoder.

Local execution is Linux only. No source macro, ASN.1 compiler, formal proof,
PKCS#8/X.509 verifier, cryptographic authenticity, secret/constant-time behavior,
SIMD or performance result is inferred.
