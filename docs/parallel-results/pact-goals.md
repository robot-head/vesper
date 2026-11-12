# Pact canonical trait goals — lane report

Lane: Pact canonical trait goals (design §16.2–16.3). Owned paths: `crates/vesper-pact/**`, this file.
Branch: `claude/project-thread-2yotx7`, based on `origin/work` at `3be6891`. Not merged into `work`.

## What was built

`crates/vesper-pact` is a standalone crate (temporary `[workspace]` table, nested `Cargo.lock`) with:

| Module | Content |
| --- | --- |
| `ids` | Stable numeric ids: `PackageId`, `TypeCtorId`, `TraitId`, `EffectId`, `ImplId`, `ModelId`, `AssocIdx`. |
| `term` | One term language for templates, environments and queries: `Ty`, `ModeTerm`, `EffectRow`/`EffectLabel`, `Brand`, `GenericArg`, `TraitRef`, `Projection`, `Goal`. Variables are `Var::Bound` (declaration parameter), `Var::Placeholder` (rigid parameter in scope) or `Var::Infer` (query existential). |
| `program` | Versioned DTOs (`SchemaVersion` 0.1, `ProgramDto`, `QueryDto`, `Env`) and validation into `Program`. |
| `infer` (private) | Substitution, resolution, first-order unification of types, modes, brands and effect rows (open tails, shared fresh tail). |
| `canon` | Canonical goals: inference variables renumbered by first occurrence; placeholders canonicalized per top-level query. |
| `solver` | Tabled solver with cycle detection, provisional answers and fixed-point iteration; five-valued `Outcome`. |
| `derivation` | Proof trees (elaboration evidence) and an independent structural checker. |
| `coherence` | Covered-first orphan rule, pairwise overlap of canonical impls, well-formedness of associated values. |
| `mode` | `mode(τ)` as a join of atoms and the decision procedure for `mode(τ) ⊑ m`. |
| `diag` | Name-free sorted diagnostics with stable codes `PACT0001`–`PACT0103`. |

No parser, checker, CLI, macro or root-workspace file was touched.

### The fragment

Goals:

* `Implements { trait_ref, brand }` — `Self: Trait[args]` through a model brand.
* `ProjectionEq { projection, ty }` — associated-type equality `<Self as Trait[args] via brand>::A == ty`.
* `ModeBound { ty, bound }` — `mode(ty) ⊑ bound` on `copy ⊑ affine ⊑ linear`.

Trait parameters can be types, modes (`m: mod`), effect rows (`r: eff`) or brands. Associated types carry a modal bound (which may mention the trait's mode parameters, e.g. `type Elem: mod m`) and trait bounds. Type constructors declare their mode as `base ⊔ mode(args[i]) for i in joins`, which is how the checker's existing `k_i ⊑ m_i` field kinding can be summarized at the boundary.

Effect rows are sets keyed by effect identity, plus an optional tail variable. Labels are `Effect { effect, args }` or lexical `Capability(var)` labels. Lexical capabilities appear as `Capability` placeholders and are canonicalized like any other placeholder.

### Resolution

1. **Normalization.** The obligation driver normalizes each registered goal: every projection (innermost first) becomes a fresh variable `?v` with an obligation `ProjectionEq(p, ?v)`. Unification only sees rigid projections afterwards. Rigid projections are compared syntactically and are not treated as injective: two different ones that still contain inference variables are `Ambiguous`.
2. **Canonicalization and tabling.** Every nested goal is canonicalized and looked up in a table keyed by `(interned environment, CanonicalGoal)`. Answers are canonical responses: values for the query's binders, plus a derivation over fresh response binders.
3. **Candidate assembly.** The goal's brand decides which candidates are considered.
   * `Named(m)`: only model `m`. A model of another trait gives `NoSolution(ModelTraitMismatch)`.
   * Brand placeholder `M`: only environment assumptions with that brand.
   * `Canonical`: environment assumptions, plus alias bounds when the self type is a rigid projection. If none applies, canonical impls are tried.
   * Unresolved inference brand (implicit resolution): the innermost `use model` scope containing a model whose head unifies wins and binds the brand to `Named(m)`. Two such models in one scope give `Ambiguous(ConflictingScopedModels)`. With no matching model, the brand binds to `Canonical`. Named models are never found by canonical search.
4. **Projection candidates.** Equality assumptions are tried first. Next, if the trait holds through the environment, the projection is rigid. Otherwise the impl/model value is used, and projections inside that value are normalized and recorded.
5. **Combining.** If any candidate is unsupported or overflows, the whole goal is unsupported or overflows, because its answer might differ. Several distinct answers, or a proof alongside an ambiguous candidate, give `Ambiguous`. An unresolved self type gives `Ambiguous(SelfTypeUnknown)` immediately. The solver never picks an impl just because it is the only one that exists.

### Cycles

A goal found on the active stack is a cycle.

* If every goal on the cycle path is an `Implements` goal of a trait declared `coinductive`, the provisional answer is "holds" (identity substitution). The derivation records a `CoinductiveHypothesis` leaf, and `Solution::proof_kind` is `Coinductive`; the verdict string is `proven-coinductive`.
* Any other cycle is inductive. Its provisional answer is `NoSolution(InductiveCycle)`, i.e. least fixed point. A cycle through even one inductive goal is inductive.
* Cycle heads are re-evaluated until the answer stops changing. The budget is `max_fixpoint_iterations` (16); exceeding it gives `Overflow(FixpointIterations)`.
* Results that used a provisional answer of a goal still on the stack are not tabled. A head reached through both kinds of cycle in one evaluation gives `Unsupported(MixedCycle)`.
* Unbounded non-cyclic recursion stops at `max_depth` (default 48) with `Overflow(Depth)`. A total step budget gives `Overflow(Steps)`.

`Outcome` is five-valued: `Proven(Solution)`, `Ambiguous(Ambiguity)`, `NoSolution(Failure)`, `Unsupported(UnsupportedReason)`, `Overflow(OverflowKind)`. Only `Proven` is a success. Coinductive proofs, inductive-cycle failures, unsupported reasoning and indeterminate overflow are separate variants or reasons.

### Coherence

* **Orphan rule.** A canonical impl is legal when its trait is local, or when scanning `(Self, T_1, …, T_n)` (type arguments only) reaches a local constructor before any uncovered impl parameter. Parameters nested inside foreign constructors are covered.
* **Overlap.** For each pair of canonical impls of a trait, the heads are unified with renamed parameters. Unifiable heads overlap. This includes modal and effect arguments: `Container[mod copy]` and `Container[mod affine]` for `Bits` are disjoint, and `Source[eff [Net, Cancel]]` and `Source[eff [Net]]` are disjoint, but an open-row `Source[r]` overlaps both.
* **Named models** are exempt from both checks.
* **Well-formedness.** Each impl and model is checked under its own where-clauses (parameters become placeholders, where-clauses become assumptions). Each associated value must satisfy its modal bound and its trait bounds.

### Derivations

`check_derivation(program, env, derivation)` re-checks a proof without search. For each step it verifies:

* impl/model heads and where-clause children instantiate exactly, with correct argument kinds;
* brands match their source (`Canonical` for impls, `Named(m)` for models);
* assumptions exist in the environment;
* associated values match after replaying the recorded normalizations;
* rigid projections have a rigid self type;
* mode bounds hold when recomputed;
* every coinductive hypothesis closes a cycle (up to renaming of inference variables) made only of coinductive `Implements` goals.

Every proof in the test suite is re-checked this way. Tampered derivations are rejected: wrong impl, missing child, wrong brand, and a coinductive hypothesis used inside an inductive `Clone` cycle.

## Public API

```rust
// Boundary DTOs
pub const SCHEMA_VERSION: SchemaVersion;              // 0.1
pub struct ProgramDto { schema, packages, type_ctors, effects, traits, impls, models }
pub struct QueryDto { schema, env: Env, binders: Vec<ParamKind>, goal: Goal }
pub struct Env { placeholders: Vec<PlaceholderDecl>, assumptions: Vec<Goal>, model_scopes: Vec<Vec<ModelId>> }
impl Program { pub fn new(ProgramDto) -> Result<Program, Vec<Diagnostic>> }
pub fn validate_query(&Program, &QueryDto) -> Result<(), Vec<Diagnostic>>;

// Solving
impl<'p> Solver<'p> {
    pub fn new(&'p Program) -> Self;
    pub fn with_config(&'p Program, SolverConfig) -> Self;
    pub fn solve(&mut self, &QueryDto) -> Result<Outcome, Vec<Diagnostic>>;
    pub fn stats(&self) -> SolverStats;  pub fn table_len(&self) -> usize;
}
pub enum Outcome { Proven(Solution), Ambiguous(Ambiguity), NoSolution(Failure), Unsupported(UnsupportedReason), Overflow(OverflowKind) }
pub struct Solution { binders, var_values, derivation: Derivation, proof_kind: ProofKind }
impl Failure { pub fn render(&self) -> String }      // deterministic explanation tree
pub fn canonicalize_query(&QueryDto) -> Result<(Env, CanonicalGoal), DuplicateLabel>;

// Coherence and certificates
pub fn check_coherence(&Program) -> Vec<Diagnostic>;  // sorted
pub fn check_orphan(&Program, &ConformanceDecl) -> Result<(), String>;
pub fn conformance_env(&ConformanceDecl) -> Env;
pub fn check_derivation(&Program, &Env, &Derivation) -> Result<(), DerivationError>;
pub fn diag::render(&[Diagnostic]) -> String;
```

All DTO types implement `serde::{Serialize, Deserialize}`. The JSON form is pinned by fixtures (`tests/fixtures/world.json`, `tests/fixtures/fold_source_query.json`). A test fails if the format drifts; re-bless with `PACT_BLESS=1` after bumping `SCHEMA_VERSION`.

## Validation

Run from `crates/vesper-pact` on this branch:

```sh
cargo fmt -- --check                                  # clean
cargo test --locked                                   # 47 passed, 0 failed
cargo test --locked --release                         # 47 passed, 0 failed
cargo clippy --locked --all-targets -- -D warnings    # clean
cargo doc --no-deps                                   # no warnings
```

Toolchain: cargo 1.97.0. Dependencies are `serde 1.0.229` (derive) and `serde_json 1.0.154`, the same versions as the root `Cargo.lock`.

Test coverage (`tests/solver.rs` 33, `tests/coherence.rs` 6, `tests/dto.rs` 5, unit 3):

* **Recursive goals.**
  * Coinductive `Send` for nested `List`: proven, marked coinductive.
  * Coinductive cycle with a failing side obligation: no solution.
  * Inductive self-cycle `Loop`: `InductiveCycle`.
  * Cycle `Send → Clone → Send` through an inductive goal: classified as inductive.
  * Polymorphic recursion: `Overflow(Depth)`. Step budget: `Overflow(Steps)`.
  * Repeated cycle queries give identical results.
* **Equivalent canonical queries.**
  * Renamed and unused placeholders, renamed inference variables and capability labels all share a key.
  * The second solve of an alpha-equivalent query does zero new evaluations, and answers come back in each caller's placeholder numbering.
  * Variable sharing is kept apart (`Pair[?0, ?0]` ≠ `Pair[?0, ?1]`).
* **Negative and coherence cases.**
  * Blanket overlap, open-row overlap, a covered parameter versus an uncovered one, the foreign/foreign orphan case.
  * Modal and effect arguments that are disjoint.
  * Associated values that violate a mode bound or an alias bound.
  * Exact snapshots of the sorted diagnostics, re-run byte-identical.
* **Distinct named models.**
  * Canonical and named `Ord` give different evidence.
  * Named models are invisible to canonical search.
  * `use model` scoping: none, single, inner shadows outer, conflicting models in one scope, models of other traits.
  * A brand naming a model of the wrong trait fails.
  * Brand-stamped `CacheBTree` merge is accepted for the same brand and rejected for mixed brands. The brand is inferred from the other argument.
  * Placeholder brands are separate from canonical.
* **Modal and effect arguments.**
  * Mode argument inference; ambiguity between two modal impls.
  * The mode lattice: a definite violation dominates unknowns; placeholder bounds.
  * Modal projection bounds.
  * Row inference, open-row extension, a rigid row tail from the environment.
  * Merging duplicate `Fail` labels fails closed with `Unsupported`.
* **Associated-type equalities.**
  * Impl values and value mismatch.
  * Adapter values that contain projections (`Fused`), together with row inference.
  * `S: Source[r1, Item = T]` assumptions, as in `fold_source`.
  * Rigid projections; alias bounds.
* **Deterministic diagnostics.**
  * An exact rendered failure tree, identical in a fresh solver and in a warmed one (cache hits).
  * Exact validation and coherence diagnostic snapshots.
* **DTOs.**
  * JSON round trip against the checked-in fixtures; a query read from a fixture solves.
  * Schema-version rejection.
  * Malformed declarations: arity, kind, scope, unbound or unconstrained parameters, head projections, duplicate labels, non-dense ids.
  * Malformed queries.

## Integration requirements

1. **Workspace registration.**
   * Remove the `[workspace]` table and `crates/vesper-pact/Cargo.lock`.
   * Add `crates/vesper-pact` to the root `members` and `default-members`.
   * Optionally move `serde`/`serde_json` to `{ workspace = true }`. The versions already match.
   * Delete `crates/vesper-pact/.gitignore` if the root ignores nested `target/` directories.
2. **Stack size.** The solver recurses on the native stack. The default `max_depth = 48` was measured to fit a 2 MiB test-thread stack in debug builds; depth 96 overflowed. Callers that raise `max_depth` must also raise the thread stack size, or the coordinator can add an explicit stack-growth dependency later.
3. **Lifetime.** A `Solver` borrows one `Program`, and its table is valid only for that program. Rebuild the solver (and run `check_coherence`) whenever the declaration set changes. For Salsa, the natural key is one solver per `ProgramDto` revision.

## Adapter specifications (not implemented in this milestone)

**Checker → DTO** (`src/check.rs` world metadata).

* **Ids.** Assign dense ids in a deterministic order, for example by resolved `DefId`. Each Loom package node becomes a `PackageId`. Each ADT, primitive and tuple arity becomes a `TypeCtorId`. Traits, impls, models and effects each get their own id space.
* **Mode schemes.** Derive each constructor's `ModeScheme` from the declared modal kind and the generic parameters that occur in its fields, consistent with E0105 (`k_i ⊑ m_i`).
* **Templates.** Generic parameters of an impl or model become `Var::Bound` in declaration order. Every parameter must occur in the head.
* **Environments.** Function generics in scope become placeholders. `T: mod copy` becomes `PlaceholderDecl::mode_bound`. `where` clauses become `Env::assumptions`. `Trait[r, Item = T]` desugars to an `Implements` plus a `ProjectionEq`. `use model` blocks become `model_scopes`, outermost first. Lexical capabilities become `Capability` placeholders.
* **Queries.** Checker metavariables become `Infer` binders. An implicit trait use sends `Brand::Var(Infer)`. An explicit `via M` sends `Named(M)`.
* **Outcome handling.**
  * `Proven`: elaborate dictionaries from `Derivation` (conformance source plus args) and apply `var_values` to the metavariables.
  * `Ambiguous`: defer the obligation and retry after further unification; if it is still ambiguous at the end, report "annotations needed".
  * `NoSolution`: render the `Failure` tree with names and spans.
  * `Unsupported` and `Overflow`: hard errors. Never accept the program.
* **Default brands.** Brand-stamped collections need their defaulted brand made explicit before the DTO is built: `CacheBTree[K, V]` becomes `CacheBTree[K, V, Canonical]`.

**Parser.** Add surface syntax for:

* `trait` with `type A: mod m + Bound`, and mode, row and brand parameters;
* `impl[...] Trait[...] for T where ...`;
* `pub model Name: Trait for T`;
* `use model Name;`;
* `via Model` at use sites, and brand parameters (for example `M: brand`).

Lower these to resolved HIR items with `DefId`s. Pact never sees the text.

**CLI/LSP.** `vesper check` should run `check_coherence` over the full resolved Loom graph and print the `PACT01xx` codes with names. An optional `vesper pact explain <goal>` could print `Derivation::render()` and `Failure::render()`.

**Forge macros.** Expansion runs before DTO construction. Generated impls and models are ordinary declarations, and coherence runs after expansion. Hygienic `DefId`s map to the same id tables. Pact has no macro-specific logic.

**SemVer (§9) and Lean (§11).** The overlap check gives SemVer the classification "new blanket canonical impl overlaps a possible downstream impl". Making that usable needs a public overlap query, currently internal. Lean extraction can map `ConformanceSource::Model` to a named instance and `Impl` to a class instance.

## Ambiguities in the design and decisions taken

1. **Orphan wording.** "First uncovered type constructor" is read as Rust RFC 2451: the first local type must come before any uncovered parameter, and parameters inside foreign constructors are covered. Non-type arguments are ignored.
2. **`Canonical[Trait, K]`** is represented as a nullary `Brand::Canonical`. The trait and key are fixed by the position the brand occupies (the constructor parameter or the goal), so equality is preserved.
3. **Lexical model scopes versus canonical impls.** The innermost matching `use model` shadows the canonical impl for implicit resolution. Conflicts within one scope are ambiguous. Scopes never reach impl where-clauses, because template brands must be explicit.
4. **Environment versus impl precedence.** Applicable where-clauses and alias bounds take precedence over impls, as in rustc's parameter-environment preference. A projection whose trait holds through the environment, with no equality assumption, is rigid.
5. **Coinduction.** Coinduction is a per-trait flag. Cycles that pass through any non-coinductive goal, including projection and mode goals, are inductive.
6. **Mode and effect arguments are invariant.** There is no submoding or row subsumption when matching trait arguments; modal subsumption is expressed only through `ModeBound` goals.
7. **One label per effect key.** Each effect key may appear at most once per row. `Fail[A]` together with `Fail[B]` is outside the fragment and fails closed.
8. **No negative reasoning in overlap.** Heads that unify overlap even when their where-clauses could never both hold.
9. **Unannotated placeholder mode is `linear`.** This is the conservative upper bound. Note that `vesper_core::Mode::default()` is `Affine`, so the adapter must state bounds explicitly where `affine` is intended.
10. **No impl-only inference.** An unresolved self type is always ambiguous; the solver never infers the self type from the fact that only one impl exists.

## Limitations

* This is not a formal soundness result. The derivation checker is structural, and its mode recomputation shares `mode::decide` with the solver.
* Not supported:
  * supertrait elaboration;
  * higher-ranked bounds;
  * specialization;
  * negative impls;
  * automatic structural auto-trait derivation (auto traits need explicit impls);
  * well-formedness goals for types (for example `CacheBTree[K, V, M]` implying `K: Ord via M`);
  * `dyn` object safety and vtables (§16.2 second-class dynamic dispatch);
  * const generics;
  * capability inference variables;
  * projections in impl heads.
* Ambiguous answers carry no partial substitutions. The caller has to re-query after more inference.
* Environment assumptions are not normalized. A non-rigid projection inside an assumption only matches syntactically, which can lose a proof but cannot invent one.
* Cycle participants that depended on a provisional answer are recomputed rather than tabled. The worst case is exponential, bounded by the step budget.
* Fixed-point re-evaluation with a changing answer is implemented, but no test exercises it. With coherent impls, ground inductive cycles have a single candidate, and no fixture produces a provisional answer that changes.
* Impl/model well-formedness assumes only `ModeBound(param, const)` where-clauses. Other mode where-clauses are not assumed, which can produce spurious well-formedness errors but never false acceptance.
* Canonicalization does not fully normalize the order of capability labels when renaming placeholders. The effect is a possible cache miss, never a wrong answer.
* There are no performance measurements or claims. Candidate contexts are cloned per attempt.
