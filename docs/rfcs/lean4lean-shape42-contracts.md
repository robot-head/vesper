# Normalized-map equality and shape (#499–#503)

Base: own #449 at `8b99003edd8aa964020471291bf81abb89cc0ce8`.
Before edits, all 80 open issue scopes and actual filenames of all 28 open
PRs were reviewed. The design revision is unchanged at
2026-11-13T04:46:25.948Z. This work is independent of external source/reference,
metatheory, compiler/extractor and Wave 8 implementation lanes.

The actual `isEquiv'` first tests opaque native `Lean.Level.beq`. No available
logical reflection theorem justifies that branch, and this work will not add
an axiom or claim complete `isEquiv'` soundness. The actual map equality branch
is transparent and can be proved separately. Completeness, canonicality and
reification remain separate goals.

Root-owned `Shape.lean` freezes these predicates in
`Vesper.Lean4LeanLevel42`:

```text
MapKeysSorted s := ∀ p n, s.get? p = some n → KeySorted p
MapVarsSorted s := ∀ p n, s.get? p = some n → VarsSorted n.var
MapNoEmpty s := ∀ p n, s.get? p = some n → n.isEmpty = false
```

`KeySorted` and `VarsSorted` are the existing strict prefix-first Name.cmp
pairwise predicates. No predicate is defined by a supplied acceptance flag,
expected trace, semantic hypothesis or physical TreeMap equality.

- #500 owns NEW `MapEquality.lean` and `*-equality.md`: actual VarNode and
  Node Boolean equality reflection, actual NormLevel Boolean equality iff
  pointwise get? equality, and mapValue equality. No map WF/sorting premise.
- #501 owns NEW `ConstructionShape.lean` and `*-construction.md`: genuine
  update and recursive construction proofs. Target public API:
  `normalizeAux_keysSorted l p k acc` under KeySorted p and MapKeysSorted acc;
  `normalizeAux_varsSorted l p k acc` under MapVarsSorted acc. The sorted
  current path must be carried through the actual levelSize recursion.
- #502 owns NEW `SubsumptionShape.lean` and `*-subsumption.md`: actual pruning,
  minimization and outer-fold shape. Target public API:
  `subsumption_keysSorted s`, `subsumption_varsSorted s` under their respective
  initial invariants, and unconditional `subsumption_noEmpty s` if valid.
  Arbitrary raw maps and duplicate/empty source rows must not be silently
  excluded from generic helper claims. Existing MapWF is a separate property.
- #503 owns additive fixed Packet::Shape support in the existing shared runner
  and NEW `*-acceptance.md`. The seven old specs/catalogs and the complete
  validator suffix remain byte-identical. Root owns NEW fixed aggregator,
  test target, narrow BUILD scheduling/copy entries and integration report.

The final theorem module is `Lean4LeanLevel42.ShapeFinal`, matching the
unchanged collector's fixed proof-module namespace. The projection module
is `Lean4LeanShape42Kernel`. A top-level module with authored theorems would
be rejected by required-root inventory and is not used.

The final aggregator combines actual full-normalization sorted keys, sorted
variable lists and no empty rows for every Level. Actual normalized-map BEq
implies equal levelValue under the existing structural NoMVars premises.
Neither theorem describes whole-checker/compiler verification.

The new packet uses its minimal actual dependency closure of 24 modules,
preserving the existing hard source-count bound. `Shape` imports SortedKeys,
NormalizeValue and MapSubsumption. The final semantic bridge composes the
existing normalizeAux and subsumption denotation theorems directly. It does
not import the redundant NormalizeFinal, Comparison or older aggregators.
All seven earlier fixed packets and their complete root catalogs remain
unchanged and are verified separately. Root's final public API is
`normalize_keysSorted`, `normalize_varsSorted`, `normalize_noEmpty`,
`normalize_shape` and `normalized_beq_value`.

No existing proof, model/metatheory, runtime/native/compiler, kernel/profile,
extractor/auditor/projector or Cargo source changes. New sources use no sorry,
admit, project axioms, unsafe, partial or native_decide. Fixed handwritten
Lean modules are allowed; generated Lean uses AST plus one render.

Use isolated worktrees and immutable versioned dependencies. Root grants
exclusive Cargo and at most two Lean compiler leases. Audit every actual
authored root and alternate-proof alias using genuine original objects and
the existing exact kernel projection. Preserve all prior negative controls,
policy, budgets and seven complete matrices. Retain failures before retrying.
The own stacked PR targets unprotected #449 and must not auto-merge there.
