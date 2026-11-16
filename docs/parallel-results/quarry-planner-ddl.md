# Quarry planner, online DDL and O_DIRECT (§24): spec, conformance tests, O_DIRECT

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

This is wave-nine lane 24 ([prompt](../agent-prompts/wave-nine/24-quarry-planner-ddl.md)),
claimed by issue [#601](https://github.com/robot-head/vesper/issues/601).

- Base: `origin/main` at `5a018bdd8b54f9be5974843d9bb244a207a04834`.
- Branch: `claude/project-thread-ytqflb`.
- Edits are inside `crates/vesper-quarry/**` plus this report. There are no
  new dependencies, no root patch, no Lean change and no `unsafe` code.

## Scope change

During the lane, on 2026-11-16, the owner approved a new direction. Rust
crates that will move to Vesper source get only bug fixes and thin OS
glue, not new features. Quarry's planner and DDL logic is in that group.
So this lane delivers the **specification and conformance tests** that
the Vesper port must pass, instead of a Rust planner. It also delivers the
small `O_DIRECT` device option, which is OS glue.

A partly written Rust version of the online index build (not compiled, not
merged) is kept outside the repository as a reference for the port, in the
project files: `wip/quarry-online-ddl-draft.patch` and
`wip/quarry-online-ddl-draft-online.rs`.

## What was built

| Item | Where | Status |
| --- | --- | --- |
| Planner and online-DDL specification | `crates/vesper-quarry/spec/planner-ddl.md` | Covers the access paths (scan, index eq/range/lower/upper, index nested-loop join), the sargability rules (including outer-join placement), the exact-statistics integer cost model and its tie-breaks, the conformance data set, the golden format, and the online DDL protocol: DROP and CREATE TABLE under live transactions, and index builds with catch-up from logical `Rows` WAL notes, restart and blocking fallback. It also lists the acceptance tests the port must add. |
| Conformance cases | `tests/fixtures/planner/cases.txt` | 22 cases with expected plans. They cover point lookups (literal, unique, parameter, NULL parameter), wide ranges that should scan, open and exclusive bounds, swapped operands, two candidate indexes, unselective text, empty text ranges, unindexed columns, and `OR`, `<>` and `IS NULL`. They also cover index nested-loop inner and left joins, an unselective outer side, RIGHT JOIN placement, a probe with too many matches, and a self-join that uses two paths. |
| Golden results | `tests/fixtures/planner/golden.txt` | Plan plus exact rows and order for every case, produced by the Rust executors. |
| Differential harness | `tests/planner_spec.rs` | Builds the data set. It checks that each `plan:` line is well formed and names real aliases, indexes and columns, and that the row and vectorized executors agree (the row executor is skipped only on a typed work `Limit`). It compares the rendered output with `golden.txt` (`VESPER_BLESS=1` regenerates it). When `QUARRY_PORT_CMD` is set, it runs the port on `cases.txt` and requires byte-identical output. A second test checks that the store's `lookup` and `lookup_range` return exactly the golden rows for the indexed single-table and join-probe cases. |
| `O_DIRECT` data-file I/O | `src/device.rs` (`DirectMode`, `DirectIo`, `DIRECT_ALIGN`, `FsDevice::open_with`, `FsDevice::direct_io`) | `Prefer` or `Require` opens a second, `O_DIRECT` handle to `quarry.data` after a probe write and read of a scratch file. Block-aligned data-file reads and writes go through 4096-aligned buffers. Those buffers are an over-allocated `Vec` with an aligned window, so no `unsafe` is needed. The log and unaligned requests stay buffered. `Prefer` falls back and says why (`Unavailable(reason)`), and `Require` returns an `Io` error. The flag value is given for Linux x86_64, x86, riscv64, aarch64 and arm. Every other target reports `Unavailable("O_DIRECT is implemented for Linux only here")`. |
| Tests for `O_DIRECT` | `tests/direct_io.rs` | Status reporting, aligned and unaligned round trips across the two handles, `ShortRead` past the end, and a store that commits, checkpoints, crashes (no close) and recovers over direct I/O. |

`FsDevice::open` keeps its old behaviour (`DirectMode::Off`). The
[crate README](../../crates/vesper-quarry/README.md) gained a section
pointing to all of the above.

## Checks run

All commands were run from the repository root with `CARGO_TARGET_DIR`
outside the checkout.

```
cargo fmt --all -- --check                                              # ok
cargo +1.100.0 clippy -p vesper-quarry --locked --all-targets -- -D warnings   # ok
cargo test -p vesper-quarry --locked                                    # all 15 test binaries ok
cargo test -p vesper-quarry --test planner_spec                         # 2 passed
cargo test -p vesper-quarry --test direct_io -- --nocapture             # 3 passed, status Active
VESPER_DIRECT_IO_DIR=/dev/shm cargo test -p vesper-quarry --test direct_io   # 3 passed, Active on tmpfs
python3 tools/bazel/sync_build_files.py                                 # no changes
```

`O_DIRECT` was actually exercised on this cloud host (Linux 6.18). It was
`Active` both on ext4 (`/tmp` and the build directory) and on tmpfs
(`/dev/shm`, which supports `O_DIRECT` on this kernel). The fallback path
is exercised by macOS and Windows CI, where it reports `Unavailable`.
`cargo test --workspace --locked` was left to CI ("Test all targets").

## Decisions

- **Spec and tests instead of a Rust planner**, following the owner's
  direction (see "Scope change").
- **Exact statistics and an integer cost model.** Exact statistics and
  integer costs make every expected plan in the fixtures a fixed,
  checkable consequence of the spec. No floating-point ties remain, and
  every tie-break is explicit. A port may use sketches or histograms
  internally, but it must reproduce these plans on the conformance data.
- **Plans are part of the golden file.** Rust cannot check them today. The
  harness checks that they are well formed, and the `QUARRY_PORT_CMD` hook
  checks them for real against the port.
- **The vectorized executor is the reference** for golden rows. The
  nested-loop row executor runs out of its 1,000,000 work budget on the
  unselective join. It must still agree everywhere else.
- **`O_DIRECT` uses a second handle**, not a replacement, so the log,
  header and unaligned paths stay simple and buffered. Linux keeps
  direct and buffered I/O on one inode coherent.

## Remaining gaps (deferred to the Vesper port)

- Index selection in the SQL layer, `ANALYZE` statistics, and executing
  index and index-join paths.
- Online DDL and online index builds. Rust DDL and `create_index` still
  require no active transactions, and the spec lists the acceptance tests
  for the port.
- Parallel I/O for scans.
- TPC-C-like and range-scan benchmarks. No performance numbers were
  measured in this lane.
- `O_DIRECT` on macOS (`F_NOCACHE`) and Windows
  (`FILE_FLAG_NO_BUFFERING`). These would need `libc` or `windows-sys`,
  which this lane was not allowed to add.
- The Rust ownership in this crate is evidence about this implementation
  only, not about Vesper's type system.
