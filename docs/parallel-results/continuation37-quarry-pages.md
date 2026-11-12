# Quarry §24.2: checked pages and recoverable pin owners

Issue: [#48](https://github.com/robot-head/vesper/issues/48).
Implementation branch: `continuation37/quarry-pages`, based on `5e699f8`.
The issue and exact refreshed §24.2 were read before implementing this slice.

Added `page` and `pool` modules beside the existing Quarry SQL engine. SQL parser,
planner, execution, and tests are unchanged. No dependency or root manifest,
lockfile, CLI, workflow, compiler, other production crate, or Warp file changed.

## Working behavior

`page::SlottedPage` stores an actual 16 KiB byte page with the specification's
exact 32-byte little-endian header. Page ID/LSN/sibling/header fields occupy the
specified offsets. Software CRC32C covers the whole page with the checksum field
zeroed. Header parsing uses byte fields rather than Rust layout casts.

The explicit supported record format uses four-byte `(offset,len)` directory
slots and descending opaque records. Decode checks the entire directory,
header/free-space bounds, record ranges, and pairwise nonoverlap using bounded
sorted ranges. It rejects unsupported kinds/levels/flags and zero-length slots.
The free pointer equals the lowest live record address; interior holes are
permitted. Actual insert/read/update methods preserve slot identities; updates
compact records into a fresh checked page. Failed mutations preserve exact old
bytes and borrowed input records.

`pool::BufferPool` has fixed bounded frame/pin capacities. Private nonclone
shared/exclusive pin handles carry pool identity, frame generation, page identity,
and active serial. Shared readers coexist; exclusive readers/writers conflict.
Every access/release checks origin, generation and active membership. No pinned
frame is evicted or overwritten. Pool install borrows its image on failure;
eviction returns an owned page image plus dirty metadata rather than silently
losing an unpersisted page.

Exclusive edits live in a staged page owned by the pin. Abort discards staging,
leaving committed bytes and existing dirty metadata intact. Dirty release requires
actual changed bytes and a nonzero LSN strictly newer than committed page LSN;
it publishes the new page/checksum atomically. Earliest dirty LSN survives later
commits, clean shared releases, eviction and reinstall. Release failures return
the complete supplied owner/staging in `ReleaseError` for retry/abort.

Hard ceilings are 256 frames, 4,096 pins, one 16,384-byte page per frame, 3,270
slots, and 16,348 bytes per record. Resident page bytes total at most 4 MiB;
exclusive staged pages add at most another 4 MiB, plus bounded metadata and
one-page update buffers. Decode sorts at most 3,270 ranges. CRC work is bounded
by one page and record copies by available page capacity. Pool identities,
generations and pin serials increment with checked arithmetic and reject wrap.
The crate README documents the exact format, budgets and API semantics.

## Validation

All checks used `/tmp/continuation37-quarry-target`, one build job, zero dev/test
debug information, and disabled incremental compilation:

```sh
cargo fmt -p vesper-quarry --check
cargo clippy -p vesper-quarry --all-targets -- -D warnings
cargo test -p vesper-quarry --all-targets
cargo test -p vesper-quarry --doc
cargo run -p vesper-quarry --example quarry_pages
```

Passed: four unit tests, eleven page/pool integration tests, sixteen unchanged
SQL integration tests (including 3,584 optimizer/input configurations), and two
compile-fail doctests proving shared/exclusive pin handles lack Clone.
The self-checking example reports a CRC32C-validated 16,384-byte page with two
actual records, dirty LSN 9, successful staged rollback, recoverable failed
release, and explicit eviction/reinstall.

Tests independently check the standard CRC32C `123456789 -> e3069283` vector;
exact LE header bytes; opaque binary records; update shrink/growth and full-page
atomicity; CRC corruption; CRC-valid malformed offsets/count/free pointers,
overlapping slots, zero lengths and unsupported headers; maximum slot directory;
fragment compaction; full pool/pin budget and shared/exclusive conflicts; pinned
eviction; wrong-pool read/stage/all release paths with owner recovery; stale
serial/generation reuse; generation/serial exhaustion; strict/maximum LSN rules;
earliest dirty LSN preservation; and abort after prior dirty commits.

## Limits of the evidence

This is a bounded in-memory host format and explicit ownership protocol. There
is no SQL-to-page storage mapping, source-language linear exhaustion, automatic
cancel cleanup, io_uring/O_DIRECT/DMA, hardware CRC acceleration, durable WAL,
writeback acknowledgment, ARIES/crash recovery, MVCC, B+Tree, tenant isolation,
optimistic lock coupling, or Lean proof. LSN checks enforce numeric ordering,
not existence or durability of WAL records. Dirty eviction transfers ownership
and metadata to the caller; it does not flush data.

Pin types are nonclone and must-use, but Rust permits dropping/forgetting them or
dropping the pool. A discarded handle leaves the frame pinned until pool teardown;
this slice cannot statically force linear exhaustion. Public safe code cannot
manufacture stale duplicate handles; internal adversarial tests construct stale
keys to validate defenses without adding a handle-forging API. Allocation failure
outside Result semantics is not modeled. Caller-owned image clones and multiple
pools are outside each pool's memory ceiling.
