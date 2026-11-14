# Actual normalization-map denotation (#435)

Frozen parent #432: `1ef989ea415c1b9e8367da8eb97c20e9cae1d721`.
Claims #436–#439 precede source. No actual helper/vendor/provenance, parent
proof, auditor/projector/profile, runtime/compiler, Cargo or workflow change.
One new BUILD copy_test_cwd label uses the existing strict regular-input mode.
The separate inherited Kinetic Clippy compatibility dependency remains #434.

Ownership under `lean/Lean4LeanLevel42/`: #436 NEW NodeResidual.lean;
#437 NEW MapMinimize.lean; #438 NEW MapSubsumption.lean and NormalizeFinal.lean.
Each owns its NEW `docs/rfcs/lean4lean-normalization42-` residual/minimize/
subsumption report. #439 owns NEW tests/lean4lean_normalization42.rs, acceptance
report, sixth static Packet arm in tests/common/lean4lean_proof_packet.rs and
one new BUILD regular-input label. Coordinator owns this contract, NEW
lean/Lean4LeanNormalization42.lean aggregate and final integrated evidence.

Isolated worktrees, immutable versioned source/object dependency roots with
verified identities, exclusive Cargo and at most two Lean compiler leases.
#436 lease1; #437 lease2 when dependencies ready. #438 may draft but needs
explicit coordinator compiler grant after a lease frees. No Cargo in lanes.
All proof helpers PUBLIC ASCII theorem commands; no sorry/admit/project axiom,
unsafe/partial or native_decide. Fixed handwritten Lean allowed; generated
Lean must use vesper-lean-syntax AST and one render, including audit scripts.

## Residual structure and self context (#436)

Namespace Vesper.Lean4LeanLevel42; import frozen Subsumption, NameOrder and
needed Semantics/ListProofs. Use ACTUAL Node.subsumeBy/subsume/subsumeVars.

```lean
def nodeVarsValue (ρ : Valuation) (p : List Name) (n : Node) : Nat :=
  if enabled ρ p then varsValue ρ n.var else 0
```

Required public APIs:

- `subsumeVars_sublist (xs ys) : (Normalize.subsumeVars xs ys).Sublist xs`.
- `node_subsumeBy_var_sublist`: actual output var list is a sublist of n1.var.
- `node_subsume_var_sublist`: same for actual subset-wrapper Node.subsume.
- `node_subsume_WF`: NodeWF p1 n1 implies NodeWF p1 of the actual result,
  requiring no WF of the right node.
- `node_isEmpty_value_zero`: n.isEmpty = true implies nodeValue ρ p n = 0.
- `node_subsume_vars_union`, with NO WF premise:

```lean
max (nodeVarsValue ρ p1 (Node.subsume p1 n1 p2 n2)) (nodeValue ρ p2 n2) =
  max (nodeVarsValue ρ p1 n1) (nodeValue ρ p2 n2)
```

- `node_subsume_same_context`, for arbitrary residual r and C:

```lean
NodeWF p original →
nodeVarsValue ρ p original ≤ max (nodeVarsValue ρ p r) C →
max (nodeValue ρ p (Node.subsume p r p original)) C =
  max (nodeValue ρ p r) C
```

Reuse frozen node_subsumeBy_vars_union and removed-constant bounds. The self
step leaves variables unchanged but can remove a constant using ORIGINAL
variables; their coverage by current variables/external context is essential.

Include public `self_context_needs_vars_counterexample`: p=[u], ρu=1,
original={const1,var[u+0]}, artificial residual={const1,var[]}, C=0.
Residual WF, sublist and whole-value equality all hold, yet actual self
comparison changes value one to zero. The variable coverage premise fails.
Do not assume a self-comparison value result or whole-map preservation.

## Actual fixed-map minimization (#437)

Import NodeResidual and frozen MapValue. Required APIs:

- `minimize_var_sublist acc p original`: actual minimized vars sublist original,
  with no WF/lookup premise.
- `minimize_WF`: NodeWF p original implies NodeWF p actual minimized result.
- `minimize_vars_context` and `minimize_value_context`, under ONLY
  MapWF acc and actual acc.get? p = some original:

```lean
max (nodeVarsValue ρ p (acc.minimize p original)) (mapValue ρ (acc.erase p)) =
  max (nodeVarsValue ρ p original) (mapValue ρ (acc.erase p))

max (nodeValue ρ p (acc.minimize p original)) (mapValue ρ (acc.erase p)) =
  mapValue ρ acc
```

Prove the semantic PAIR simultaneously over the actual fixed acc.toList
fold. Let C=mapValue ρ (acc.erase p). An external row q≠p is bounded by C;
existing whole-node union and the new gated-variable union absorb that bound.
The actual self row equals original by lookup uniqueness; use the variable
invariant and original WF for safe constant removal. Initialize via actual
mapValue_decompose and lookup. Do not use union with a map containing old self.
No sortedness or residual≤original premise is needed for semantic steps.

Public `minimize_missing_lookup_counterexample`: empty acc, p=[], original
constant one; minimized nonstored node still has value one but map value is
zero. This demonstrates the actual original-entry lookup requirement.

## Actual replacement, outer fold and final map (#438)

Import MapMinimize and needed fixed proof APIs. A reducible proof abbreviation
is allowed for the EXACT actual outer-step source expression:

```lean
def minimizeEntry (acc : NormLevel) (p : List Name) (original : Node) : NormLevel :=
  let n := acc.minimize p original
  if n.isEmpty then acc.erase p else acc.insert p n
```

This factors the real source expression; it must be definitionally linked to
the actual NormLevel.subsumption fold, never a replacement algorithm.
Required public APIs:

- `minimizeEntry_value`: MapWF acc and actual original lookup imply exact
  mapValue preservation.
- `minimizeEntry_WF`: the same premises imply MapWF preservation.
- `minimizeEntry_get_other`: q≠p implies output lookup q equals input lookup,
  without semantic premises.
- `subsumption_value`: MapWF acc implies
  mapValue ρ acc.subsumption = mapValue ρ acc.
- `subsumption_WF`: MapWF acc implies MapWF acc.subsumption.

Generalize a list-fold theorem with distinct source keys and each remaining
row's CURRENT lookup equal to its original node. Updates touch only the
visited key; distinct keys preserve remaining lookups. Specialize to the
ORIGINAL map.toList via actual Std lookup/membership/nodup APIs. Do not
re-enumerate a newly generated list after each step or assume the fold result.

Public witnesses (agree exact names with #439):
`subsumption_needs_WF_counterexample` at singleton []↦{const1,var[u+0]}, ρu=0,
where actual subsumption changes one to zero; and
`subsumption_erases_dominated_row` with actual distinct-key deletion.

NormalizeFinal imports frozen NormalizeValue and new MapSubsumption.
Required top APIs about ACTUAL unchanged Normalize.normalize:

```lean
theorem normalize_value (ρ : Valuation) (l : Level) (h : NoMVars l) :
  mapValue ρ (Normalize.normalize l) = levelValue ρ l
theorem normalize_WF (l : Level) : MapWF (Normalize.normalize l)
```

Include `normalize_value_witness` for an actual imax expression. Reuse proved
normalizeAux_empty_value/WF, then actual subsumption. No cached-domain,
canonicality, .le/reification, whole-checker or compiler/native claim.

## Fixed genuine acceptance (#439)

Exact minimal 19-module order: Lean4Lean.List, Lean4Lean.Level, followed by
Lean4LeanLevel42.Semantics, ListProofs, Subsumption, OrderedInsert, VarInsert,
NameOrder, MapValue, MapHelpers, PathPriming, LevelSemantics,
NormalizeInvariants, NormalizeValue, NodeResidual, MapMinimize,
MapSubsumption, NormalizeFinal, then aggregate Lean4LeanNormalization42.
All helper module names have the Lean4LeanLevel42 prefix. Prior aggregate
and SortedKeys modules are outside this new imported closure.

Closed Packet::Normalization, label lean4lean_normalization42, module
Lean4LeanNormalization42, projection Lean4LeanNormalization42Kernel, scope
audited_normalization_map_denotation, marker LEAN4LEAN_NORMALIZATION42_EVIDENCE.
Required guards include contracted APIs/witnesses; unchanged extraction
audits EVERY actual public authored root plus ALL actual aliases. Preserve
existing128-root batching, standard-three policy and finite300s budgets.

ALL FIVE old catalogs/specifications and complete POLICY-through-EOF
validator/control/policy/cap bytes remain unchanged. Add ONLY the new static
packet and one copy_test_cwd entry for test/lean4lean_normalization42. Keep
all actual provenance/source/original/projection/partition/alias/axiom and
missing/corrupt/changed-source/forbidden-alias controls; no arbitrary module,
expected-answer or weaker-policy override. Coordinator runs ALL SIX genuine
matrices plus strict Clippy, fmt/Bazel/whitespace/source associations; no
guessed budget/scheduling increase. Existing implementation-proof audit
status stays unchanged; this is actual normalization-map denotation/WF only.
