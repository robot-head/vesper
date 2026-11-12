# Issue #74: bounded actual Arrow IPC stream reader

Implemented on `protocol41/ipc` from `9eeaecf` after reading issue #74,
`docs/rfcs/protocol41-contracts.md`, refreshed specification §38, and the existing
`arrow.rs`. Only new IPC module/tests/example/fixtures and this report are owned.
The existing i64 body validator is unchanged. The coordinator registers
`pub mod ipc;` in `crates/vesper-grammar/src/lib.rs`; this was temporarily wired
for local checks and restored before commit. No new Cargo dependencies.

## Actual supported format and scope

`ipc::parse_exact(input, Limits)` checks an actual Arrow IPC **stream**, with a
Schema followed by zero or more RecordBatch FlatBuffer messages and an explicit
EOS marker. Modern continuation framing and legacy framing are supported, with
consistent framing style per stream; metadata versions V4/V5 (enum values 3/4)
are checked per message. Stream-file footers/magic are outside this profile.
`parse_prefix` returns checked stream, consumed length and borrowed tail;
`parse_exact` rejects any bytes following EOS. Missing EOS is rejected explicitly.

The schema profile is flat signed 64-bit integer fields with borrowed UTF-8 names
and declared nullability, little endian. Empty schemas and empty batches work.
Each batch's FieldNode/Buffer vectors must exactly match that schema. Body lengths,
rows, null counts, nonnullable constraints, offsets, exact i64 value lengths,
bitmap counts and cross-column nonempty buffer overlap are checked. Actual bodies
then pass through existing `arrow::I64Column::validate`, rather than a replacement
mock decoder. Null payloads and padding are opaque. Unknown/unsupported versions,
headers, integer widths/sign, big endian, dictionaries, compression, nested fields,
variadic buffers, nonempty custom metadata and nonzero feature flags are rejected.
No recursive user-defined type construction or unbounded typed DTO input exists.

Mandatory IPC eight-byte body/descriptor alignment is distinct from optional
`parse_{exact,prefix}_with_alignment(..., arrow::AlignmentProfile::Relative64)`
and `HostAddress64` suitability checks, which inspect nonempty buffers. An ordinary
PyArrow stream succeeds with default eight-byte validation while failing optional
64-byte validation. Default parsing accepts an unaligned host byte slice because
it uses byte copies for scalar reads. Empty FlatBuffer struct vectors correctly
need no element-payload alignment. Opaque metadata/body padding may be nonzero;
IPC is not represented as a canonical encoding.

The FlatBuffer reader checks little-endian scalars, signed vtable displacement
(including forward shared vtables), bounded vtable slots/object sizes and scalar
field separation, forward checked uoffsets, vector lengths before allocation,
UTF-8 strings and terminators. It rejects partial object overlaps and role-confused
aliases while allowing exact immutable sharing of like objects/shared vtables.
The finite schema has no recursively followed children: nonempty child vectors
are rejected, and table traversal depth is explicit. Pairwise overlap checks and
repeated alias visits consume work; shared offsets cannot escape fanout budgets.

## Public API and resources

`Stream` exposes `.schema()`, `.batches()`, `.work_used()`, and
`.metadata_allocation_bytes()`. Schema `.fields()` yields `Field` values with
`.name()`/`.nullable()`, and `.metadata_version()` reports the Arrow enum. Batch
`.rows()`, `.columns()`, `.body_bytes()`, `.metadata_version()` expose checked
metadata and existing borrowed `I64Column`s. `Parsed` exposes `.stream()`,
`.into_stream()`, `.consumed()`, `.tail()`. Outputs have private fields, and heap
containers deliberately do not implement Clone. Errors expose kind and byte
offset, implement Display/Error, and preserve body-validator error kinds.

`Limits::validate()` enforces hard ceilings before allocation; all parser entry
points perform it and reject oversize input first. Defaults: input 32 MiB,
metadata 1 MiB/message and 4 MiB total, 64 fields, 256 batches, 1,048,576 rows per
batch/4,194,304 total, 256 name bytes, 2,048 metadata objects/message, depth 8,
64 Mi abstract work units, 8 MiB cumulative requested vector payload capacity.
Hard ceilings respectively: 128 MiB, 4/16 MiB, 1,024 fields, 4,096 batches,
8,388,608/16,777,216 rows, 4,096 name bytes, 8,192 objects, depth32, 512 Mi work,
64 MiB allocation. Zero budgets are meaningful failures except depth must be1–32.
Vector reservations use checked sizes and fallible allocation; actual input ranges
and vector counts precede payload allocations. No unchecked recursive clone or
caller-supplied recursive object is accepted. Total rows are checked before body
scans; global work also precharges each delegated validator's row scan.

Allocation is cumulative requested Vec element capacity, not allocator rounding,
peak RSS or zero allocation. Work counts input/read/reserved bytes, row scans and
overlap comparisons, not instructions/time. All validation work shares one budget.
Input and body bytes/names are borrowed; small bounded metadata vectors allocate.
`I64Column::checked_sum` is an optional projection with its own caller work budget;
checked sequential i64 overflow is a projection error, not invalid IPC. The reader
never aggregates during parse. No SIMD execution, DMA, source macros, Region,
constant-time, canonical roundtrip, proof or zero-allocation claim is made.

## Independent reference and verification

`tests/fixtures/ipc/generate.py` uses official Apache Arrow **pyarrow20.0.0** in a
user-local temporary virtualenv, producing eleven real stream fixtures and reading
them back with Arrow. `expected.json` records byte lengths, SHA-256, decoded arrays
and mathematical sums; fixture README gives provenance/upstream schema links and
reproduction commands. No Rust implementation code produces fixtures. Fixtures
cover continuation V5, legacy V4, signed/null values with bitmap tails, empty/all-null
batches, zero-field/schema-only streams, mathematical sum overflow, and unsupported
real Arrow compression/dictionary/list/i32/u64/custom-metadata streams.

Eleven IPC integration tests verify actual values/nulls/sums and versions, borrowed
payloads, exact/prefix/EOS behavior, every byte truncation, optional alignment,
hostile root/uoffset/vtable/vector counts, object/scalar aliases, string length,
terminator/UTF-8, invalid tags/version/endianness/nullability, negative/overflowing
nodes/descriptors, body overlap/bitmap errors, all budgets and exact work/allocation
boundaries. 1,984 bounded single-byte mutations exercise parser failure without
panics. `sum-overflow.arrow` parses and its optional checked i64 sum fails as expected.

With temporary module registration, one build job, no incremental compilation,
private target `/tmp/vesper-protocol41-ipc-target`, dev/test debug info disabled:

```sh
cargo fmt -p vesper-grammar --check
cargo test -p vesper-grammar
cargo clippy -p vesper-grammar --all-targets -- -D warnings
cargo run -p vesper-grammar --example ipc
```

All passed: 31 integration tests (11 new IPC), 2 doctests, strict all-target Clippy.
The self-checking example verifies actual nullable values and batch sums, then
prints `checked 2 fields, 3 batches, 13 rows; sums 11/450 and null/12`; assertion or
validation failures exit nonzero. Existing compiler/core/Arrow paths remain unchanged.
