# Quarry host SQL core

`vesper-quarry` is a dependency-free, bounded host library implementing the catalog,
query checking, relational optimization, and reference row execution slice of
specification §24. It performs no I/O. It is **not** an embedded transactional
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
3. Create `DataSet::new(&catalog)` and supply rows with `set_rows(table, rows,
   limits)`. This validates scalar types, nullability, widths, and input budgets
   before replacing existing rows. Empty tables must be supplied explicitly.
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
leaves catalog/data unchanged. Optimization can change logical work and candidate
materialization totals, so a low-budget execution may fail in one form and
succeed in another; differential row equivalence is asserted when both complete.
Default join budgets intentionally reject large Cartesian products.

Tests cover expected results, all nullable Boolean truth-table combinations,
bag joins, multi-join nullability, typed scalar/bind validation, stale catalogs,
atomic failures, unsupported SQL, resource exhaustion, and 3,584 before/after
optimizer comparisons (64 nullable input subsets × four joins × 14 predicates).
No source-mode safety, allocation-free execution, SIMD/AoSoA, egglog integration,
WAL/ARIES/MVCC, buffer-pool pins, crash recovery, tenant capability enforcement,
network/database service, or Lean proof is provided by this host slice.
