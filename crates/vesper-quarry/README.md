# Quarry host SQL, page core and durable store

`vesper-quarry` is a bounded host library implementing the catalog, query
checking, relational optimization, reference row execution, vectorized
execution (single-table plans and hash/nested-loop joins, with AoSoA tile
output), recoverable slotted-page/pin slices, a durable write-ahead-logged
record store, and a transactional B+Tree table store with MVCC snapshot
isolation (`store::Store`) of specification §24. Its only dependency is
`vesper-simd` (for the selection-mask counting kernel). It is **not** a source
`@sql` macro, WASI component, or compiler integration: source `@sql` remains a
Forge-macro step, and nothing here is reachable from Vesper source yet. The
older `durable::Db` holds opaque records; `store::Store` holds typed SQL tables
and runs `PreparedQuery` plans over committed snapshots.

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
No source-mode safety, allocation-free execution, AoSoA tiles, egglog
integration, MVCC, tenant capability enforcement, network/database service, or
Lean proof is provided by this host slice. The vectorized executor and the
durable store are described below.


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
MVCC, B+Tree storage, tenant isolation, or a formal pin proof. On its own the
pool's LSN checks validate numeric ordering only; the `durable` module below is
what ties page LSNs to WAL records that reached the device.

## Durable store: file-backed pages, WAL recovery and checkpoints

`durable::Db<D: device::Device>` stores opaque records (up to 16,347 bytes) in
the slotted pages above, through the same `BufferPool` pin protocol, over a
data file and a write-ahead log. Run the self-checking crash example:

```sh
cargo run -p vesper-quarry --example quarry_durable
```

### Devices and the crash model

`Device` is positional read/write/set_len/sync over two files. `FsDevice`
uses `std::fs` (`quarry.data`, `quarry.wal`; `sync` is `File::sync_all`; on
Unix the store directory and the parent of every directory the open created
are synced). It holds an exclusive `File::try_lock` on `quarry.lock` for its
lifetime, so a second open of the same directory fails with
`DeviceError::Locked` instead of two stores writing one log. `SimDevice` is a
deterministic crash/fsync model: writes are visible at once and durable only
after `sync` of that file. `crash(choice)` returns the medium after a power
loss: `LoseUnsynced`, `KeepUnsynced`, or `Subset { seed }` (a seeded subset of
the unsynced operations of each file survives in order, and the last surviving
write per file is torn at a seeded byte). `crash_after(n)` makes the
`n+1`-th mutating operation fail with `Crashed`. This models a page cache; it
says nothing about whether a real filesystem, kernel or disk honours `fsync`.
Quarry does not reuse `vesper-sim-session`'s `wal-frame-v1` model: that model
logs checked source values for the CEK evaluator, not byte pages.

### Log format

The log starts with two 32-byte header slots (`QRYWAL01`, generation, base LSN,
CRC32C); the valid header with the larger generation wins. Frames follow from
byte 64: `[len u32][lsn u64][crc u32][payload]`, CRC32C over generation, length,
LSN and payload. LSNs are consecutive record numbers from the base. Scanning
stops at the first torn header, torn payload, oversize length, bad checksum or
out-of-sequence LSN (`wal::TailDefect`), and recovery durably cuts the file
there before writing anything. A frame that checks but does not decode is
`Corrupt`, not a tail. Payloads are at most 32 KiB + 64 bytes, enough for an
update carrying two maximum-size images. A log longer than its two header
slots with no valid header is `Corrupt` (initialization writes only the
headers), so logged commits are never reinitialized away. Binding the generation into each frame's CRC makes
frames left over from before a truncation invalid even if their bytes survive.

Records (`wal::LogRecord`): `PageImage` (full page before its first change
after a checkpoint, or a new page), `Insert`, `Update` (before and after slot
images), `Compensation` (redo-only, with `undo_next`), `Commit`, `End`.

### Protocol

- **Steal / no-force.** Changes go to an exclusive pin's staged page; the
  record is appended and the pin released dirty at its LSN
  (`unpin_dirty(pin, lsn)`). Every failure releases the pin with
  `unpin_aborted`; `stats().pinned` is 0 after every call, including
  corrupt-page and full-page errors (tested). Eviction is LRU over unpinned
  frames; a dirty victim is written after the log is flushed through its page
  LSN (the WAL rule), so uncommitted pages may reach disk.
- **Torn pages.** A page's first change after a checkpoint logs its full image
  first (as PostgreSQL's full-page writes do). Redo restores that image over a
  missing, checksum-failing or older page, then replays. A corrupt page that
  no logged image covers is reported as `Corrupt`, never repaired silently.
- **Commit** appends `Commit` and syncs the log; `Ok` means durable.
  **Abort** undoes the transaction's changes newest first, logging a
  `Compensation` per change and then `End`; it does not sync.
- **Recovery** (`Db::open`): cut the log tail; redo every page change whose
  LSN is newer than its page (repeating history, losers included); roll back
  every transaction without `Commit`/`End`, newest change first across all
  losers, resuming partially undone ones from their last compensation's
  `undo_next`; then checkpoint. Recovery is crash-safe and is tested by
  crashing it at every one of its own device operations.
- **Checkpoint** (sharp): requires no active transaction; writes back every
  resident page, syncs the data file, writes the other header slot with the
  next generation and base LSN, syncs, truncates the log to its headers and
  syncs. A crash in between leaves either the old header (old frames replay
  idempotently against durable pages) or the new one (old frames fail the
  generation check).
- **Concurrency.** Up to 64 transactions; a page changed by an active
  transaction is locked to it until commit/abort (no-wait: `Conflict`).
  Readers of a locked page get `Conflict` (no dirty reads, no MVCC). This
  page-granular exclusion, plus per-page rollback accounting (an insert is
  placed only where the page, fully rolled back, still has room for the
  insert's slot and one-byte tombstone), is what makes undo always fit.
- **Log space.** Each change reserves room for its compensation and each
  transaction for its `Commit`/`End`, so `LogFull` is reported before a
  rollback could run out of log. Recovery may append compensations past the
  limit (bounded by the records it undoes).
- **Failures.** Any I/O, crash or corruption error inside a call poisons the
  handle (`State` afterwards); reopen to recover. A commit that fails with an
  I/O error has an unknown outcome until reopening.
- **Owners.** `Txn` is non-Clone and `#[must_use]`; `commit` and `abort` are
  its only exits. A dropped `Txn` is counted (`leaked_txns`), keeps its locks
  and blocks checkpoints until the store is reopened, whose recovery rolls it
  back. This is Rust ownership plus a counter, not Vesper's source-level
  linear typing.

Limits (`DbLimits`): 2–256 frames, up to 65,536 pages (1 GiB), 64 KiB–256 MiB
of log, 1–64 active transactions. Deletes leave tombstones and slots are never
reused or compacted away; there is no free-space map beyond the transaction's
last page and the last page of the file, no B+Tree, no secondary index, no
fuzzy checkpoint, no group commit, no `O_DIRECT`/io_uring, and no hardware
CRC32C (a table-driven software CRC is used).

### Crash-point testing

`tests/durable.rs` runs a seeded workload (two interleaved transactions,
inserts, updates, deletes of 1–3 KiB records, commits, aborts and checkpoints
over a three-frame pool) and then, for **every** device operation index of the
run, reruns it crashing there and recovers under four crash choices. The
recovered contents must equal the state after exactly the acknowledged
commits, or after one more when that commit was in flight. Two seeds give 270
crash points and 1,080 recoveries; the test also asserts that the enumeration
exercised page-image repair, redo, loser rollback, resumed rollback and torn
log tails. A second test crashes recovery itself at each of its device
operations for three crash points. Disabling page-image repair, loser undo or
the WAL rule each makes the test fail.

## Vectorized execution

`vector::VectorEngine` plus `PreparedQuery::execute_vectorized` and
`PreparedQuery::aggregate_vectorized` run single-table plans (a scan with its
pushed predicates and any filters above it) over column batches (default
1,024 rows, configurable 1–65,536). Each batch transposes the table's columns
into typed vectors with byte null masks; predicates evaluate as whole-batch
loops into three-valued byte masks (Kleene AND/OR/NOT, IS [NOT] NULL, NULL
constants), and comparisons of fixed-width columns against constants or
columns run as straight slice loops. The selection mask is counted by the
`vesper-simd` Baseline128 JIT byte scanner when `VectorEngine::detect()` finds
it (`simd_active()`), otherwise by a portable count; the count sizes the
selection vector and answers `COUNT(*)`/`COUNT(col)`. Rustc auto-vectorization
of the comparison loops is not claimed and no speedup was measured.

Results (rows, order and column definitions) equal the row executor's;
`work` counts batch cells and is not comparable.

Plans with joins (`src/vector/join.rs`) materialize each input as a column
relation. An inner, left, right or full join whose ON clause has equality
conjuncts between the two sides builds a hash table on the right side's keys
(`-0.0` and `0.0` hash alike; NULL keys never match) and probes it from the
left in row order; the remaining ON conjuncts run as a vectorized residual
over batches of candidate pairs. Without an equality conjunct the join is a
nested loop over pair batches. Outer joins null-extend in the row executor's
order (left-major, then unmatched right rows). Pair and output budgets
(`intermediate_rows`, `work`) are enforced as in the row executor; scratch
vectors are reused across batches.

`PreparedQuery::execute_tiled` writes the result into a reusable
`tiles::TileSet` (§24.3 `tiled(16)`): 16-row tiles, each fixed-width column a
contiguous `[u64; 16]` lane array (i64/f64 bit patterns, booleans 0/1), text
and bytes as `(offset, len)` lanes into one arena, and a 16-bit null mask per
column. Re-executing into a warmed set of the same shape allocates no tiles
and grows no arena (asserted by `allocated_tiles`/`arena_capacity`, not by a
counting allocator, so no zero-allocation claim is made). Aggregates (`vector::Aggregate`): `CountRows`,
`Count`, `Sum` (U64/I64 exact, `Overflow` iff the exact total does not fit;
F64 in row order), `Min`, `Max` (first wins among equals), with
`vector::aggregate_rows` as the row-at-a-time reference. SQL has no aggregate
syntax yet, so aggregates are host calls over a query's output columns.

`tests/vector.rs` compares 39 predicates × 5 projections, unoptimized and
optimized, under five engine/batch configurations (including batch size 1 and
7), against the row executor; and 16 aggregates for every predicate against
`aggregate_rows`.

`tests/vector_join.rs` compares 7 ON clauses × 6 WHERE clauses × 4 join kinds
over three seeded datasets, unoptimized and optimized, under four engine/batch
configurations (batch 1, 7, 64 and the default), for both `execute_vectorized`
and `execute_tiled` (8,064 comparisons), plus three-way, self and outer-chain
joins with aggregates, tile layout and reuse, and join budgets.

## Transactional table store (`store::Store`)

`store::Store<D: Device>` keeps SQL tables in B+Trees over 16 KiB durable
pages, on the same `Device` crash model as `durable::Db`, with its own WAL.

* **DDL and catalog.** `execute_ddl` applies `CREATE TABLE`/`DROP TABLE`
  through `HermeticCatalog` and persists the catalog, table roots and counters
  in the meta page (page 0). DDL requires no open transactions.
* **Pages.** Internal pages are fixed key/child arrays. Leaves use a PAX
  layout: per column a null bitmap and fixed-width data (or `(offset, len)`
  into a heap growing down from the page end), plus a key array of versions
  `(rowid, !commit_ts)` and a tombstone bitmap. A free-space bitmap (page 1)
  records free pages; freed pages and leaf slots are reused, and leaf heaps
  are compacted when tombstoned or collected versions leave garbage. Rows are
  at most 4,096 encoded bytes.
* **MVCC.** `begin` takes a snapshot at the newest durable commit. Writes are
  buffered per transaction (deferred update, no steal) under no-wait
  record-level locks on `(table, rowid)`; a lock held by another transaction,
  or a committed version newer than the snapshot, is `Conflict` (first
  committer wins). This is snapshot isolation: write skew is allowed, and
  `tests/store.rs` checks that dirty writes, dirty reads, non-repeatable
  reads, lost updates, phantoms and read skew do not occur while write skew
  does. A version is collected once no
  open snapshot can see it.
* **Commit groups.** A commit stages copies of the pages it touches, applies
  the tree operations to the copies, appends one log group (page images on
  first touch after a checkpoint, otherwise byte diffs, then `Commit`) and
  only then installs the pages. `commit` syncs; `commit_deferred` returns
  before the sync, and one `flush` makes all pending commits durable and
  visible (ten deferred commits, one sync). `SharedStore` lets threads commit
  through a leader that waits a short window and syncs once for the group
  (8 threads × 25 commits: 200 commits, 40 syncs in one run; counts vary).
* **Fuzzy checkpoints.** The log has two regions. `checkpoint_begin` switches
  regions; `checkpoint_step` writes back a few pages dirty at the switch while
  commits continue; when they are all written the data file is synced and the
  previous region dropped. Groups start a checkpoint when a region is half
  full.
* **Recovery.** Open scans both regions, redoes complete groups by page LSN
  (an incomplete trailing group is ignored, never partly applied), writes the
  pages back, syncs and starts a fresh log generation. No undo is needed
  because uncommitted writes never reach the log or the disk.
* **SQL.** `dataset(&txn)` materializes a snapshot as a `DataSet`;
  `query`/`query_vectorized` run a `PreparedQuery` over it.

`tests/store_crash.rs` runs seeded workloads (inserts, updates, deletes,
aborts, deferred commits, checkpoints, vacuum) and, for every device operation
of the run, crashes there under several crash choices and also tears the WAL
at every frame boundary of the last write; each recovery must equal the state
after exactly the acknowledged commits (or one more when a commit was in
flight) and pass structural checks of every tree and the page partition. One
run: 427 crash points, 2,079 recoveries, 371 WAL frame boundaries. Removing
the log flush before page write-back, or redoing an incomplete trailing group,
each makes the test fail. Crashes during recovery and during store creation
are enumerated too.

`lean/QuarryWal.lean` (checked by `tests/lean_model.rs` when `VESPER_LEAN` is
set) proves, about small models only: redo by page LSN from any WAL-consistent
disk yields the full history and is idempotent; recovery ignores a torn
trailing group; undo of losers under strict record locks restores the winners'
state, and is the identity under no-steal; and the §24.4 pin conservation for
a reference pool scan on its Ok and Err paths. It is not a proof about the
Rust code.

Not built: latch coupling or concurrency inside one `Store` (callers serialize
through `&mut` or `SharedStore`), leaf merge or rebalancing (empty leaves are
freed), secondary indexes, serializable isolation, rows over 4,096 bytes.
