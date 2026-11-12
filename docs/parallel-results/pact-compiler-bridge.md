# Pact compiler bridge — lane report

Lane 4 of the [next wave](../agent-prompts/next-wave/04-pact-compiler-bridge.md). Owned paths: `crates/vesper-pact-bridge/**` and this file. Nothing else was edited.

* Branch: `claude/project-thread-eptuhp` (the session's assigned branch; the prompt suggested `next/pact-compiler-bridge`).
* Base: `origin/main` at `c2ba0b1` (contains the required baseline `0e6caf9` and the prompt commit `2a8e46e`).
* The root compiler (`vesper`) and `vesper-pact` are used read-only, through their public APIs.

## What was built

A new library crate `vesper-pact-bridge`, with integration tests and a runnable example (`examples/bridge.rs` with `geometry.ves` and `geometry.bindings.json`).

| Module | Content |
| --- | --- |
| `source` | `CheckedPackage::check(identity, source)`: real `parser::parse` and `check::check`. Any parse or check diagnostic is returned as `ErrorKind::Source` with codes and spans. |
| `identity` | `PackageIdentity { name, version }` (validated canonical spelling), `DefKind`, `DefPath`. The builtin package `@vesper/builtin@0.0.0` owns primitives, `Unit`, tuples and the checker prelude. |
| `tables` | `IdTables`: dense Pact ids, each table sorted by `DefPath`, with reverse maps and `rename_ids` for rendering Pact output with names. |
| `modes` | Mode-scheme derivation from `World::mode`, with exhaustive verification. |
| `binding` | Versioned (`0.1`) DTOs: `BindingsDto` (branded types, traits, impls, models) and `QueryBindingDto` (environment, inference variables, goal). `TypeDto::from_source` converts checked `ast::Type`s. |
| `lower` (private) | Name resolution, term translation, well-formedness of instantiations, and lowering to `ProgramDto`. |
| `session` | `SessionBuilder`, `CoherentSession`, `Engine`, `BridgeOutcome`, `Evidence`, `Certificate`, `Consumed`, `Selection`, `Revision`. |
| `error` | `BridgeError { kind, diagnostics }`. Each diagnostic has a stable code (`E…`, `PACT…` or `BRIDGE000x`), an optional `DefPath` subject and an optional source span. |

### Public API

```rust
impl CheckedPackage { pub fn check(PackageIdentity, &str) -> Result<Self, BridgeError>; pub fn module(&self) -> &Module; pub fn world(&self) -> &World; }
impl SessionBuilder {
    pub fn new() -> Self;
    pub fn package(self, CheckedPackage, BindingsDto) -> Self;
    pub fn solver_config(self, SolverConfig) -> Self;
    pub fn lower(&self) -> Result<LoweredProgram, BridgeError>;   // NOT validated or coherent; no query API
    pub fn build(&self) -> Result<CoherentSession, BridgeError>;  // Program::new + check_coherence gate
}
impl CoherentSession {
    pub fn solve(&self, &QueryBindingDto) -> Result<BridgeOutcome, BridgeError>;
    pub fn engine(&self) -> Engine<'_>;                           // shared answer table
    pub fn translate_query(&self, &QueryBindingDto) -> Result<TranslatedQuery, BridgeError>;
    pub fn translate_type(&self, &PackageIdentity, &TypeDto) -> Result<Ty, BridgeError>;
    pub fn consume(&self, &Evidence) -> Result<Consumed, BridgeError>;
    pub fn accept_certificate(&self, &Certificate) -> Result<Evidence, BridgeError>;
    pub fn revision(&self) -> &Revision; pub fn tables(&self) -> &IdTables; pub fn program_dto(&self) -> &ProgramDto;
    pub fn ground_mode(&self, &Ty) -> Option<Mode>;
}
pub enum BridgeOutcome { Proven(Box<Evidence>), Ambiguous { .. }, NoSolution { .. }, Unsupported { .. }, Overflow { .. } }
```

`Evidence` has private fields, so only a session can construct it. `Certificate` is its public serde form and is untrusted until `accept_certificate` re-validates it.

## How each requirement is met

1. **Session from checked source.** Sources pass the real front end with zero diagnostics. Trait, impl and model declarations are not expressible in source yet, so they come from `BindingsDto`. That DTO cannot declare types: every `TypeDto::Def` must resolve to a checked source ADT of a session package, a builtin or prelude type, or an in-scope parameter. A bare name shadows types when it is a parameter, as generic names do in `World::mode`. This boundary is temporary. The source traits/models lane should lower surface declarations to the same shapes.
2. **Identities.**
   * `DefPath = (package name@version, kind, declared name)`. Anonymous impls use a producer-chosen `key`.
   * Tables are sorted by `DefPath`, so whitespace and declaration order change neither indices nor the `Revision` (tested).
   * Indices are still revision-local: adding a declaration can shift them.
   * Spans, addresses, local variable names and `$cap`/`$fncap`/`$latent` labels are never identities. `$` labels are rejected by `from_source`, and `$` impl keys are invalid.
   * Tuple constructors have non-identifier names (`(tuple/N)`), so they are reachable only as tuples.
3. **Types and modes.**
   * Supported: primitives, `String`, `Unit`/`()` (one constructor, as `compatible` treats them), tuples of arity 1–12, nominal ADTs including the prelude, row parameters, mode parameters (traits, environments), brand parameters, and lexical capability placeholders.
   * Mode schemes are probed from `World::mode` with symbolic `$probeN` arguments. A type parameter is joined if raising it alone raises the result.
   * Each scheme is accepted only after it agrees with `World::mode` on every argument-mode combination allowed by the declared bounds, at most 3^8 combinations. Disagreement or a larger domain is `Unsupported`, with the ADT's span. Nothing is approximated, and `World::mode` is unchanged.
   * Phantom parameters get no join. Row parameters never join. Brands are zero-sized and never affect mode.
   * Instantiations are checked against declared parameter bounds: definite violations are `Invalid`. An unknown mode (a projection argument) is `Unsupported` in templates and environments. In query goals it is re-checked on the solution, and an ill-formed solution is reported as `Unsupported`, not `Proven`.
   * Impl/model type-parameter bounds become `ModeBound(param, const)` where-clauses, the form Pact's well-formedness check assumes.
4. **Gate.** `build` runs `Program::new`. Its diagnostics and every `check_coherence` diagnostic are mapped to names and return `ErrorKind::Incoherent`. No session exists otherwise. A test shows the raw solver returning `Proven` on an incoherent program (two impls with identical heads) while `build` refuses it. Sessions are immutable: changing declarations means building a new session, which rebuilds the tables and the revision.
5. **Queries and evidence.**
   * Environment parameters become placeholders, inference parameters become binders, and `use_models` become model scopes. An absent query brand is a fresh inference brand (implicit resolution). An explicit model is `Named(m)`.
   * All five outcomes map one-to-one to `BridgeOutcome`, with named explanations.
   * `Proven` is accepted only after these checks pass:
     * the revision matches;
     * `validate_query` passes;
     * every binder occurs in the goal;
     * solution values have the binders' count and kinds;
     * the substituted goal validates and equals the derivation's root goal;
     * `check_derivation` passes against the query environment;
     * the substituted goal is well-formed: every constructor and effect application, including projection trait arguments, effect-label arguments and constructor brands, meets its declared bounds and brand traits.
   * `consume` runs the same checks again, then returns the selected impl or model (`Selection`), the named substitution and the rendered derivation.
   * Brands are kept in the type: `CacheBTree[K, V]` from source becomes `CacheBTree[K, V, Canonical]`, while a binding can name `CacheBTree[K, V, ReverseUuidOrd]`. Their layouts and modes agree but their types differ (tested). A named brand must identify a model of the declared trait.
6. **Diagnostics.** Names and spans appear only in `BridgeDiagnostic`s and in rendered explanations. A test checks that the Pact query JSON contains none of the source names.

## Validation

Run in `crates/vesper-pact-bridge`:

```text
cargo fmt -- --check                                   ok
cargo clippy --locked --all-targets -- -D warnings     ok
cargo test --locked                                    30 tests: 6 modes + 24 session, all pass
cargo run --locked --example bridge                    ok (output below, abridged)
```

```text
Layer[Point]: Eq selected impl @demo/geometry@1.0.0::eq.layer[@demo/geometry@1.0.0::Point]
<Layer[Point] as Container>::Elem = @demo/geometry@1.0.0::Shape[@demo/geometry@1.0.0::Point]
Point: Ord (implicit) -> impl @demo/geometry@1.0.0::ord.point
Point: Ord (use model ByY) -> model @demo/geometry@1.0.0::ByY
certificate of 965 bytes re-validated: true
Canvas: Eq -> no-solution
overlapping impl blocks the gate: PACT0102 ...
after a declaration change: stale-evidence error ...
```

Test coverage against the acceptance list:

* **Mode agreement.** Every copy/affine/linear argument combination allowed by the bounds, for 13 fixture ADTs, is compared with `World::mode`. The fixtures cover modalities, bounds, phantom parameters, row parameters, recursion (`List`), nested recursion (`Rose`) and argument-swapping recursion (`Swap`). Nested and tuple instantiations, and placeholder bounds, are compared as well. Ill-formed instantiations are rejected, and too-wide ADTs are `Unsupported`.
* **Stability.** Reordering and reformatting a source and its bindings leave identities, tables and revision unchanged. Row and capability names alpha-rename to identical Pact queries, and reordering environment parameters gives the same canonical goal.
* **Aliasing.** Two packages that both define `Point` get distinct ids. An impl for one never applies to the other.
* **Rejections.**
  * Source diagnostics, including E0105.
  * Invalid package names and the reserved builtin name.
  * Unknown types, traits and packages; wrong arity; wrong argument kind; duplicate traits and impl keys; package mismatch; future schema versions; missing or unknown associated types; duplicate parameters and `Self`; invalid keys.
  * Brands naming a model of the wrong trait.
  * Unconstrained impl parameters (`PACT0008`).
  * Capability parameters on impls, tuple arity above 12, function types and `$cap` labels.
* **Coherence.** Identical-head overlap, blanket overlap and orphan impls all block the gate. The orphan declaration is accepted as a named model.
* **Outcomes.**
  * Distinct named models, `use model` shadowing, and conflicting scoped models (`Ambiguous`).
  * Unknown self type (`Ambiguous`).
  * Projection substitution, `NoSolution` with names, depth `Overflow` and step-budget `Overflow`.
  * Pact `Unsupported` for a duplicate effect label after row merging.
  * A solver proof whose solution violates a parameter bound becomes `Unsupported`, without evidence.
* **Invalidation.** Evidence is stale after a binding change or a source change, and accepted again after an identical rebuild.
* **Post-solution well-formedness** (review follow-up). Every constructor and effect application in the substituted goal is re-checked, including projection trait arguments, effect-label arguments and constructor brand arguments. Source effect parameter bounds (`effect E[T: mod copy]`) are enforced. The revision also hashes bridge-only constraints: parameter bounds, brand traits and effect bounds.
* **Certificates.** Malformed certificates are diagnosed without panics: tampered revision, step, impl id, brand, values, goal id, names or schema, a derivation for a different goal, and garbage JSON. A JSON round trip is accepted.

Not run: the root workspace suite, Lean and AArch64 checks. No root, compiler or Lean file changed. The crate is not registered in the root workspace.

## Integration steps (coordinator)

1. Remove the `[workspace]` table, `crates/vesper-pact-bridge/Cargo.lock` and `.gitignore`.
2. Add the crate to the root `members` and `default-members`. Optionally switch `serde`, `serde_json` and `sha2` to workspace dependencies. The nested lock uses the same versions as the root lock: serde 1, serde_json 1, sha2 0.10.9.
3. Regenerate the root lockfile, then run the workspace fmt, clippy and test commands.
4. After checked arithmetic merges, re-run this crate's tests. Fixtures contain no arithmetic: function bodies are literals. The bridge reads only `World::mode`, `World::adts`/`effects`, the parser and `check::check`. If arithmetic changes `World::mode`, schemes are re-derived and re-verified automatically. If it adds prelude ADTs or effects, they join the builtin package automatically.

No cross-boundary patch is required.

## Decisions

* **Capability identity.** Pact's `Capability(var)` label has no effect. The bridge records each capability placeholder's effect in the query environment and rejects any use with another effect. Because impls cannot mention capabilities, Pact answers are parametric in what a capability placeholder denotes. Two environments that differ only in a capability's effect may share a table entry without affecting answers.
* **Implicit brands** exist only in query goals. In templates, environment assumptions and associated-type bounds, an absent brand is `Canonical`, because Pact requires explicit template brands.
* **Mode assumptions** in environments are rejected. Bounds belong on parameters, as Pact requires. Inference variables cannot carry bounds.
* **Unit:** `Unit` and `()` are one constructor.
* **Revision** is SHA-256 over the lowered `ProgramDto`, the package list and all table `DefPath`s. It also covers the bridge-only constraints that the DTO lacks: source parameter bounds, brand traits and effect parameter bounds. It identifies the declaration set and is not provenance. A colliding revision still cannot pass the derivation re-check.

## Remaining limits

* There is no trait/impl/model/`via` source syntax, no dictionary codegen and no source trait checking. The binding DTO stands in for lowered surface declarations.
* Not supported:
  * function types as goal types (no Pact constructor);
  * capability parameters on impls/models, and capability inference variables (outside Pact's fragment);
  * bounds on trait parameters;
  * tuples above arity 12;
  * mode schemes whose verification domain exceeds 3^8 combinations.
* Scheme agreement is checked exhaustively per constructor. Agreement for ground instantiations relies on `World::mode` computing argument modes before visiting fields, which the nested-instantiation tests exercise but no proof establishes.
* Cross-package source references are not possible, because Vesper source has no imports. Packages relate only through qualified references in bindings and queries.
* The Pact limits listed in the [Pact report](pact-goals.md) apply unchanged, including that a derivation proves an applicable declaration, not global uniqueness. The coherence gate supplies the uniqueness side.
