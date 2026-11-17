# Quarry planner and online DDL: specification for the Vesper port

Status: specification and conformance tests only. The owner decided on
2026-11-16 that Quarry's planner and DDL logic will be written in Vesper
source, not grown further in Rust. This file is what that port must
implement. `tests/planner_spec.rs` and `tests/fixtures/planner/` are its
acceptance tests. Manuscript authority: the §24 chapter of
`docs/manuscript/content.typ` (legacy numbering; see `docs/section-map.md`).

The Rust crate implements none of the planner below. It does have the
pieces the port reuses or differentially tests against:

- the checked SQL front end (`HermeticCatalog::prepare`);
- the predicate-pushing optimizer (`PreparedQuery::optimized`);
- the row and vectorized executors;
- the MVCC table store with secondary indexes (`Store::lookup`,
  `Store::lookup_range`);
- quiescent `create_index` and `execute_ddl`, which need no active
  transactions.

## 1. Access paths

The planner picks one access path for each FROM binding, in FROM order:

| Path | Notation | Meaning |
| --- | --- | --- |
| Scan | `alias=scan` | Every row the snapshot sees. |
| Index | `alias=index(I,eq)` | Index `I` with a point bound. |
| Index | `alias=index(I,range)` | Index `I` with a lower and an upper bound. |
| Index | `alias=index(I,lower)` | Index `I` with only a lower bound. |
| Index | `alias=index(I,upper)` | Index `I` with only an upper bound. |
| Index join | `alias=index_join(I,A.C)` | Index nested-loop join: for each distinct non-null value of the outer column `A.C`, probe index `I` of this binding. |

Executing a plan must return exactly the rows, in exactly the order, that
the plan with every binding set to `scan` returns. Rows are always fed to
the join and filter operators in row-id order. Each path therefore yields
a row-id-ordered subset of the table that still contains every row the
binding's predicates accept. The executor then re-applies every predicate
as a residual filter. When two bindings read the same table, each keeps
its own row set.

### Sargable predicates

Only the conjuncts that `PreparedQuery::optimized` places in a binding's
`Scan.predicates` are candidates. A `WHERE` conjunct on the nullable side
of an outer join stays in a `Filter` above the join, so it is never
sargable (see case `right_join_preserved_side_only`). A candidate is
sargable on an index on column `c` when it has one of these forms, with
`v` a literal or a parameter:

- `c = v`
- `c < v`, `c <= v`, `c > v`, `c >= v`
- one of the above with the operands swapped (`v > c` is `c < v`)

`<>`, `OR`, `NOT`, `IS [NOT] NULL` and column-to-column comparisons are
never sargable. NULL values are not in an index. A bound whose value is
NULL accepts no row, so the path yields no rows.

All sargable bounds on the same column combine: the tightest lower bound
and the tightest upper bound win. Equal lower and upper values with both
bounds inclusive make the path `eq`. Exclusive bounds may be scanned
inclusively, because the residual filter removes the extra rows. Each
binding uses at most one index.

### Index nested-loop join

An index join is allowed only for the right-hand binding `b` of an
`INNER` or `LEFT` join, never for `RIGHT` or `FULL`. The `ON` conjuncts
must include `b.c = a.d`, where `b.c` has an index and `a` is a binding
on the left side.

To execute it, the left side is evaluated first. The probe keys are the
distinct non-null values of `a.d` in the left side's rows. `b`'s rows are
the union of the index lookups for those keys. That union is exactly the
set of `b` rows that can appear in the output. Under `LEFT`, unmatched
`b` rows never appear, so restricting `b` cannot change the result.

## 2. Statistics and cost model

Costs are integers in milli-units and are compared exactly. Statistics
come from `ANALYZE`, which reads every row visible to a snapshot. They are
exact:

- `n(t)`: the row count of table `t`;
- for each column: `nonnull(t,c)` and `distinct(t,c)`, the number of
  distinct non-null values.

The cost formulas use these quantities:

- `pages(n) = max(1, ceil(n / 64))`
- `height(n) = 1 + ceil(log_128(pages(n)))`, so 1 for one page and 2 for
  2 to 128 pages
- `m`: the estimated rows a path yields. For a literal bound, or a
  parameter whose value is known at planning, `m` is the exact number of
  analyzed rows inside the combined bounds. A NULL bound gives `m = 0`.
  With an unknown parameter, `m` is `ceil(n / distinct)` for `eq`,
  `ceil(n / 4)` for a one-sided bound and `ceil(n / 10)` for a range.
- `keys`: the estimated outer rows of an index join. It is `m` (or `n`
  for a scan) of the outer binding's own path. Residual predicates are
  ignored, and the result is capped at `distinct(outer column)`.

| Path | Cost |
| --- | --- |
| scan | `1000·pages(n) + 10·n` |
| index | `1000·height(n) + 1010·m` |
| index_join | `1000·height(n)·keys + 1010·ceil(keys · nonnull(t,c) / distinct(t,c))` |

The cheapest path wins. On equal cost, `scan` beats `index`, and `index`
beats `index_join`. Among indexes with equal cost, the
lexicographically smaller index name wins. Plans never reorder joins:
the vectorized executor's own join order is applied after access
selection.

## 3. Conformance data set and golden file

`build_store` in `tests/planner_spec.rs` defines the conformance data set:

| Table | Rows | Columns |
| --- | --- | --- |
| `p` | 400 | `id` = 1..=400; `age = (id·7) mod 80`; `name = 'p'‖id`; `city` = one of 8 names (`(id mod 8)`-th of ankara, bergen, cairo, delhi, oslo, perth, quito, rome), or NULL when `13 ∣ id` |
| `o` | 600 | `id` = 1..=600; `owner = (id·11) mod 400 + 1`; `amount = (id·37) mod 1000` |
| `c` | 8 | `code` = the city; `label = 'L-'‖code` |

Indexes: `p_id`, `p_age`, `p_city`, `o_owner`, `o_amount`.

Each case in `cases.txt` states its plan and query. `golden.txt` records
the plan line and the exact result rows (the vectorized executor is the
reference; the row executor must agree wherever it fits its work budget).
A port passes when, run as `QUARRY_PORT_CMD <path to cases.txt>`, it
prints `golden.txt` byte for byte. Its plans come from its own planner,
and its rows come from executing those plans.

Rendering format, per case:

```
== NAME
plan: <the plan line>
columns: name:Type, ...
row: v1 | v2 | ...      (NULL, integers, 1.5, true, 'text' with '' escapes, x'hex', U64 with a u suffix)
rows: COUNT
```

## 4. Online DDL and index builds

Today `create_index`, `drop_index` and `execute_ddl` return `State` while
any transaction is active. The port must run them online, under the
following protocol.

### Online CREATE TABLE and DROP TABLE

DDL statements and index builds are serialized with each other. They are
not serialized with transactions.

- `DROP TABLE t` fails with `Conflict` while an active transaction holds
  a write lock on `t` (no-wait).
- A transaction that writes `t` after the drop fails at commit with
  `Conflict`.
- A read of a dropped table is `NotFound`.
- Prepared queries and plans of an older catalog revision, or ones that
  name a dropped index, are `Stale`.
- The row-id counters of surviving tables never move backwards. Ids
  handed to uncommitted inserts are never reused.

### Online index build with catch-up from the WAL

1. Under the writer mutex, mark the table *building*. From then on,
   every commit group that writes it also logs a logical
   `Rows{table, rowids}` note before its `Commit` record. Redo ignores
   the note. Flush the log and record its next LSN `L0`. Every group
   before `L0` is already installed.
2. Without the mutex, walk every stored version of the table and record
   each row's set of value prefixes.
3. Catch up without the mutex. Briefly take the writer mutex to flush the
   log and read the next LSN `L1`. Read back the notes in `[L0, L1)` and
   re-read the versions of those rows. Then set `L0 := L1`. Repeat while
   a pass finds more than 64 rows, up to 8 passes.
4. Under the writer mutex, read the notes in `[L0, now)` and recompute
   those rows from the staged tree. Then write and publish the index in
   one synced system group, and clear the building mark.

If a checkpoint has dropped the log region holding `L0`, the build
restarts (at most twice). After that it builds from a full walk under the
writer mutex, which is always correct but blocks writers. Report the
time the writer mutex was held.

A draft Rust version of this protocol (uncompiled, not merged) is in the
project files at `wip/quarry-online-ddl-draft.patch` and
`wip/quarry-online-ddl-draft-online.rs`, as a reference for the port.

### Acceptance tests the port must add

- While four writer threads update, delete and insert rows, an index
  build completes. Afterwards `check()` passes and every lookup equals a
  filtered scan, both for snapshots taken before and after the build.
- With a 64 KiB log region, writers force a checkpoint during the build.
  The restart or fallback happens and the index is still exact.
- The existing serializable write-skew and invariant tests
  (`tests/store_ssi.rs`) stay green while builds and unrelated CREATE and
  DROP statements run concurrently.
- The crash enumeration covers a build: recovery at every crash point
  yields either no index or an exact one.

## 5. Out of scope here (deferred to the port)

The following are deferred to the port:

- parallel I/O for scans;
- TPC-C-like and range-scan benchmarks;
- `ANALYZE` itself.

The `O_DIRECT` device option is OS glue and is implemented in Rust
(`device::DirectMode`).
