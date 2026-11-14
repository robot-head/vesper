import Lean4LeanLevel42.NodeResidual
import Lean4LeanLevel42.MapValue

/-! The actual fixed-map minimization fold preserves both its variable and
whole-node denotations relative to the map with the visited key erased. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

/-- Every actual comparison only removes variables from the residual. -/
theorem minimize_fold_var_sublist (p : List Name)
    (rows : List (List Name × Node)) (original : Node) :
    (rows.foldl (fun n row => Node.subsume p n row.1 row.2) original).var.Sublist
      original.var := by
  induction rows generalizing original with
  | nil => exact List.Sublist.refl _
  | cons row rows ih =>
    exact (ih (Node.subsume p original row.1 row.2)).trans
      (node_subsume_var_sublist p row.1 original row.2)

/-- The unchanged source minimizer cannot introduce a variable. -/
theorem minimize_var_sublist (acc : NormLevel) (p : List Name) (original : Node) :
    (acc.minimize p original).var.Sublist original.var := by
  unfold NormLevel.minimize
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact minimize_fold_var_sublist p acc.toList original

/-- Well-formedness follows from variable removal alone; lookup is unnecessary. -/
theorem minimize_WF (acc : NormLevel) (p : List Name) (original : Node)
    (hw : NodeWF p original) : NodeWF p (acc.minimize p original) := by
  intro v hv
  exact hw v ((minimize_var_sublist acc p original).subset hv)

/-- A comparison at the identical key retains the current residual variables. -/
theorem minimize_self_var (p : List Name) (residual original : Node) :
    (Node.subsume p residual p original).var = residual.var := by
  unfold Node.subsume
  split
  · simp [Node.subsumeBy]
    split <;> rfl
  · rfl

/-- A fixed source list carries paired variable and whole-value invariants.
Every source row is a genuine lookup in the fixed map. -/
theorem minimize_fold_context (ρ : Valuation) (acc : NormLevel)
    (p : List Name) (original : Node) (hw : MapWF acc)
    (hget : acc.get? p = some original)
    (rows : List (List Name × Node))
    (hrows : ∀ row ∈ rows, acc.get? row.1 = some row.2)
    (residual : Node)
    (hv : max (nodeVarsValue ρ p residual) (mapValue ρ (acc.erase p)) =
      max (nodeVarsValue ρ p original) (mapValue ρ (acc.erase p)))
    (hn : max (nodeValue ρ p residual) (mapValue ρ (acc.erase p)) =
      mapValue ρ acc) :
    max (nodeVarsValue ρ p
      (rows.foldl (fun n row => Node.subsume p n row.1 row.2) residual))
        (mapValue ρ (acc.erase p)) =
      max (nodeVarsValue ρ p original) (mapValue ρ (acc.erase p)) ∧
    max (nodeValue ρ p
      (rows.foldl (fun n row => Node.subsume p n row.1 row.2) residual))
        (mapValue ρ (acc.erase p)) = mapValue ρ acc := by
  induction rows generalizing residual with
  | nil => exact ⟨hv, hn⟩
  | cons row rows ih =>
    rcases row with ⟨q, n⟩
    have hrow : acc.get? q = some n := hrows (q, n) (by simp)
    have hrest : ∀ row ∈ rows, acc.get? row.1 = some row.2 := by
      intro row hmem
      exact hrows row (by simp [hmem])
    have hstep :
        max (nodeVarsValue ρ p (Node.subsume p residual q n))
            (mapValue ρ (acc.erase p)) =
          max (nodeVarsValue ρ p residual) (mapValue ρ (acc.erase p)) ∧
        max (nodeValue ρ p (Node.subsume p residual q n))
            (mapValue ρ (acc.erase p)) =
          max (nodeValue ρ p residual) (mapValue ρ (acc.erase p)) := by
      by_cases he : p = q
      · subst q
        have heNode : n = original := Option.some.inj (hrow.symm.trans hget)
        subst n
        constructor
        · simp only [nodeVarsValue, minimize_self_var]
        · apply node_subsume_same_context ρ p original residual
            (mapValue ρ (acc.erase p)) (hw p original hget)
          omega
      · have hother : (acc.erase p).get? q = some n := by
          rw [map_get_erase, if_neg he]
          exact hrow
        have hb := nodeValue_le_mapValue (ρ := ρ) hother
        have hvar := node_subsume_vars_union ρ p q residual n
        have hnode := node_subsume_union ρ p q residual n (hw q n hrow)
        constructor <;> omega
    exact ih hrest (Node.subsume p residual q n)
      (hstep.1.trans hv) (hstep.2.trans hn)

/-- Actual minimization preserves variables in the fixed erased-key context. -/
theorem minimize_vars_context (ρ : Valuation) (acc : NormLevel)
    (p : List Name) (original : Node) (hw : MapWF acc)
    (hget : acc.get? p = some original) :
    max (nodeVarsValue ρ p (acc.minimize p original)) (mapValue ρ (acc.erase p)) =
      max (nodeVarsValue ρ p original) (mapValue ρ (acc.erase p)) := by
  have hn : max (nodeValue ρ p original) (mapValue ρ (acc.erase p)) =
      mapValue ρ acc := by
    rw [mapValue_decompose ρ acc p, hget]
    simp [Nat.max_comm]
  have hpair := minimize_fold_context ρ acc p original hw hget acc.toList
    (fun row hmem => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hmem) original rfl hn
  unfold NormLevel.minimize
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact hpair.1

/-- The actual stored original is preserved relative to the erased-key map. -/
theorem minimize_value_context (ρ : Valuation) (acc : NormLevel)
    (p : List Name) (original : Node) (hw : MapWF acc)
    (hget : acc.get? p = some original) :
    max (nodeValue ρ p (acc.minimize p original)) (mapValue ρ (acc.erase p)) =
      mapValue ρ acc := by
  have hn : max (nodeValue ρ p original) (mapValue ρ (acc.erase p)) =
      mapValue ρ acc := by
    rw [mapValue_decompose ρ acc p, hget]
    simp [Nat.max_comm]
  have hpair := minimize_fold_context ρ acc p original hw hget acc.toList
    (fun row hmem => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hmem) original rfl hn
  unfold NormLevel.minimize
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact hpair.2

/-- Without a real original lookup, the minimizer need not denote the map. -/
theorem minimize_missing_lookup_counterexample :
    let acc : NormLevel := {}
    let original : Node := { const := 1 }
    let ρ : Valuation := fun _ => 0
    acc.get? [] = none ∧
      nodeValue ρ [] (acc.minimize [] original) = 1 ∧
      mapValue ρ acc = 0 ∧
      nodeValue ρ [] (acc.minimize [] original) ≠ mapValue ρ acc := by
  exact ⟨rfl, rfl, rfl, by decide⟩

end Vesper.Lean4LeanLevel42
