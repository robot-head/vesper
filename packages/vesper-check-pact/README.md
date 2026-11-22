# @vesper/check-pact

Trait resolution of the Vesper checker, in Vesper (self-hosting wave SH2,
lane 13, #1185). It ports `src/pact_frontend.rs` and
`src/pact_frontend/{mono,builtin_traits}.rs` onto the tables of
[`@vesper/check-data`](../vesper-check-data/README.md). Rust stays the
default checker.

Part 1 is the declaration pass for a package without dependencies:

| File | Rust |
|---|---|
| `src/decl.ves` | the trait, `impl`/`model`, `use model`, bound, ADT and effect declarations as flat tables, and their decoder |
| `src/tyops.ves` | `as_projection`, `contains_projection`, `mentions`, `Frontend::conform_type`, `check::compatible`, `key_segment`, `row_arg` |
| `src/frontend.ves` | `Frontend::collect_local`, `validate_trait`, `validate_conformance`, `compare_signature`, `collect_bound`, `trait_key`, `assoc_cycle`, and the failures of `lower_bindings` |
| `src/lib.ves` | the entry `check_pact_declarations` |

`tests/self_host_check_pact.rs` checks it against
`pact_frontend::declaration_pass` on the corpus, the trait checker tests and
their mutants. The report is
[self-host-check-pact.md](../../docs/parallel-results/self-host-check-pact.md).
