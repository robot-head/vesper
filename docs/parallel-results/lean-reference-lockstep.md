# Lean reference and lockstep harness (wave eight, lane 4)

Claim: #453. Baseline: `1094e649c95e4d81eef4e705d13882ab1339542d` (a descendant
of the required `ea1ed29`), merged with main before pushing.

## Summary

- `lean/ReferenceEval.lean` is an executable Lean 4 reference for the core
  calculus the checker accepts today. It has a checker (`checkExpr`) and a
  fuel-stepped CEK evaluator (`evalSteps`), and it runs as `lean --run`
  over S-expression programs.
- `lean/ReferenceEval/Soundness.lean` proves the reference checker sound
  against Appendix A's model (`lean/MetatheoryFull.lean`, imported read-only)
  on the fragment the two share. It uses no `sorry`, no `admit` and no new
  axioms. `#print axioms` reports only `propext`, `Classical.choice` and
  `Quot.sound`.
- `tests/lockstep.rs` runs fixtures and a seeded generator through
  `vesper check`, the evaluator (`Runtime::run`) and the native JIT, and
  through the Lean reference, then compares verdicts and results.
- Results: the CI subset (49 fixtures and 400 generated programs) and a
  10,000-program nightly run both found **0 disagreements**.
- One imprecision in the Rust checker was found. It is reproduced on purpose
  by the reference and filed as #497.

These results are evidence about the Rust compiler on the programs that were
run, not a theorem about it. The soundness theorem is about the Lean reference
checker; the lockstep harness is what ties that checker to `src/check.rs`,
and only on the programs it ran.

## What was built

### The reference (`lean/ReferenceEval.lean`)

The file is self-contained (no imports). It has no `partial`, `unsafe`,
`native_decide`, `implemented_by`, `extern` or `opaque`. Every recursive
function is structural or fuel-indexed.

The fragment covers:

- **Scalars:** `i8`…`i64` and `u8`…`u64`, `bool`, unit, and contextually typed
  integer literals (the checker's `IntLiteral` pseudo-type). Out-of-range
  literals are rejected with E0209.
- **Checked arithmetic:** `+ - * / %`, negation, comparisons and `&& || !`.
  `wrapping_*` calls are included.
- **Obligations:** checked arithmetic raises `Fail[ArithOverflow]` unless the
  checker proves the operation in range from literal facts. `/` and `%`
  raise `Fail[RuntimeError]` unless the divisor is known nonzero. Nonzero
  facts are refined by `x == 0` / `x != 0` conditions and merged at joins.
  The right operand of `false && _` and `true || _` is treated as dead.
- **Control and data:** `let` (with and without annotations), blocks, `if`,
  first-order function calls (arity E0207), structs (field access E0202 and
  field count E0204), enums with `match` (exhaustiveness E0212), and the
  builtin `ArithOverflow` and `RuntimeError` types.
- **One-shot handlers:** user effects with one operation, the builtin `Fail`
  payload effects, and deep handlers with a return clause.
  - **Linear continuations:** E0302 (never resumed), E0303 (resumed twice),
    E0309 (resumed on only some paths), E0402 (aborted while a continuation
    is live) and E0410 (aborted inside a resume argument).
  - **Effect resolution** matches `src/check.rs`: an exact instance, else the
    innermost capability with that name (E0400), else E0401.
  - **Resume requirements:** each resume needs the effects observed in the
    handled body, minus the handled one, plus those of the return clause.
  - **Captures:** handler clauses see only captured owned-copy bindings
    (E0404).
- **Evaluator:** a CEK machine with first-order frames, deep handlers and a
  continuation store. It counts captures and resumes, enforces one-shot use
  at run time and reports the number of live linear continuations at exit.
  It evaluates struct fields in source order and arguments left to right,
  and it prints struct fields sorted by key, as `src/runtime.rs` does.
- **Output:** each program prints one line, either `reject CODE` or
  `accept TY value V ; captures=n resumes=n live_linear=n steps=n`.

### The soundness proof (`lean/ReferenceEval/Soundness.lean`)

Appendix A's `HasType` is a u64 calculus in A-normal form over `natV`,
`var`, `arith`, `letE` and `ifz`. The overlap with the reference is defined
by `Shape`:

- atoms: names that are not constructors, and non-negative literals;
- checked `+ - *` of two atoms (`arith`);
- `let x: u64 = v; b` (`letE`);
- `if a == 0 { t } else { e }` on an atom (`ifz`).

`tr` translates the overlap into the model's terms, using de Bruijn indices.

Theorems:

- `ok_typed` (Lemma A): every well-scoped overlap term (`OK ns e`)
  translates to a term with type `HasType Sig (ctxOf ns) RCtx.empty (tr ns e)
  .nat .copy [.abort]`. It holds for every effect signature `Sig`.
- `shape_ok` (Lemma B): if the reference `checkExpr` accepts a `Shape` term
  (with any fuel, effect context, expected type or start state), then the
  term is well scoped over the environment's names, and the checker leaves
  those names unchanged.
  - The proof works through `let` (push and pop), `if` (refine on both
    branches, then merge or pick the non-`never` side), binary operators
    (`binTail`) and effect requests (`requireEff`).
- `reference_checker_sound`, and its closed-program form
  `reference_checker_sound_closed`, combine A and B: whatever the reference
  accepts on the overlap is well typed in Appendix A at `u64`, as a copy
  value, with at most the `abort` effect.

**Rules covered:** `HasType.var`, `natV`, `arith`, `letE` and `ifz`, with
`Split`, `Discard`, `FrameOK` and `RowSub` for the unrestricted context.
**Not covered:** the model's `abort`/`handle` rules, linear and affine
modes, regions, records and sums, and the reference's signed kinds, `/` and
`%`, structs, enums and user handlers. These either have no counterpart in
Appendix A's calculus or would need a translation of the effect rows.

### The lockstep harness (`tests/lockstep.rs`)

- **Mirror AST and translator.** The harness renders each program both as
  Vesper source and in the reference's wire format. It also translates the
  parsed Rust AST into the fragment, so fixtures written as `.ves` files go
  through the same pipeline as generated programs.
- **Seeded generator.** It uses SplitMix64. It generates typed programs over
  integers, booleans, a struct `Pt`, an enum `Sh`, a user effect `Ask`,
  helper functions, nested handlers and resumes. It avoids struct literals
  where the parser restricts them: if conditions and match scrutinees.
  - A third of the seeds apply one of nine mutations: an unbound name,
    `bool` for an integer, an integer for `bool`, a dropped handler, wrong
    arity, a non-exhaustive match, resuming twice or never, an unknown field,
    and arithmetic inside a resume argument.
- **Comparison.**
  - **Verdicts:** reference and `vesper check` must agree exactly, reject
    codes included.
  - **Accepted programs:** the evaluator's value must equal the reference
    value, the JIT result must equal the evaluator's, and the reference must
    report `live_linear=0`.
  - **JIT gaps:** a JIT "unsupported" error is counted, not ignored. The
    count was 0.
- **Goldens.** `tests/fixtures/lockstep/{fixtures,generated}.golden` hold the
  reference's verdicts.
  - Without `VESPER_LEAN`, CI compares Rust against the goldens.
  - With `VESPER_LEAN`, the live reference must match the goldens too.
  - `VESPER_LOCKSTEP_BLESS=1` rewrites the goldens.
- **Fixtures.** `tests/fixtures/lockstep/*.ves` holds 46 programs (25
  accepted and 21 rejected, formatted with `vesper fmt`). The repository's
  `examples/*.ves` files that fit the fragment are also used. Examples that
  don't fit are listed by name (closures, counter, fail, projections,
  transaction) rather than silently dropped.

## Tests

| Test | What it checks |
| --- | --- |
| `fixtures_agree_with_the_reference` | 49 fixtures (28 accepted, 21 rejected) |
| `generated_programs_agree_with_the_reference` | seed `0x5eed_0453`, 400 programs, with minimum accepted, rejected and code counts |
| `nightly_campaign` (skipped unless `VESPER_LOCKSTEP_PROGRAMS` is set) | `VESPER_LOCKSTEP_PROGRAMS` programs from `VESPER_LOCKSTEP_SEED` against the live reference |
| `soundness_proof_checks_with_standard_axioms_only` | compiles MetatheoryFull and ReferenceEval to `.olean` in a private dir, checks Soundness.lean, audits axioms |
| `reference_is_executable_lean_without_escape_hatches` | no banned keywords; required entry points exist |
| `generator_is_deterministic_and_round_trips` | the same seed gives the same program; source → parse → translate gives back the wire form |

## Checks

All Lean runs used Lean 4.24.0 (the release `.zip` on Linux x86_64).

- `VESPER_LEAN=…/bin/lean cargo test --locked --test lockstep`: all tests
  pass, with 0 disagreements on fixtures (49) and generated programs (400).
- `VESPER_LEAN=…/bin/lean VESPER_LOCKSTEP_PROGRAMS=10000 <lockstep test binary> nightly --nocapture`:
  10,000 programs in 451 s, 6005 accepted and 3995 rejected, 0
  JIT-unsupported and **0 disagreements**.
  - Every accepted program completed with a value.
  - Reject codes: E0200 1120, E0201 606, E0202 9, E0207 44, E0212 75, E0302 51,
    E0303 37, E0400 1043, E0401 997, E0402 13.
- `lean Soundness.lean` with MetatheoryFull and ReferenceEval `.olean`s on
  `LEAN_PATH` finishes in about 10 s with no errors or warnings.
  - `#print axioms` gives `[propext, Classical.choice, Quot.sound]` for
    `ok_typed`, `reference_checker_sound` and
    `reference_checker_sound_closed`, and `[propext, Quot.sound]` for
    `shape_ok`.
- `cargo fmt --all -- --check`, `cargo test --workspace --locked`, and
  `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  results are in the PR description.
- `vesper fmt --check tests/fixtures/lockstep/*.ves`: clean.

## Root patch

None. The new paths are only the lane's exclusive ones. `vesper_crate()`
registers every root integration test as `//:test/<name>` and gives it the
root package's files as data, so Bazel picks up `//:test/lockstep`, with
`lean/**` and the fixtures, at the default timeout.

## Decisions

- **The reference mirrors the checker, imprecisions included.** Where
  `src/check.rs` is conservative, the reference is equally conservative, and
  the case is filed instead of "fixed" in the model. Otherwise every such
  program would be a disagreement, and the harness would stop being able to
  detect real ones.
- **Block expressions carry no integer fact.** In `src/check.rs`, `let`,
  `if`, `match` and calls drop facts. The reference was changed to match
  after the first lockstep run showed the difference.
- **Fuel everywhere.** The checker is fuel-indexed mutual recursion in
  `StateT St (Except Diag)`, so the proof can unfold it, and running out of
  fuel is an explicit E0999 that the harness would report.
- **Goldens in CI.** CI does not need a Lean toolchain to run the harness.
  With `VESPER_LEAN` set, the live reference is still checked against the
  goldens, so the two can't drift apart.

## Findings

- **#497:** after `if x == 0 { .. } else { .. }`, the merge drops the fact
  for an immutable `x`, so a later `x + 1` gets a spurious E0401. The
  reference reproduces this on purpose.
- **Parser:** `Name {` is never a struct literal anywhere inside an `if`
  condition or `match` scrutinee, even under parentheses or inside a block.
  This is documented here rather than filed, because it is a deliberate
  grammar restriction. The generator works around it.
- **JIT gaps:** none were hit on the fragment (0 of 6005 accepted programs).

## Remaining gaps

- The soundness theorem covers only the overlap listed above.
  - Linear continuations, handlers and effect rows beyond `abort` are not
    tied to Appendix A's `handle` and `resume` rules.
  - Extending the proof needs a translation of the reference's effect rows
    into the model's labels.
- The evaluator is not proved to agree with the model's small-step
  semantics. Agreement with the Rust evaluator is tested, not proved.
- Out of the fragment: closures, mutable state, regions, projections,
  transactions, multi-operation effects, generic functions and modules.
  Programs using them are skipped and listed.
- Nightly-sized runs are a manual or scheduled command
  (`nightly_campaign`); no nightly workflow was added, because CI belongs to
  the integration coordinator.
