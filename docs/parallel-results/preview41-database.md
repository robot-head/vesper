# Explicit current-source macro preview database

Claim #152 owns the new `database::macro_preview` child, its narrow parent
registration, and `database_macro_preview` tests. Frozen APIs follow
`docs/rfcs/preview41-contracts.md` and amendment a450203.

`preview` reads the complete `SourceFile.text` input and calls the actual
`forge_frontend::expand_source` with the fixed default package and empty grants.
It does not call legacy parse/check or signature queries. The sealed snapshot
retains bounded original bytes and the real immutable outcome. Equality and
retention use Arc allocation identity, never cloned checked Worlds.

The fixed profile is source/expanded 128KiB, 1024 map entries, metadata 2MiB,
identifier 256 bytes and compiler work 128Mi. Actual tests showed that the initial
256KiB metadata proposal rejected tiny support-function programs before checking;
the coordinator published the 2MiB amendment before changing the implementation.
All other compiler limits remain unchanged. A separate prepaid 1Mi snapshot work
envelope covers the source retention/hash/association before copying. A constant
proof bounds that work for every permitted source length. Oversized legacy inputs
reach the compiler's byte refusal before snapshot hash/copy, retain no source or
hash, and receive a distinct opaque identity.

Repeated queries on an unchanged Salsa input reuse the real checked Arc. An
explicit `set_text`, even with equal text, creates a fresh input revision and
reruns compilation; session no-op handling owns avoiding that setter. No private
interned input, header shortcut, guest cache or approved-artifact cache was added.
Every changed helper body, macro definition, argument or comment observes the
complete source input. Ordinary `guarded_analysis` remains separate and rejects
the original macro-bearing source before and after successful preview.

Verification on Linux with Rust 1.99.1:

- `cargo test --locked -p vesper --test database_macro_preview`: six tests passed,
  none ignored. Actual Salsa execution events prove no legacy query involvement.
- `cargo clippy --locked -p vesper --lib --test database_macro_preview -- -D warnings`,
  `cargo fmt --check`, and `git diff --check` passed.
- Unchanged inputs reuse the actual Arc; forced revisions and changed argument,
  definition and Unicode comment bytes produce fresh current identities/maps.
- An unchanged helper header with `wrapping_add(x, 1)` replaced by `x + 1` reaches
  the real authority gate and fails under empty grants. That actual refusal has
  no expanded diagnostic candidate; it must not be represented as expanded spans.
- Real linear duplication fails at Check with E0303; real leakage fails with
  E0302. Repeated original argument spans have distinct expanded byte anchors.
- Malformed, unsupported generic macro callers, and oversized input refuse.

These are full-input compiler memoization tests, with no claim of per-function
incrementality, latency, resident-memory bounds, or original-source semantics.
No Wasmtime, Lean, native program, URI filesystem lookup or guest is run by the
preview query. Native multi-platform acceptance belongs to the integration lane.
