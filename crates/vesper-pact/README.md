# vesper-pact

Canonical trait goals and tabled trait resolution for Vesper Pact (design §16.2–16.3).

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

See [`docs/parallel-results/pact-goals.md`](../../docs/parallel-results/pact-goals.md) for the supported fragment, the design decisions, the adapter specifications and the limitations.

```sh
cargo fmt -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

The `[workspace]` table and the nested `Cargo.lock` are temporary until the crate is registered in the root workspace.
