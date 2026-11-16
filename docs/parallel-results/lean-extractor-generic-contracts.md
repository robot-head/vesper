# Extracted contracts over type parameters

Issue: #593. Base: `6a75d9c3`. Branch: `codex/extractor-generic-contracts`.

The extractor now emits contracts for type-generic functions in its existing
pure and single unlabeled Fail models, including recursive fuel companions.
Contract predicates declare the same number of type parameters with the same
mode bounds in the same order. Names may differ: `f[T](x: T)` may use
`ensures_f[U](before: U, result: ...)`. Signature validation substitutes the
predicate's type names before comparing its value parameter types.

The generated contract is a closed proposition universally quantifying all
type parameters before its ordinary inputs. Calls explicitly instantiate the
function and predicates, including phantom parameters that cannot be inferred
from value arguments. For example:

```vesper
fn choose[T](x: T, flag: bool) -> bool { flag }
fn ensures_choose[U](x: U, flag: bool, result: bool) -> bool {
    result == flag
}
```

Its contract requires the postcondition for every extracted type and input,
not just a single monomorphic instantiation. `choose$admissible` is instead an
existential over the type and value parameters, satisfying `requires_choose`
when present. A witness such as `⟨UInt64, 5, true, trivial⟩` establishes one
admissible input at one type. It does not assert that every type has inhabitants.
This preserves the existing separation between a universal contract and
existential input inhabitation.

## Verification

The `generic_contracts` fixture has kernel-checked proofs for generic struct
inputs and results, two independent type parameters, matching copy bounds,
renamed requires/ensures parameters, a phantom type, checked arithmetic in the
Fail model and a recursive fuel companion. Its phantom input witness uses
`Empty`, which needs no inhabitant because it appears in no value parameter.

Typed negative tests reject missing or extra type parameters, different mode
bounds, incorrect value types after substitution, swapped type parameter roles,
incompatible requires predicates and row-polymorphic contracted functions.

The CLI regression admits a universal proof with a type/input witness, rejects
the same proof after an incorrect implementation change, and rejects a valid
UInt64-only theorem offered as the universal contract. It uses the existing
kernel replay and exact closed-contract type checks without changing admission.

Validation uses Lean 4.35.0:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test lean_extract
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test verify_extract_cli
cargo +stable clippy --locked -p vesper --all-targets --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
target/debug/vesper fmt --check tests/fixtures/lean_extract/generic_contracts.ves
git diff --check
```

## Limits

Contracts remain partial-correctness propositions for the extracted model.
Neither input witnesses nor fuel contracts prove termination, successful
execution, extraction correctness or native behavior. Type binders use the
extractor's existing `Type` universe; mode bounds are checked for signature
compatibility but the Lean model does not encode source ownership modes.
Predicates must still be pure, non-recursive Boolean functions with no `inout`
parameters. Effect-row polymorphism, labeled/multiple Fail and other effect
models remain unsupported.

This branch starts from main independently of the inout support in PR #826 and
the arithmetic bundle in PR #825. Integration of those changes must preserve
both the type binders and returned-state projections. Remaining #593 work
includes partial/fuel equivalence, decreases inference and first-class
recursive companions; this slice does not close the issue.
