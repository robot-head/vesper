# Contracts over multiple Fail effects (#593)

Branch: `codex/extractor-multi-fail-contracts`. This slice builds on PR #830's
single labeled Fail support and targets that branch as a dependent PR. Main was
pulled at `d5ec6ff1` before implementation.

Contracts now support closed rows containing any number of `Fail[E]` instances,
including labels with identical payload types. A single instance retains the
existing `Except E` model. Multiple instances use a right-associated error sum
in declaration order: `[left: Fail[A], right: Fail[B], last: Fail[C]]` selects
`Except (Sum A (Sum B C))`. The corresponding injections are `Sum.inl`,
`Sum.inr ∘ Sum.inl`, and `Sum.inr ∘ Sum.inr`.

Each instance receives explicit named `$capN` evidence, whose `raise` injects
the payload into its own sum position before calling `MonadExcept.throw`.
This avoids ambiguous instance synthesis and preserves label identity even
when `A` and `B` are the same type. Function bodies, forwarding calls and
runtime handlers retain their existing capability passing. The type, record,
lambda and sum expressions use the structured Lean AST.

Recursive companions use `ExceptT (Sum ...) Option`. Contracts constrain only
successful `.ok result` or `some (.ok result)` observations, exactly as in the
existing single-Fail model. Requires predicates and separate existential input
witnesses keep their existing signatures and admission checks. Tagged errors
are part of the model; this does not introduce an error-postcondition language.

## Evidence

The new fixture proves contracts for a three-instance labeled row, forwarding
through that row, a distinct-payload unlabeled row, and a recursive fuel
companion. Each error branch is checked against its specific sum injection,
including the two `u64` labels; the successful branch returns 42. The reference
evaluator runs the labeled handlers and returns 42. A mixed Fail/non-Fail row
still produces a typed unsupported error.

The end-to-end admission test accepts the exact closed contract proof and
input witness for a two-instance model, then rejects the unchanged proof after
the successful branch returns an incorrect value. All 15 extraction tests and
the targeted admission regression pass with Lean 4.35.0, as do scoped Clippy,
formatting and manifest checks. Commands:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test lean_extract
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test verify_extract_cli multi_fail_contract_admission
cargo +stable clippy --locked -p vesper --test lean_extract --test verify_extract_cli --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
target/debug/vesper fmt --check tests/fixtures/lean_extract/multi_fail_contracts.ves
git diff --check
```

## Limits

The propositions describe normal-return partial correctness of the extracted
model. They do not prove termination, successful execution, extraction
equivalence or native behavior. Open rows and non-Fail model monads remain
unsupported. This branch does not integrate the separate pending generic and
inout slices; their type binders and returned-state projections must be kept
when those PRs are combined. Partial/fuel equivalence, decreases inference and
first-class recursive companions remain open under #593.
