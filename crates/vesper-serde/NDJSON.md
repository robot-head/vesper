# Validated NDJSON and caller-owned event columns

`ndjson::NdjsonBatch::parse` validates every record before returning the batch
or any record view. Records may be any JSON value. The complete existing JSON
parser checks UTF8, escapes, number grammar, nested unknown values and decoded
duplicate keys. The batch retains immutable input references, one bounded index
per record and one metadata vector. This is a host batch API with allocations;
it does not provide constant-memory streaming or a source-language allocator.

LF separates records; one immediately preceding CR is stripped with that LF.
Other CR bytes remain JSON whitespace or invalid string controls according to
the scalar parser. Escaped `\n` is string data. Empty input is valid; final LF
or CRLF is optional. Leading, interior or additional trailing empty/whitespace
frames fail. Multiline records are rejected rather than stitched together.

`RecordRef` exposes its zero-based ordinal, one-based physical line, original
payload byte span and validated root Node. Its `json_error` translates existing
local parser/conversion offsets into global input coordinates. Out-of-record
offsets return Overflow. EOF errors point to input length, next ordinal and the
physical EOF line, including an allowed final LF. Cursors borrow the immutable
batch; failed typed reads preserve their position.

`BatchLimits` independently bounds total input, record count, retained tokens,
index capacity, record-vector payload and logical work, as well as each record's
existing JSON limits. Defaults are 1 MiB input, 1,024 records, 65,536 tokens,
4 MiB retained index, 256 KiB record metadata and 32 Mi work units. Each parser
call receives the minimum of its own cap and the remaining aggregate allowance;
a new line does not replenish budgets. `stats()` reports actual retained sums
and prepaid framing/metadata/parser work. These metrics exclude allocator
bookkeeping and do not claim CPU instruction, timing or RSS bounds.

`event_columns` implements a fixed schema with required decoded keys `id`,
`topic` and `raw_meta`. Unknown fields are allowed after full batch validation.
IDs use exact existing u64 conversion; decimal/exponent spellings and overflow
fail. Topic is a validated JsonString view: unescaped Unicode borrows input,
while escaped text decodes later into caller scratch. Raw metadata may be any
validated JSON value. Missing-field checks precede value conversion in the fixed
Id/Topic/RawMeta order. Errors retain exact global record/line/byte coordinates.

`read_event` advances one ordinal only after success. `project_into` projects all
remaining records into caller-initialized parallel slices of IDs, optional topic
views and optional raw Nodes. It validates count, every capacity and every row,
and prepays both schema passes plus column writes before the first write. Every
returned error preserves all slices and the cursor. Success changes only the
returned prefix, preserving suffixes even when slice lengths differ. There is
no allocated typed row vector, unsafe or uninitialized column output.

Run the example with its small self-checking corpus, or supply bounded stdin:

```sh
cargo run -p vesper-serde --example ndjson_columns
cargo build -p vesper-serde --example ndjson_columns
python3 scripts/check-ndjson-columns.py --binary target/debug/examples/ndjson_columns
```

The example accepts `--stdin`, limited to 64 KiB and 128 records. It projects
actual columns, checks typed cursor agreement and reencodes them through the
existing scalar codec. Python supplies an independent corpus and verifies exact
u64 IDs, Unicode strings and nested raw metadata; malformed batches and later
schema errors emit no records. Encoding/parser work remains under unchanged
finite codec limits. Source derives/Region typing, linear construction, actual
TiledSeq/aligned AoSoA allocation, incremental chunks, SIMD/native backends,
zero-total-heap operation and performance/formal-proof guarantees remain outside
this slice.
