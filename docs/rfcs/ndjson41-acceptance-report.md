# Independent validated NDJSON column acceptance (#213)

The reviewed `docs/rfcs/ndjson41-contracts.md` contract and public claim #213
preceded all writes. Owned files are only the new roundtrip tests, example,
Python harness, `crates/vesper-serde/NDJSON.md` profile and this report. No shared
codec, allocator, Cargo/dependency, BUILD, workflow or wrapper changes are owned.

Dependency copies `b0553c4` → `e61ab75` (batch) and `af30295` → `888b752`
(columns) are not acceptance work.

Six independent Rust tests exercise the actual batch, typed cursor, parallel
column projection and existing scalar encoder/parser:

- LF/CRLF and optional final terminator, retained bare CR JSON whitespace,
  strict blank-line rejection, rejected multiline records, empty batches and
  physical EOF line/record/byte coordinates. Public local-error relocation
  accepts legitimate payload EOF and rejects offsets outside the record.
- Actual mixed Unicode records with escaped-equivalent schema keys, exact
  u64::MAX and an integer above binary64's exact range, scratch-decoded topics,
  unknown validated fields, arbitrary nested raw metadata, scalar reencoding
  and a second actual batch/typed-cursor read. Unequal output slice lengths and
  every unused suffix slot remain valid and unchanged.
- Later malformed unknown values, escaped duplicate keys, bad surrogate escapes
  and invalid UTF8 prevent batch publication. Error kinds and absolute offsets
  are compared against real scalar-parser errors after a Unicode prefix.
- Later missing/type/range failures preserve every prepopulated ID/topic/raw
  column slot and the initial cursor. Missing-field precedence and original
  record/line/byte positions are exact. Single-row reads can advance through the
  valid prefix, then fail without invalidating earlier borrowed views.
- Exact declared one- and two-row work boundaries prepay both schema passes,
  three slot writes per row and one cursor commit. A work allowance sufficient
  for one row cannot replenish for another. Count/capacity/configuration errors
  preserve all columns/cursor, including EOF configuration coordinates.
- Aggregate input/record/metadata/token/index/work caps cover the whole batch.
  Exact successful actual stats reproduce the same admission; insufficient
  aggregate budgets refuse rather than resetting per record. Tests retain the
  contract's distinction between component errors and clamped scalar Json errors.

The bounded `ndjson_columns` example defaults to three self-checking records and
accepts `--stdin` with 64 KiB/128-record limits. It validates the complete batch,
projects actual caller-owned columns, compares typed-cursor views, checks output
sizes and reencodes each record through the existing scalar codec. Initialized
column and byte sentinels check suffix preservation. It does not allocate a
typed row vector or claim a total absence of host/parser allocations.

The independent Python harness supplies 96 generated rows with exact u64 IDs,
escaped/unescaped Unicode, control characters, combining text, unknown fields
and nested raw metadata. It checks decoded types recursively, preserving u64
precision and float bits inside raw metadata. Two actual output runs must be
byte deterministic; LF/CRLF input and omitted final terminator produce the same
records. Malformed later rows, duplicate unknown keys, UTF8/escape failures,
schema errors and example caps require nonzero exit with no output records.
Processes and captured input/output have finite limits; `--binary` selects the
actual built executable.

Actual Linux validation in exclusive `/tmp/ndjson41-acceptance-target`, one
Cargo job/debug0/incremental0:

- All crate targets pass 73 tests across 11 suites, zero failures or ignores:
  six new independent acceptance tests and 67 existing JSON/float/binary tests.
- Both existing README Rust doctests pass.
- Strict crate all-target Clippy with `-D warnings`, workspace formatting,
  Python syntax and whitespace checks pass.
- The actual example and independent Python oracle pass 15 processes, including
  96 oracle rows producing 8,006 encoded bytes per full corpus run.

No runtime assertion, diagnostic expectation, default or production limit needed
to change after actual execution. Source derive/Region typing, linear resource
construction, actual TiledSeq/aligned AoSoA allocation, incremental chunks,
SIMD/native integration, zero-total-heap operation, throughput/RSS guarantees,
formal proof, BuildBuddy and other-platform execution are outside this evidence.
The coordinator separately integrates peer tests and performs protected review.
