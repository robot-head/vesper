# Actual level-comparison soundness (#444–#448)

Frozen dependent base: PR #442, `94cee47a8af4a549f324dd2016079d3f67a6e51c`.
The design document metadata remains 2026-11-13T04:46:25.948Z. Before this
claim the coordinator reviewed all 47 open issues and actual changed-file
lists for all 24 open PRs. External native #346/#384 stays untouched.

This wave proves soundness of the actual `NormLevel.le` and `Lean.Level.geq'`.
It does not prove comparison completeness, canonicality, reification,
whole-checker correctness or compiler/native equivalence. `Level.beq` is an
opaque extern; no axiom or unchecked fast-path bridge is introduced and no
unconditional `isEquiv'` soundness claim is made.

## Disjoint ownership and frozen APIs

All public declarations use namespace `Vesper.Lean4LeanLevel42` and ASCII
names. Existing helper/vendor/parent proofs and runtime/auditor/profile/Cargo
sources are immutable. Every lane owns its new module and one matching report.
The coordinator owns this contract, aggregate, integration and final evidence.

### #445: MapAll.lean

Import only `Lean4LeanLevel42.MapValue`. Prove the actual traversal/lookup
bridge; the first two binder groups below are explicit:

```lean
norm_all_eq_toList_all (s : NormLevel) (f : List Name → Node → Bool) :
  s.all f = s.toList.all (fun row => f row.1 row.2)

norm_all_eq_true_iff (s : NormLevel) (f : List Name → Node → Bool) :
  s.all f = true ↔ ∀ p n, s.get? p = some n → f p n = true

norm_all_lookup {s : NormLevel} {f : List Name → Node → Bool}
  {p : List Name} {n : Node}
  (hall : s.all f = true) (h : s.get? p = some n) : f p n = true
```

Pinned Std exposes forIn/toList bridges rather than an all/toList theorem.
Handle the actual dependent versus constant-value iteration and the real
short circuit. A new assumed equivalence or replacement map is unacceptable.
Report: `docs/rfcs/lean4lean-comparison42-all.md`.

### #446: CompareFold.lean

Import only `Lean4LeanLevel42.NodeResidual`. The exact abbreviation is:

```lean
abbrev compareStep (p : List Name) (n : Node)
    (q : List Name) (m : Node) : Option Node :=
  if Lean.Level.Normalize.subset Name.cmp q p then
    let r := n.subsumeBy false m
    if r.isEmpty then none else some r
  else some n
```

Explicit `Name.cmp` is definitionally the actual captured local comparator.
Freeze these explicit binders and names:

```lean
compareStep_none_bound (ρ : Valuation) (p q : List Name) (n m : Node)
  (hw : NodeWF q m) (h : compareStep p n q m = none) :
  nodeValue ρ p n ≤ nodeValue ρ q m

compareStep_some_union (ρ : Valuation) (p q : List Name) (n m r : Node)
  (hw : NodeWF q m) (h : compareStep p n q m = some r) :
  max (nodeValue ρ p r) (nodeValue ρ q m) =
    max (nodeValue ρ p n) (nodeValue ρ q m)

compareFold_none_bound (ρ : Valuation) (p : List Name)
  (rows : List (List Name × Node)) (n : Node) (B : Nat)
  (hw : ∀ row ∈ rows, NodeWF row.1 row.2)
  (hb : ∀ row ∈ rows, nodeValue ρ row.1 row.2 ≤ B)
  (h : rows.foldlM (fun r row => compareStep p r row.1 row.2) n = none) :
  nodeValue ρ p n ≤ B
```

No source-node WF, sorting, map or priming premise. Prove the real Option
early exit by induction, using actual node union and empty-value facts.
Do not substitute minimization. Report: `lean4lean-comparison42-fold.md`.

### #447: MapComparison.lean

Import MapAll, CompareFold and NormalizeFinal. Freeze final APIs:

```lean
norm_le_sound (ρ : Valuation) (left right : NormLevel)
  (hw : MapWF right) (h : left.le right = true) :
  mapValue ρ left ≤ mapValue ρ right

geq_sound (ρ : Valuation) (u v : Level) (hu : NoMVars u) (hv : NoMVars v)
  (h : Lean.Level.geq' u v = true) : levelValue ρ v ≤ levelValue ρ u
```

No left WF or sorting premise. Actual lookup supplies each right-row WF and
its bound in the unchanged right map. Use actual `foldlM_eq_foldlM_toList`,
the frozen traversal/fold APIs and actual normalize value/WF.

Required public witnesses (their exact closed proposition may use local lets):

- `norm_le_needs_right_WF_counterexample`: left singleton empty key/constant
  one, right singleton empty key/zero-offset variable, zero valuation.
  Left WF holds, right WF fails, actual le succeeds and values are one/zero.
- `norm_le_split_dominators`: left row at `[u]` has constant two and `u+0`;
  right has constant two at `[]` and `u+0` at `[u]`. Show actual successful
  comparison, WF and genuine discharge by distinct rows.
- `norm_le_raw_empty_row_incomplete`: explicit empty row on the left versus
  empty right map; WF and semantic inequality hold but actual le is false.
- `geq_split_dominators`: actual geq' succeeds for upper `max 2 u` and lower
  `imax 2 u`, with structural NoMVars and a semantic consequence.

Report: `docs/rfcs/lean4lean-comparison42-maps.md`. Helpers may be added in
the owned module; all actual public theorem roots will be audited.

### #448: fixed acceptance packet

Own new `tests/lean4lean_comparison42.rs`, matching acceptance report and a
narrow seventh static `Packet::Comparison` arm in the shared runner. Preserve
ALL SIX older catalogs/specifications and the complete `const POLICY` suffix
byte-for-byte (47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`).

Fixed label `lean4lean_comparison42`, aggregate `Lean4LeanComparison42`,
projection `Lean4LeanComparison42Kernel`, scope
`audited_level_comparison_soundness`, marker `LEAN4LEAN_COMPARISON42_EVIDENCE`.
The required catalog contains the twelve named APIs/witnesses above. Guards
only ensure presence; derive and audit EVERY actual authored root and ALL
actual aliases with existing 128-root batching and standard-three policy.

Fixed 22-module source order: Lean4Lean.List, Lean4Lean.Level, Semantics,
ListProofs, Subsumption, OrderedInsert, VarInsert, NameOrder, MapValue,
MapHelpers, PathPriming, LevelSemantics, NormalizeInvariants, NormalizeValue,
NodeResidual, MapMinimize, MapSubsumption, NormalizeFinal, MapAll, CompareFold,
MapComparison, Lean4LeanComparison42. Unqualified helper module names here
have prefix `Lean4LeanLevel42.`. The previous aggregate is not imported.

Add only its BUILD `copy_test_cwd` label and the same #443 macOS-only
compatibility select. No runtime proof selector, expected answer or weaker
policy flag. Every actual source/provenance/artifact/partition/audit/refusal
control, cap and finite 300-second budget is immutable.

## Execution and completion

Each writer works in an isolated worktree. At most two Lean compiler leases
exist; writers compile only after coordinator grant, with pinned Lean 4.24,
one thread, `-j1 -M4096`, genuine immutable dependency roots and source/object
hash manifests. No writer runs Cargo. No new proof source may contain sorry,
admit, project axioms, unsafe, partial or native_decide. Generated Lean must
use the existing AST and one render; fixed handwritten modules are allowed.

Root runs all seven genuine matrices with the exclusive shared Cargo lease,
strict targeted Clippy, formatting, BUILD synchronization, whitespace and
source associations. Any inherited #434 compatibility dependency is recorded
honestly. Push an integrated reviewable checkpoint; no auto-merge onto the
unprotected parent. Required protected-main CI remains enforced.
