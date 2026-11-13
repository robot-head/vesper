# Quarry durable pages, WAL recovery and vectorized execution (§24)

Wave-six lane 10 ([prompt](../agent-prompts/wave-six/10-quarry-durable.md)), claim issue
[#252](https://github.com/robot-head/vesper/issues/252). Base: `origin/main` at `07322e3`
(a descendant of the required `cda092f`). Branch: `claude/project-thread-jrrtor`. The
commit and PR are in the PR description.

Everything is inside `crates/vesper-quarry/**` plus this report. The root patch is
one line of `Cargo.lock` (the new `vesper-simd` dependency edge of `vesper-quarry`).
The crate was already registered and already had a `BUILD.bazel`; Bazel derives the
new dependency from Cargo metadata, so no BUILD change was needed. No Lean file was
added (the Lean model was optional).

## What was built

| Item | Where | Status |
| --- | --- | --- |
| File-backed slotted pages with checksums | `device.rs`, `durable.rs` | Pages at `page_id × 16 KiB` in `quarry.data`, CRC32C-checked on every load (now table-driven, same values) and page id checked |
| Buffer pool keeping the §24.2 pin protocol, eviction with dirty write-back | `durable.rs` over the existing `pool.rs` | Every change goes through `fix_exclusive` → staged change → `unpin_dirty(pin, lsn)` or `unpin_aborted`; LRU eviction of unpinned frames, dirty victims written after the WAL is flushed through their page LSN |
| Write-ahead log with ARIES-style redo/undo and crash recovery | `wal.rs`, `durable.rs` | Physiological slot records, before/after images, compensation records with `undo_next`, repeating-history redo by page LSN, newest-first loser undo that resumes partial rollbacks, full-page images against torn page writes |
| Checkpoints that truncate the log | `durable.rs`, `wal.rs` | Sharp checkpoint (no active transactions): write back, sync data, ping-pong header with a new generation and base LSN, truncate. Recovery always ends with one |
| Crash testing with a simulated crash/fsync model | `device.rs`, `tests/durable.rs` | Every device-operation crash point of seeded workloads × four crash choices, plus crashes inside recovery |
| Vectorized scans, filters and aggregates using `vesper-simd` | `vector.rs`, `tests/vector.rs` | Column batches, three-valued mask kernels, Baseline128 JIT mask counting, COUNT/SUM/MIN/MAX, differential tests against the row executor |

Public API added: `device::{Device, FileId, DeviceError, FsDevice, SimDevice,
CrashChoice, SimStats}`, `wal::{LogRecord, TailDefect, HEADER_BYTES, MAX_PAYLOAD}`,
`durable::{Db, DbLimits, DbError, DbErrorKind, Txn, RecordId, RecoveryReport,
DbStats, MAX_VALUE}`, `vector::{VectorEngine, Aggregate, aggregate_rows,
DEFAULT_BATCH}`, `PreparedQuery::{execute_vectorized, aggregate_vectorized}`,
`page::crc32c`, and `ErrorKind::Unsupported`. The crate README documents the
formats, protocol and limits.

## Decisions

- **Own crash model, not `vesper-sim-session`.** The wave-three model
  (`wal-frame-v1`) logs canonical JSON of checked source values for the CEK
  evaluator and needs source scenarios; Quarry needs byte pages and a page
  cache. `SimDevice` models per-file unsynced operations that survive a crash
  as a seeded in-order subset with one torn write per file. Reading the
  wave-three crate was enough to keep the semantics aligned (explicit crash
  choices, recorded and deterministic).
- **ARIES with page-granular exclusive locks.** A page changed by an active
  transaction stays locked to it, so an undo image always fits its page and
  multiple concurrent losers can be rolled back. This costs concurrency (no
  record locks, no MVCC, no-wait conflicts) and is documented.
- **Full-page images** on first change after a checkpoint, instead of a
  doublewrite buffer, to repair torn page writes. Rot in a page no image covers
  is `Corrupt`, never silently repaired.
- **Generation-bound frame checksums.** Frames from before a truncation fail
  the CRC even when their bytes survive a lost `set_len`, so a crash during a
  checkpoint cannot splice old records into the new log.
- **Reserved log space** for every compensation and finishing record, so
  `LogFull` arrives before a rollback could fail.
- **Poison on I/O.** After any device, crash or corruption error inside a call
  the handle refuses further work; reopening runs recovery. This avoids
  reasoning about in-memory state that is ahead of the device.
- **Vectorized scope.** Only single-table plans: the row executor's nested-loop
  joins have no columnar counterpart yet (`Unsupported`). `vesper-simd` offers a
  byte-count kernel, so it counts selection masks (sizing the selection vector
  and answering `COUNT`); comparison kernels are portable slice loops.

## Checks

Run on the cloud host (x86_64 Linux, 4 cores), with `CARGO_TARGET_DIR` outside the
checkout and `CARGO_PROFILE_{DEV,TEST}_DEBUG=0`:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p vesper-quarry --all-targets --locked -- -D warnings` | clean |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | see PR description |
| `cargo test -p vesper-quarry --locked` | all pass, including 12 new durable and 5 new vector tests |
| `cargo test --workspace --locked` | see PR description |
| `cargo run -p vesper-quarry --example quarry_durable` | self-check passes |

Crash enumeration (`every_crash_point_recovers_a_committed_prefix`): seed 1 has
36 commits and 153 crash points, seed 2 has 33 commits and 117 crash points;
4 crash choices each, 1,080 recoveries, every one equal to the acknowledged
committed prefix (or one more when that commit was in flight). Across the
enumeration, recoveries restored page images 825 times, redid changes 909
times, rolled back losers 593 times and cut torn log tails 116 times (counts
of recoveries in which each happened). `crashes_during_recovery_recover_the_same_prefix`
crashes recovery at every one of its device operations for three crash points
under two crash choices. Mutation checks: disabling page-image restore, skipping
loser undo, or dropping the WAL-rule flush before a page write each fails the
enumeration (run by hand, not kept as tests).

Differential vectorized tests: 39 predicates × 5 projections × {plain, optimized}
× 5 engine configurations (JIT and portable counting, batch sizes 1, 7, 64 and
1,024) = 1,950 comparisons with exact row/order/column equality; 17 aggregates for
each of the 39 predicates under two configurations against `aggregate_rows`. The
Baseline128 JIT was active on this host (`simd_active()`), which the
mask-count test also cross-checks against the portable count.

## Remaining gaps

- Source `@sql` stays a Forge-macro step; nothing here is reachable from Vesper
  source, and the durable store is not connected to the SQL catalog or
  `DataSet` (it stores opaque records).
- No B+Tree, PAX pages, free-space map, slot reuse or compaction of tombstones;
  no fuzzy checkpoints, group commit, record-level locking or MVCC.
- No `O_DIRECT`, io_uring, hugepage DMA or hardware CRC32C. `FsDevice` relies
  on `File::sync_all`; whether the OS and disk honour it was not tested, and
  the crash model says nothing about real filesystems.
- Vectorized execution covers single-table plans only, has no AoSoA tiles, and
  allocates per batch. No allocation-freedom or performance claim is made;
  nothing was benchmarked.
- `Txn` and pins are Rust owners with leak counting, not Vesper linear types;
  the §24.4 Lean pin-conservation and tenant-isolation theorems are not
  attempted, and no Lean model of the WAL was written.
