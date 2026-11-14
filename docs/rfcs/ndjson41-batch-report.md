# Complete validated NDJSON batches (#211)

Read the reviewed published #210–213 contract before runtime writes. Owned
changes are only `src/ndjson.rs`, thirteen new batch tests and this report.
Column source `af30295` was imported solely as dependency copy `b2e68ed`; that
copy is not batch-owned work and must not be integrated as such. No existing
index/encode/helper/float/lib, Cargo/BUILD, allocator, core, CLI or workflow files
changed.

NdjsonBatch construction admits the full input and framing work, counts real
frames, and prepays the complete requested record-vector payload and fixed frame
work before a single fallible metadata reservation. Every payload then passes
the existing complete JSON validator before Batch or any view is returned.
Each parser receives minimum per-record/remaining aggregate token, index and work
allowances. Actual successful token counts, index capacities and parser work are
subtracted with checked arithmetic; no line replenishes another's budget.

Only LF frames records; one CR paired with that LF is removed. Other CR bytes
keep their ordinary JSON meaning. Empty input is valid, a single final LF/CRLF
does not create a virtual record, and every actual empty/JSON-whitespace-only
frame is rejected. Escaped newlines remain string data. Multiline JSON is not
stitched together. Late malformed unknown data, duplicate decoded keys or invalid
Unicode prevents construction of the entire batch.

Private immutable records retain bounded scalar indices and original payload
spans. RecordRef and Node lifetimes borrow Batch, while input strings borrow the
original bytes; advancing a cursor does not invalidate earlier views. Exact
global offsets, zero-based ordinals, physical LF line numbers and EOF positions
are reported through typed errors. Public json_error only relocates bounded
diagnostic data and cannot manufacture a validated view. The crate-private
monotonic commit seam supports the separate transactional projection lane.

## Actual validation

With exclusive `/tmp/ndjson41-batch-target`, one Cargo job, debug0/incremental0:

- `cargo check --locked -p vesper-serde` passes on the coherent batch source.
- All thirteen new tests pass, including actual roots/spans/borrowed UTF8 input;
  empty/final LF/CRLF/bare CR and every blank-line location; escaped versus raw
  newlines; late unknown syntax, decoded duplicates and invalid Unicode; every
  truncation of a final nested Unicode record; and exact global diagnostic
  relocation with legitimate EOF and rejected out-of-range offsets.
- Tests retain views across cursor mutation and verify physical EOF lines and
  repeated End failures without advancing. They exercise every zero/above-HARD
  setting and preserved per-record depth/string/token/index/work guards.
- Actual successful statistics determine exact aggregate input/count/token/
  index/metadata/work boundaries. Each exact limit is admitted; one-less values
  refuse at the expected global or later-record location. Individually valid
  records cannot reset exhausted token/index/work allowances.
- With the actual column source dependency, `cargo test --locked -p vesper-serde
  --all-targets` passes eighty tests across six nonempty suites, zero failures or
  ignored tests: seventeen existing binary tests, eleven float decode, eight
  float encode, six float roundtrip, twenty-five existing JSON and thirteen new
  batch tests. Existing example targets compile. This run does not include the
  separate writer's new column or NDJSON executable acceptance tests.
- `cargo clippy --locked -p vesper-serde --all-targets -- -D warnings` passes.
- `cargo test --locked -p vesper-serde --doc` passes both executable doctests.
- `cargo fmt --all -- --check` and `git diff --check` pass.

Index and metadata allocation accounting is explicit; retaining one bounded
index per record plus record metadata is intentional. This is a scalar host
batch, without incremental chunks, source Region/linear-resource typing,
TiledSeq/aligned AoSoA storage, zero-total-heap, SIMD, native/backend, timing/RSS
or proof claims. These are local Linux checks; root combines the separate column
and independent executable/Python acceptance evidence and hosted platform gates.
