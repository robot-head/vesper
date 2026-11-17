# Row-lacks and recursive-capture foundations for #584

Base: `042c8590`. Branch: `codex/metatheory-row-lacks`.

This milestone extends `lean/Metatheory/Polymorphism.lean` using its existing
`Label`, `Row`, `Row.inst`, `compθ`, `liftθ`, `openθ` and `Ty.inst` definitions.
It adds a constraint context mapping each row variable to the user labels its
instances must exclude. `RowLacks.Respects` checks substitutions between two
such contexts; `RowLacks.Closing` additionally requires concrete row images.

## Proved

The central characterization is:

```lean
RowLacks.excludes Δ ρ l ↔
  ∀ θ, RowLacks.Closing Δ θ → Label.user l ∉ Row.inst θ ρ
```

This is both sound and complete for the stated interpretation. If a variable
does not exclude `l`, `expose_closing` constructs a permitted concrete
substitution that introduces `l`. `empty_closing` supplies a concrete witness
for every constraint context, so the universal interpretation is not vacuous.

The 25 new theorems also establish:

- Preservation of lacks constraints by substitution and substitution composition.
- Preservation under index shifting and lifting beneath a row binder.
- Opening with an open row whose variables carry the required constraints,
  and absence of the excluded label after a subsequent closing substitution.
- Agreement with the existing `Avoids` predicate on closed rows.
- General type-opening/substitution commutation for open rows, instantiating
  the opening row itself. The old closed-row `inst_open` theorem is derived
  from this general identity and remains available to all existing proofs.
- A constrained open row that excludes a label, rejection of an instance
  that captures that label, and a counterexample showing that syntactic
  `Avoids` alone is not stable under substitution into an open row.

## Validation

Both `metatheory_poly` tests pass with Lean 4.35.0. They compile the complete
model, check that all 312 theorem names have exactly one axiom report, and
reject missing reports, forbidden source escapes, diagnostics and any axiom
outside `propext`, `Classical.choice`, and `Quot.sound`. The headline list now
includes the new substitution, composition, lifting, opening, exact semantic
characterization and capture-counterexample results. Existing progress,
preservation, fuel safety and linear-exhaustion results are checked in the
same run.

## Recursive environment ownership

The additional 12 theorems state the restriction of the current recursive
closure calculus precisely. `vt_fixc_environment_copy` shows that a recursive
closure's environment has copy kind; `vt_fixc_resource_free` derives that its
exact resource context is empty, regardless of the function's own mode. The
recursive case of `head_pres` now uses this invariant directly.

`typed_fixc_only_discarded_resources` distinguishes ownership from discarded
ambient resources: a typing derivation can discard unrelated affine resources,
but the recursive value itself cannot own them. Direct captures of handles
and either affine or linear one-shot continuations are rejected. Resource
splitting also cannot assign the same live entry to two components; identical
component contexts must be empty.

The `RecursiveCapture` example uses the existing operational semantics. An
untyped recursive closure consumes its captured handle and calls itself.
Four steps reach another consumption of the same, now dead, id. That endpoint
has no step and is neither a value nor an abort. The current typing rules
reject this capture; this is a counterexample to admitting unrestricted
capture with the existing unfolding rule, not a counterexample to the proved
type safety theorem. Supporting noncopy recursive environments requires a
different ownership or unfolding discipline and a new preservation proof.

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test metatheory_poly
cargo +stable clippy --locked -p vesper --test metatheory_poly --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
git diff --check
```

## Remaining scope

`HasType.rapp` still requires `RowClosed`; this milestone does not lift that
restriction or extend progress/preservation to open row instantiation. Doing
so requires tracking constraints in typing and transforming expression-level
row annotations consistently with `inst_open_general`.

Runtime-generative handler allocation, its label context and lexical-scoping
proofs, first-class System F type abstraction/application, and a redesigned
recursive closure discipline allowing affine/linear environments remain open.
The existing
metatheory models remain fragments. These theorems do not establish soundness
of the Rust compiler or the entire manuscript, and do not close #584.
