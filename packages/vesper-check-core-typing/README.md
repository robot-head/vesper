# @vesper/check-core-typing

The core typing of `src/check.rs` in Vesper (self-hosting wave SH2, lane 7,
#1179), built on [`@vesper/check-data`](../vesper-check-data/README.md).
Rust stays the default checker; nothing here replaces Rust code. The lane
lands in several PRs; this is what has been ported so far.

| File | Rust (`src/check.rs`) | Vesper |
|---|---|---|
| `src/ints.ves` | `numeric::IntegerKind::{fits, default_for}` | `Lit` (sign + u64 magnitude), `fits`, `default_fits`, `lit_cmp` |
| `src/relations.ves` | `compatible`, `effects_include`, `refine_annotation`, `unresolved`, `union_effects`, `join_types`, `infer` | same names (`union_effects` is `union_rows`) |
| `src/patterns.ves` | `is_uninhabited`, `matrix_covered`, `Checker::irrefutable`, `Checker::coverage` | `Pats` (flat pattern table), `Matrix`, `is_uninhabited`, `matrix_covered`, `irrefutable`, `arms_cover` |
| `src/lib.ves` | | the differential's tasks |

Types are ids of the check-data arena: Rust's `clone` of a type is a copy
here and `==` is one compare. Patterns are rows of one flat table; pattern
0 is the wildcard that specialization fills missing payloads with.

Integer literals are a sign and a 64-bit magnitude until `i128` lands (SH2
lane 2, #1174); literal typing (`src/integer_widths/literal_typing.rs`)
waits on it.

## Tests

`tests/self_host_check_core_typing.rs`, with lane 4's harness, over every
checked corpus world:

- `relations_match_rust`: `compatible` both ways, `unresolved`,
  `join_types` (type and E0200 count) and `refine_annotation` of type pairs
  (each type and its four next types in `Debug` order, its mirror, and
  literals against literals, integer kinds and world types), and `infer`
  over every generic function's signature.
- `patterns_match_rust`: `is_uninhabited` of every world type, coverage of
  every `match` (and its mutants: each arm dropped, a constructor pattern
  missing a field), `irrefutable` of every arm and destructuring `let`.
- `mutants_make_coverage_fail`: the mutants make E0212 and refutable
  patterns occur.
- `unoptimized_agrees`: the JIT with the optimizer off gives the same texts.
- `speed_relations_and_patterns` (ignored; `CHECK_HARNESS_SPEED=1`): ABBA.
