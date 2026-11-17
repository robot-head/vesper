# Lean extractor proof support (#593)

Base: `b91fbae747012d47977f4b0d66cd0e2bd6ca861b`, current main when this
slice began. Open PRs #750, #752, #823 and #824 did not overlap this source
change. This report records the delivered arithmetic simplification slice;
#593 remains open for its broader recursion and contract requirements.

## Delivered behavior

Every extracted module now contains the ordinary theorem
`Vesper.«$arith_simps»`. Its conjunction supplies 24 standard-library
rewriting rules: `toNat` forms of `≤`, `<`, addition, subtraction,
multiplication, and numeric conversion for `UInt8`, `UInt16`, `UInt32`,
and `UInt64`. Lean's simplifier splits conjunction theorems into their
component rules, so proofs can use:

```lean
simp only [Vesper.«$arith_simps»] at *
omega
```

The arithmetic rules retain the machine modulus. In particular, unsigned
subtraction rewrites to `(2^width - b.toNat + a.toNat) % 2^width`, and
addition/multiplication retain their modulo as well. Dropping that modulo
requires a separate range argument. Existing pure and `Except` contract
proofs now use the named bundle instead of repeated `UInt64` lemma lists.

The emitter constructs both proposition and proof through the existing
structured Lean AST. Each component is proved by a fixed standard-library
lemma; `And.intro` joins the evidence. The reserved `$` name cannot collide
with an ordinary source function named `arith_simps`. No new Lean imports,
custom attribute initialization, tactic metaprograms or raw-source printer
escape were added. The ordinary theorem is checked during compilation and
kernel replay like other generated declarations. This is an unsigned
arithmetic simp bundle, not a general-purpose arithmetic decision procedure.

## Validation

With the pinned Lean 4.35.0:

- All 12 `lean_extract` integration tests passed. The existing fixture proves
  the pure clamp contract, the preconditioned `Fail[ArithOverflow]` contract,
  and a recursive fuel contract.
- New proofs check bounded addition/multiplication and concrete
  underflow/overflow at each of the four widths. A source function named
  `arith_simps` compiles and evaluates alongside the generated bundle.
- Both `verify_extract_cli` tests selected by `contract_proof_binding` passed,
  including fresh compilation/kernel replay, exact closed contract signatures,
  and rejection of unrelated proofs and hidden hypotheses.

Commands:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --test lean_extract
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --test verify_extract_cli contract_proof_binding
cargo +stable clippy --locked -p vesper --all-targets --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
git diff --check
```

## Single labeled Fail contract follow-up

Base: `d5ec6ff1`. Branch: `codex/extractor-labeled-fail-contracts`.

A function with one `Fail[E]` effect can now have a label on that effect.
For example, `fn choose(n: u64, reject: bool) -> u64 eff [stop: Fail[u64]]`
can use `ensures_choose(n: u64, reject: bool, result: u64) -> bool`. Calls to
`stop.raise(n)` use the existing explicit capability evidence; the contract
selects the same canonical Fail model already used for unlabeled effects.
With exactly one effect instance, this introduces no ambiguity about which
label supplies the evidence. This initial slice refused multiple instances;
the follow-up below adds their distinct tagged model.

The proposition constrains successful returns in `Except E`; recursive fuel
companions use `ExceptT E Option` and constrain successful `some (.ok result)`
observations. It does not constrain error payloads or guarantee success or
termination. Separate kernel-checked fixture theorems establish the concrete
`.error 13` and `.ok 42` observations, and a fuel induction establishes the
recursive contract. A labeled forwarding call and a source handler are also
covered. No runtime, effect evidence, ownership or admission rule changes.

All 14 extraction tests pass with Lean 4.35.0. The targeted end-to-end admission
test accepts the closed contract proof/input witness and rejects that proof
after the successful branch returns an incorrect value. Strict scoped Clippy,
workspace/fixture formatting, Bazel manifest synchronization and diff checks
pass. Additional commands for this slice:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test lean_extract
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test verify_extract_cli labeled_fail_contract_admission
cargo +stable clippy --locked -p vesper --test lean_extract --test verify_extract_cli --no-deps -- -D warnings
target/debug/vesper fmt --check tests/fixtures/lean_extract/labeled_fail_contracts.ves
```

## Multiple Fail contract follow-up

The [multiple-Fail report](lean-extractor-multi-fail-contracts.md) records the
dependent follow-up: a right-associated error sum in row order, explicit
per-instance capability evidence, and kernel-checked pure/recursive contracts.
Labels with identical payload types remain distinguishable by their sum tags.
Open rows and non-Fail model monads remain unsupported.

## Remaining #593 work

- General fuel/partial-definition equivalence or direct structural termination
  evidence; successful finite fuel execution is currently partial correctness.
- Total companions for recursion through first-class function/closure values.
- Contracts over `inout` state transitions, generics, and handled non-`Fail`
  effects. Existing typed refusals remain on this branch.
- Source-level termination/decreases inference and a compiler extraction
  refinement proof. The arithmetic helper establishes none of these results.
