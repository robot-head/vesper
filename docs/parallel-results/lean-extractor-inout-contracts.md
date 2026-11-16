# Extracted contracts over inout state

Issue: #593. Base: `be35233b`. Branch: `codex/extractor-inout-contracts`.

The extractor now states contracts for supported non-generic functions with
`inout` parameters using its existing returned state tuple. For a function `f`,
`requires_f` receives every original input in declaration order. `ensures_f`
receives those same original inputs, the ordinary return value, and the final
value of each `inout` parameter, in declaration order. Immutable inputs are not
repeated in the final-state suffix.

For example, `replace(inout x: u64, value: u64) -> u64` uses
`ensures_replace(before: u64, value: u64, result: u64, after: u64) -> bool`.
The predicate can independently assert `result == before` and `after == value`.
A unit-returning function still supplies its unit result before the final state.

The generated proposition projects the actual extracted output tuple; original
inputs remain separate quantified binders. For pure total functions it checks
the returned tuple directly. For a supported single unlabeled `Fail[E]` row it
checks successful `Except.ok` returns. Recursive functions use the existing
total fuel companion and check successful `Option.some` returns (with the
existing `ExceptT` model when applicable). Preconditions and separate existential
input witnesses retain their existing meaning and admission checks.

## Verification

With Lean 4.35.0, all 14 `lean_extract` tests pass. The new fixture proves:

- A scalar update returns the original value and produces the specified state.
- Two mutable inputs with an immutable input between them preserve suffix order.
- A struct field update preserves the untouched field.
- A unit-returning reset exposes the updated state.
- A function declaring `Fail[ArithOverflow]` exposes its successful post-state.
- A recursive drain satisfies its postcondition on successful fuelled returns.

Negative coverage refutes a contract that confuses final and original state,
rejects missing, extra or mistyped state parameters, and rejects mutating
postcondition predicates. The CLI admission regression accepts a closed proof
and input witness, then rejects the same proof after the implementation changes
from the specified assignment to an incorrect assignment.

Commands:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test lean_extract
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test verify_extract_cli
cargo +stable clippy --locked -p vesper --all-targets --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
target/debug/vesper fmt --check tests/fixtures/lean_extract/inout_contracts.ves
git diff --check
```

## Limits

These are normal-return partial-correctness contracts. They do not establish
termination, successful execution, abort post-state, or equivalence between a
partial definition and its fuel companion. The source checker's E0402 rejection
of abortable checkpoints across active `inout` borrows remains in force; the
declared-Fail fixture uses wrapping arithmetic and exercises no abort checkpoint.

Generic contracts, open effect rows, labeled or multiple Fail instances, and
other effect models remain unsupported. Predicates must remain pure,
non-recursive, non-generic Boolean functions without `inout` parameters. The
generated arithmetic simp bundle is a separate pending slice in PR #825; this
change does not depend on it. Issue #593 remains open for its remaining scope.
