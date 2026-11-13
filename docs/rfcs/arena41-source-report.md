# Affine region slot arena: source claim224

Frozen published contracts be2d342 preceded implementation. Owned source
commits7b77d08 and dde30ee change only new
`crates/vesper-alloc/src/region/arena.rs`; this lane additionally owns
`tests/arena_core.rs` and this report. Imported flat implementation/follow-up
and shared registration are coordinator-approved dependencies, not owned edits.
No existing RegionVec/slab/tiled/allocator metadata, error enum, Cargo, build,
workflow, compiler or CLI path changed. No push, PR or merge performed.

The new module forbids unsafe code. `RegionSlotArena<T>` owns one checked
RegionVec with a Header at index0 and ordinary Vacant/Occupied enum entries.
Arena identity, live count, free-list head and generation counter are resident;
the outer owner retains no duplicate state. T supports ordinary affine Rust
ownership, including non-Copy/non-Clone/non-Default Drop values. Existing
RegionVec relocation and Rust enum drop glue own/finalize exactly occupied
values on normal paths; remove transfers the actual T to its caller.

Opaque manually-Copy keys impose no payload Copy/Eq/Hash/Debug bounds. Keys
retain invariant payload type, process-unique arena ID, checked slot and
generation, with diagnostic-only inspectors. All access checks the region owner
before arena identity/index/generation; no-op paths cannot bypass it. Pair access
validates both keys before alias classification and uses safe split_at_mut to
return references in requested order.

Identity minting uses a checked compare-exchange loop; local header generations
use the same prospective checked successor. MAX is never issued or wrapped.
Failed insertion preserves actual input value/allocation and does not consume
a generation. Remove/get/pair/reserve/release remain usable after generation
exhaustion. Reserve discounts existing vacancies, reserves memory only and
preserves ordinary RegionVec watermark/growth behavior. Usable capacity excludes
the header and clamps to the u32 slot limit as published.

Pure constructor layout/slot/byte checks precede a private inline-never
allocation/header helper. Consequently unsupported representable Entry<T>
types do not enter a constructor frame containing a full Entry temporary.
The complete actual entry type is bounded to64KiB; ZST values and alignments
above64 are permitted when their actual Layout and RegionConfig support them.
This bounds resident entries, not transitive T-owned heap or total process RSS.

Actual Linux validation, one job/debug0/incremental0, exclusive cache
`/tmp/collections41-arena-source-target`:

- `cargo test -p vesper-alloc --all-targets`:93passed, zero ignored;
  72old allocator tests,10flat-core dependency tests,8arena-core tests and
  3new private arena unit tests.
- `cargo test -p vesper-alloc --doc`:25passed (4runnable and21compile-fail),
  zero ignored across this actual
  combined source. Arena checks owner cloning, nonstatic payload rejection,
  projection borrowing and key/owner invariance between valid static function
  pointer types; a positive ordinary coercion controls the variance regression.
- `cargo clippy -p vesper-alloc --all-targets -- -D warnings`:passed, without
  lint allowances. Checked CAS avoids the current toolchain's deprecated
  fetch_update spelling while retaining the frozen nonwrapping policy.
- Workspace fmt and diff whitespace checks:passed.

Public tests cover actual non-Copy/drop ownership and hashable keys for payloads
without Eq/Hash/Clone/Debug; remove/reuse and vacancy-aware reservation; reversed
pair order and invalid-before-alias errors; protected failure retaining String
allocation plus unchanged stats/generation; consuming owner recovery and stale
no-op validation; row/byte overflow; a real greater-than16MiB Entry-type
constructor refusal without creating its payload; ZST/128-aligned fields; and
key stability across owner movement/growth. Private tests exercise local MAX-1/
MAX identity/generation counters without touching process-global counters,
actual exhausted-arena input preservation with still-usable live values, and
checked malformed slot/generation rejection without a public key forge API.

Logs: `/tmp/arena41-source-tests.log`, `/tmp/arena41-source-doc-final.log` and
`/tmp/arena41-source-clippy-final.log`. Independent model/lifecycle claims225/226
and coordinator strict-provenance Miri/combined validation remain separate.
This lane claims no hosted/platform runtime or Miri result, static source-region
brand, Vesper linear mode, RegionRef lowering, Pin, GC, Lean proof or throughput.
Panic/user-callback/physical-OOM boundaries remain those of existing RegionVec.
