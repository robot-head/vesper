# Checked integer arithmetic

Lane 01 of the next wave ([prompt](../agent-prompts/next-wave/01-checked-arithmetic.md)).

- Branch: `claude/project-thread-vmzghp`. It started from `c2ba0b1` and was rebased onto `origin/main` at `3ccb996` before validation.
- The PR and commit are listed on the branch. This lane changes compiler semantics, so it should merge before the other next-wave lanes.

## Contract: `vesper.arith.checked/1`

The contract is defined in `crates/vesper-core/src/numeric.rs` (`ARITHMETIC_SEMANTICS`). Every backend uses it.

- **Checked operators.** For `u8`, `u32`, `u64` and `i64`, the operators `+`, `-`, `*`, the compound forms `+=`, `-=`, `*=`, and value-level signed negation return the exact result when it fits the kind. Otherwise they raise `Fail[ArithOverflow]`.
  - Unsigned `-x` raises unless `x == 0`.
  - A failed compound assignment leaves its target unchanged.
- **The ArithOverflow type.** `ArithOverflow` is the reserved builtin `copy struct ArithOverflow {}`, constructed as `ArithOverflow {}`. Its interface identity is `std/core::type::ArithOverflow`. It is a separate type from `RuntimeError`, and the two never share a handler.
- **Division.** `i64::MIN / -1` raises `ArithOverflow`, and `i64::MIN % -1` is `0`.
  - Division or remainder by zero still raises `Fail[RuntimeError]::DivisionByZero`. Indexing still raises `IndexOutOfBounds`.
  - When a divisor could be zero and could be -1, the zero check runs first.
  - Unsigned division cannot overflow.
  - Floats are unchanged and keep IEEE behavior.
- **Wrapping intrinsics.** `wrapping_add`, `wrapping_sub` and `wrapping_mul` are ordinary calls. They are pure and total, and they compute modulo 2^bits.
  - Both arguments must be integers of the same kind and passed by value. Literal arguments take their type from context.
  - The checker rejects type arguments or an arity other than 2 (E0207), non-integer arguments (E0200), `inout` or `sink` arguments (E0308), and user functions named `wrapping_*` (E0100).
  - A local closure binding with the same name shadows the intrinsic.
  - The intrinsics are not exported as interface items.

## What changed

- **Checker** (`check.rs`):
  - `World.overflow_ops` records each operation that might fail.
  - The checker discharges a failure only when a constant fact proves the operation in range. The fact is the checked result, and an expression such as `0 * x` is in range.
  - Assignment replaces a fact. A field write sets none. Inout calls and resumptions clear the facts of the places they can reach.
  - Each possible overflow requires a `Fail[ArithOverflow]` capability.
  - Effect selection is typed. A request whose arguments are concrete picks the innermost capability with exactly those arguments. Otherwise it picks the innermost capability with that name. Labels select by label as before.
  - The abort rules E0402 and E0410 apply to checked failures.
- **Runtime, IR and IR checks**:
  - The IR sets `Instruction.fallible` on every instruction that might fail, and gives it a state token. `ir::state_action` is the single predicate shared with `ir_check`.
  - At run time a failure goes to the capability for its own instance, `Fail[ArithOverflow]` or `Fail[RuntimeError]`. A call substitutes generic arguments into the error type. `Try` and turbofish operations are typed in the same way.
- **AST optimizer** (`optimize.rs`):
  - The identities `x+0`, `x-0` and `x*1` are kept.
  - Annihilation and self-cancellation (`x*0`, `x-x`) fire only when the subterm's e-class contains a variable or a constant, which cannot fail.
  - Commutation, reassociation and `(a+b)-b` cancellation apply only to the wrapping intrinsics.
  - Constant folding uses the checked operation, so an overflowing constant is never folded and still raises at run time. Wrapping calls fold modulo 2^bits.
  - The egglog read-forwarding optimizer in `ir_opt.rs` needed no change. Fallible instructions keep their tokens, and calls stay barriers.
- **Native** (Cranelift JIT and objects):
  - Each checked operation is followed by an inline overflow check (umulhi/smulhi, sign checks and the MIN/-1 check), then a branch to the same typed failure path the evaluator uses.
  - A new *fail-only ABI* covers a function whose effect row holds only unlabeled `Fail` instances. Such a function takes a hidden pointer to a failure record, and its caller re-raises the failure. Before this, a recursive checked function hit the 64-frame inline specialization limit. It now compiles natively, with no fallback to the evaluator.
- **Lean extraction** (`lean.rs`, `lean_ir.rs`):
  - Before each checked operation, the generated code tests a guard over exact `Nat`/`Int` arithmetic and raises `ArithOverflow` if the guard fails.
  - Lean's `Int64` `%` traps on `MIN % -1`, so the extractor emits `if b == -1 then 0 else a % b`.
  - `lean_ir` hands fallible instructions to the effectful generator.
  - `lean/CheckedArithmetic.lean` proves that whenever a guard passes, the native `UIntN`/`Int64` operation equals the exact result. It covers `+`, `-` and `*` for all four kinds and `i64` negation, proves that the guard agrees with the contract's denotation, and checks the boundaries and division corners with `decide`. It uses no `sorry` and only Lean's standard axioms. It proves nothing about how extraction preserves meaning in general.
- **Interface** (`interface.rs`): `ArithOverflow` is a `std/core` builtin, so an exported row reads `Fail[std/core::type::ArithOverflow]`.

## Migration and source-compatibility impact

This change breaks existing source. Code that added, subtracted or multiplied values the checker cannot prove in range now needs `eff [Fail[ArithOverflow]]` in its row or an enclosing handler. Code that relied on wrapping has to say so.

- **Tests.** I migrated the existing tests one at a time:
  - Where a fixture's intent was plain arithmetic on unknown values, it now uses `wrapping_*`.
  - Tests about handlers or failures now declare and handle `ArithOverflow`.
  - `numeric::signed_division_overflow_wraps` became `signed_division_overflow_raises`.
  - The only `src/io_session.rs` edit is to a `#[cfg(test)]` fixture.
- **Examples.**
  - `fibonacci`, `fail` and `resumptions` declare and handle `ArithOverflow`. `fail` shows nested typed handlers.
  - `closures` uses an effectful closure type and builds the closure inside its handler.
  - `counter` and `projections` use wrapping, because an overflow there would abandon a live continuation or cross an `inout` parameter, which E0402 forbids.
  - Every example gives the same result in `run`, `jit`, Lean and AArch64.

## Validation

All of these checks ran on the rebased tree. Lean was the real 4.24.0 toolchain (`VESPER_LEAN=/opt/lean/lean-4.24.0-linux/bin/lean`).

- `cargo test --workspace --locked --no-fail-fast` passed: 717 tests, 0 failed.
- `cargo fmt --all --check` passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` passed.
- `lean lean/CheckedArithmetic.lean` passed. `#print axioms` lists only `propext`, `Quot.sound` and `Classical.choice`.
- `tests/checked_arithmetic.rs` has 18 tests, all passing. Every accepted program must give the same answer in:
  - the evaluator, optimized and unoptimized;
  - the JIT, optimized and unoptimized;
  - the evaluator and JIT after AST equality saturation;
  - Lean `#eval`.

  The same programs also emit x86_64 and AArch64 objects. The tests cover:
  - endpoints and overflow or underflow for all four kinds, including contextual literals, unsigned high values and the `i64` minimum literal;
  - compound assignments that leave their targets unchanged, including field targets;
  - negation and the division and remainder corners;
  - floats, wrapping totality and invalid intrinsic uses;
  - typed handler selection, both nesting orders, the nearer handler of the same type winning, labels, and generic `E` instantiated at the call site;
  - recursive checked functions running natively;
  - the abort rules with linear temporaries, linear handler state, live continuations and `inout`;
  - fact invalidation by assignment, field writes, inout calls and resumptions;
  - effectful closures, optimizer preservation of failures and of unfolded overflowing constants, and interface export.
- `scripts/test-aarch64.sh` passed: all 8 examples exited with the expected codes under `qemu-aarch64`. I installed `qemu-user` and the aarch64 binutils in the container to run it. A separate AArch64 program with a recursive `u64` factorial overflow and `MIN / -1` exited 42, the same as `run` and `jit`.
- No CI run has been observed for the branch yet.

## Remaining limitations

1. **The abort rules are conservative.** E0402 forbids checked arithmetic that might fail in two places: a function that has an `inout` parameter, and a linear handler clause while its continuation is still live. Code there has to use `wrapping_*`, or finish the continuation before doing checked arithmetic. Relaxing this would need the checker to prove that an abort restores the loaned place. That is a design question for the coordinator.
2. **Facts are constants only.** The checker does not track ranges or path conditions, so `if b != -1 { a / b }` still needs an `ArithOverflow` handler. Signed `/` by an unknown divisor always needs one.
3. **Untyped `Fail::raise(e)`.** A raise without a turbofish, or a call on the AST path whose generic error type cannot be substituted, falls back to the innermost `Fail` by name, as before. Calls on the IR path pick the typed capability.
4. **The native fail-only ABI is narrow.** It covers only rows made entirely of unlabeled `Fail` instances. Other recursive effectful functions keep the existing inline-specialization limit.
5. **Lean closures.** Lean extraction cannot handle an effectful closure that is created outside the handler that serves it. This limit predates this lane, but checked arithmetic makes it more visible.
6. **Contextual typing through `handle`.** A literal-only binary expression in a `handle` body, such as `200 + 56`, still defaults to `i64` there. This predates the lane and is unchanged on base. A typed `let` works around it.

## Coordinator requests

- **SemVer.** Bump `vesper_semver::SEMANTICS_VERSION` and its compatibility notes for `vesper.arith.checked/1`. Interfaces exported before this change assume wrapping arithmetic, so they should not be compared as if they had the same semantics. The crate is not in my ownership.
- **Docs.** Global docs such as design §11.2 and the implementation notes should point to this contract and to the migration note above.
- **CI.** Add `lean lean/CheckedArithmetic.lean` to the two CI Lean steps, next to `EffectPlaces.lean`.
