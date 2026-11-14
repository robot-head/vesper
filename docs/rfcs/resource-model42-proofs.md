# Finite resource model proof architecture (#322)

Design checkpoint against the frozen `resource-model42-contracts.md` at
`ddf5d6c`, based on main `30de7fef8be040a890e34f26370ee15b1429642e`.
This report is currently a proof plan, not completed proof or execution
evidence. No Proofs.lean file, build, kernel replay or test execution is claimed
at this checkpoint. Source implementation waits for the syntax writer's first
coherent concrete API and the checker/evaluator interfaces built on it.

Only new `lean/ResourceModel42/Proofs.lean` and this report belong to the proof
lane. Existing Full metatheory, compiler, runtime, Cargo and other writers'
files remain unchanged. The model is the frozen pure ANF fragment, not the
full §42 oracle or a source/backend differential campaign.

## Dependency order and proof authority

The intended dependency graph is Syntax -> Checker/Evaluator -> Proofs.
Syntax defines the admitted source/runtime representations and computable
operations; Checker provides actual inference and an opaque checked wrapper
with its genuine inference-success equality; Evaluator provides validated
copy invocation values, initialization and actual bounded steps/runs.
Proofs imports these definitions and establishes correspondence to
`Vesper.MetatheoryFull`. Neither Checker nor Evaluator imports Proofs.
Cross-module operational helpers must therefore be accessible to the proof
module, rather than hiding essential definitions behind unchecked assertions.

A checker-success equality is evidence about the actual computable checker;
a caller-supplied typing premise or accepted boolean cannot replace it. A
successful model transition must construct an actual Full.Step witness, not
just preserve a terminal empty-store predicate. Public named helper theorems
are part of the audit catalog just like the headline results.

## Strong finite-context checking soundness

Inference computes the result type and a Boolean demand vector over declared
local types. The vector marks every referenced variable, including copy
variables. Sequential composition permits repeated demands only at copy
kind; noncopy demands must be disjoint. Branches agree on every noncopy demand
and combine copy demands. Affine-only disagreement is the frozen conservative
profile refusal, not a claim that all such programs are semantically invalid.

The central statement uses a strong ContextFits relation, not only a minimal
used-variable mask. For each declared local type tau and its demand bit:

- A demanded local is present in the Full context at erased tau and exactly
  tau.kind. A ticket can never be assigned copy binding mode.
- An unused linear local is absent.
- An unused copy or affine local may be absent or present at that same type
  and actual kind.
- Outside the finite declared context the Full context is absent.

This permits the unused affine/copy binder that Full.letE still extends into
the body context. A minimal-mask-only theorem cannot justify that case.
Inference must also establish the demand vector's length equals the finite
context length.

The principal planned theorem is: successful inference plus ContextFits
constructs Full.HasType for the erased expression, inferred erased result
and actual result kind, with empty resource context and empty effect row,
for every irrelevant effect signature. It will be proved from the actual
algorithm and syntax, rather than assumed in the checked wrapper.

The context split is explicit. Copy bindings can be supplied to both children;
a used noncopy binding goes only to its demanding child; a spare unused affine
binding can go deterministically left. Both branches receive a common context
compatible with their equal noncopy masks. Let/unpack extend body contexts at
inferred component kinds and require each linear head to be used. Unpack has
left binder index 1 and right binder index 0. Field well-formedness
`component.kind <= override` remains a real premise of Full.pair typing.
Pair nominal floors are only copy or affine; no extra nominal root obligation
is invented or discarded.

Copy-only declared input types are independent of invocation values. After
validating those values, reversed nested pure lets close the program with
input 0 innermost. A separate closure lemma must establish the initial closed
Full.HasType/WT configuration from checker soundness and actual typed inputs.
Those initialization lets execute and their silent steps count as fuel.

## Operational erasure and finite ledger

The proof module must relate source embedding and runtime erasure, establish
that every runtime value erases to Full.IsVal, and show decorated atom/expression
substitution commutes with Full.subst. Runtime values may contain closed pairs
or handles after substitution even though source atoms have no handle or pair
literal constructor. Pair-as-value recognition erases identically; it costs no
invented Full transition. Unpack reduction is exactly substitution of right at
zero, followed by left at zero.

Event operations retain their source occurrence labels when values are moved
or copied. These labels remain logical model sites, not parsed Vesper spans.
Full erasure removes sites and maps the single ticket kind to Full.hdl; it
makes no additional nominal-kind refinement claim.

Interpret the finite retained live/dead ledger as a Full.RCtx: a true entry at
serial r means some linear, and false/out-of-bounds means none. The Full next
counter is the total ledger length, not the number of live entries. Required
lemmas cover append-live mint, mark-dead consume, unchanged length on consume,
and freshness at all serials greater than or equal to the ledger length.
Consumed tombstones are retained, preventing identity reuse.

For each successful root transition, prove the actual embedded before/after
configurations are related by one Full.Head/Step. Let-initializer focus is
lifted through Full's let evaluation context. Internal focus recursion is not
additional operational fuel and cannot execute the body or an unselected
branch. The public resource trace is a filter of the complete successful
silent/mint/consume step prefix; the Full.Steps theorem must use that complete
prefix and retain the exact action order and identities.

## Admission, progress and non-vacuity

Saturation/prepayment is required before materializing substitution growth,
including copy-pair doubling, both branch syntax, whole enclosing let contexts,
and both intermediate/final unpack substitutions. A focused redex may use a
remaining-context budget or an equivalent precomputed whole-expression plan;
checking a new oversized expression only after constructing it is insufficient.
Type/value expansion, runtime depth, input-let prefix, ledger and event limits
all have their own frozen bounds. Refusal preserves the actual prior term,
ledger and event prefix without cleanup consumes.

Success-soundness alone would allow an evaluator that always returns an
invariant failure. The proof interface therefore also requires:

- Completed results erase to actual Full values.
- Checked reachable states cannot produce RuntimeInvariant; the alternatives
  are a value, an actual transition, or an explicit measured capacity refusal.
- Fuel exhaustion is outside a successful step and preserves the prefix.
  Terminal recognition happens before testing zero fuel.

Existing Full.progress/type_safety are useful once initial typing, erasure and
step correspondence exist, but they do not automatically prove that the new
computable evaluator selects the appropriate case. Representation-specific
closed-atom, value-recognition and dynamic-ledger arguments are still needed.
Capacity refusal and invocation/profile errors must remain distinct from
semantic failure; this fragment has no language abort or arithmetic effect.

## Full-derived conclusions and limits

From actual checked initialization and Full.Steps correspondence, derive
preservation and progress/refusal classification. Completed results may have
any admitted kind. Returning a ticket or linear pair is legal and leaves a
visible residual, so no global exhaustion assertion applies to every Completed
result.

For completed copy-kind roots only, apply the existing
Full.total_linear_exhaustion and Full.terminal_no_linear theorems to derive
exactly one terminal consume per minted serial and absence of live linear
ledger entries. The evaluator must not enforce an empty-ledger completion
guard and then present that guard as the proof. With no continuation forms,
Full release counts correspond directly to actual consume actions in the
model's ordered prefix.

All new named theorems will be public. The final catalog must be extracted
from actual implemented declarations and audited completely, partitioning
requests to at most 128 roots and checking the complete union. The frozen
transitive allowlist is propext, Quot.sound and Classical.choice, including
existing Full theorem dependencies. No project axioms, sorry/admit,
unsafe/partial definitions or native-decide escapes are permitted.

Actual validation commands, theorem counts, build/audit results and any
remaining gaps will replace this planning-only status after implementation.
No source/Rust/native refinement, full-language completeness, external
Lean4Lean profile coverage, physical-resource result, 100000-case campaign,
whole-process resource bound or self-hosting claim follows from this plan.
