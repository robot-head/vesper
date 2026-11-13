# Validated NDJSON and atomic caller columns (#210–213)

Public issue/file claims, actual external-PR overlap checks and reviewed frozen
contractb078031 preceded three writers in separate worktrees. The coordinator
registered empty development modules first; coherent source then supplied the
actual public APIs. This branch follows float204 and imports landed mainb130f50,
preserving both the float profile and owner193 CI changes without conflicts.
No existing index/encode/helpers/float, allocator, Cargo/dependency, BUILD/workflow,
compiler/core AST or CLI implementation is changed by this feature.

NdjsonBatch validates every LF-framed record before returning any batch or view,
with strict blank-record policy, optional final LF/CRLF and exact original
record/line/byte coordinates. A paired CR is framing; other CR remains ordinary
JSON input. All JSON values are permitted at this stage, including unknown nested
values, whose syntax/UTF8/escapes/decoded duplicate keys are completely validated.
Record metadata is admitted before its sole fallible reservation. Each parser
call receives only its remaining aggregate token/index/work allowance, clamped
by per-record guards; budgets do not restart per line. Stats expose retained
sums and logical framing/metadata/parser work.

EventEnvelope projection requires exact u64 id, a validated topic string view
and arbitrary raw_meta Node. Required presence precedes value conversion in a
fixed order. One field scan performs three prepaid decoded-key comparisons;
escaped topics decode later into caller scratch. Typed reads commit their cursor
only after success. project_into validates all remaining schemas and capacities,
prepays both deterministic schema passes plus column-slot writes and commit,
then writes only the returned prefix of initialized caller slices. Every returned
error preserves all three complete columns, suffixes and cursor. There is no
allocated typed AoS staging, unsafe output or fresh budget in the write pass.
Views borrow the immutable batch/input; cursor advancement does not invalidate
returned data. Public JSON-error relocation is diagnostic, never an approval.

Actual coordinator Linux validation of the complete integrated feature:

- `cargo test --locked -p vesper-serde --all-targets`:97tests across13targets,
  zero failures or ignores (67 existing JSON/binary/float,13 batch,11 columns,
  6 independent acceptance). Both existing README doctests pass.
- Strict all-target crate Clippy, workspace formatting and whitespace checks
  pass. The actual ndjson_columns example builds and runs its self-checking
  three-row corpus, preserving u64::MAX,2^53+1, Unicode and raw nested metadata.
- The actual example plus independent Python oracle pass15 bounded processes.
  Its96 generated rows produce8,006 encoded bytes per complete corpus run;
  independent JSON decoding verifies exact integer types/values, Unicode and
  nested raw metadata. Repeated output is deterministic, LF/CRLF/omitted final
  terminators agree, and later malformed/schema/quota inputs emit no records.
- Independent tests exercise late-record validation, exact global Unicode/EOF
  errors, diagnostic-offset bounds, all framing policies, actual aggregate caps
  and no per-line budget reset. Prepopulated Some-valued columns, mismatched
  capacities, suffixes and cursor state survive every returned schema/quota
  error. Exact one/two-row boundaries cover both schema passes and all writes.
- A separate read-only source reviewer found no actionable framing, budget,
  lifetime, schema-ordering or failure-atomicity issue. No runtime assertion,
  default, limit or production behavior needed to change after acceptance.

Owned leaves:b0553c4/396d906/226cf55(batch),af30295/5a20976(columns),
3d56f75(independent acceptance). Dependency-only copies were not reimported as
authored work. Coordinator logs /tmp/ndjson41-root-{final,doc,clippy,python,
example-build,example}.log record actual execution; final exit marker is zero.

This is an allocating scalar host batch with one bounded index per record and
record metadata, plus caller-owned parallel columns. It is not constant-memory
streaming, source derives/Region typing, linear construction/rollback, actual
RegionTiled2/TiledSeq ingestion, aligned AoSoA allocation, SIMD/native lowering,
zero-whole-process allocation, throughput/RSS guarantees or formal proof. The
separate tiled215 foundation remains independent. Logical admission units are
not CPU instructions or elapsed-time bounds. Hosted/platform results and
protected parent/child merge remain separate from the local evidence above.
