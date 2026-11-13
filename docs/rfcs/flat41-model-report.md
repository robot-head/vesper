# Independent flat-table scalar model (#222)

The published reviewed collections/flat contracts and issue #222 preceded this
test lane. Only `crates/vesper-alloc/tests/flat_model.rs` and this report are owned
changes. No runtime implementation, registrations, allocator metadata, shared
tests, Cargo/build/CI or other lane files were edited. Runtime execution waited
for the coordinator's coherent source/export commits and approved source fixes.

Thirteen independent tests use the actual public Region flat-table API. The
oracle is a scalar Vec of numeric identity/ownership records, not another hash
table or a copy of the implementation's probing algorithm. Stored/incoming keys
and values are affine Rust payloads with Drop; they intentionally implement none
of Clone, Default or Debug. Key equality includes the stable hash input and
logical ID, while separately observed ownership serials distinguish equal but
different incoming keys. Actual keys always satisfy the Eq/Hash contract.

The tests cover:

- Three hundred initial inserts force repeated growth, then 2400 deterministic
  insert/replace/remove/get/mutable-get/reserve operations compare with the
  independent scalar model. Every resident key/value serial and data value is
  checked, including replacements returning the actual previous key object.
- Controlled full-hash collisions preserve surviving probe chains through early
  tombstones. Separate actual DefaultHasher searches construct equal h2 tags
  with different full hashes; those candidates must skip Eq. A full16lane
  starting group wraps from the last to first group, exercising the global
  rather than per-group live threshold. Iterator order agrees with ascending
  occupied control group/lane tags.
- A two-group sequence leaves every lane DELETED with no EMPTY control. Missing
  get/remove terminates and a subsequent insertion reuses a vacancy. The test
  observes actual initial/removed controls rather than inventing private tag
  constants or hash outputs stable across toolchains.
- Explicit reserve/rehash and iteration run with panic traps armed in Hash/Eq;
  no user callback or payload destruction occurs. Growing insertion performs
  exactly the one incoming hash and the14necessary collision search comparisons,
  with no repeated callback during ownership relocation.
- Injected Hash/Eq panics in shared/mutable lookup, remove, insert and disjoint
  lookup leave entries, controls, owner identity and RegionStats unchanged.
  Invalid cross-region/stale owners fail before callbacks. These are the
  contract's before-mutation callback boundaries, not a general Rust-panic
  rollback guarantee.
- Disjoint lookup preserves query order, rejects two distinct equal query
  objects resolving to the same occupied slot, permits equal missing queries,
  and resolves a second query before exposing mutable values. Zero-sized keys
  and/or values retain logical alias and exactly-once Drop semantics without
  assuming distinct pointer addresses for zero-sized values.
- One injected value Drop panic during clear leaves the surviving full-collision
  probe chains searchable and the table reusable. Complete subsequent clear
  restores EMPTY controls; replacement/removal/release/region destruction
  account for every created owned key and value exactly once.
- Iteration provides exact remaining length/size_hint and fused exhaustion,
  invokes no Hash/Eq, and remains valid after its owner leaves scope because
  it borrows Region independently. Ending an owner scope does not destroy the
  remaining resident entries; eventual Region destruction does.

Actual local Linux validation used exclusive
`/tmp/collections41-flat-model-target`, one Cargo job, debug0/incremental0:

- `cargo test -p vesper-alloc --locked`: all123tests across eight nonempty
  suites pass with zero failures or ignores, including all13owned model tests,
  the source lane's10flat-core tests, existing allocator/collection/slab suites,
  arena counter tests and actual positive/compile-fail documentation tests.
- `cargo clippy -p vesper-alloc --locked --all-targets -- -D warnings`, owned
  rustfmt and whitespace checks pass.

The actual dependency imports were source `e1b0e4c`→`3d9fce3`, arena source
`5fcb1b3`→`5930700`, registrations `4e37bbe`→`33f250e`, arena source fix
`dde30ee`→`988c92f`, and flat source fix/core tests `da02a88`→`33d91a0`.
These are dependency copies, not model-owned commits; do not import them again
as model work. The root coordinator owns combined lifecycle/layout/example,
strict-provenance Miri and hosted-platform evidence. This lane did not run Miri
or claim other-platform execution, SIMD/SwissTable instruction behavior, source
linearity, whole-process allocation/RSS bounds, throughput or formal refinement.
The ownership claim remains reserved through reviewed merge.
