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
* `check_derivation` re-validates a proof without search.

See [`docs/parallel-results/pact-goals.md`](../../docs/parallel-results/pact-goals.md) for the supported fragment, the design decisions, the adapter specifications and the limitations.

```sh
cargo fmt -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

The `[workspace]` table and the nested `Cargo.lock` are temporary until the crate is registered in the root workspace.
