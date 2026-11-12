# Actual formatter arena projection — issue #117

`Document::proof_model` projects the exact immutable private formatter arena
referenced by a current-owner DocId. It retains every constructor, payload and
expanded occurrence in document order. Public statistics and ordered comments
are read-only; model fields and constructors remain private, without Deserialize.
The model is document-algebra evidence, not checked source or a proof that Rust
rendering refines Lean. The separate source adapter supplies source association.

Before any payload clone or owned model allocation, an iterative scan validates
references and depth and charges expanded nodes, text, metadata and work. Shared
children count on every occurrence; exponentially expanded graphs cannot pass
by presenting a small unique arena. Scanner storage is paid before reservation.
Tree payloads, child storage, and separately retained comment strings/vector
storage are all included. Construction work is prepaid after the scan and before
copying. The owned tree's depth is at most the finite configured hard cap128;
the existing renderer and its work counters remain unchanged.

These are logical budgets, not allocator/RSS bounds or an OOM-recovery guarantee.
Owned strings/vectors use fallible reservation; bounded Box construction uses
Rust's ordinary allocator behavior. Unknown/foreign IDs and all rejected budgets
return no model. Existing documents remain usable after projection failure.

Actual formatter suite passes40tests across6suites, no ignores, using the real
new source and unchanged old formatting/macro/recovery suites. New coverage is
one constructor/wrapper unit test, seven public API tests and a private-model
construction compile-fail case. Cases include Unicode/CRLF, comment order,
shared occurrence statistics, equal numeric foreign IDs, abandoned document IDs,
exponential DAGs, aliased text bytes, separately retained comment metadata,
zero/above-HARD limits, and unchanged rendering after successful/failed projection.

The root owns crate registration; the initial local registration was uncommitted
wiring. With actual root source capture675b70a, emitter25ad710 and structured Lean
list support3287fa2 cherry-picked into the private worktree, the combined formatter
suite passes41tests across6suites, no ignores, including three private-construction
compile-fail cases. Strict all-target formatter Clippy passes with warnings denied;
owned rustfmt and diff checks pass. No production stub or broadened lint allowance
was introduced; the actual emitter consumes the crate-private model getter.
Verification uses only private /tmp/stylus41-recovery-target, one job/debug0/
incremental0. The coordinator's target cache is untouched.
