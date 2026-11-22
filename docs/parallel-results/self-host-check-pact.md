# Self-hosting SH2 lane 13: trait resolution through the Pact port

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1185. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/13-pact.md`](../agent-prompts/self-host-sh2/13-pact.md).
Rust stays the default checker, and no Rust code was deleted.

Part 1 is the declaration pass; part 2a (below) swaps it into the
hybrid build. The lane ports about
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
| Swap point (part 2a) | `src/pact_frontend.rs`: `DeclarationPassTable`, `install_declaration_pass`, `rust_declaration_diagnostics`, `Frontend::with_base` |
| Hybrid module (part 2a) | `crates/vesper-hybrid/{modules/pact/**,port/pact.rs,build.rs,Cargo.toml,BUILD.bazel,src/hybrid.rs,tests/differential.rs}`, `.github/workflows/hybrid.yml` (divergent step), `//:check_pact_package` |
| Shared encoder (part 2a) | `tests/common/pact_words.rs` |
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

## Part 2a: the hybrid swap

The declaration pass now runs from Vesper in the hybrid build
(`crates/vesper-hybrid/modules/pact`, library `hybrid_pact`). Rust stays
the default compiler, which installs nothing and behaves as before.

**Swap point.** `pact_frontend::DeclarationPassTable` with one entry,
`diagnostics(module, base_world, identity)`, installed by
`install_declaration_pass`; `RUST_DECLARATION_PASS` is the Rust table
(`rust_declaration_diagnostics`). `check_package` now builds the base
world once and hands it to both the table and `Frontend::new`
(`Frontend::with_base`), so the swap adds no second `World::collect`.
For a package without dependencies, non-empty Vesper diagnostics reject
the package (with empty bindings, which no caller reads on failure); a
clean answer leaves collecting, the bindings and the Pact session to Rust.
A module with a literal over 64 bits stays in Rust.

**Encoder.** The part-1 test's encoder moved to `tests/common/pact_words.rs`
(on `check_words::Enc`, the format the other checker swaps use), shared by
the test and the hybrid, with the text decoder.

| Check | Result |
| --- | --- |
| `declaration_pass_matches_rust`, `unoptimized_jit_agrees` (shared encoder) | 0 of 1,874 cases differ |
| `check_package_agrees_through_the_swap_point`: the checker tests and their mutants, `check_package` diagnostics compared as values with the Vesper pass installed | 0 differ |
| Hybrid differential harness (`check`, `emit ir`, `emit object` over 553 files) | 0 of 2,212 runs differ |
| `pact_module_runs_from_vesper` | Vesper answers the shapes example and rejects `overlap_rejected.ves` |
| `VESPER_HYBRID_STATS=time` over the corpus (30 packages) | 0 answers differ |
| `divergent-pact` (every trait package gets a made-up E0600) | the harness must fail (Hybrid workflow step and Bazel divergent build) |

No port-harness shard entry: shard 7 runs `crates/*` ports, and hybrid
modules are tested by the Hybrid compiler workflow, as for every other
checker swap.

**Numbers.** Release build, Linux x86-64, 4 cores, the 33 corpus worlds the
pass runs on, ABBA best of 6 (`CHECK_HARNESS_SPEED=1 cargo test --release
--test self_host_check_pact -- --ignored`), both sides behind the swap point
with the base world already built:

| Side | What is timed | Best of 6 | Ratio |
| --- | --- | --: | --: |
| Rust | `rust_declaration_diagnostics`: `Frontend::with_base` (it clones the base world it owns, collects, validates and builds the binding DTOs) | 446 ms | 1 |
| Vesper, as the hybrid calls it | encoding, the pass, decoding | 17.7 ms | 0.04x |
| Vesper, the pass alone | decoding the input and the pass | 3.4 ms | 0.01x |

The Rust figure includes cloning the base world, which `check_package`
avoids by moving it, so the ratio flatters Vesper; the like-for-like
claim is only that the swap's whole cost is small. The hybrid does not get
faster from it yet: on a clean answer Rust still runs `Frontend::new` for
the bindings and the session (part 2b). Whole corpus, debug hybrid against
the debug default (`tests/differential.rs`, every swap on): 42.60 s against
41.20 s (1.034x). `VESPER_HYBRID_STATS=time` over the corpus (debug): 30
packages, encode 14.7 ms, Vesper 4.0 ms, decode 0.4 ms against 48.7 ms of
debug Rust.

## What is left

- **Part 2b: the session.** Lower the declarations to `@vesper/pact`
  programs directly (no `vesper-pact-binding` DTOs) and run coherence, so
  a clean answer no longer needs Rust's `Frontend::new` and the swap
  replaces Rust work instead of checking ahead of it.
- **Part 3: call resolution** (`Elaborator`: query contexts, `select`,
  `normalize`, `TraitCalls::resolve`) through `@vesper/pact`'s solver.
- **Part 4: monomorphization and the builtin traits** (`mono.rs`,
  `builtin_traits.rs`).
- **Dependencies.** `add_dependency` and cross-package trait references
  (the corpus checks single packages; the checker tests cover the
  diagnostics with mutants only).
- The seed (`vesper-pact-seed`) and `vesper-pact-binding` stay until parity.
