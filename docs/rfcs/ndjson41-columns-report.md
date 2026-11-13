# Transactional EventEnvelope columns (#212)

The published reviewed contract and public ownership claim preceded writes.
Owned files are only `vesper-serde/src/event_columns.rs`, its new
`tests/event_columns.rs` and this report. No shared parser/encoder/helper/float,
module registration, dependency, build/CI/wrapper or allocator files changed.

`read_event` and `project_into` consume actual immutable `NdjsonBatch` records.
Their envelope retains exact u64 semantics, a validated `JsonString` topic view
and a borrowed validated raw metadata Node. Required presence is checked in
fixed Id/Topic/RawMeta order before any conversion; ID conversion precedes topic
type validation. Unknown fields remain allowed only because Batch construction
already checked their complete JSON syntax, UTF8, escapes and duplicate keys.

Each schema pass scans object fields once and performs three decoded fixed-key
comparisons per field under the frozen single raw-key work charge. Escaped keys
match without allocating decoded Strings. No repeated Node.field calls refresh
independent allowances. Type/range errors preserve actual value offsets through
RecordRef relocation; missing fields identify their record's payload start.

Single-row reading validates configuration and schema under the finite allowance
and prepays the cursor commit before advancing one ordinal. Batch projection
first validates configuration, ALL remaining row count and each output capacity,
then checks every row and prepays both complete schema passes, three slot writes
per row and the final commit. It retains only one ephemeral stack row; no typed
AoS staging vector is allocated.

The second immutable pass shares the same fixed-key accumulator and deterministic
conversions, with no new fallible work checks or returned errors after the first
write. No public callback, mutable source or user-supplied approval participates.
Safe borrowed views prevent the Batch/input from changing between passes. The
cursor commits once to its prevalidated end after filling all three prefixes.
Every returned error preserves every initialized column byte/value and cursor;
successful writes preserve all suffix slots even when slice lengths differ.

ProjectionLimits retains the exact positive default/HARD values from the
contract. Standalone configuration errors use the initial batch origin; cursor
operations relocate them to their current record or actual physical EOF. Empty
remaining input returns zero after configuration validation. Count errors precede
capacity errors. No existing allowance was raised.

Actual validation in exclusive `/tmp/ndjson41-columns-target`, one Cargo job,
debug0/incremental0:

- `cargo test -p vesper-serde --locked`: all80tests across seven nonempty suites
  pass with zero failures or ignores: 11new columns tests and69existing JSON,
  binary/archive, finite-float and doctest cases.
- New actual Batch tests cover escaped-equivalent keys, every missing-field
  subset and fixed precedence, exact ID/type/range errors and global offsets,
  u64::MAX, borrowed Unicode and explicit scratch decoding, all raw metadata
  kinds, later-row failures, initialized Some/None sentinels and exact pointer/
  content preservation, separate short columns, mismatched successful suffixes,
  configuration origins, EOF and projection after a prior raw read.
- Independent frozen-policy work boundaries are exercised: a simple row's
  schema costs1303units; read_event needs1304 including commit; two-row
  projection needs5219 including both schema passes, six slot writes and commit.
  Exactly sufficient allowances succeed; one less refuses before any write and
  identifies the real failing record. Escaped/unknown key widths share the same
  finite schema stage rather than fresh lookup budgets.
- Strict `cargo clippy -p vesper-serde --locked --all-targets -- -D warnings`,
  package formatting and whitespace checks pass.

Actual Batch source `b0553c4` was imported as dependency-only cherry-pick
`9140771`, not column-owned work. Root integrates the Batch lane's independent
tests and actual executable/Python acceptance separately; these local results do
not stand in for hosted platform evidence. No source derive, Region typing,
linear construction, aligned TiledSeq/AoSoA allocator, incremental streaming,
zero-total-heap, SIMD/native, timing/RSS or proof claim is made. Ownership remains
reserved through reviewed merge.
