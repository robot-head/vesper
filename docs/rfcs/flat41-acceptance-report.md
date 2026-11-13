# Independent flat-table lifecycle and physical layout (#223)

Public claim #223 and the published `collections41`/`flat41` contracts preceded
all writes. Owned files are only `tests/flat_region.rs`, `tests/flat_layout.rs`,
`examples/flat_table.rs`, `FLAT.md` and this report under the claimed paths.
No implementation, shared allocator, Cargo/dependency, BUILD, workflow or core
files are edited by this lane.

Dependency copies are source `e1b0e4c` → `e815b7f`, arena `5fcb1b3` → `ec4a0b3`,
public exports `4e37bbe` → `28fe25c`, and approved fixes `dde30ee` → `1b1a893`,
`da02a88` → `0af97df`. None is acceptance-owned work.

Nine independent lifecycle tests use actual finite Regions, a counting/failing
backing and non-Copy affine payloads:

- Replacement and removal transfer the actual previous key/value objects with
  distinct drop identities. No early drop occurs, and final region reclamation
  cannot drop the transferred pairs again.
- Refused backing growth returns the original incoming value, keeps all rows,
  control address, owner region and RegionStats unchanged, and permits a later
  successful retry. A finite reservation refusal occurs before another backing
  request. Constructor refusal similarly preserves region accounting.
- Within-capacity premark edits, replacement, removal and insertion survive
  rewind. Protected growth preserves every row/stat and returns ownership;
  replacement at the live threshold remains valid. Scratch allocations become
  stale while the surviving table can grow after consuming the watermark.
- Cross-region/reset-stale validation covers no-op reserve, missing lookup,
  disjoint lookup, iteration, control bounds, clear and insertion. Panic-enabled
  query Hash/Eq callbacks must never run on these invalid owners.
- Actual reused control bytes cannot revive an old allocation serial. A
  postmark table remains stale after relocation and covering rewind.
- Failed by-value release returns the owner, which remains usable in its proper
  region. Owners dropped at scope exit or explicitly forgotten with ManuallyDrop
  do not discard payloads. Reset/rewind/drop finalize all remaining values exactly
  once, including a forgotten table that grew repeatedly.
- A single injected value destructor panic during clear leaves the remaining
  probe chains searchable, the recorded length accurate and the table reusable.
  Later successful clear/release produces exactly one drop per value.

Three physical-layout tests inspect only public borrowed controls/entries and
actual backing ranges:

- Controls have 16 bytes and the required alignment. Four initial groups are
  contiguous at a constant actual native stride, with used region bytes equal
  to exactly four strides. No hidden physical group reservation is accepted.
- Primitive, array, ZST and 64/128-aligned native payload shapes are exercised.
  For non-ZST keys/values, actual entry addresses lie inside the current region
  group buffer and a live backing allocation. No private layout casts, pointer
  reconstruction or ZST pointer-inequality assumption is used.
- Occupied control tags match independent DefaultHasher upper-seven-bit tags.
  Inserts within capacity request no extra backing chunk; growth preserves the
  native stride and initialized count without per-entry payload allocation.
- Invalid configuration precedes shape/capacity checks. A supported group near
  64 KiB succeeds; an unsupported complete group, capacity/configuration refusal,
  alignment refusal and reserve arithmetic overflow preserve accounting, with
  allocation attempts checked where relevant.

The backing fixtures' narrowly scoped unsafe trait implementations forward the
same live pointer/Layout to SystemBacking. They add counters/address observation
and injected refusal, never payload access or pointer reinterpretation. The new
flat-table implementation itself retains its separate forbid-unsafe boundary.

The actual example starts with 96 entries, demonstrates replacement/disjoint
value mutation, then compares 512 deterministic mixed operations against an
independent BTreeMap after every step. It finishes with 55 rows, eight groups and
128 control bytes. The reference map is an external oracle, not table storage.

Native validation in exclusive `/tmp/collections41-flat-region-target`, one
Cargo job/debug0/incremental0:

- All 12 owned tests pass; all 97 package all-target tests in eight suites pass,
  zero failures or ignored tests. The final owner-lifecycle form is rerun.
- All 25 package doctests pass: four runnable examples and 21 compile-fail cases.
  The standalone `FLAT.md` usage snippet also executes successfully via rustdoc.
- Strict package all-target Clippy with `-D warnings`, workspace formatting and
  whitespace checks pass. The actual scalar-oracle example passes.

No runtime expectation, default or production cap was changed to obtain these
results. The coordinator runs strict-provenance Miri separately; native checks
do not stand in for that evidence. This lane makes no hosted/other-platform,
wire ABI, SIMD/Isa instruction-count, source linearity/brand, zero transitive
heap, throughput/RSS, Lean-refinement or performance-superiority claim.
