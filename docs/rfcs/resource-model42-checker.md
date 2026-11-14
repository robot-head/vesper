# Finite resource checker (#322)

Design checkpoint under [the frozen model contract](resource-model42-contracts.md).
The checker lane owns only `lean/ResourceModel42/Checker.lean` and this report.
Syntax must publish its coherent API before checker implementation begins.
No checker source or proof has been implemented or executed at this checkpoint.

The admitted pure ANF fragment contains unit, bounded u64-like naturals,
de Bruijn locals, strict let, structural binary pair/full unpack, numeric-zero
branches and one atomic linear ticket's mint/consume. Pair floors are copy or
affine; component kinds must be below their field modalities. There are no
caller-supplied inferred kinds, ownership masks, typing proofs, accepted flags,
invocation values or static facts. Initial declared inputs are copy-only.

The proposed actual inference computes a result type, bounded expanded type
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

Validation at this checkpoint: read-only review of actual Full typing/context
rules and the historical model plan; coordination with Syntax, evaluator,
proof and independent acceptance owners; no Lean or Cargo builds. Exact
implemented APIs, public theorem catalog and executed evidence will replace
this design-only checkpoint after source authorization and execution.
