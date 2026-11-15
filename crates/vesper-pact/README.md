# vesper-pact

Canonical trait goals and tabled trait resolution for Vesper Pact ([manuscript §§6.2–6.3](../../docs/manuscript/README.md)).

This crate is standalone. The compiler hands it a versioned, id-only `ProgramDto` and asks `QueryDto`s about it.

* `Solver::solve` returns one of five outcomes:
  * `Proven`, with a checkable `Derivation`;
  * `Ambiguous`;
  * `NoSolution`, with a deterministic `Failure` tree;
  * `Unsupported`;
  * `Overflow`.
* Coinductive proofs are marked with `ProofKind::Coinductive`.
* `check_coherence` enforces the covered-first orphan rule and the no-overlap rule for canonical impls, and checks associated values. Named models are exempt.
* `check_derivation` validates certificate terms and re-checks a proof without search. Malformed ids, arities, variable kinds and instantiation arguments return `DerivationError`.

Run `check_coherence` and reject its diagnostics before accepting solver evidence for a declaration set. `Program::new` validates the DTO shape; it does not enforce coherence. In an incoherent program, overlapping impls can produce identical substitutions, which the solver treats as one answer even when their evidence differs. A checked derivation establishes that its recorded declaration applies, not that it is globally unique.

```sh
cargo fmt -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```
