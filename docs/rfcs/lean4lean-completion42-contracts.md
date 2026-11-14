# Reconstruction completion prerequisites (#539–#543)

Base own #538 `e012926cd53a5b1da9b479a86e7c81e45d85e6c6`.
Before editing, all 86 open issue scopes and all 42 PR metadata/actual
paginated filename lists were reviewed: 770 path occurrences, 720 distinct
paths, no API errors or external conflict. Evidence:
`/tmp/lean4lean-completion42-ownership-20261114T1358.json`, SHA256
`d8520a12b582517449cc02590685c185a783a3711db96728301214f728c58c7c`.
Live design re-read: modified 2026-11-13T04:46:25.948Z, Section 42 unchanged.
External source/reference #453/PR510, Place #455/507 and extractor #461/520
remain read-only. Preserve own endpoint #498 registration at integration.

## Frozen definitions and owned modules

All names live in `Vesper.Lean4LeanLevel42`. Root owns Completion.lean,
importing MapValue only. Its transparent definitions are frozen:

```lean
EdgeCovered s acc a :=
  ∃ p n k, s.get? p = some n ∧ (⟨a,k⟩ : VarNode) ∈ n.var ∧ p ⊆ a :: acc
Admissible s acc [] := True
Admissible s acc (a :: rest) :=
  EdgeCovered s acc a ∧ Admissible s (a :: acc) rest
Completes s acc rem := ∃ order, order.Perm rem ∧ Admissible s acc order
MapCompletes s := ∀ p n, s.get? p = some n → Completes s [] p
VarSupport s t := ∀ p n a k, s.get? p = some n → ⟨a,k⟩ ∈ n.var →
  ∃ q m j, t.get? q = some m ∧ ⟨a,j⟩ ∈ m.var ∧ q ⊆ p
```

Offsets remain arbitrary. Neither algorithm success nor semantic correctness
defines a certificate. Root also owns core support reflexivity/transitivity,
accumulator monotonicity and fixed-key certificate transport. MapCompletes
transport MUST additionally require target-key origin:
`∀ p n, t.get? p = some n → ∃ m, s.get? p = some m`.
VarSupport alone cannot exclude newly introduced uncertified keys.

- #540 owns NEW Feasible.lean and completion42-feasible report. Import
  Completion and Addable. Prove actual addable→EdgeCovered unconditionally
  from operational witness and subset soundness. Own exchange/list algebra,
  actual feasible.go soundness, completeness with sorted stored keys,
  KeySorted accumulator and `rem.length ≤ fuel`, then actual feasible iff
  Completes. Do not assume remaining-name nodup or `a ∉ acc`; none insertion
  is legitimate. Any needed extra premise must be justified by actual behavior.
- #541 owns NEW VariableSupport.lean and completion42-support report. Import
  Completion and MapSubsumption. Prove actual removed-variable witness for
  subsumeVars, erased-context minimize/minimizeEntry support, actual outer
  subsumption support and final-key origin. A minimizeEntry helper requires
  `acc.get? p = some original`; arbitrary insertion cannot inherit origin.
  Compose MapCompletes preservation only with both support and key origin.
  Add MapHelpers only if genuinely necessary; no arbitrary lookup assumption
  standing in for a proved fold invariant.
- #542 owns NEW ReifyCongruence.lean and completion42-congruence report.
  Import Addable/MapEquality as needed. Prove actual feasible and lexChain
  lookup congruence, unconditional lexChain permutation including fallback,
  actual toTree/reified Level congruence from lookup and actual NormLevel-BEq.
  No physical TreeMap equality or native opaque Level.beq reflection.
- #543 owns only the additive Packet::Completion runner extension and NEW
  completion42-acceptance report. Root owns imports-only CompletionFinal,
  tests/lean4lean_completion42.rs, narrow BUILD registration/tags and final
  integration report. Freeze actual public theorem names before guard catalog.

Fixed IDs: label `lean4lean_completion42`, source module
`Lean4LeanLevel42.CompletionFinal`, projection `Lean4LeanCompletion42Kernel`,
scope `audited_completion_prerequisites_and_reconstruction_congruence`,
marker `LEAN4LEAN_COMPLETION42_EVIDENCE`.

The exact existing Addable/MapSubsumption/MapEquality closure contains 18
modules including Lean4Lean.List/Level. These five new modules yield 23;
MapHelpers yields 24. Do not import PathPriming, LevelSemantics, NormalizeInvariants,
Shape/ShapeFinal or ReifyCycle into this packet. Preserve all nine previous
catalogs/specifications and the complete validator suffix byte-for-byte,
including source cap 24, batch 128, whole-matrix 300-second budget,
standard-three policy and every real refusal/artifact control. No arbitrary
module selector, supplied answer or weaker-policy mode.

## Execution and claims

Use isolated worktrees, immutable complete private dependency roots and one
LEAN_PATH; a namespace prefix shadows fallback directories. At most two Lean
compiler leases globally, `-j1 -M4096`, LEAN_NUM_THREADS=1. One Cargo/shared
target lease. No project axioms, sorry/admit, unsafe/partial or native_decide
in new proof sources. Keep failed attempts and exact source/object/tool/report
associations. Audit every actual authored theorem and alternate alias, run
the new and all nine prior complete matrices, and separately validate actual
Bazel packaging. No automatic merge onto own unprotected stack parent.

This wave proves completion prerequisites, support preservation and
deterministic reconstruction congruence. NormalizeAux completion preservation,
actual lexChain admissibility/minimality, full tree denotation, normalized
reification correctness, canonicality, whole-checker/compiler/native equivalence
and source-campaign completion remain separate. Existing raw-cycle results
do not assert that normalization produces the cycle.

Final implementation note: the same Admissible nil/cons equations are realized
through the existing List.foldr at 19d0d39. No additional generated recursion
helper is introduced or omitted; public interfaces and proof premises remain.
