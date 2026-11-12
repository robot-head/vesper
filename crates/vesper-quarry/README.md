# Quarry host SQL and page core

`vesper-quarry` is a dependency-free, bounded host library implementing the catalog,
query checking, relational optimization, reference row execution, and recoverable
slotted-page/pin slices of specification §24. It performs no I/O. It is **not** an embedded transactional
storage engine, source `@sql` macro, WASI component, or compiler integration.

Run the self-checking example from the repository after workspace registration:

```sh
cargo run -p vesper-quarry --example quarry
```

Before registration, use `cargo run --manifest-path crates/vesper-quarry/Cargo.toml
--example quarry` (on one line). The example prepares a real SQL query, executes
it before and after optimization, checks exact rows and outer-join nullability,
and prints logical work counters. These counters are not timing benchmarks.

## API

1. Replay explicit SQL strings with `HermeticCatalog::apply_ddl(sql, limits)`.
   A whole call succeeds atomically or leaves the catalog unchanged. Tables are
   ordered by normalized name; duplicate tables and columns are errors.
2. Call `catalog.prepare(sql, parameter_definitions, limits)` to obtain an opaque
   `PreparedQuery`. Column identities include binding ID, global slot, alias, and
   column name. Repeated use of the same table requires distinct aliases.
3. Create `DataSet::new(&catalog)` and supply borrowed rows with
   `set_rows(table, &rows, limits)`. This validates scalar types, nullability, widths, and input budgets
   before cloning and replacing existing rows. Every failure preserves both the
   stored dataset and the caller's input rows for recovery/retry. Empty tables must
   be supplied explicitly.
4. Call `query.execute(&catalog, &data, &binds)`. Bind maps use normalized lowercase
   names without `:` and must exactly match the declared parameters. Null is
   accepted only for nullable declarations; all f64 values must be finite.
5. `query.optimized()` returns a new checked query. `plan()`, `columns()`,
   `parameters()`, and `output()` expose read-only inspection. Public logical plan
   values cannot be used to manufacture an executable prepared query.

Prepared queries and data retain an exact catalog snapshot, including its
monotonic revision and full schema content. Execute compares both, so it rejects
schema changes, same-revision different catalogs, and drop/recreate cycles.
Unrelated schema changes also invalidate plans deliberately. This uses exact
bounded content comparisons instead of a collision-prone homemade hash.

## Explicit grammar

Keywords and unquoted identifiers are ASCII case-insensitive. Identifiers match
`[A-Za-z_][A-Za-z0-9_]*`, have at most 128 bytes, and cannot use reserved SQL
keywords. Text literals preserve UTF-8 and use doubled single quotes (`'O''Neil'`).
Whitespace is ASCII. Comments, quoted identifiers, and backslash escapes are not
part of this dialect.

```text
migration := (CREATE TABLE ident '(' column (',' column)* ')'
            | DROP TABLE ident) (';' migration)? ';'?
column    := ident scalar_type (NULL | NOT NULL)?
scalar_type := U64 | I64 | BIGINT | F64 | DOUBLE | BOOL | BOOLEAN
             | TEXT | BYTES | BLOB

query     := SELECT selection (',' selection)* FROM table
             (join_kind? JOIN table ON expr)* (WHERE expr)? ';'?
table     := ident ((AS)? ident)?
join_kind := INNER | LEFT OUTER? | RIGHT OUTER? | FULL OUTER?
selection := '*' | expr (AS ident)?
expr      := expr OR expr | expr AND expr | NOT expr
           | atom compare atom | atom IS NOT? NULL | atom
atom      := ident | ident '.' ident | ':' ident
           | signed_i64 | string | TRUE | FALSE | NULL | '(' expr ')'
compare   := '=' | '<>' | '!=' | '<' | '<=' | '>' | '>='
```

Precedence is parentheses, comparison/IS NULL, NOT, AND, OR. Comparisons do not
chain. No arithmetic or implicit casts are provided: comparison operands must
have exactly the same scalar type, with contextual typing only for NULL.
Integer literals are checked i64; u64/f64/bytes values are supplied through typed
parameters or tables. Non-column projections require `AS name`. A bare NULL
projection is rejected because it has no inferable scalar type. All resulting
output names must be unique (use `AS` to disambiguate). `*` expands the current
binding columns in FROM order and follows the same duplicate-name rule.
Parameters require explicit `ParamDef` types/nullability; duplicate, unknown, and
unused declarations are errors. Dotted parameters such as `:grant.org_id` are
unsupported; extract the host value and bind `:org_id` explicitly.

No aggregates, functions, casts, arithmetic, DISTINCT, ORDER BY, LIMIT, OFFSET,
GROUP BY, HAVING, UNION, subqueries, IN, LIKE, CROSS/NATURAL JOIN, USING, qualified
stars, INSERT/UPDATE/DELETE, ALTER, DDL constraints/defaults/keys, or implicit
transactions are implemented. Trailing or multiple query statements fail.

## Relational and NULL semantics

The checked AST stores typed column/parameter/literal nodes and Boolean
operators. No raw SQL predicate string survives checking. Dependencies are
computed by walking actual checked column-reference nodes, never supplied by a
caller. Output types and conservative nullability are inferred from expressions,
base declarations, and null extension at every left/right/full outer join.
WHERE does not refine output nullability.

The optimizer splits only AND predicates and pushes single-side selections into
scans, including across nested inner joins. A WHERE predicate crosses a left
outer join only into its preserved left side, a right join only into its
preserved right side, and never crosses a full join. Predicates on nullable sides
remain above the join even when they are `IS NULL` or null rejecting. OR is not
distributed. ON predicates are not moved. No join reordering or projection
pruning is claimed.

The executor materializes bounded host rows, performs nested-loop joins,
retains duplicate rows (bag semantics), null extends unmatched rows, and evaluates
SQL three-valued Boolean logic. WHERE/ON retain only TRUE: FALSE and UNKNOWN are
rejected. NULL never equals NULL in a join. Text comparisons use UTF-8 byte order;
bytes use lexicographic byte order; Boolean ordering is false before true. f64
comparison uses finite IEEE values, with +0 and -0 equal. Without ORDER BY, SQL
result order is unspecified; this implementation emits deterministic source and
left-major join order followed by unmatched right rows. Supported rewrites retain
that order as tested.

## Bounds and failure behavior

Limits may be lowered to zero, but cannot exceed these hard ceilings:

| Resource | Ceiling |
| --- | ---: |
| SQL bytes per operation | 65,536 |
| Lexed tokens | 8,192 |
| Parsed expression nodes / DDL statements / bind definitions / expanded outputs | 512 each |
| Recursive parentheses and NOT depth | 32 |
| Catalog tables | 64 |
| Columns per table | 128 |
| Query FROM bindings (including self-joins) | 16 |
| Total data rows | 10,000 |
| Total data/parameter bytes | 4,194,304 |
| Logical execution work | 1,000,000 |
| Cumulative intermediate candidate rows | 10,000 |
| Cumulative intermediate bytes | 8,388,608 |
| Output rows | 10,000 |
| Output bytes | 4,194,304 |

Byte accounting includes `size_of::<Value>()` per cell, vector header sizes, and
text/blob payload lengths. Input row caps are checked before walking incoming
row contents. Parameters share the execute-time input byte budget. Work charges
validation cells, expression visits/leaf payload bytes, candidate-pair width, and
materialization width/payload. Candidate rows rejected by ON or scan predicates
still consume intermediate/work budget. Filters move existing rows. Intermediate
budgets count cumulative production, which bounds live retained materializations
conservatively; no limit assumes all candidates match. A bounded temporary row
may be allocated before checking its cumulative charge. String/vector allocator
capacity and allocator overhead are not exact byte-accounted memory metrics;
hard structural limits additionally bound these overheads. Host callers own
already-allocated incoming rows/strings; this library cannot limit allocations
made by its caller before an API call.

Every resource counter and catalog revision increments with checked arithmetic.
A failure returns a categorized error, produces no partial query result, and
leaves catalog/data unchanged. `set_rows` borrows its input, so failed
updates also preserve the caller's supplied rows. Optimization can change logical
work and candidate materialization totals, so a low-budget execution may fail in one form and
succeed in another; differential row equivalence is asserted when both complete.
Default join budgets intentionally reject large Cartesian products.

Tests cover expected results, all nullable Boolean truth-table combinations,
bag joins, multi-join nullability, typed scalar/bind validation, stale catalogs,
atomic failures, unsupported SQL, resource exhaustion, and 3,584 before/after
optimizer comparisons (64 nullable input subsets × four joins × 14 predicates).
No source-mode safety, allocation-free execution, SIMD/AoSoA, egglog integration,
WAL/ARIES/MVCC, crash recovery, tenant capability enforcement,
network/database service, or Lean proof is provided by this host slice.


## Slotted pages and recoverable buffer-pool pins (§24.2)

The independent `page` and `pool` modules add actual in-memory page operations.
They do not connect SQL rows to a disk format. Run the self-checking lifecycle:

```sh
cargo run -p vesper-quarry --example quarry_pages
```

`page::SlottedPage` owns exactly 16,384 bytes, with no Rust-struct wire casts.
`SlottedPage::decode(&bytes)` validates size, CRC32C, supported header values,
and the complete slot layout before copying borrowed input. CRC32C uses the
Castagnoli reflected polynomial `0x82f63b78`, initial/final XOR `0xffffffff`,
and treats checksum bytes 16–19 as zero. It covers every other byte, including
unused space. This is portable software CRC, not an authenticated hash or a
hardware-acceleration claim.

| Header byte offset | Field | Little-endian width |
| --- | --- | --- |
| 0 | page_lsn | u64 |
| 8 | page_id | u64 |
| 16 | checksum_crc32c | u32 |
| 20 | slot_count | u16 |
| 22 | free_space_offset | u16 |
| 24 | page_kind_tag | u8 |
| 25 | level | u8 |
| 26 | flags | u16 |
| 28 | right_sibling_page | u32 |

Kind 1 is the supported opaque-record slotted format; level and flags must be
zero. The sibling ID is stored metadata. No tree traversal or PAX interpretation
is implemented. Slots start at byte 32 and each encode `(offset: u16le,
length: u16le)` in four bytes. The directory grows upward and record payloads
grow downward. The free interval is `[32 + 4*slot_count, free_space_offset)`.
Every record is nonempty, wholly within `[free_space_offset, 16384)`, and disjoint
from all other records. The free offset equals the minimum live record offset
(or 16,384 for an empty page). Interior payload holes are accepted and reclaimed
on update; no slot may overlap the directory or declared free interval.

`new(page_id, sibling_id)`, `insert(&record)`, `read(slot)`, and
`update(slot, &record)` operate on real bytes. Slot indices survive updates.
Updates preflight total packed capacity, build and compact a fresh page, then
replace the old page only on success. Inserts preflight their directory/payload
ranges before writing. Mutation failures leave exact prior bytes and borrowed
record inputs intact. Raw mutable page bytes are never exposed. Maximum record
size is 16,348 bytes, with at most 3,270 one-byte records. Insert/update each copy
at most one page's payload; checksum work is fixed at 16,384 bytes × eight CRC
steps. Decode sorts at most 3,270 checked ranges, avoiding unbounded or quadratic
slot-overlap scanning. Page buffers, range scratch, and update staging allocate
on the host heap.

`pool::BufferPool::new(PoolLimits { frames, pins })` permits 1–256 resident frames
and 1–4,096 total pins (defaults 16/256). No implicit eviction occurs. Each frame
holds one page image and either multiple distinct shared pins or one exclusive
pin. Every pin carries a private pool identity, frame slot, slot generation,
page ID, and active-pin serial. Checked u64 identity/generation/serial exhaustion
returns an error. Nonclone handles have `#[must_use]` diagnostics; compile-fail
doctests verify the missing Clone implementation.

- `install(&PageImage)` borrows/copies a validated image and rejects full pools
  and duplicate page IDs without changing either input or resident frames.
- `fix_shared(id)` / `view_shared(&pin)` provide committed-page reads.
  `unpin_clean(pin)` consumes a shared pin. This does not clear dirty metadata.
- `fix_exclusive(id)` creates a private staged copy. `insert(&mut pin, record)`,
  `update(&mut pin, slot, record)`, and `view_exclusive(&pin)` access that stage.
  Readers and other writers conflict until release.
- `unpin_dirty(pin, wal_lsn)` requires changed bytes, a nonzero LSN, and strict
  advancement over the committed page LSN. It updates header bytes/checksum,
  publishes the stage, and releases the pin atomically. The frame's dirty LSN is
  the first dirty commit LSN; later commits advance page LSN and retain that
  earliest dirty LSN. LSN `u64::MAX` permits no further advancement.
- `unpin_aborted(pin)` discards staging and releases the exclusive pin. Committed
  bytes, page LSN, and prior dirty LSN remain exactly as before the write.
- All release errors return `ReleaseError { error, pin }` with the original
  supplied owner/stage intact for retry or abort. Read/stage operations borrow
  handles and preserve them on error. Wrong-pool, stale-generation, and inactive
  serial checks never release a different live pin.
- `evict(id)` refuses every pinned frame and returns an owned `PageImage`,
  including its dirty metadata. Reinstalling that image preserves the dirty LSN.
  It neither writes nor acknowledges persistence. `PageImage::clean(page)` is an
  explicit caller designation for an initial clean image; no writeback or
  dirty-acknowledgment operation is implemented.

Resident page bytes total at most 4 MiB per pool. Exclusive staging adds at most
another 4 MiB, plus bounded pin/frame metadata and temporary one-page compaction
buffers. Caller-owned images/clones and multiple independently created pools
are outside this per-pool bound. Pin handles can be moved, but private fields
prevent safe Rust callers from cloning/forging them. Internal adversarial tests
construct stale keys to exercise runtime defenses that safe public usage cannot
otherwise manufacture.

Rust callers can still drop/forget handles or tear down a pool with live pins.
Dropping a pin does not silently unpin: that frame stays unavailable until pool
teardown, deliberately favoring retained ownership/state over unsafe reuse.
Maintain the pool while its pins are live and explicitly commit/abort/release.
This is a host protocol, not Vesper compile-time linear exhaustion, automatic
cancellation recovery, latch-free optimistic traversal, zero-copy DMA, io_uring,
durable WAL/ARIES, MVCC, B+Tree storage, tenant isolation, or a formal pin proof.
LSN checks validate numeric ordering only; they do not prove a WAL record exists
or reached durable storage.
