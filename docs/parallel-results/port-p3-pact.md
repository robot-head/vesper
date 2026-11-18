# Port P3a lane 3: vesper-pact in Vesper

Claim: #1038. Base: `main` at 3c075a4. Lane prompt:
`docs/agent-prompts/port-p3/03-pact.md` (plan PR #1044).

## Summary

- `@vesper/pact` (`crates/vesper-pact/vesper/`, 13 modules, about 7,600
  lines) runs the whole engine: program and query validation, placeholder
  and inference-variable canonicalization, unification, the tabled solver
  with coinductive and inductive cycles, modal bounds, projection
  normalization, named models and lexical model scopes, coherence (orphan,
  overlap, well-formedness) and the independent derivation checker.
- Beacon's seed pattern (#901) is used:
  - `vesper-pact` builds the Rust code by default. Under the feature
    `vesper-port`, build.rs links the port and the public engine entry points
    forward to it.
  - `vesper-pact-seed` (`seed/`) is the frozen Rust build. The root `vesper`
    package, `vesper-pact-binding` and `vesper-pact-bridge` depend on it by
    rename (the bridge too, because it links the compiler and would otherwise
    see two `vesper_pact` crates).
  - Rust stays the default and nothing is deleted, by the owner's parity
    rule.
- All 54 crate tests pass on the Rust build, the port and the seed. The
  bridge's 34 session tests also pass with the port underneath
  (`cargo test -p vesper-pact-bridge --features vesper-pact/vesper-port`,
  run once locally; CI runs the bridge on the seed).
- Speed (ABBA, release, 9 epochs): the port is **1.10x** the Rust build's
  time on `world.json` and **0.33x** (3x faster) on a generated 64-type
  world; instruction counts are 1.29x and 0.50x. Details below.

## The `&dyn Fn` goal builders

The Rust solver passes `goal_of: &dyn Fn(&InferCtxt) -> Goal` closures that
rebuild a candidate's goal with its brand resolved in the candidate's
context. Every closure is one of two shapes, so the port passes the goal
node itself (`Implements` or `ProjectionEq`), and `goal_of` in
`solver.ves` resolves its brand in the context. This is the closed set of
goal builders the lane prompt asked for: there is no closure, trait object
or generic parameter left.

## API inventory

| Item | Port-backed build |
| --- | --- |
| Terms, ids, DTOs, `Outcome` and its parts, `Derivation`, `Step`, `Diagnostic` (+ `Display`, `Debug`, serde) | Shared Rust types (unchanged) |
| Folds (`fold_*`, `instantiate_*`, `VarFolder`), `Failure::render`, `Derivation::render` | Rust (they work on the Rust types) |
| `Program::new` | Encodes the DTO once, `program_check` validates it; the words are kept in the `Program` |
| `Solver::new`, `with_config` | `solver_new`: the port decodes the program and owns the table and stack |
| `Solver::solve` | `s_solve` (the answer table persists across calls, as in Rust) |
| `Solver::stats`, `table_len` | Read from the port's handle |
| `check_coherence` | `coherence` |
| `canonicalize_query` | `canonicalize` |
| `check_derivation` | `derivation` |
| `check_orphan`, `conformance_env`, `validate_query` | Rust: these take a single declaration or query, not a program run. Their logic is also ported, as part of `coherence` and `solve`. |
| New: `IMPLEMENTATION`, `live_allocations` (port only) | `"vesper"`/`"rust"`; the port's leak counter |

The serde DTOs stay Rust. `src/port.rs` converts a call's inputs to words
once and rebuilds the results; the encodings are documented there and in
`vesper/src/program.ves` and `vesper/src/out.ves`.

## Design

- **Hash-consed terms.** Every term, goal, derivation and answer is a node
  in one interning arena (`term.ves`). Equal terms are the same id, so
  equality and table keys are O(1), and each node caches flags (has bound
  variables, placeholders, inference variables, projections, an unsorted
  row), which let folds skip whole subterms. This is why the port beats the
  Rust build on the larger world: the Rust code clones and hashes `Goal`
  trees for every key and comparison.
- **Contexts.** An `InferCtxt` is a `ChunkSeq[u64]` of `kind | value` slots;
  cloning it for a candidate copies one sequence.
- **Candidates.** The Rust build keeps each candidate's `(InferCtxt,
  Derivation)` and builds the canonical response in `combine`. The response
  reads only that candidate's context, so the port builds it as soon as the
  candidate succeeds and keeps `(source, response or error)` on a phase
  stack (`solver.ves` explains why that is equivalent, including the
  duplicate-label case).
- **Errors.** Duplicate labels, unsupported goals and malformed certificates
  come back as typed results, as in Rust. The one Rust panic the port can
  reach (a query naming an undeclared placeholder, which only
  `canonicalize_query` accepts unvalidated) is reported as a word and
  panics in the wrapper with a message. The port has no `unsafe`.

## Unit tests

`src/infer.rs` has four in-crate unit tests (`open_rows_share_a_fresh_tail`,
`rigid_and_closed_rows`, `tail_bound_by_label_arguments_is_not_rebound`,
`occurs_check_rejects_infinite_types`). They test the Rust `InferCtxt`,
which stays compiled in every build, so they are kept as they are. The
port's row and occurs-check unification is covered by the public tests that
exercise the same cases (`open_row_impl_extends_rows`,
`rigid_row_tail_from_environment`, `effect_row_argument_is_inferred`,
`unbounded_non_cyclic_recursion_overflows`). New: `tests/implementation.rs`
checks that the feature selects the build.

`tests/dto.rs` reads its fixtures relative to the working directory, so it
now also looks one directory up, where the seed package's tests find them.

## Divergence checks

One edit per ported module, in `build.rs` (`DIVERGENT`) and mirrored in
`BUILD.bazel`. `port/test-port.sh` (run by `scripts/test-port-harness.sh`)
builds them all at once and asserts that each guarding test fails and
`capability_labels_canonicalize_by_first_occurrence` still passes. Each edit
was also run alone to check that its own test catches it.

| Module | Edit | Caught by |
| --- | --- | --- |
| term | goals `Implements` are no longer shared | `missing_impl_is_no_solution_with_deterministic_explanation` |
| fold | duplicate labels are never detected | `merging_duplicate_effect_labels_fails_closed` |
| infer | row extension keeps labels the other row has | `open_row_impl_extends_rows` |
| canon | single-model scopes are dropped | `implicit_brand_uses_lexical_model_scopes` |
| mode | an equal mode constant is no longer below itself | `mode_bounds_follow_the_lattice` |
| render | type ids print as `ty#n` | `associated_value_must_satisfy_alias_bounds` |
| diag | diagnostics sort in reverse | `malformed_declarations_are_rejected_with_sorted_diagnostics` |
| program | schema minor 2 is accepted | `schema_version_is_enforced` |
| solver | the step budget is 1,000 steps too large | `step_budget_overflow_is_indeterminate` |
| query | placeholders are not restored in answers | `query_dto_from_fixture_solves` |
| coherence | the orphan rule's local-trait test is inverted | `coherence_violations_are_reported_deterministically` |
| deriv | extra where-clause proofs are accepted | `tampered_derivations_are_rejected` |
| out | the coinductive flag is inverted | `coinductive_cycle_is_proven_and_marked` |

Two coverage gaps showed up while choosing the edits. Removing "an
assumption that applies shadows the impls" from `implements`, and dropping
the coinductive-cycle counter, both pass every test. Neither changed a
result here, but a test for each would be worth adding to the crate.

## Speed against Rust

`examples/abba.rs`, release builds on one Linux cloud machine. A pass is
`Program::new`, `check_coherence`, one `Solver` solving the workload, and
`check_derivation` of every proof. The Rust arm is the seed's `abba_rust`
example, the port arm `vesper-pact --features vesper-port`. Each epoch is
A B B A windows of 20 passes; 9 epochs. Instruction counts are
`valgrind --tool=callgrind` over one whole process (`once`: building the
workload plus one pass; for `world.json` that includes parsing the JSON in
both arms).

| Workload | Rust | Port | Port / Rust (wall, median) | Instructions, Rust | Instructions, port | Ratio |
| --- | --- | --- | --- | --- | --- | --- |
| `world.json`: 27 queries (nested `Vec`/`Option`/`List` for `Clone`/`Send`/`Ord`, a placeholder query, an inference query, `fold_source_query.json`) | ~640 us/pass | ~700 us/pass | **1.10x** | 5.98 M | 7.69 M | 1.29x |
| Generated world, 64 generic types, 65 impls, 128 queries of depth 12 | ~50 ms/pass | ~16 ms/pass | **0.33x** | 444 M | 222 M | 0.50x |
| Same, 256 types (2 passes, one window) | 404 ms/pass | 187 ms/pass | 0.46x | | | |

The 2x gate of the port plan passes on both workloads. The port is close
to parity on the small fixture, where fixed costs dominate: encoding the
DTO and rebuilding `Outcome` and `Derivation` trees from words in the
wrapper, and decoding the program in `solver_new` again. On larger programs
hash-consing wins.

By the owner's rule Rust stays the default until the port matches it in
both functionality and speed; on the small fixture it is still 10% slower,
so this PR switches no dependent and deletes nothing. The compiler links
the seed either way until Stage 2.

## Compiler gaps

Each was worked around in the port; repros are minimal packages with
`capability_ceiling = []` checked with `vesper check`.

1. **Argument loans block reading the same value in another argument**
   (E0301). `put(inout s, get(in s, 0))` is rejected with "access to 's'
   conflicts with active argument loan 's'". The port hoists about 50 such
   arguments into locals. Two-phase borrows exist for receivers (#1004) but
   not for plain `inout` arguments.
2. **No `continue`** (E0201 "unknown name 'continue'"). Loop bodies were
   restructured into `if/else`.
3. **`self::m::g()` path calls do not parse** ("expected RBrace, found
   ColonColon"); every name must be imported with `use self::m::{g}`.
4. **Integer literals in `if` expressions default to `i64`**:
   `let x = if c { 1 } else { 2 }; x` in a function returning `u64` fails
   with E0200. The literal type is not inferred from later use, so the port
   annotates `let x: u64`.
5. **`-` on `u64` raises `Fail`** (E0401 in a package without `Fail`), and
   so does `narrow_u8`; `wrapping_sub`/`wrapping_u8` are used instead. `/`
   and `%` by a constant do not.
6. **`linear` is reserved**, so it cannot name a local (E0002 "expected
   pattern").

## Files

- `crates/vesper-pact/vesper/` (the port), `src/port.rs` (the wrapper),
  `build.rs`, `port/binding.rs` (generated), `port/test-port.sh`,
  `examples/abba.rs`, `tests/implementation.rs`.
- `crates/vesper-pact/seed/` (the seed package and its hand-written
  `BUILD.bazel`).
- Forwarding: `src/{program,solver,coherence,canon,derivation,lib}.rs`.
- One line each in `scripts/test-port-harness.sh` and the `fixtures()` skip
  predicate of `tests/fixtures/self_host/driver.rs`.
- Root patch (own commit): workspace members and `Cargo.lock`.
- Seed switch (own commit): the root `vesper-pact` dependency,
  `vesper-pact-binding` and `vesper-pact-bridge`.
