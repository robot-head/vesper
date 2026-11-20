# Self-hosting SH2 lane 13: trait resolution through the Pact port

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1185. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/13-pact.md`](../agent-prompts/self-host-sh2/13-pact.md).
Rust stays the default checker, and no Rust code was deleted.

This is part 1 of the lane: the declaration pass. The lane ports about
5,100 lines (`src/pact_frontend.rs`, `src/pact_frontend/mono.rs`,
`src/pact_frontend/builtin_traits.rs`) and needs several PRs; "What is
left" lists the rest.

## Summary

- **`@vesper/check-pact`** (`packages/vesper-check-pact`), on
  `@vesper/check-data`: the declaration pass of
  `pact_frontend::check_package` for a package without dependencies. That
  is `Frontend::collect_local` with `validate_trait`,
  `validate_conformance`, `compare_signature`, `collect_bound`,
  `trait_key` and `assoc_cycle`, and the failure paths of
  `lower_bindings` (`Frontend::dto`, `unique_assoc_trait`, `where_args`,
  and the binding crate's `TypeDto::from_source` / `ArgDto::from_source`).
  About 900 lines of the Rust.
- **Census design kept.** Declarations are flat tables (`decl.ves`) whose
  lists are ranges into shared pools; types are check-data arena ids;
  every `String`-keyed `BTreeMap` of the Rust (`names`, `traits`, the base
  world's functions) is a symbol-indexed table. `Frontend::conform_type`,
  `check::compatible`, `effects_include` and the impl-key encoding run on
  the hash-consed arena (`tyops.ves`).
- **Differential: 0 differences on 1,874 cases**, 2,795 diagnostics
  compared by code, span and message (help and labels are compared too;
  this pass attaches none).
- **Swap point.** `pact_frontend::declaration_pass` (`#[doc(hidden)]`) runs
  the pass on its own: the reference of the differential, and the place
  the hybrid swap of part 2 replaces.

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-pact/{Loom.toml,README.md,src/*.ves}` |
| Rust reference (additive) | `src/pact_frontend.rs`: `declaration_pass` |
| Test | `tests/self_host_check_pact.rs` |
| Bazel data and timeout | `BUILD.bazel` |
| Scoreboard row | `docs/maintenance/self-hosting-plan.md` |
| Report | `docs/parallel-results/self-host-check-pact.md` |

`@vesper/check-data` is unchanged: the pass's inputs are its own task
section (decl.ves documents it), not World fields, so no other lane's
input moves. `src/pact_frontend.rs` is outside the lane's exclusive paths;
the hunk only adds a function, and the PR flags it.

No port-harness shard entry: like lane 4, the package has no crate or
`vesper build --lib` object yet, and its test is a root-package test that
"Test all targets" and Bazel run. It joins shard 7 with the hybrid module.

## The differential

`declaration_pass` returns the module the pass collects from (after the
function-object, `dyn` and builtin-trait lowerings, so the builtin `Eq`,
`Ord` and `Hash` declarations are in it) and its diagnostics. The test
encodes that module and the base world (`World::collect` of the module
without trait items), runs `check_pact_declarations` and compares texts.

| Cases | Count |
| --- | --: |
| Corpus worlds the pass runs on (of the harness corpus) | 29 |
| The trait checker tests' sources (`tests/source_traits.rs`, `tests/source_models.rs`, `tests/bounded_generics.rs`) and the rejected trait fixtures | 39 |
| Mutants of both (one changed declaration each, distinct inputs only) | 1,806 |
| **Cases differing** | **0** |

The mutants change one declaration of each kind: conformance methods
(dropped, duplicated, renamed, pass, projections, parameter and result
types, arity, generics, effects, labels, open rows), associated types
(dropped, duplicated, unknown, mode, cycles, unknown projections),
trait references (unknown, from a dependency, arity, mode), self types the
binding cannot lower, where-clauses, overlapping conformances, models
named like types, trait declarations (generics, receivers, duplicates,
`Self` parameters, new assoc types and methods), `use model`, every bound
failure, and projections in ADT fields and effect operations. All ten codes
of the pass fire (E0102, E0600-E0607, E0613) and the test checks each of 43
message stems appears. The unoptimized JIT gives the same texts.

The test takes about 4 minutes unoptimized on a 4-core host (most of it
checking the corpus) and about a minute in release, so Bazel gives it the
`long` timeout.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
only `src/` change is the additive `declaration_pass` in
`src/pact_frontend.rs`, which nothing in the compiler calls. The
`bootstrap-stage1` workflow is dispatched on the branch; the PR links the
run.

## Numbers

Release build, Linux x86-64, 4 cores, the 29 corpus worlds
(`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_check_pact -- --ignored`):

| Side | What is timed | Best of 6 |
| --- | --- | --: |
| Rust | `declaration_pass`: the lowerings before the pass, `World::collect`, collecting, validating and building the binding DTOs | 1.03 s |
| Vesper | decoding the input and the pass (validation and the lowering checks, no DTOs) | 1.7 ms |
| Encoding | the test's encoder, which also runs the Rust pass | 1.30 s |

This is not a parity number: the Rust side is dominated by work the Vesper
side does not do (the builtin-trait prelude and `World::collect` over
large packages). A like-for-like ABBA and callgrind count of
`Frontend::new` against the Vesper pass comes with the swap in part 2,
which times both behind the same swap point.

## Compiler gaps

None new. The workarounds this part needed are known gaps:

1. **A `const` cannot be imported from another module** (check-data gap 1):
   each module repeats `K_*` and `NONE32`.
2. **No element borrows of sequences that own sequences** (census C5, SH2
   lane 1, #1173, not merged): every declaration is a copy row with list
   ranges into shared pools (the census state-pool pattern), not a struct
   holding its own `ChunkSeq`s.

## What is left

- **Part 2: the session and the hybrid swap.** Lower the declarations to
  `@vesper/pact` programs directly (no `vesper-pact-binding` DTOs) and run
  coherence; swap the pass into `crates/vesper-hybrid` as the non-default
  module `pact` behind `declaration_pass`; like-for-like ABBA and callgrind;
  the shard 7 entry.
- **Part 3: call resolution** (`Elaborator`: query contexts, `select`,
  `normalize`, `TraitCalls::resolve`) through `@vesper/pact`'s solver.
- **Part 4: monomorphization and the builtin traits** (`mono.rs`,
  `builtin_traits.rs`).
- **Dependencies.** `add_dependency` and cross-package trait references
  (the corpus checks single packages; the checker tests cover the
  diagnostics with mutants only).
- The seed (`vesper-pact-seed`) and `vesper-pact-binding` stay until parity.
