# Executable resource model integration (#322)

Status: **draft implementation; semantic bridge and audit incomplete**.
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
| Evaluator and invocation | `26ec38f` | `6746b2c` | Compiles; private invocation retains actual admission equality |
| Erasure/substitution | `012a4c7` | `d4c13a4` | 11 public foundation theorems compile |
| Finite ledger | `6ae4905` | `79ae227` | 20 public store/history/embedding theorems compile |
| Initial acceptance | `bccfecb` | `1d27bb1` | 28 fixed kernel-computed syntax/admission/metric assertions compile |

Independent source reviews found no additional blocker in the actual syntax,
checker and evaluator checkpoints after correcting blanket rejection of zero
limits. The reviews ran no builds and do not establish the unproved bridges.

The initial acceptance count is **28**, corrected from an earlier estimate of
29 by inventorying the actual declarations. A checker scratch smoke helper
failed because it required a nonexistent `Except` equality instance; no smoke
success is claimed. Acceptance instead uses actual computed equalities.

The coordinator aggregate imports implemented production/proof modules only.
It does not import acceptance. The final acceptance module will import the
aggregate so its fresh artifact closure includes all implemented proofs.

## Required release gates still pending

- Actual inference success constructs `MetatheoryFull.HasType` under the
  strong finite-context relation, without a caller-supplied typing premise.
- Separately admitted copy invocation values close the source into an actual
  initial well-typed configuration; input-closing silent lets count as fuel.
- Every committed evaluator transition erases to one actual Full transition;
  complete successful prefixes and event projections correspond.
- Checked reachable configurations cannot report an invariant failure.
  Resource refusals must identify genuinely exceeded independent logical
  metrics, including proposed substitutions and enclosing contexts.
- Preservation/progress and completed-copy exhaustion/exact-once corollaries
  derive from actual correspondence and the existing Full theorems. Returning
  an owned value remains legal and may retain a live residual.
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

The [frozen contract](resource-model42-contracts.md) defines the fragment,
limits, result ownership, error categories and exclusive module writers.
Proof helpers have been split into independent static, erasure, ledger,
operational and metric modules. Shared Cargo builds require an exclusive
lease; Lean compilation uses private versioned roots and immutable dependency
checkpoints, with at most two compiler leases active.
