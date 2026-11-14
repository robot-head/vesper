# Finite resource checker (#322)

Implementation checkpoint under [the frozen model contract](resource-model42-contracts.md).
The checker lane owns only `lean/ResourceModel42/Checker.lean` and this report.
The compiled Syntax dependency is d51a21b. Checker source checkpoint b9ce0c0
contains actual type/ownership inference and its opaque checked wrapper. The
independent Proofs module still owns the substantive typing theorem; compiling
this checker is not presented as that theorem or as source refinement.

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

Actual validation so far: pinned Lean 4.24 compiled Checker b9ce0c0 successfully
against immutable compiled Syntax/Full dependencies. The final successful
compile log `/tmp/resource-model42-checker-v1.log` is empty. Its immutable
source/object checkpoint is `/tmp/resource-model42-checker-api-v1`. Git diff
checks passed and two independent source reviewers found no blocker. An
initial scratch semantic smoke command failed because its temporary helper
used equality on Except without a DecidableEq instance; that is a test-helper
failure, not successful semantic evidence. No scratch assertion pass is
claimed. The helper was corrected without another compilation after the
explicit compiler-lease handoff. Independent fixed semantic acceptance and
actual transitive kernel audit remain pending; no Cargo command was run here.
