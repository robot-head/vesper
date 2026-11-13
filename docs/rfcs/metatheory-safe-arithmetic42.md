# Closed safe literal arithmetic in the full metatheory (#313)

This slice extends `lean/MetatheoryFull.lean` with a literal-only, effect-free
arithmetic typing case. The actual pinned Lean 4.24 compilation succeeds,
including all existing preservation/progress/linear-exhaustion proofs and
149 theorem axiom reports. Focused Rust validation passes 18 checked-arithmetic
and three full-metatheory tests. Focused strict Clippy validation also passes.

The source baseline is `c226ee98695135ad59e64318213e79f911ad6325`; scope was
published in #313 before edits, including the later operand-bound clarification
(comment 6857073095). Only the Lean model, its existing Rust integration test
and this report are changed. The Rust checker and backend implementations are
unchanged.

## Exact rule and preserved obligations

`HasType.arithLiteral` applies only to the syntactic expression
`arith op (natV a) (natV b)`. It requires discardable expression contexts,
`a < u64Bound`, `b < u64Bound`, and an actual proof
`ArithOp.eval op a b = some c`. It contributes no effect: its row-polymorphic
form admits the empty row and remains valid under ordinary ambient row
weakening. No symbolic operand, result-range assertion without evaluation,
or arbitrary constant-fact environment is added.

Substitution, discardable-resource weakening, row weakening, head preservation
and progress have explicit cases for this rule. Successful head evaluation
preserves natural-value typing; an abort head contradicts the rule's actual
success proof. All downstream preservation, type safety, one-shot release and
total-linear-exhaustion theorems recompile without weakening their statements.
The field restriction `k_i <= m_i`, context splitting and every resumption-frame
condition remain intact.

The original general `ArithOp.eval`/`Ty.nat` semantics are preserved. Because
that existing evaluator bounds the result but does not globally bound natural
operands, the **new rule** explicitly bounds both literals. The concrete
`oversized_literal_sub_eval` theorem shows bound-minus-bound evaluates to zero;
`oversized_literal_sub_not_pure` proves that the oversized literals nevertheless
cannot receive pure typing. This closes the new rule's operand-admission edge
without changing unrelated calculus semantics.

## Concrete proof and checker evidence

`literal_arith_pure_iff` characterizes pure typing as precisely both operand
bounds plus successful evaluation. Formal add/subtract/multiply boundary
examples are pure. Max-u64 plus one, zero minus one, and max-u64 times two have
actual `none` evaluation and cannot receive pure typing at any result type or
mode.

`txnAcrossAbort` retains its name and rejected live-frame intent, but now uses
max-u64 plus one. Its former literal 1+2 no longer justifies the rejection
once the new rule exists. `txn_across_abort_rejected` handles both arithmetic
typing cases: the general effectful case violates the frame premise, while the
literal-only case contradicts actual overflow.

A separate `txnAcrossSafeArith` keeps a freshly minted handle live while
computing literal 1+2, then consumes the handle. The model proves empty-row
copy-result typing, the exact run
`[mint 0, silent, silent, silent, consume 0]`, and an instance of the unchanged
exact-once exhaustion theorem. This is a kernel-checked model trace, not a
claimed observed Rust/JIT resource trace.

The Rust test exercises the real public parser/checker, independently of the
Lean proof artifact. Four safe literal expressions with a live linear Ticket
are accepted. For each of the three overflowing/underflowing expressions,
the complete diagnostic vector is exactly `[E0401]` without Fail and exactly
`[E0402]` when Fail crosses the live Ticket. The same operation with Fail and
no live Ticket is accepted. These finite fixtures do not establish a general
Rust-checker refinement.

## Executed validation

All commands ran in `/workspace/vesper-worktrees/metatheory-safe-arithmetic42`.
The actual Lean binary is the pinned 4.24.0 installation, not an older compiled
Vesper CLI. No CLI probe was used. The Rust tests compiled the current worktree
against the shared target under the coordinator's exclusive build lease.

```sh
LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean lean/MetatheoryFull.lean
rustfmt --check --edition 2024 tests/metatheory_full.rs
CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 VESPER_LEAN=/home/agent/.local/share/vesper-lean/bin/lean LEAN_NUM_THREADS=1 cargo test -p vesper --test metatheory_full --test checked_arithmetic --jobs 1
CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo clippy -p vesper --test metatheory_full --test checked_arithmetic --jobs 1 -- -D warnings
git diff --check
```

The direct Lean command exited 0 with no warnings/errors or sorryAx. Its report
is `/tmp/metatheory-safe-arithmetic42-final.log`. The existing integration test
independently checks source escape-hatch hygiene, requires an audit root for
every theorem, recompiles a private regular-file copy with the pinned compiler,
and matches all declared theorem names to exactly one axiom report each. All
149 reports use only the existing standard allowlist `propext`,
`Classical.choice`, `Quot.sound`; there are no new project axioms or proof
escapes. In particular, the safe typing and concrete run use only propext;
the exhaustion instance inherits the existing exhaustion theorem's standard
axiom closure.

The focused Cargo log `/tmp/metatheory-safe-arithmetic42-cargo.log` records
18/18 checked-arithmetic tests (16.37 seconds) and 3/3 metatheory tests
(8.39 seconds), including actual Lean execution. Rust formatting and whitespace
checks pass. Strict Clippy exited 0 with `-D warnings` in 43.40 seconds; its log is
`/tmp/metatheory-safe-arithmetic42-clippy.log`.

## Scope

This supplies a closed, in-range literal bridge only. It does not add symbolic
range facts, all four integer kinds to this calculus, an executable resource
oracle, full-language/source refinement or compiler verification. No physical
resource or actual backend mint/consume trace claim is made. No external
Lean4Lean replay, complete package suite, performance/RSS claim or §42
completion is implied by these focused checks.
