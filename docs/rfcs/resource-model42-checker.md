# Finite resource checker (#322)

Implementation checkpoint under [the frozen model contract](resource-model42-contracts.md).
The checker lane owns only `lean/ResourceModel42/Checker.lean` and this report.
The compiled Syntax dependency is d51a21b. Checker source checkpoint b9ce0c0
contains actual type/ownership inference and its opaque checked wrapper. The separately claimed `CheckerProofs.lean` checkpoint9be3eae now proves the
substantive checker-to-Full typing theorem. Runtime/ledger/substitution/closure
and operational release proofs remain owned by their separate modules. No
source/compiler refinement is inferred from this finite model.

The admitted pure ANF fragment contains unit, bounded u64-like naturals,
de Bruijn locals, strict let, structural binary pair/full unpack, numeric-zero
branches and one atomic linear ticket's mint/consume. Pair floors are copy or
affine; component kinds must be below their field modalities. There are no
caller-supplied inferred kinds, ownership masks, typing proofs, accepted flags,
invocation values or static facts. Initial declared inputs are copy-only.

Actual inference computes a result type, bounded expanded type
metrics and one Boolean demand per declared local. Bits record all references,
including copy references. Sequential composition permits overlapping demand
only for copy locals and unions those bits. Noncopy overlap fails. Let and
full unpack infer binder modes from actual component kinds and require every
linear local to be demanded. Unpack binds left at index one and right at zero.
Both branch arms are checked regardless of a constant condition. Their result
types and noncopy demand must agree; affine-only demand disagreement is an
explicit conservative profile refusal, rather than full-language invalidity.

The checked wrapper has a private constructor and stores genuine inference
success. Readonly accessors and public inference/helper definitions allow the
independent proof module to derive actual `Full.HasType`, rather than assume a
caller typing premise or define soundness as another success flag. The strong
`ContextFits` statement requires demanded bindings at their true type/kind,
excludes unused linear entries and permits unused copy/affine extras. This is
necessary for unused affine let binders in Full's body context. Any inferred
root kind is legal; owned results retain ownership, while the completed-copy
exhaustion theorem has its explicit copy-result premise.

Inferred pair types prepay their full expanded tree size and depth before
construction. Cached metrics are derived from real types, never caller
assertions. Compact repeated copy-pair construction therefore cannot bypass the
type bound. Syntax occurrences, lexical context and traversal depth are also
finite under the frozen limits. The evaluator's independent dynamic value
bound can be lower than the checker type bound, so runtime growth refusal can
retain an earlier actual mint prefix. These are logical finite model limits,
not a claim to bound every allocation in Lean or its compiler.

Semantic first errors, profile/admission refusal, invalid invocation, runtime
invariant, fuel and measured runtime limits remain distinct. No complete Rust
diagnostic correspondence, backend trace agreement, source refinement, native
observer, 100000-program campaign or full section 42 completion is claimed.

The exact public inference seam is `inferAtom limits context atom` and
`infer limits context expression`, where `Context = List TypeInfo`,
`Usage = List Bool`, and `InferInfo` contains `result : TypeInfo` and `uses`.
The smart pair constructor prepays expanded type metrics; even a variable info
created under a larger configuration is checked against the current bound.
`checkProgram : ValidatedProgram → Except CheckFailure CheckedProgram` runs
actual inference. The private checked wrapper exposes `.program`, `.info`,
`.resultType`, `.uses` and a genuine inference-success equality `.check_ok`.
No invocation values occur in any of these checking signatures.

Structured failures are `CheckFailure.semantic CheckError` and
`CheckFailure.profile CheckProfile`. Semantic error kinds are unknownVariable,
typeMismatch, duplicateUse, unusedLinear, fieldModality, literalOutOfRange and
branchOwnership. Blame carries `site` and optional de Bruijn `slot`; these are
logical model labels, not Rust byte spans. Profiles distinguish type-bound,
context-bound and affine-only branch-demand refusals. Linear-demand mismatch
has global priority over an earlier affine slot within the branch comparison.

Explicit named checker theorem catalog:
`Vesper.ResourceModel42.CheckedProgram.check_ok`. This is the wrapper's actual
inference-success equation, not a substitute for the independently owned
`Full.HasType` and execution correspondence proofs. Public helper definitions
are available for those proofs to unfold; the real auditor must receive
semantic theorem roots, not definition names.

Actual source and validation:

- Checker b9ce0c0 compiled with pinned Lean 4.24 against immutable Syntax/Full.
- CheckerProofs9be3eae compiled with the same real compiler, no errors or
  warnings. `inferAtom_success_sound` and `infer_success_sound` construct actual
  `Full.HasType` with empty resources/row from inference success and strong
  ContextFits. `checkedProgram_declared_sound` discharges the context condition
  from the independently validated copy-only initial declarations, without a
  caller typing or ownership-mask premise.
- All 24 fixed handwritten smoke assertions passed after correcting the
  temporary helper's missing Except equality instance. The earlier helper
  failure was not counted as a semantic pass. Cases cover copy reuse,
  noncopy overlap, ownership-return acceptance, linear scope loss, strict
  unused initializers, full-unpack indices, both branch arms, linear-first
  mismatch classification, affine-only refusal, field restriction, natural
  range, finite input usage and inferred copy-pair type expansion.
- Real Lean `#print axioms` on the atom/inference/checked/declared soundness
  roots found only propext, Classical.choice and Quot.sound; the inference
  length root uses only propext. This preliminary query is not a substitute
  for the independently owned complete kernel replay and axiom audit.

Logs are `/tmp/resource-model42-checker-v1.log`,
`/tmp/resource-model42-checker-proofs-v1.log`,
`/tmp/resource-model42-checker-smoke-v2.log` and
`/tmp/resource-model42-checker-axiom-smoke-v1.log`. The first three successful
logs are empty. Immutable complete proof sources/objects are at
`/tmp/resource-model42-checker-proofs-api-v1`. Git diff checks passed; two
independent source reviews of the checker found no blocker. Compiler leases
were explicitly released after commands exited. No Cargo command ran here.

## Complete CheckerProofs theorem catalog

All 42 explicit named theorems are public, under the prefix
`Vesper.ResourceModel42.CheckerProofs.`. Every entry must participate in the
actual theorem-only audit; none is a definition submitted as a theorem root.
The checker module's additional root `Vesper.ResourceModel42.CheckedProgram.check_ok`
remains in the separate catalog above.

```text
sourceAtom_isAtom
split_right_all
frame_empty_row
contextFits_demand
contextFits_absent
mode_discardable_of_ne_linear
demand_zero
demand_succ
zeroUsage_demand
singletonUsage_demand
usageUnion_demand
firstOverlap_none_disjoint
mergeUsage_disjoint
mergeUsage_union
entryFits_split
contextFits_entry
contextFits_split
contextFits_full_binding
contextFits_unused_binding
contextFits_zero_discard
contextFits_singleton_lookup
contextFits_singleton_rest
checkBinderUse_linear
contextFits_extend
usage_tail_tail
firstDemandMismatch_none_equal
mergeBranchUsage_properties
entryFits_branches
contextFits_branches
except_bind_success
liftType_success
inferAtom_success_sound
infer_success_sound
checkedProgram_success_sound
contextFits_declared_copy
checkedProgram_declared_sound
zeroUsage_length
singletonUsage_length
usageUnion_length
mergeUsage_length
inferAtom_success_length
infer_success_length
```

These are actual constructive checker proofs, not completeness for Full/Rust,
full diagnostic agreement, execution correspondence, or a resource campaign.
The separately owned complete model acceptance and transitive kernel audit
remain required release evidence.
