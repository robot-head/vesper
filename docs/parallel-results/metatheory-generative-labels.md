# Row-lacks foundation for #584

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
model, check that all 300 theorem names have exactly one axiom report, and
reject missing reports, forbidden source escapes, diagnostics and any axiom
outside `propext`, `Classical.choice`, and `Quot.sound`. The headline list now
includes the new substitution, composition, lifting, opening, exact semantic
characterization and capture-counterexample results. Existing progress,
preservation, fuel safety and linear-exhaustion results are checked in the
same run.

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
proofs, first-class System F type abstraction/application, and recursive
closures capturing affine/linear environments remain open. The existing
metatheory models remain fragments. These theorems do not establish soundness
of the Rust compiler or the entire manuscript, and do not close #584.
