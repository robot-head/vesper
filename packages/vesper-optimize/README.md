# @vesper/optimize

The AST arithmetic optimizer of `src/optimize.rs` in Vesper (self-hosting
wave SH3, lane 10, #1249). Rust stays the default optimizer; nothing here
replaces Rust code.

| File | Rust | Vesper |
|---|---|---|
| `src/budget.ves` | `WitnessBudget` (`work`, `metadata`, `names`, `reserve`) | `Budget` and a model of each Rust vector's capacity |
| `src/input.ves` | the checker's inputs | `Tree` (an `@vesper/ast` `Ast` plus two capacity columns), `TypeRow`, `Cert` (`TermRow`, `ProofRow`) and their decoders |
| `src/witness.ves` | `src/optimize/witness.rs` | `bind_source`, `validate_terms`, `compare_input`, `compare_output`, `build_candidate`, `check_rule`, `verify` |
| `src/lib.ves` | | the entries `optimize_verify`, `optimize_decode`, `optimize_sizes` |

The search (the `egg` runner of `src/optimize.rs` and the producer half of
`src/optimize/certificate.rs`) moves onto `@vesper/egraph` (SH3 lane 8) in
a later part. `witness_lean.rs` stays in Rust until SH5.

## Same answer, same budget

The checker is a resource-bounded gate, so matching Rust means more than
accepting and refusing the same certificates. Every check, limit and
charge runs in Rust's order, and the differential compares the
`WitnessError` (as its position plus one) and all three spent counters.
Two things make that possible:

- Rust charges `metadata` for every vector growth by the capacity
  `Vec::try_reserve_exact` asks for (`max(required, 2 * capacity, 1)`), so
  `budget::reserve` takes the length of the Vesper sequence that plays the
  vector's part and a `cap` counter that models the Rust capacity.
- Rust charges `size_of` of the element types. The `S_*` constants in
  `witness.ves` are their values on 64-bit targets. `Term`, `Equality`,
  `Expr`, `Arg` and `Type` are checked against Rust by
  `rust_sizes_match_the_vesper_constants`; the private frame types are
  checked by the budget counters matching on every case.

The input carries what an AST does not keep: the capacities of the
candidate's strings and vectors and of the certificate's two vectors.

## Tests

`tests/self_host_optimize.rs` installs a recording table at
`src/optimize.rs`'s swap point (`install_optimize`), runs the Rust optimizer
over every checked corpus world, the optimizer's own fixtures and 400
generated integer islands, and compares every proposal, and about 70,000
mutants of them, with the Vesper checker. The report is
[docs/parallel-results/self-host-optimize.md](../../docs/parallel-results/self-host-optimize.md).
