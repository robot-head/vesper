# Quarry B+Tree, MVCC and vectorized joins (§24)

Wave-seven lane 11 ([prompt](../agent-prompts/wave-seven/11-quarry-btree-mvcc.md)),
claim issue [#334](https://github.com/robot-head/vesper/issues/334). Base:
`origin/main` at `283c811`. Branch: `claude/project-thread-4sbjsb`. The PR is in
the PR description.

Everything is inside `crates/vesper-quarry/**`, the new `lean/QuarryWal.lean`
and this report. No root file changed: no new dependency, no new crate, so no
root patch. The crate's `BUILD.bazel` now declares the Lean sources as test
data (`extra_test_data = ["//:lean"]`, `copy_test_cwd = ["test/lean_model"]`,
as in `vesper-chrono`).

## What was built

| Item | Where | Status |
| --- | --- | --- |
| B+Tree over durable pages | `src/store/btree.rs`, `src/store/pages.rs` | Keys are versions `(rowid, !commit_ts)`, newest first. Split propagation and root growth; empty non-root leaves are freed and their parent entries removed; the root collapses when it has one child |
| PAX leaf layout | `src/store/pages.rs` | Per column a null bitmap and fixed-width data, or `(offset, len)` into a heap growing down from the page end; a key array, a slot directory, a used bitmap and a tombstone bitmap. §24.2 32-byte header, CRC, page LSN. Every field is validated on decode |
| Free-space map, slot reuse, tombstone compaction | `pages.rs`, `src/store/mod.rs` | A bitmap page (page 1) of free pages; freed pages are reused before the file grows; leaf slots are reused; `vacuum` compacts leaf heaps and garbage-collects versions |
| MVCC snapshot isolation | `src/store/mod.rs` | Snapshot = newest durable commit. Deferred update (no steal), no-wait record locks on `(table, rowid)`, first committer wins. A version is collected once no open snapshot needs it |
| Group commit | `mod.rs` (`commit_deferred`, `flush`), `src/store/shared.rs` | Deferred commits share one sync; `SharedStore` runs a leader/follower group commit across threads |
| Fuzzy checkpoints | `mod.rs`, `src/store/log.rs` | Two-region WAL (`QRYWAL02`, ping-pong 64-byte header slots, generation-bound frame CRCs). Begin switches regions; steps write back pages dirty at the switch while commits continue; the end syncs data and drops the previous region |
| Recovery under the existing crash model | `mod.rs`, `src/device.rs` | Redo of complete commit groups by page LSN from both regions, ignoring an incomplete trailing group; sharp write-back; fresh generation. Uses the lane-10 `SimDevice` crash model, extended with `crash_tearing` (tear the last WAL write at a chosen byte) and `last_pending_write` |
| Pinned frame pool | `src/store/frames.rs` | Shared/exclusive pins as non-Clone `#[must_use]` owners; a pin dropped without release is counted (`StoreStats::leaked_pins`); dirty unpin needs an advancing LSN; write-back flushes the log through the page LSN first |
| Durable store ↔ SQL catalog and `DataSet` | `mod.rs`, `src/lib.rs` | `execute_ddl` runs `CREATE/DROP TABLE` through `HermeticCatalog` and persists it in the meta page; `dataset`, `query`, `query_vectorized` run `PreparedQuery` plans over a snapshot |
| Multi-table vectorized plans | `src/vector/join.rs` | Hash join on equality conjuncts (any join kind), vectorized residual over candidate-pair batches, nested loop otherwise, outer null extension in row-executor order, budgets enforced |
| AoSoA batches with reuse | `src/vector/tiles.rs` | `TileSet`: 16-lane tiles, hot `[u64; 16]` lane arrays, cold `(offset, len)` lanes into an arena, null masks; reused across executions |
| Lean model | `lean/QuarryWal.lean`, `tests/lean_model.rs` | 8 theorems, see below |

Public API added (all additive): module `store` (`Store`, `StoreLimits`, `Txn`,
`Commit`, `StoreRecovery`, `StoreStats`, `VacuumReport`, `SharedStore`,
`StoreError`, `StoreErrorKind`, `StoreResult`, `StoreRecord`, `LogTail`,
`TreeShape`, `MAX_ROW_BYTES`, `LOG_HEADER_BYTES`, `MAX_LOG_PAYLOAD`),
`vector::{Tile, TileSet, TILE_LANES}`, `PreparedQuery::execute_tiled`, and
`device::SimDevice::{crash_tearing, last_pending_write}`. No existing signature
changed; `durable::Db` is untouched. Joins that used to return
`ErrorKind::Unsupported` from `execute_vectorized` now run.

## Lean model (`lean/QuarryWal.lean`, Lean 4.24.0)

Proved about small models only, not about the Rust code. No `sorry`, `admit`,
`axiom`, `native_decide` or `partial`; every theorem has a `#print axioms`
line, and the audit accepts only `propext`, `Classical.choice`, `Quot.sound`
(the report shows `propext` and `Quot.sound` only).

* `redo_recovers_history`: page-LSN redo from any WAL-consistent disk (each
  page at some log prefix) equals forward execution of the whole log.
* `redo_idempotent`: redo of a recovered disk changes nothing.
* `complete_groups`, `recovery_ignores_torn_tail`: recovery of complete commit
  groups followed by a torn trailing group equals the complete groups.
* `undo_restores_winners`: under strict record locks, undoing losers in
  reverse order after repeating history gives the winners-only state.
* `no_steal_needs_no_undo`: with no loser writes in the log, undo is the
  identity (the store's case, which is why its recovery is redo-only).
* `scan_restores_pool`, `scan_conserves_page_pins`: the §24.4 statement for a
  reference pool scan: on the Ok path and on a CRC error at any page, pin
  counts, so the pinned-frame count, are unchanged.

## Checks

All run on this branch in the cloud container (Linux x86_64, Rust 1.97 and
1.100, Lean 4.24.0 release).

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo test -p vesper-quarry --test store` | 10 passed |
| `cargo test -p vesper-quarry --test store_crash` (debug, `CARGO_PROFILE_TEST_DEBUG=0`) | 3 passed, about 49 s |
| `cargo test -p vesper-quarry --test vector_join` | 4 passed (8,064 join comparisons) |
| `VESPER_LEAN=… cargo test -p vesper-quarry --test lean_model` | 2 passed, 8 theorems audited |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | clean |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | clean |
| `VESPER_LEAN=… cargo test --workspace --locked --no-fail-fast` (`CARGO_PROFILE_*_DEBUG=0` for disk space) | 4,034 passed, 1 failed, 2 ignored. The failure is the root package's `lean4lean42_acceptance::genuine_both_model_replay_and_independent_refusal_matrix`, which requires `VESPER_LEAN4LEAN42_ROOT` (prepared only in CI) and is untouched by this lane |
| `cargo test -p vesper-quarry --locked` | 76 passed |

`tests/vector.rs::joins_stale_inputs_and_limits_are_typed_errors` asserted the
old `Unsupported` result for a join; it now asserts the vectorized join equals
the row executor.

Crash enumeration (`tests/store_crash.rs`, seeds 11 and 12): 427 crash points,
2,079 recoveries, 371 WAL frame boundaries torn, with page images restored in
1,576 recoveries, diffs redone in 897, incomplete trailing groups dropped in
237, the previous log region replayed in 988, and torn log tails in 1,566.
Every recovery must equal the state after exactly the acknowledged commits (or
one more when a commit was in flight) and pass tree and page-partition checks.
Crashes inside recovery and at every operation of store creation are
enumerated too.

Mutation checks, run by hand: removing the log flush before page write-back
(`frames.rs`) fails the crash test; redoing an incomplete trailing group
(`mod.rs`) fails the crash test and the creation test. Both were reverted.

Isolation anomalies (`tests/store.rs::snapshot_isolation_anomalies`): dirty
write, dirty read, non-repeatable read, lost update, phantom and read skew are
each shown not to happen; write skew is shown to happen (snapshot isolation is
not serializable).

Measured counts (not timings): 10 `commit_deferred` calls then one `flush` is
one log sync. `SharedStore` with 8 threads × 25 commits and a 2 ms window: 200
commits, 40 log syncs in one run; the test asserts only fewer syncs than
commits, since the count depends on scheduling. No throughput or latency was
measured, and no zero-allocation claim is made: tile reuse is asserted through
`allocated_tiles` and `arena_capacity`, not a counting allocator.

## Decisions

* **Deferred update instead of steal + undo.** Uncommitted writes live in the
  transaction and never reach the log or a page, so recovery is redo-only and
  commit groups are atomic in the log. The Lean model proves the general undo
  case separately and the no-steal special case.
* **Versions in the tree key.** `(rowid, !ts)` keeps a row's versions adjacent
  and newest first, so a snapshot read is one seek. Tombstones are versions.
* **Page images on first touch after a checkpoint switch, diffs afterwards.**
  This makes torn data pages repairable from the log alone; diffs carry
  `prev_lsn` and are applied only on the exact predecessor.
* **Two log regions** give a fuzzy checkpoint without a log-truncation pause:
  the old region is dropped only after every page dirty at the switch has been
  written back and the data file synced.
* **Snapshot = durable commits.** A deferred commit is invisible until its
  group is synced, so no reader can see a commit that a crash could lose.
* **Joins materialize their inputs** as column relations; the hash table is on
  the right input. This keeps the row executor's output order exactly.

## Remaining gaps

* No latch coupling or concurrency inside one `Store`: callers serialize
  through `&mut` or `SharedStore`. Group commit amortizes syncs, not CPU.
* No leaf merge or rebalancing (empty leaves are freed). The §24.2 header's
  right-sibling field is not used; scans descend from the root.
* DDL requires no open transactions. Rows are at most 4,096 encoded bytes.
* No serializable isolation, no secondary indexes, no `O_DIRECT`/io_uring.
* Vectorized joins materialize inputs; there is no streaming probe and no
  join reordering.
* Rust ownership of pins and transactions is not evidence about Vesper's
  linear typing, and the Lean theorems are about the models, not this code.
* Whether a real filesystem or disk honours `fsync` is outside the
  `SimDevice` model.
