# Independent rewrite witness adversarial evidence (#234)

This lane owns only `tests/rewrite_witness.rs`,
`src/optimize/adversarial_tests.rs`, and this report. It uses the published
rewrite42 contract, including the real private installation seam and the
canonical candidate value-span amendment. It does not change the checker,
producer, optimizer traversal, AST, integer implementation, dependencies, or
baseline fixtures.

The 15 public tests bind actual parsed and checked source islands to malformed
certificate DTOs. They cover all four closed integer kinds; contextual literal
metadata; missing, inferred, generic, and mixed types; full-range unsigned
constants; checked overflow; unknown rules and versions; unused malformed
records; term/proof topology; substitution shape; ordered equality endpoints;
earlier-only exact totality witnesses; actual intrinsic origins; variable and
callee swaps; canonical candidate spans; every limit field; expanded AST
occurrences; and complete maximal-island retention without child fallback.

The 11 private tests obtain genuine producer proposals and call the actual
`install_proposal` seam used by optimizer traversal. Refusal checks compare the
entire original expression, including every span. Mutations include certificate
versions, unknown rules, unused invalid terms/proofs, cycles, endpoints, kinds,
values, substitutions, self-referential guards, synthetic intrinsic origins, and
a copied span from a real unrelated checked `u8` value. Resource attacks spend
the carried producer budget before installation, require preflight refusal of
an expanded shared DAG, and retain excessive capacity in short names,
operators, callee names, two-element argument vectors, and empty generic type
vectors. Genuine positive controls exercise folding, actual wrapping rewrites,
high-`u64` fold-only handling, and congruence.

## Execution evidence

The writer ran `rustfmt --edition 2024` on both owned Rust files and
`git diff --check`; both passed. Per the coordinator-only build contract, this
writer did not run Cargo, rustc, tests, Clippy, or use a compiler build cache.

The coordinator executed the actual integrated public targets at root
`9b7299e`: **15/15 independent public tests passed**, together with the checker
lane's 12/12 tests; zero failures or ignored tests. The coordinator supplied
`/tmp/rewrite42-checker-tests.log`, which this lane inspected.

The first actual coordinator library run produced 61 passing and seven failing
tests. Five owned genuine-proposal tests refused before their mutation with
`WitnessError::Limit`; two producer-owned tests also failed. The producer owner
is correcting conservative default accounting. These failures are retained as
positive-control failures, and the independent expectations have not been
weakened. Final private-test and strict-lint evidence is pending the
coordinator's rerun; this report does not claim those checks passed.

## Review boundary

Focused independent source inspection found the checker paths for all-term
validation, ordered proofs, totality guards, candidate provenance, and
checked/wrapping arithmetic consistent with the frozen finite profile after
the coordinator's candidate-span and retained-capacity hardening. This is
adversarial testing and source review, not a formal refinement proof or a claim
about optimizer islands outside the declared profile.

Owned test commits are `f20b08b`, `a932baa`, and `a1c1ded`. The bounded shared-DAG
regression deliberately expands beyond the default output cap while remaining
small enough to fail safely if the materialization guard regresses.
