# Issue 209: independent tiled lifecycle acceptance

Contract: reviewed frozen `37f9097`, `docs/rfcs/tiled41-contracts.md`.
Owned paths are the independent `tests/tiled_region.rs`,
`examples/tiled_columns.rs`, `TILED.md` under `crates/vesper-alloc/`, and this
report. No production, package, build, workflow or old-test edits are included.

The tests exercise both premark append cases, protected growth atomicity,
postmark allocation and actual relocation followed by rewind, same-address
reuse, stale and foreign validation across every public operation (including
no-ops and out-of-range lookup), failed release ownership recovery, zero-capacity
tracking, reset, overflow, finite reservation/allocation/alignment limits,
backing refusal, release, and forgotten-owner cleanup. The test backing records
actual pointer/layout pairs and detects unmatched, repeated or wrong-layout
deallocation. A separate scalar pair formula supplies the row oracle.

The actual column example scans and mutates 73 rows across five width16 tiles,
then checks every copied logical pair against its independently computed scalar
expectation. Documentation includes a runnable positive example and six
compile-fail ownership/field/borrow examples.

Actual runtime dependencies were imported in the approved order:

| Published source | Local dependency copy | Purpose |
|---|---|---|
| `9919666` | `82f88bf` | Real tiled implementation |
| `3529ef5` | `481fbe1` | Actual public reexports |
| `6c03c6d` | `6001948` | Strict Clippy and meaningful source invariance docs |

These dependency copies are not lifecycle-owned commits and must not be
reimported as lifecycle work. Source/core and independent layout test lanes
were not imported into this worktree; their results belong to their owners.

Actual verification on this source stack, using the private
`/tmp/tiled41-lifecycle-target` with one Cargo job, dev/test debug0 and
incremental0:

* `cargo test --locked -p vesper-alloc`: 86 native tests pass, comprising all
  72 existing allocator tests and all 14 new lifecycle tests, plus all 18
  package doctests. No ignored or failed tests.
* `cargo run --locked -p vesper-alloc --example tiled_columns`: passes; output
  confirms 73 logical rows, 5 actual column tiles, initial sums 18615 and 38836.
* `rustdoc --test crates/vesper-alloc/TILED.md --edition 2024 --extern
  vesper_alloc=<private-cache-rlib> -L dependency=<private-cache-deps>`:
  the runnable example and all six compile-fail cases pass. The final
  compile-fail checks rejection of nonstatic field types, not an isolated
  variance proof; the source lane separately tests variance with valid static
  function-pointer fields.
* `cargo clippy --locked -p vesper-alloc --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, and `git diff --check`: pass. The explicit
  forgotten-owner test narrowly allows `clippy::forget_non_drop`, because
  forgetting this move-only owner is a required lifecycle scenario and the
  owner intentionally carries no Rust destructor.

An initial test incorrectly expected successful release of a zero-capacity
owner to return chunk reservation immediately. Existing Region semantics keep
chunks region-owned until reset/drop; the final test checks released slot and
used bytes, unchanged retained reservation, and separate reset/drop cleanup.
No production change or cap/assertion relaxation was needed.

Native runtime evidence establishes the actual tested lifecycle and logical
row behavior. This lane makes no Miri, hosted, performance, SIMD, parser,
compiler or formal refinement success claim.
