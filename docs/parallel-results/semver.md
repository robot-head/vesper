# SemVer compatibility library checkpoint

`crates/vesper-semver` implements a conservative, compiler-independent compatibility engine for design §9. Its serializable `Interface` DTO has separate schema and language-semantics versions, positional generic binders, stable nominal/capability identities, exported declaration contracts, and resolved external type kinds. Inputs must be trusted, resolved compiler exports; the crate is not a second source typechecker.

`compare(old, new)` returns a `Report` classified as Patch, Minor, Major, or Unknown. `is_compatible()` certifies only Patch/Minor reports with no pending adapters. Issues have a declaration/type path, formal rule name, reason, and a human-readable caller witness. Unknown schemas/types/binders or unsupported features fail closed even when both snapshots are identical. Serde rejects unknown fields and duplicate export, field, operation, or variant identities.

Implemented rules include contravariant generic modal bounds and function parameters; covariant function/result modes and result types; invariant mutable parameter types and nominal arguments; capability and row subset checks; both row-wide and per-capability resumption guarantees; projection subtree-prefix narrowing; public export removal/kind checks; closed/public struct contracts; exhaustive versus open enum extension; added-variant payload modal bounds; and open/sealed effect resumption changes. Effect operation signatures remain invariant even when operation names are unchanged. Empty projection sets are not interpreted as no access: the DTO requires explicit `Projection::All`, rejects empty path sets as ambiguous, and its subset helper defensively treats empty selections as full-root.

An inout-to-in change records the required old-signature reborrow adapter and remains non-compatible until an actual compatibility wrapper is published. There is no unchecked flag that makes the result compatible. Sink-to-borrow and shared-to-mutable/consuming changes are rejected, including potentially linear generic arguments.

## Validation

25 unit tests cover the rule matrix, higher-order variance, linear sink preservation, open enum linear payloads, open versus sealed effect guarantees, invariant operation signatures, lexical identities, modal/result metadata, unsupported binders, duplicate JSON keys, and the case where an already-affine row would otherwise hide a weakened capability.

Commands run:

```sh
cargo fmt --manifest-path crates/vesper-semver/Cargo.toml --check
CARGO_TARGET_DIR=/workspace/vesper/target cargo test --manifest-path crates/vesper-semver/Cargo.toml
CARGO_TARGET_DIR=/workspace/vesper/target cargo clippy --manifest-path crates/vesper-semver/Cargo.toml --all-targets -- -D warnings
```

## Boundaries and integration

The crate is registered in the root workspace and uses its shared lockfile. It depends only on serde, with serde_json used in tests. It does not modify existing compiler modules or provide a CLI command.

The DTO is an initial transport contract, not the canonical `.vhir` format. A compiler adapter must export resolved kinds, qualified stable IDs, binder scopes and already-validated projection trees; closure environments and source validity are not reconstructed here. Nested function signatures have explicit closed binder contexts; higher-ranked/captured generic binders and nominal effect-row arguments need a richer schema. Nominal modal analysis conservatively joins published kind bounds with argument modes, so phantom/conditional variance may cause false breaking reports. Public field and effect operation changes are deliberately invariant until their full bidirectional variance rules are implemented. The checker does not reason about trait obligations, ABI/layout compatibility, region reachability, SIMD profiles, FFI contracts, proof certificates, or behavior/termination.

No Merkle fingerprint is emitted. Interface equality is a source-contract comparison only; body/private-layout changes must still invalidate object derivations and dependent proof artifacts. A source MIF must not be reused as either an artifact cache key or a proof-validity key. Witness descriptions are not synthesized source and have not been submitted to the Vesper checker. There are no new Lean metatheorem claims.
