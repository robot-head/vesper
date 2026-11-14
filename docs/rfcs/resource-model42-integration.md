# Executable resource model integration (#322)

Status: **model proofs compile; combined artifact audit remains blocked**.
PR: <https://github.com/robot-head/vesper/pull/323>.

This implements a restricted logical fragment of §42.1. It does not establish
Rust/compiler refinement, observed native resource events, source adaptation,
ordered compiler-diagnostic agreement, a 100000-program resource campaign,
full-calculus decidability or self-hosting.

## Implemented and compiled checkpoints

All source checkpoints below were compiled with the real pinned Lean 4.24.0
toolchain, alongside unchanged `MetatheoryFull`. The individual lane reports
record commands, artifact roots, intermediate development failures and the
actual final successful compilation. Compilation is not a substitute for the
remaining semantic theorems or independent transitive kernel/axiom audit.

| Component | Author checkpoint | Integrated checkpoint | Actual result |
| --- | --- | --- | --- |
| Syntax and validation | `d51a21b` | `c821dde` | Compiles; 2 public type-cache theorems |
| Type/ownership checker | `b9ce0c0` | `e9a5e20` | Compiles; private checked wrapper retains actual inference equality |
| Checker soundness | `9be3eae` | `0430f2a` | 42 public theorems compile, including actual inference-to-Full typing |
| Evaluator and invocation | `26ec38f` | `6746b2c` | Compiles; private invocation retains actual admission equality |
| Erasure/substitution | `012a4c7` | `d4c13a4` | 11 public foundation theorems compile |
| Finite ledger | `6ae4905` | `79ae227` | 20 public store/history/embedding theorems compile |
| Operational correspondence | `ede8c75` | `211628a` | 29 public actual-step/prefix/value/event/no-invariant/measured-refusal theorems compile |
| Input closure and checked runs | `64e4394` | `7fd124b` | 33 public theorems compile, including initial Full.WT and public-trace exact-once consumption |
| Metric correctness/completeness | `defc692` | `444288d` | 68 public theorems compile; refusals witness actual excess and admissible metrics cannot falsely refuse |
| Semantic acceptance | `0748342` plus import integration | `73751db` / `449cec4` | All 78 fixed assertions compile in both fresh full runs |

Independent source reviews found no additional blocker in the actual syntax,
checker and evaluator checkpoints after correcting blanket rejection of zero
limits. The reviews ran no builds and do not establish the unproved bridges.

The initial acceptance count was **28**, corrected from an earlier estimate of
29 by inventorying the actual declarations. The later 47 checker/evaluator
cases bring the actual passing total to **75**. A first checker scratch smoke
helper failed because it required a nonexistent `Except` equality instance;
after repair, 24 scratch semantic assertions compiled successfully. These are
separate from the 75 independently maintained acceptance assertions.
Three later identity-history controls bring the fresh-compiled acceptance
total to **78**. The complete manual theorem catalog contains **284** roots,
including these assertions; it is not a count of independently audited roots.

The coordinator aggregate imports implemented production/proof modules only.
It does not import acceptance. The final acceptance module will import the
aggregate so its fresh artifact closure includes all implemented proofs.
The coordinator independently compiled the aggregate at `71f5219` using the
genuine immutable dependency artifacts (0.24 seconds, exit zero). An initial
attempt failed to resolve a module across separate partial LEAN_PATH roots;
assembling a complete private dependency root fixed that infrastructure issue.
This was an aggregate import check, not fresh compilation or an axiom audit.

## Actual strict-audit blocker and prerequisite

The first complete Cargo run compiled all 12 modules and 78 assertions, then
the unchanged strict auditor rejected a generated recursive Repr helper. The
unused Repr derivations were removed in `5d00aa0`. A second fresh run again
compiled all 12 modules/78 assertions and then rejected a derived equality
helper. Neither run passed an audit partition or the later controls.

A complete diagnostic inventory established that Lean 4.24 also creates
partial native `_unsafe_rec` helpers for ordinary total structural recursion,
including unchanged MetatheoryFull definitions. An equality-only rewrite cannot
resolve that general compiler-artifact mismatch and is not integrated.

Public prerequisite [#358](https://github.com/robot-head/vesper/issues/358) adds
the new [kernel-only artifact tool](resource-model42-kernel-artifacts.md).
The existing strict auditor and CLI remain unchanged. The tool preserves the
logical declarations, inventories omitted partial native helpers, rejects
explicit unsafe and source-partial opaque inputs, checks complete logical
dependencies and actual kernel replay, and checks export/reimport equality.
Its fixed small controls include actual strict-audit success for projected
total recursion and continued rejection of forbidden axioms. These outcomes
are not full-model audit success. Different equation-theorem proof copies in
the larger diagnostic require further evidence and an explicitly reviewed
policy before they can be admitted.

## Required release gates still pending

- Actual inference-to-typing, input closure and successful operational
  correspondence are implemented and compile as listed above; their complete
  independent kernel/axiom audit remains pending.
- Genuine resource-refusal witnesses and preservation/progress/completed-copy
  exhaustion corollaries are implemented and compile. The final artifact
  pipeline must independently audit all their actual roots.
- Actual checker/evaluator semantic controls, wrapper privacy controls, fresh
  module compilation and transitive kernel replay/standard-axiom policy audit
  run through the new Cargo acceptance target. Missing/wrong Lean and actual
  missing/corrupt artifact controls must fail rather than skip.
- The exact public theorem inventory is audited in requests of at most 128
  theorem roots, with complete union coverage. Computable definitions enter
  actual theorem dependency closures; they are not submitted as theorem roots.

The draft is not eligible for merge until these gates are implemented and
validated. No empty-ledger completion guard, conditional success theorem or
refusal tag alone can replace them.

## Scope and coordination

The original model baseline is main `30de7fe`; the primary checkout has since
fast-forwarded to merged `d1caa0a`, including external capability-verifier
integration #308 and reference tracing #319. Model changes remain confined to
new `lean/ResourceModel42*`, `tests/resource_model42.rs` and these reports.
Existing compiler, runtime, native, Cargo and CI files remain outside the claim.
The branch also incorporates already-merged main `c025b9a` (#321 and #276);
those dependency merges do not add implementation-owned changes to those paths.

The [frozen contract](resource-model42-contracts.md) defines the fragment,
limits, result ownership, error categories and exclusive module writers.
Proof helpers have been split into independent static, erasure, ledger,
operational and metric modules. Shared Cargo builds require an exclusive
lease; Lean compilation uses private versioned roots and immutable dependency
checkpoints, with at most two compiler leases active.
