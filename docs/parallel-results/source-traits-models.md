# Source traits and models — lane report

Wave three, lane 1 ([prompt](../agent-prompts/wave-three/01-source-traits-models.md)), under the [frozen contracts](../rfcs/wave-three-contracts.md).

* Branch: `claude/project-thread-02wmf2` (the session's assigned branch; the prompt suggested `wave3/source-traits-models`).
* Base: `origin/main` at `1ffd0aec27dfcc24c5564585ae9068255490bf61`, a descendant of the required `02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6`.
* Edited only the assigned paths: `crates/vesper-core/src/ast.rs` (additive), `crates/vesper-syntax/**`, `src/check.rs`, `src/interface.rs`, new `src/pact_frontend.rs`, `crates/vesper-pact-bridge/**`, the new root-free crate `crates/vesper-pact-binding/**`, new `tests/source_traits.rs`, `tests/source_models.rs`, `examples/traits/**` and this file.
* **The branch needs the coordinator patch below to build.** Root `Cargo.toml`/`Cargo.lock` and `src/lib.rs` are coordinator-owned, so the module registration and the two new root dependencies are not committed. All results below were produced with that patch applied locally (uncommitted).

## Result

Source programs can now declare traits with associated types, canonical impls and named models, and select them with receiver calls, trait paths, model paths and lexical `use model` scopes. Every trait call is resolved by a Pact query against a coherence-checked session, and only re-checked, consumed `Proven` evidence selects a declaration. The selected method is a private generated function, and the call becomes an ordinary checked call of it. A second, ordinary checking pass over that fully elaborated module produces the `World` every backend consumes. No `ExprKind`, `Type`, Place-IR `Op`/`Terminator`, layout, arithmetic or ABI shape changed, and no backend file was edited.

`examples/traits/shapes.ves` (two traits, an associated type, a canonical impl, two named models, nested shadowing, `Trait::method` and `Model::method` paths, an `inout` receiver) runs on the evaluator and the JIT with the same result, `4304070304`, emits an AArch64 ELF object and extracts to Lean.

## Dependency direction

`vesper-pact-bridge` depends on root `vesper`, so root could not depend on it. I extracted everything in the bridge that does not need the compiler into a new crate:

```text
vesper  ──►  vesper-pact-binding  ──►  vesper-core, vesper-pact
vesper-pact-bridge  ──►  vesper, vesper-pact-binding
```

* `vesper-pact-binding` holds the former bridge modules `binding`, `error`, `identity`, `lower`, `modes`, `session` and `tables`, moved with `git mv`. They now read checked declarations through a `SourceWorld` trait (`adts`, `effects`, `mode`) instead of `World`. Root implements `SourceWorld for World` in `pact_frontend`.
* `SourcePackage::new(identity, Arc<dyn SourceWorld>, &Module)` replaces the bridge-internal use of `CheckedPackage`. `SessionBuilder::new(prelude)` takes the checker prelude explicitly.
* `CheckedPackage` stays in the bridge, above the compiler. The bridge re-exports the whole binding API, and its `SessionBuilder` wrapper keeps the old `new()`/`package(CheckedPackage, BindingsDto)`/`solver_config`/`lower`/`build` signatures. All 30 original bridge tests and the example pass unchanged.
* New: `CheckedPackage::source_bindings()` returns the bindings lowered from the package's own source declarations, and `CheckedPackage::source_package()`.
* `BridgeError::new`, `BridgeError::single` and `BridgeError::source` are now public, because the bridge constructs errors across the crate boundary.

## Accepted grammar

Keywords are contextual (lower-case identifiers in these positions), so no existing identifier became reserved.

```text
item        ::= ['pub'] trait_decl | ['pub'] model_decl | impl_trait | 'use' 'model' Upper ';' | <existing items>
trait_decl  ::= 'trait' Upper [generics] '{' { assoc_decl | fn_signature ';' } '}'
assoc_decl  ::= 'type' Upper [':' 'mod' mode] ';'                      // default bound: linear
impl_trait  ::= 'impl' [generics] trait_ref 'for' type [where] '{' { assoc_bind | function } '}'
model_decl  ::= 'model' Upper [generics] ':' trait_ref 'for' type [where] '{' { assoc_bind | function } '}'
assoc_bind  ::= 'type' Upper '=' type ';'
trait_ref   ::= ['@' package '::'] Upper [type_args]                   // package: dependency trait
where       ::= 'where' type ':' trait_ref { ',' type ':' trait_ref }
generics    ::= <existing> | name ':' trait_ref                        // impl/model: a where-clause
statement   ::= 'use' 'model' Upper ';' | <existing statements>
type        ::= '<' type 'as' Upper [type_args] '>' '::' Upper         // qualified projection
              | name '::' Upper                                        // shorthand, trait inferred
              | <existing types>
call        ::= expr '.' method '(' args ')'                           // receiver call
              | Trait '::' method '(' args ')' | Model '::' method '(' args ')'
```

Trait parameters are type parameters with modal bounds (`T: mod copy`) or effect rows (`r: eff`). Trait method signatures use `Self`, `Self::A` and the trait parameters. A `use model M;` applies to the rest of its enclosing block (the whole module at item level); later and inner uses shadow earlier and outer ones. Spans are kept on every declaration, associated type, binding, where-clause and use. The CST stays lossless.

The surface metadata is additive in `vesper-core::ast`: `Item::Trait(TraitDecl)`, `Item::Conformance(Conformance)`, `Module::model_uses`, `Module::trait_bounds` (both `#[serde(default)]`), plus `TraitRef`, `AssocTypeDecl`, `AssocTypeBinding`, `WhereClause`, `ModelUse`, `TraitBoundSyntax` and `World::elaboration: Elaboration` metadata. Projections in type positions use a reserved frontend-only encoding, `Type::Named("$projection", [self, trait, assoc])`. No source identifier can spell it. The frontend normalizes every occurrence or rejects the module, and a final scan rejects any residue in an accepted World (`E0612`).

## Supported static fragment

* **Declarations.** Traits with type and row parameters, associated types with modal bounds, and methods that take a `self` receiver (`in`, `inout` or `sink`). Canonical impls and named models may be generic over modally bounded type parameters, with where-clauses on impl/model parameters (row arguments in where clauses are lowered as rows, like those in a conformance head). Trait type parameters lower to kind-only Pact parameters; their modal bounds are enforced by the frontend (`E0606`).
* **Signature checking.** Each impl or model must give exactly the trait's methods and associated types. After substituting `Self`, the trait arguments and its own associated types, each method must match the trait signature: same arity, the same pass mode and place projections for every parameter, the same parameter and result types, and effects included in the trait's. Each associated type binding must satisfy its modal bound, and each trait argument its parameter bound. Then the generated function's body is typechecked against its signature by the ordinary checker, including ownership, effects, handlers, checked arithmetic and the abort/loan/one-shot rules.
* **Lowering.** Traits, impls and models become `TraitDto`/`ImplDto`/`ModelDto`. Identities are package-qualified `DefPath`s: traits and models by name; impls by an injective key built from the trait and self-type head, such as `Score.Point` or `Show.Box-0-T-2`. Never by span or allocation order. Pact `Program::new` and `check_coherence` gate everything, so orphan and overlap rejection is unchanged. Named models are never canonical.
* **Calls.** For `x.m(...)`, an inherent `T::m` keeps priority, so original method syntax is unchanged. Otherwise every trait declaring `m` is queried with `Implements(T, Trait[?args])` under the call's `use model` scopes. Exactly one proven trait must remain. `Trait::m(x, ...)` names the trait. `Model::m(x, ...)` queries with an explicit model brand. The consumed `Selection` must be a concrete impl or model of this package. Its generated function is then called with the receiver as the first argument (pass mode from the declaration). Method choice never uses the printed name. Receiver types come from places, literals, non-generic calls, struct literals and chained method calls (typed through the inherent method or conformance their own receiver selects).
* **Projections.** Projections in function signatures, `let` annotations, explicit call types and closure and handler types are normalized. `Self::A` inside a conformance uses that conformance's own binding. Any other projection is solved by Pact under the lexical model scopes in effect, and then instantiated from the selected declaration, so under `use model ByB;` the projection `<Key as Rank>::Weight` is `bool`. Explicit trait arguments (`<X as Convert[bool]>::Output`) constrain the selection goal; omitted ones are inferred. Methods of a generic inherent `impl[T] Box[T]` normalize with the impl's parameters in scope.
* **Generated functions.** They are named `impl$<key>$<method>` or `model$<Name>$<method>` (not spellable in source). They are recorded in `World::elaboration.generated` with `public: false`. Interface export omits them and exports each trait, impl and model as `Unsupported`, so no SemVer certificate can claim compatibility for them (tested: an interface with them is not compatible with itself).
* **Dependencies (API only).** `FrontendOptions::dependencies` adds already-written packages whose declarations join the coherence check. Source may name their traits as `@scope/name::Trait`. This is what makes the orphan rule observable from source.

Explicitly rejected, before any World is accepted: trait bounds on function generics or `where` clauses on functions (`E0607`, generic dictionary dispatch); calls on generic receivers with no static implementation (`E0607`); calls discharged only by a where-clause assumption (`E0607`); calls or projections selecting a dependency's implementation (`E0607`, no cross-package linking); generic trait or impl methods, static trait functions without `self`, labelled capabilities in trait signatures, projections in ADT fields or effect signatures (`E0607`); unresolved or escaping projections (`E0612`).

### Diagnostics

| Code | Meaning |
| --- | --- |
| E0600 | Invalid trait declaration: name clash, duplicate parameter, associated type or method, misplaced `self` |
| E0601 | Malformed conformance: unknown trait or package, arity, missing or extra methods or associated types, cyclic associated type bindings |
| E0602 | Method signature mismatch: arity, parameter or result type |
| E0603 | Pass mode or projection mismatch |
| E0604 | Effect not permitted by the trait signature |
| E0605 | Incoherent declarations: overlap, orphan, other Pact validation (Pact code in the message) |
| E0606 | Modal bound violated by an associated type or trait argument |
| E0607 | Outside the static fragment, including generic dictionary dispatch and cross-package dispatch |
| E0608 | No solution |
| E0609 | Ambiguous: several traits, several impls, or an underdetermined receiver |
| E0610 | Internal elaboration inconsistency |
| E0611 | Solver overflow (depth or step budget) |
| E0612 | Unresolved or escaping projection |
| E0613 | Wrong model brand, or an unknown model in `use model` |
| E0614 | Stale evidence (`pact_frontend::revalidate`) |

## Evidence and revisions

`pact_frontend::check_package(module, identity)` returns `FrontendChecked { checked, bindings, session, revision, evidence }`. Each `ResolvedCall` records the call span, generated function, selection and a serializable `Certificate`. `revalidate(session, certificate)` runs `accept_certificate` and `consume` again.

The session `Revision` hashes the lowered Pact program and tables, as before, plus a new BLAKE3 **declaration fingerprint** supplied through `SourcePackage::with_fingerprint`. The fingerprint covers types including fields, effects, traits, conformances and all function signatures. It ignores spans, comments, whitespace and order. It does not cover function bodies, because evidence selects declarations, not behaviour. As a result, a change to a trait, impl, model or source type invalidates evidence (`E0614`), even when the lowered Pact shape is unchanged, such as a new `i64` field. Reformatting does not, and an identical rebuild accepts the certificate. The bridge's `CheckedPackage` sets the same fingerprint. `check::check` (no package context) uses `@local/main@0.0.0`.

Evidence is consumed only inside the session that produced it. Nothing is cached across checks: each `check` re-solves every call. The revision identifies a declaration set. It is not provenance.

## Public API

```rust
// vesper::pact_frontend (new; needs the coordinator patch)
pub fn check_module(&Module) -> Checked;                 // what check::check calls
pub fn check_package(&Module, &PackageIdentity) -> FrontendChecked;
pub fn check_package_with(&Module, &PackageIdentity, &FrontendOptions) -> FrontendChecked;
pub struct FrontendOptions { pub solver: Option<SolverConfig>, pub dependencies: Vec<Dependency> }
pub struct Dependency { pub identity: PackageIdentity, pub module: Module }
pub struct FrontendChecked { pub checked: Checked, pub bindings: BindingsDto, pub session: Option<CoherentSession>, pub revision: Option<Revision>, pub evidence: Vec<ResolvedCall> }
pub fn revalidate(&CoherentSession, &Certificate) -> Result<(), Diagnostic>;
pub fn declaration_fingerprint(&Module) -> String;
pub fn has_frontend_syntax(&Module) -> bool;
pub fn prelude_world() -> World;
impl SourceWorld for World;

// vesper::check
pub fn check(&Module) -> Checked;                         // unchanged signature; elaborates first
pub struct World { /* … */ pub elaboration: Elaboration }   // additive

// vesper-pact-binding (new crate): the former bridge API, plus
pub trait SourceWorld; pub struct SourcePackage;          // root-free package view
impl SessionBuilder { pub fn new(prelude: Arc<dyn SourceWorld>) -> Self; /* package(SourcePackage, BindingsDto) … */ }
impl SourcePackage { pub fn with_fingerprint(self, String) -> Self; }

// vesper-pact-bridge: unchanged API, plus
impl CheckedPackage { pub fn source_bindings(&self) -> &BindingsDto; pub fn source_package(&self) -> &SourcePackage; }
```

Modules without trait syntax skip the frontend. `check_core` runs exactly as before, and their World has an empty `elaboration`.

## Validation

All commands were run with the coordinator patch applied, `CARGO_TARGET_DIR` set to a target used only by this checkout, `CARGO_BUILD_JOBS=2`, and debug info and incremental compilation off.

```text
cargo fmt --all -- --check                                    ok
cargo clippy --workspace --locked --all-targets -- -D warnings ok
cargo test -p vesper --test source_traits --test source_models
    source_traits: 24 passed (one is the real-Lean test, which skipped: no lean binary)
    source_models: 10 passed
cargo test -p vesper-pact-bridge                              6 modes + 24 session + 4 source_bindings passed
cargo run -p vesper-pact-bridge --example bridge              ok, unchanged output
cargo test -p vesper --locked (before the new tests)          all existing root suites passed
cargo test --workspace --locked --no-fail-fast --exclude vesper-loom-build
                                                              1221 passed, 0 failed, 0 ignored, 135 suites (after merging main at 865baab)
vesper run|jit examples/traits/shapes.ves                     4304070304 on both
vesper emit … --emit object --target aarch64-unknown-linux-gnu  ELF 64-bit, ARM aarch64
vesper check examples/traits/overlap_rejected.ves             E0605 (PACT0102)
```

What the acceptance tests cover:

* **Positive.** One trait with an associated type, a canonical impl and two named models, with concrete calls and nested local shadowing selecting the intended bodies. Also: inherent-method priority, a generic impl with projection normalization, row-parameterized traits whose effect is handled, linear `sink self` receivers (reuse and leak are still `E03xx`), call and struct-literal receivers, module-level `use model`, same-block shadowing, associated types that follow the selected model, explicit model paths, a named model of a dependency trait, and a bounded where-clause chain. Every positive program runs on the evaluator and the JIT with matching results, emits an AArch64 object (`EM_AARCH64` checked) and goes through `lean::extract`.
* **Negative.** Identical-head and blanket overlap, an orphan impl of a dependency trait (the same declaration is accepted as a model), cross-package overlap, parser errors, missing, extra and mistyped methods, a static trait function, mode, effect and modal-bound mismatches, wrong and unknown model brands, an unknown trait, no solution, ambiguity (two traits, two impls of an underdetermined receiver), depth overflow (an endless where-clause chain, and a tight `SolverConfig`), generic dictionary dispatch in four forms, escaping and unknown projections, projections in ADT fields, and stale evidence after trait, model, field and package changes.
* **Core only.** The elaborated World's Place IR names every generated function, contains no `Indirect` dispatch and no `$projection`, and no checked body still calls a trait method by name.

Not run here:

* **Real Lean compilation.** There is no Lean toolchain on this host, and installing elan was not permitted in this session. `lean::extract` succeeded for every positive program. `lean_accepts_and_evaluates_elaborated_programs` compiles the output and compares `#eval Vesper.«main»` with the evaluator when `VESPER_LEAN` or `lean` is present; it skipped here.
* **AArch64 execution.** No QEMU, so objects were emitted and inspected, not executed.
* **`vesper-loom-build` `tests/build.rs`.** 14 of its 20 tests fail here with `UnknownFingerprint { missing: ["engine_dependencies"] }`. That crate's `build.rs` looks for the consuming workspace's `Cargo.lock` by walking up from the target directory, and deliberately fails closed when `CARGO_TARGET_DIR` is outside the workspace, as it is in this isolated setup. Its source is untouched by this lane. Rerun it with an in-workspace target directory.
* **CI.** The root patch is committed on the branch, so CI runs the same configuration validated here.

## Coordinator patch

The user approved committing this patch on the branch itself (2026-11-12), so it is now part of the PR. It is kept here for reference (the `Cargo.lock` hunk is what `cargo` generated):

```diff
--- a/Cargo.toml
+++ b/Cargo.toml
@@ members = [
     "crates/vesper-pact",
     "crates/vesper-pact-bridge",
+    "crates/vesper-pact-binding",
     "crates/vesper-loom-cas",
@@ default-members = [
     "crates/vesper-pact",
     "crates/vesper-pact-bridge",
+    "crates/vesper-pact-binding",
     "crates/vesper-loom-cas",
@@ [dependencies]
 vesper-loom-cas = { path = "crates/vesper-loom-cas" }
+vesper-pact = { path = "crates/vesper-pact" }
+vesper-pact-binding = { path = "crates/vesper-pact-binding" }
 ariadne = "0.5"
--- a/src/lib.rs
+++ b/src/lib.rs
@@
 pub mod optimize;
+pub mod pact_frontend;
 pub mod runtime;
--- a/Cargo.lock
+++ b/Cargo.lock
@@ name = "vesper" dependencies
  "vesper-loom-trust",
+ "vesper-pact",
+ "vesper-pact-binding",
  "vesper-pulse",
@@
 [[package]]
-name = "vesper-pact-bridge"
+name = "vesper-pact-binding"
 version = "0.1.0"
 dependencies = [
  "serde",
  "serde_json",
  "sha2",
+ "vesper-core",
+ "vesper-pact",
+]
+
+[[package]]
+name = "vesper-pact-bridge"
+version = "0.1.0"
+dependencies = [
+ "serde_json",
  "vesper",
  "vesper-pact",
+ "vesper-pact-binding",
 ]
```

The bridge no longer needs `serde` or `sha2` directly; its manifest change is committed. The lockfile hunk above is now committed too. Optionally, the `src/lib.rs` facade comment can mention `vesper -> vesper-pact-binding`. Global docs (`docs/implementation.md`, `docs/parallel-work.md`) should move "source trait/model syntax and elaboration" from remaining work to done for the static fragment.

No shared schema change is requested. A richer interface schema for traits and models would need, at least, trait declarations (parameters, associated types with bounds, method signatures over `Self`), public named models with their brands, and a canonical-impl inventory, so that adding a blanket impl can be classified as Major. That stays coordinator-owned; until then these exports remain `Unsupported`.

### Review fixes (second commit)

Automated review of the first commit raised five findings. All five reproduced and are fixed, each with a regression test in `tests/source_traits.rs`:

* Cyclic associated type bindings (`type A = Self::A`, mutual, or through a constructor) overflowed the stack during lowering. Expansion now stops on a cycle and the conformance is rejected with `E0601`. A cycle through the concrete self type (`type A = <i64 as T>::A`) is rejected by the coherence gate (`E0605`), and normalization also guards against re-entering a projection (`E0612`).
* Explicit projection trait arguments were ignored by selection. They are now part of the goal. Fixing this also showed that any trait with a type parameter failed to lower (Pact trait parameters take no modal bound); they now lower kind-only, with bounds still enforced by the frontend.
* Where-clause arguments for row parameters were lowered as types. They now follow the referenced trait's parameter kinds, in both the declaration and the query assumptions.
* Chained calls such as `x.next().value()` failed with `E0609`. Receivers are now typed through the inner call's selected function without checking the inner expression twice.
* Projections in methods of a generic inherent impl did not see the impl's parameters. They now do.

## Decisions

* **Two-pass elaboration instead of new core nodes.** The first pass resolves trait calls inside the normal checker and records them by call span, with Place-IR validation skipped because surface calls remain. The rewrite then produces plain `Call { callee: Name(generated) }` nodes, and a second ordinary check builds the accepted World. Backends therefore see only nodes they already supported. The same safety checks run twice; nothing is weakened.
* **Inherent methods first.** Existing `impl Type { … }` method syntax keeps its meaning. A trait method is used only when no inherent method of that name exists.
* **Lexical shadowing maps directly to Pact scopes.** Each `use model` is its own scope, ordered outer to inner (and earlier to later within a block). Pact selects from the innermost matching scope.
* **Generic impls without dictionaries.** A generic impl or model becomes a generic generated function that the existing checker and monomorphizing native backend instantiate. Bodies that would need a where-clause assumption at run time are rejected, not approximated.
* **Model bodies resolve their own calls lexically.** Inside a model method, `self.other()` resolves like any other call: canonical unless a `use model` is in scope.
* **Receivers.** A receiver must have a statically known type: a place, a literal, a non-generic call, a struct literal, or a chained method call on such a receiver. Underdetermined receivers are ambiguous (`E0609`) and the diagnostic suggests an annotation.

## Remaining gaps

* Generic dictionary dispatch (trait-bounded generic functions, `dyn` second-class trait objects), supertraits, default methods, static and associated functions without `self`, generic methods, and associated consts.
* Cross-package source linking. Dependencies participate in coherence only: dispatching into a dependency's implementation, or naming its types in source, is rejected.
* Model brands as type parameters ([T-ModelBrand] on `CacheBTree[K, V, M]`). The binding layer supports brand parameters, but source ADT syntax for them is not implemented.
* Interface export of trait and model contracts beyond `Unsupported`.
* Lean emits generated functions as ordinary `partial def`s, not Lean classes and instances. Real Lean validation is pending on a host with Lean, and AArch64 execution on a host with QEMU.
* Function-value use of trait methods (`let f = Show::show;`) is not elaborated.
* An inherent method called on a chained receiver (`x.next().bump()` with `bump` inherent) still goes through the checker's existing inherent dispatch, which only types place receivers. Trait methods on chained receivers work.
