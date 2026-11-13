# Independent opaque-key arena model (#225)

Read both published collection/arena contracts before writes. Owned changes are
only `crates/vesper-alloc/tests/arena_model.rs` and this report. Production keys
and identities remain opaque: every tested key comes from actual insertion;
there is no forge, counter mutation, private header inspection or shared edit.

Four deterministic scalar-oracle runs execute 2,048 mixed steps each, for 8,192
steps across two arenas sharing one Region plus foreign keys from a second
Region. The oracle records business IDs, scalar values and original caller-owned
allocation identities; it does not reproduce the arena's free list, initialized
entry array or generation allocator. Insert/remove/mutate/disjoint-mutate,
reserve, foreign/stale lookup and invalid pair operations are mixed. Each step
audits the selected arena's complete live identity/value set; periodic and final
audits cover the other arena and all retained removed keys.

Payload has no Copy, Clone, Default, Debug, Eq or Hash implementation. Its Box is
a caller-owned transitive allocation, not an implementation payload fallback.
Key Copy/Clone/Eq/Hash/Debug and owner/error Debug compile without payload trait
bounds. An interleaved live region allocation forces arena relocation; original
keys, hashes, values and transitive allocation addresses remain valid, while the
resident payload address changes. Old addresses are diagnostic integers only;
the tests never dereference a stale pointer or rebuild a key from one.

Other independent cases cover 256 empty/reuse cycles without revival; reversed
pair order and invalid-before-alias errors; vacancy-aware reserve without extra
region storage or generation consumption; foreign/stale owner validation before
key checks including reserve0; checked reservation refusal; and exact non-Copy
value/owner return from foreign insert and consuming release failures.

A bounded cyclic graph traverses and updates actual weak edges through keys,
uses disjoint mutation, removes a node, reuses its slot, verifies old edges stay
stale until explicitly repaired, and traverses the repaired cycle. The removed
object remains caller-owned after arena cleanup. No garbage-collection or
permanent-reference behavior is inferred from that host example.

## Actual local evidence

Using the exclusive `/tmp/collections41-arena-model-target`, one Cargo job,
debug0 and incremental0:

- The final `cargo test --locked -p vesper-alloc --all-targets` passes 94 tests in
  six suites, zero failures or ignores: four private implementation unit tests,
  nine independent arena model tests, twenty-three existing collection tests,
  ten flat-table core tests, thirty-six region tests and twelve slab tests.
- `cargo test --locked -p vesper-alloc --doc` passes 25 doctests: four executable
  positive examples and twenty-one compile-fail checks, including the source
  implementations' borrowing/invariance contracts.
- `cargo clippy --locked -p vesper-alloc --all-targets -- -D warnings`, workspace
  formatting and whitespace checks pass on the final owned test source.

Dependencies only: original e1b0e4c/5fcb1b3/4e37bbe are local copies
8dbd3fc/1ec861c/52be35b; approved hardening dde30ee/da02a88 are local copies
57980ca/df9a1ae. None of those copies is model-owned work. The model test commits
are 5389200 and incremental 81e322d; the latter adds one failure-preservation
case, without replacing or duplicating the former.

These are actual native Linux results. Coordinator Miri, other independent
model/lifecycle suites and hosted platforms are separate evidence. The tests do
not establish global ID/generation exhaustiveness, source-language region
branding or linear modes, exception-safe user destructors, absence of transitive
heap allocations, throughput/RSS bounds or a formal proof.
