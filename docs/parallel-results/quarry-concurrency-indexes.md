# Quarry concurrency, secondary indexes, SSI and join order (§24)

Wave-eight lane 22 ([prompt](../agent-prompts/wave-eight/22-quarry-concurrency-indexes.md)),
claim issue [#471](https://github.com/robot-head/vesper/issues/471). Base: `origin/main`
at `1094e64`, with main merged in before the push. Branch: `claude/project-thread-xs6ojp`.

Every edit is inside `crates/vesper-quarry/**` plus this report. There are no new
dependencies, no root files were touched, and no `unsafe` code was added. The Lean WAL
model (`lean/QuarryWal.lean`) was not changed, because recovery did not change.

## What was built

| Item | Where | Status |
| --- | --- | --- |
| Latch coupling: concurrent readers and one writer in one `Store` | `store/frames.rs` (rewritten), `store/mod.rs` | Frames carry logical shared or exclusive latches, and a waiting exclusive request blocks new shared ones. Readers couple view → root → child, and leaf → right sibling. A commit group X-latches every changed or freed page in tree order (level descending, leaves left to right), installs them and publishes the new view. Read and write APIs take `&self`, and `Store` is `Sync`. |
| Leaf merge and rebalance through the right-sibling field | `store/pages.rs`, `store/btree.rs` | Every leaf links to its right sibling (header bytes 28..32, validated on decode). After a removal, a leaf under a third full merges with its neighbour when the two fit, and otherwise borrows from it. Internal nodes merge or redistribute below half fanout, and the root collapses. `check()` validates the sibling chain. |
| Secondary indexes maintained in transactions | `store/index.rs` (new), `store/mod.rs`, `store/pages.rs` | One B+Tree per indexed column, keyed by (order-preserving 64-bit prefix, rowid). An entry exists exactly when a stored version of the row has a value with that prefix. Indexes change in the same commit group as the table (and in vacuum), so they are atomic under crash. A lookup rechecks each candidate against the version its snapshot sees and merges the transaction's own writes. Indexes are persisted in meta format 2 and dropped with their table, and `check()` verifies index/table agreement. |
| Serializable snapshot isolation | `store/mod.rs` | Cahill-style rw-antidependency tracking. SIREAD predicates cover rows, tables and index ranges. Edges are added at read time (to writers of newer versions and to lock holders) and at commit time (from concurrent readers whose predicates the write hits, including retained committed ones). A committer with both an in-edge and an out-edge, or one next to a committed pivot, aborts with `StoreErrorKind::Serialization`. Retained records are pruned once no transaction can still be concurrent with them. |
| Streaming hash-join probe | `vector/join.rs` | Candidate pairs are produced, filtered and emitted per probe batch. The full pair list is never materialized. |
| Cost-based join order | `vector/join.rs`, `vector.rs` | A chain of three or more inner joins is reordered greedily. The smallest input goes first, then connected inputs by estimated size (\|cur\|·\|input\| / distinct keys), and cross products go last. Results are sorted back into the written order, so rows and order equal the row executor's. The chosen orders are exposed through `PreparedQuery::vectorized_join_orders`. |

### Public API changes

- **New:**
  - `Store::{begin_with, create_index, drop_index, indexes, lookup, lookup_range}`
  - `Isolation::{Snapshot, Serializable}` (the default is `Snapshot`) and `Txn::isolation`
  - `StoreErrorKind::Serialization`
  - `StoreStats::ssi_retained`
  - `PreparedQuery::vectorized_join_orders`
- **Changed signatures:**
  - Reads, writes, `begin`, `commit` and `abort` take `&self`.
  - `catalog()` and `columns()` return owned values.
  - `device()` takes `&mut self`.
- **On-disk:** the meta page is now format 2 (tables plus indexes). Format 1 is rejected with a typed `Corrupt` error rather than migrated.

## Tests

| Test file | What it checks |
| --- | --- |
| `tests/store_concurrency.rs` | One writer commits groups that split, merge and rebalance (fanout 3, four versions per leaf, eight-frame pool) while four readers scan. Each reader snapshot must equal the writer's recorded state at that timestamp and conserve the total. A sample run checked 450 snapshots against a tree of height 5 with 21043 evictions. In a second test, four racing writer threads never lose an update. Every race becomes a typed `Conflict` that the caller retries. |
| `tests/store_index.rs` | Index build from existing versions, prefix collisions separated by the recheck, negative integers, NULL not indexed, -0.0 = 0.0, old versus new snapshots after an update moves a row, the transaction's own writes, vacuum, typed errors, persistence across reopen, and dropping an index or its table. A 300-step random workload compares every lookup with a filtered scan, in the writing transaction and in held old snapshots, and ends with a crash. |
| `tests/store_ssi.rs` | Write skew (the on-call doctors case) is rejected under `Serializable` and allowed under `Snapshot`. Also covered: phantom write skew through an index range, the read-only anomaly, disjoint transactions committing without false aborts, and a threaded invariant test with retries. A mutation check confirmed that this test fails when switched to `Snapshot` isolation. |
| `tests/store_crash.rs` | The existing crash enumeration now also creates two indexes. After every recovery, lookups must equal filtered scans and `check()` must pass. A sample run covered 490 crash points and 2434 recoveries, with 474 WAL frame boundaries torn. |
| `tests/vector_join.rs` | Reordered chains of inner joins match the row executor, and the cost order prefers connected inputs (expected order `m, r, l`). |

## Checks run

All commands were run from the repository root.

```
cargo fmt --all -- --check
cargo +1.100.0 clippy -p vesper-quarry --locked --all-targets -- -D warnings
cargo clippy -p vesper-quarry --locked --all-targets -- -D warnings
cargo test -p vesper-quarry --locked
python3 tools/bazel/sync_build_files.py        # no changes
```

`cargo test --workspace --locked` was left to CI, which runs it as "Test all targets".
Under Bazel, `store_crash` has the `long` timeout because the debug-build enumeration
takes about 85 s.

## Decisions

- **Stress harness, not loom.** `loom` would be a new dependency, which the prompt forbids. Real-thread randomized stress tests check exact snapshots against recorded history. That is evidence, not a model check of every interleaving.
- **The pool overcommits softly.** When every frame is pinned (deep coupling under an eight-frame pool), the pool grows past its capacity instead of failing, and it shrinks back on eviction.
- **The pool mutex is held during device I/O.** This is simple and correct. Finer I/O concurrency is left for later.
- **An index is built in a single group.** `create_index` and DDL need no active transactions (otherwise they return `State`).
- **Format 1 meta is rejected rather than migrated.** No persistent Quarry data predates this branch.
- **SSI applies only among `Serializable` transactions.** `Snapshot` transactions neither create nor receive edges. As with Cahill's algorithm, there are false positives: some serializable histories abort, and callers retry on `Serialization`.
- **Recovery is unchanged.** Groups are still redo-only and atomic, and an index change is just more pages in the same group. So the Lean WAL model still describes recovery and was not extended.

## Remaining gaps

- No performance was measured, and there are no benchmarks yet.
- There is no `O_DIRECT` and no parallel I/O.
- The SQL layer does not choose the indexes yet. They are reached only through `lookup` and `lookup_range`.
- DDL and index creation wait for a quiescent store.
- The Rust ownership and latch discipline here is evidence about this implementation, not about Vesper's type system.
